use crate::trust_boundary::{is_link_like, is_lower_hex_256, sync_directory};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, Metadata, OpenOptions},
    io::{self, ErrorKind, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
#[cfg(windows)]
use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};

const COPY_BUFFER_BYTES: usize = 64 * 1024;

#[cfg(test)]
std::thread_local! {
    static ROOT_PATH_VALIDATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn reset_root_path_validations() {
    ROOT_PATH_VALIDATIONS.with(|count| count.set(0));
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn root_path_validations() -> usize {
    ROOT_PATH_VALIDATIONS.with(std::cell::Cell::get)
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedPayload {
    pub content_hash: String,
    pub byte_size: u64,
    pub physical_key: String,
    pub deduplicated: bool,
    pub directory_entries_synced: bool,
}

pub(crate) struct StagedPayload {
    #[cfg(test)]
    body_identity: super::body_io::OwnedIdentity,
    verified_file: File,
    staging: StagingFile,
    pub(super) repository_root: PathBuf,
    identity: ExactFileIdentity,
    pub(super) content_hash: String,
    pub(super) byte_size: u64,
    directory_entries_synced: bool,
}

#[derive(Debug)]
pub struct PayloadCas {
    repository_root: PathBuf,
}

/// Read-only batching for GC preview. Every batch revalidates its fixed ancestry and touched
/// shards before and after checking each object, while deletion continues to use `PayloadCas`.
pub(crate) struct PayloadCasReadScan {
    cas: PayloadCas,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExactObjectUnlink {
    Missing,
    Removed { directory_entries_synced: bool },
}

struct StagingFile {
    path: PathBuf,
    parent: PathBuf,
    owned: bool,
}

impl StagingFile {
    /// Removes the file and reports whether that changed its directory, which
    /// the caller then syncs.
    fn remove(&mut self) -> io::Result<bool> {
        if !self.owned {
            return Ok(false);
        }
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.owned = false;
        Ok(true)
    }

    fn remove_and_sync(&mut self) -> io::Result<bool> {
        if !self.remove()? {
            return Ok(true);
        }
        sync_directory(&self.parent)
    }
}

impl Drop for StagingFile {
    fn drop(&mut self) {
        if self.owned && fs::remove_file(&self.path).is_ok() {
            let _ = sync_directory(&self.parent);
        }
    }
}

impl PayloadCas {
    pub(crate) fn repository_root(&self) -> &Path {
        &self.repository_root
    }

    pub fn new(repository_root: impl AsRef<Path>) -> io::Result<Self> {
        let repository_root = absolute_path(repository_root.as_ref())?;
        reject_link_components(&repository_root)?;
        let metadata = fs::symlink_metadata(&repository_root)?;
        ensure_real_directory(&repository_root, &metadata)?;
        let repository_root = fs::canonicalize(repository_root)?;
        let cas = Self { repository_root };
        cas.ensure_repository_root()?;
        Ok(cas)
    }

    pub(crate) fn into_read_scan(self) -> PayloadCasReadScan {
        PayloadCasReadScan { cas: self }
    }

    pub fn prepare_bytes(&self, data: &[u8]) -> Result<PreparedPayload, io::Error> {
        self.prepare_reader(&mut io::Cursor::new(data))
    }

    pub(crate) fn create_ipc_staging_file(&self) -> io::Result<tempfile::NamedTempFile> {
        self.ensure_repository_root()?;
        let mut directory_entries_synced = true;
        let assets_directory = self.ensure_directory(
            &self.repository_root,
            "assets",
            &mut directory_entries_synced,
        )?;
        let staging_directory =
            self.ensure_directory(&assets_directory, "staging", &mut directory_entries_synced)?;
        tempfile::NamedTempFile::new_in(staging_directory)
    }

    pub fn prepare_reader(&self, reader: &mut impl Read) -> Result<PreparedPayload, io::Error> {
        self.prepare_reader_inner(reader, None)
    }

    pub(crate) fn prepare_reader_expected(
        &self,
        reader: &mut impl Read,
        expected_content_hash: &str,
        expected_byte_size: u64,
    ) -> Result<PreparedPayload, io::Error> {
        validate_content_hash(expected_content_hash)?;
        self.prepare_reader_inner(reader, Some((expected_content_hash, expected_byte_size)))
    }

    /// Adopt an immutable payload produced in this repository's private import
    /// staging. Revalidate bytes and identity, but do not rewrite or fsync the
    /// payload a second time. The parser already synced it before returning.
    pub(crate) fn adopt_import_payload(
        &self,
        path: &Path,
        expected_hash: &str,
        expected_size: u64,
        cancelled: &impl Fn() -> bool,
    ) -> io::Result<PreparedPayload> {
        validate_content_hash(expected_hash)?;
        self.ensure_repository_root()?;
        reject_link_components(path)?;
        let canonical = fs::canonicalize(path)?;
        let jobs = self.repository_root.join("native-file-jobs").join("jobs");
        if !canonical.starts_with(&jobs)
            || !matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("payload" | "stage")
            )
        {
            return invalid_owned_path(path, "only private import payloads may be adopted");
        }
        #[cfg(test)]
        let mut observed_identity = super::body_io::PendingOwnedIdentity::new();
        let mut file = self.open_exact_owned_file(&canonical)?;
        let identity = exact_file_identity(&file)?;
        #[cfg(any(unix, windows))]
        if identity.links != 1 {
            return invalid_owned_path(path, "import staging must have exactly one link");
        }
        if identity.byte_size() != expected_size {
            return collision_or_corruption(expected_hash);
        }
        let mut hash = Sha256::new();
        #[cfg(test)]
        crate::persistent_store::hash_work::begin("cas_import_adopt");
        #[cfg(test)]
        super::body_io::body_sha_begin("owned", "cas_import_adopt");
        let mut buffer = [0_u8; COPY_BUFFER_BYTES];
        loop {
            if cancelled() {
                return Err(io::Error::other("import cancelled"));
            }
            let read = file.read(&mut buffer);
            #[cfg(test)]
            super::body_io::read_result("owned", &read);
            let length = read?;
            if length == 0 {
                break;
            }
            hash.update(&buffer[..length]);
            #[cfg(test)]
            crate::persistent_store::hash_work::update("cas_import_adopt", length);
            #[cfg(test)]
            super::body_io::body_sha_update("owned", "cas_import_adopt", length);
        }
        let content_hash = hex::encode(hash.finalize());
        if content_hash != expected_hash || exact_file_identity(&file)? != identity
        {
            return collision_or_corruption(expected_hash);
        }
        #[cfg(test)]
        observed_identity.verified(&content_hash);
        // Check the path still names the held, verified file before publishing.
        let path_file = self.open_exact_owned_file(&canonical)?;
        if exact_file_identity(&path_file)? != identity {
            return exact_object_changed();
        }
        let mut directory_entries_synced = true;
        let assets = self.ensure_directory(
            &self.repository_root,
            "assets",
            &mut directory_entries_synced,
        )?;
        let objects = self.ensure_directory(&assets, "objects", &mut directory_entries_synced)?;
        let shard =
            self.ensure_directory(&objects, &expected_hash[..2], &mut directory_entries_synced)?;
        let object_path = shard.join(&expected_hash[2..]);
        let physical_key = object_physical_key(expected_hash);
        if cancelled() {
            return Err(io::Error::other("import cancelled"));
        }
        #[cfg(any(target_os = "android", windows))]
        let published = crate::trust_boundary::rename_without_replace(&canonical, &object_path);
        #[cfg(not(any(target_os = "android", windows)))]
        let published = fs::hard_link(&canonical, &object_path);
        #[cfg(all(test, any(target_os = "android", windows)))]
        super::body_io::publication_result("rename", &published, expected_size);
        #[cfg(all(test, not(any(target_os = "android", windows))))]
        super::body_io::publication_result("hard-link", &published, expected_size);
        let deduplicated = match published {
            Ok(()) => false,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                self.verify_existing_object(
                    &object_path,
                    expected_hash,
                    expected_size,
                    &physical_key,
                )?;
                true
            }
            Err(error) => return Err(error),
        };
        directory_entries_synced &= sync_directory(&shard)?;
        // Both held handles deny writes on Windows. Drop them before unlinking
        // the staging name; the CAS name is now durable and never overwritten.
        drop(path_file);
        drop(file);
        #[cfg(any(target_os = "android", windows))]
        if deduplicated {
            fs::remove_file(&canonical)?;
        }
        #[cfg(not(any(target_os = "android", windows)))]
        fs::remove_file(&canonical)?;
        directory_entries_synced &= sync_directory(canonical.parent().expect("payload parent"))?;
        Ok(PreparedPayload {
            content_hash: expected_hash.to_owned(),
            byte_size: expected_size,
            physical_key,
            deduplicated,
            directory_entries_synced,
        })
    }

    fn prepare_reader_inner(
        &self,
        reader: &mut impl Read,
        expected: Option<(&str, u64)>,
    ) -> Result<PreparedPayload, io::Error> {
        let staged = self.stage_reader_inner(reader, expected)?;
        self.publish_staged(staged)
    }

    pub(crate) fn stage_reader_expected(
        &self,
        reader: &mut impl Read,
        expected_content_hash: &str,
        expected_byte_size: u64,
    ) -> io::Result<StagedPayload> {
        validate_content_hash(expected_content_hash)?;
        self.stage_reader_inner(reader, Some((expected_content_hash, expected_byte_size)))
    }

    fn stage_reader_inner(
        &self,
        reader: &mut impl Read,
        expected: Option<(&str, u64)>,
    ) -> io::Result<StagedPayload> {
        self.ensure_repository_root()?;
        let mut directory_entries_synced = true;
        let assets_directory = self.ensure_directory(
            &self.repository_root,
            "assets",
            &mut directory_entries_synced,
        )?;
        let staging_directory =
            self.ensure_directory(&assets_directory, "staging", &mut directory_entries_synced)?;
        let staging_path = staging_directory.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let (mut file, staging) = create_staging_file(&staging_path, &staging_directory)?;
        #[cfg(test)]
        let mut observed_identity = super::body_io::PendingOwnedIdentity::new();
        let mut hasher = Sha256::new();
        #[cfg(test)]
        crate::persistent_store::hash_work::begin("cas_stage");
        #[cfg(test)]
        super::body_io::body_sha_begin("owned", "cas_stage");
        let mut byte_size = 0_u64;
        let mut buffer = [0_u8; COPY_BUFFER_BYTES];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            #[cfg(not(test))]
            file.write_all(&buffer[..read])?;
            #[cfg(test)]
            {
                let written = file.write_all(&buffer[..read]);
                super::body_io::staging_write_result(&written, read);
                written?;
            }
            hasher.update(&buffer[..read]);
            #[cfg(test)]
            crate::persistent_store::hash_work::update("cas_stage", read);
            #[cfg(test)]
            super::body_io::body_sha_update("owned", "cas_stage", read);
            byte_size = byte_size
                .checked_add(read as u64)
                .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "payload size overflow"))?;
        }
        file.flush()?;
        file.sync_all()?;
        let identity = exact_file_identity(&file)?;
        drop(file);
        let opened = self.open_exact_owned_file(&staging_path);
        #[cfg(test)]
        super::body_io::identity_metadata_open_result(&opened);
        let verified_file = opened?;
        if exact_file_identity(&verified_file)? != identity {
            return exact_object_changed();
        }
        #[cfg(test)]
        super::body_io::verified_identity_metadata_open();
        directory_entries_synced &= sync_directory(&staging_directory)?;

        let content_hash = hex::encode(hasher.finalize());
        if let Some((expected_content_hash, expected_byte_size)) = expected {
            if content_hash != expected_content_hash || byte_size != expected_byte_size {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "payload does not match its expected content hash and size",
                ));
            }
        }
        #[cfg(test)]
        observed_identity.verified(&content_hash);
        Ok(StagedPayload {
            #[cfg(test)]
            body_identity: super::body_io::OwnedIdentity::new(&content_hash),
            verified_file,
            staging,
            repository_root: self.repository_root.clone(),
            identity,
            content_hash,
            byte_size,
            directory_entries_synced,
        })
    }

    pub(crate) fn publish_staged(&self, staged: StagedPayload) -> io::Result<PreparedPayload> {
        #[cfg(test)]
        staged.body_identity.check_scope();
        self.check_staged_repository(&staged)?;
        self.ensure_repository_root()?;
        let (mut prepared, mut staging) = self.link_staged(staged, &mut sync_directory)?;
        prepared.directory_entries_synced &= staging.remove_and_sync()?;
        Ok(prepared)
    }

    /// Publishes staged payloads together. Every directory the batch changed is
    /// synced once, after all of its entries are in place, so a batch pays per
    /// directory rather than per payload.
    pub(crate) fn publish_staged_batch(
        &self,
        staged: Vec<StagedPayload>,
    ) -> io::Result<Vec<PreparedPayload>> {
        for payload in &staged {
            #[cfg(test)]
            payload.body_identity.check_scope();
            self.check_staged_repository(payload)?;
        }
        if staged.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_repository_root()?;
        let mut changed = BTreeSet::new();
        let mut linked = Vec::with_capacity(staged.len());
        for payload in staged {
            linked.push(self.link_staged(payload, &mut |directory: &Path| {
                changed.insert(directory.to_path_buf());
                Ok(true)
            })?);
        }
        let mut synced = true;
        for directory in &changed {
            synced &= sync_directory(directory)?;
        }
        let mut staging_directories = BTreeSet::new();
        let mut published = Vec::with_capacity(linked.len());
        for (prepared, mut staging) in linked {
            if staging.remove()? {
                staging_directories.insert(staging.parent.clone());
            }
            published.push(prepared);
        }
        for directory in &staging_directories {
            synced &= sync_directory(directory)?;
        }
        for prepared in &mut published {
            prepared.directory_entries_synced &= synced;
        }
        Ok(published)
    }

    fn check_staged_repository(&self, staged: &StagedPayload) -> io::Result<()> {
        if staged.repository_root != self.repository_root {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "staged payload belongs to another repository",
            ));
        }
        Ok(())
    }

    /// Links one staged payload under its content hash, handing `sync_parent`
    /// every directory whose entries it changed. The staging file is left for
    /// the caller to remove.
    fn link_staged(
        &self,
        staged: StagedPayload,
        sync_parent: &mut dyn FnMut(&Path) -> io::Result<bool>,
    ) -> io::Result<(PreparedPayload, StagingFile)> {
        let StagedPayload {
            #[cfg(test)]
            body_identity: _body_identity,
            verified_file,
            staging,
            identity,
            content_hash,
            byte_size,
            mut directory_entries_synced,
            ..
        } = staged;
        #[cfg(test)]
        let _body_scope = super::body_io::object_scope(&content_hash);
        let opened = self.open_exact_owned_file(&staging.path);
        #[cfg(test)]
        super::body_io::identity_metadata_open_result(&opened);
        let path_file = opened?;
        if exact_file_identity(&verified_file)? != identity
            || exact_file_identity(&path_file)? != identity
        {
            return exact_object_changed();
        }
        #[cfg(test)]
        super::body_io::verified_identity_metadata_open();
        let staging_path = staging.path.clone();
        let assets_directory = self.ensure_directory_with_sync(
            &self.repository_root,
            "assets",
            &mut directory_entries_synced,
            &mut *sync_parent,
        )?;
        let physical_key = object_physical_key(&content_hash);
        let objects_directory = self.ensure_directory_with_sync(
            &assets_directory,
            "objects",
            &mut directory_entries_synced,
            &mut *sync_parent,
        )?;
        let object_directory = self.ensure_directory_with_sync(
            &objects_directory,
            &content_hash[..2],
            &mut directory_entries_synced,
            &mut *sync_parent,
        )?;
        let object_path = object_directory.join(&content_hash[2..]);

        #[cfg(target_os = "android")]
        let publication =
            crate::trust_boundary::rename_without_replace(&staging_path, &object_path);
        #[cfg(not(target_os = "android"))]
        let publication = fs::hard_link(&staging_path, &object_path);
        #[cfg(all(test, target_os = "android"))]
        super::body_io::publication_result("rename", &publication, byte_size);
        #[cfg(all(test, not(target_os = "android")))]
        super::body_io::publication_result("hard-link", &publication, byte_size);
        let deduplicated = match publication {
            Ok(()) => {
                directory_entries_synced &= sync_parent(&object_directory)?;
                false
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                self.verify_existing_object(&object_path, &content_hash, byte_size, &physical_key)?;
                directory_entries_synced &= sync_parent(&object_directory)?;
                true
            }
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("failed to publish CAS object without replacement: {error}"),
                ));
            }
        };

        drop(path_file);
        drop(verified_file);
        Ok((
            PreparedPayload {
                content_hash,
                byte_size,
                physical_key,
                deduplicated,
                directory_entries_synced,
            },
            staging,
        ))
    }

    pub fn stat_object(&self, content_hash: &str) -> io::Result<Option<u64>> {
        #[cfg(test)]
        super::body_io::stat_request();
        let Some(path) = self.existing_object_path(content_hash)? else {
            return Ok(None);
        };
        Ok(Some(fs::symlink_metadata(path)?.len()))
    }

    pub fn read_object(&self, content_hash: &str) -> io::Result<Option<Vec<u8>>> {
        #[cfg(test)]
        let _body_scope = super::body_io::object_scope(content_hash);
        let Some(path) = self.existing_object_path(content_hash)? else {
            return Ok(None);
        };
        let body = fs::read(path);
        #[cfg(test)]
        super::body_io::read_file_result(&body);
        Ok(Some(body?))
    }

    pub fn read_object_range(
        &self,
        content_hash: &str,
        start: u64,
        end_exclusive: u64,
    ) -> io::Result<Option<Vec<u8>>> {
        #[cfg(test)]
        let _body_scope = super::body_io::object_scope(content_hash);
        if end_exclusive < start {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "payload range bounds must be in ascending order",
            ));
        }
        let Some(path) = self.existing_object_path(content_hash)? else {
            return Ok(None);
        };
        let file = File::open(path);
        #[cfg(test)]
        super::body_io::open_result("managed", &file);
        let mut file = file?;
        let size = file.metadata()?.len();
        let bounded_start = start.min(size);
        let bounded_end = end_exclusive.min(size);
        let length = usize::try_from(bounded_end - bounded_start)
            .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "payload range is too large"))?;
        file.seek(SeekFrom::Start(bounded_start))?;
        let mut data = vec![0; length];
        let read = file.read_exact(&mut data);
        #[cfg(test)]
        super::body_io::read_exact_result("managed", &read, data.len());
        read?;
        Ok(Some(data))
    }

    pub fn open_object(&self, content_hash: &str) -> io::Result<Option<File>> {
        #[cfg(test)]
        let _body_scope = super::body_io::object_scope(content_hash);
        let Some(path) = self.existing_object_path(content_hash)? else {
            return Ok(None);
        };
        let file = File::open(path);
        #[cfg(test)]
        super::body_io::open_result("managed", &file);
        let file = file?;
        #[cfg(test)]
        super::body_io::escaped_handle("managed");
        Ok(Some(file))
    }

    #[cfg(test)]
    pub(crate) fn open_object_tracked(&self, content_hash: &str) -> io::Result<Option<super::body_io::TrackedBodyFile>> {
        let _body_scope = super::body_io::object_scope(content_hash);
        let Some(path) = self.existing_object_path(content_hash)? else { return Ok(None); };
        let file = File::open(path);
        super::body_io::open_result("managed", &file);
        Ok(Some(super::body_io::TrackedBodyFile::new(file?, content_hash)))
    }

    pub fn object_path(&self, content_hash: &str) -> io::Result<Option<PathBuf>> {
        #[cfg(test)]
        let _body_scope = super::body_io::object_scope(content_hash);
        let path = self.existing_object_path(content_hash);
        #[cfg(test)]
        if path.as_ref().is_ok_and(Option::is_some) {
            super::body_io::escaped_path("managed");
        }
        path
    }

    /// Whether the object is held here at `expected_size` and its bytes hash
    /// to its name. A missing object is not held. `cancelled` is asked before
    /// every read; a cancelled check is an error, not an answer.
    pub(crate) fn holds_exact_object(
        &self,
        content_hash: &str,
        expected_size: u64,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<bool> {
        #[cfg(test)]
        let _body_scope = super::body_io::object_scope(content_hash);
        let Some(path) = self.existing_object_path(content_hash)? else {
            return Ok(false);
        };
        let mut file = self.open_exact_owned_file(&path)?;
        if file.metadata()?.len() != expected_size {
            return Ok(false);
        }
        Ok(hash_open_file(&mut file, cancelled)? == content_hash)
    }

    pub(crate) fn unlink_exact_object(
        &self,
        content_hash: &str,
        expected_byte_size: u64,
        expected_physical_key: &str,
    ) -> io::Result<ExactObjectUnlink> {
        self.unlink_exact_object_inner(
            content_hash,
            expected_byte_size,
            expected_physical_key,
            |_| Ok(()),
        )
    }

    #[cfg(test)]
    fn unlink_exact_object_with_hash_hook(
        &self,
        content_hash: &str,
        expected_byte_size: u64,
        expected_physical_key: &str,
        after_hash: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<ExactObjectUnlink> {
        self.unlink_exact_object_inner(
            content_hash,
            expected_byte_size,
            expected_physical_key,
            after_hash,
        )
    }

    fn unlink_exact_object_inner(
        &self,
        content_hash: &str,
        expected_byte_size: u64,
        expected_physical_key: &str,
        after_hash: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<ExactObjectUnlink> {
        #[cfg(test)]
        let _body_scope = super::body_io::object_scope(content_hash);
        validate_content_hash(content_hash)?;
        let physical_key = object_physical_key(content_hash);
        if expected_physical_key != physical_key {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "deletion tombstone physical key is not the exact canonical object key",
            ));
        }
        let Some(path) = self.existing_object_path(content_hash)? else {
            return Ok(ExactObjectUnlink::Missing);
        };
        let mut file = self.open_exact_owned_file(&path)?;
        let identity = exact_file_identity(&file)?;
        if identity.byte_size() != expected_byte_size {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "deletion tombstone size does not match the exact canonical object",
            ));
        }
        let actual_hash = hash_open_file(&mut file, &|| false)?;
        if exact_file_identity(&file)? != identity {
            return exact_object_changed();
        }
        if actual_hash != content_hash {
            return collision_or_corruption(expected_physical_key);
        }
        after_hash(&path)?;
        let metadata = fs::symlink_metadata(&path)?;
        self.validate_owned_file(&path, &metadata)?;
        let final_file = self.open_exact_owned_file(&path)?;
        if exact_file_identity(&final_file)? != identity {
            return exact_object_changed();
        }
        fs::remove_file(&path)?;
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(
                ErrorKind::InvalidData,
                "canonical object path has no containing directory",
            )
        })?;
        Ok(ExactObjectUnlink::Removed {
            directory_entries_synced: sync_directory(parent)?,
        })
    }

    fn ensure_repository_root(&self) -> io::Result<()> {
        #[cfg(test)]
        ROOT_PATH_VALIDATIONS.with(|count| count.set(count.get() + 1));
        reject_link_components(&self.repository_root)?;
        let metadata = fs::symlink_metadata(&self.repository_root)?;
        ensure_real_directory(&self.repository_root, &metadata)?;
        let canonical = fs::canonicalize(&self.repository_root)?;
        if canonical != self.repository_root {
            return invalid_owned_path(&self.repository_root, "repository root changed");
        }
        Ok(())
    }

    fn ensure_directory(
        &self,
        parent: &Path,
        name: &str,
        directory_entries_synced: &mut bool,
    ) -> io::Result<PathBuf> {
        self.ensure_directory_with_sync(parent, name, directory_entries_synced, sync_directory)
    }

    fn ensure_directory_with_sync(
        &self,
        parent: &Path,
        name: &str,
        directory_entries_synced: &mut bool,
        mut sync_parent: impl FnMut(&Path) -> io::Result<bool>,
    ) -> io::Result<PathBuf> {
        let path = parent.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => self.validate_owned_directory(&path, &metadata)?,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                match fs::create_dir(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                let metadata = fs::symlink_metadata(&path)?;
                self.validate_owned_directory(&path, &metadata)?;
            }
            Err(error) => return Err(error),
        }
        *directory_entries_synced &= sync_parent(parent)?;
        Ok(path)
    }

    fn existing_object_path(&self, content_hash: &str) -> io::Result<Option<PathBuf>> {
        #[cfg(test)]
        super::body_io::presence_query();
        validate_content_hash(content_hash)?;
        self.ensure_repository_root()?;
        let assets_directory = self.repository_root.join("assets");
        if !self.existing_owned_directory(&assets_directory)? {
            return Ok(None);
        }
        let objects_directory = assets_directory.join("objects");
        if !self.existing_owned_directory(&objects_directory)? {
            return Ok(None);
        }
        let shard_directory = objects_directory.join(&content_hash[..2]);
        if !self.existing_owned_directory(&shard_directory)? {
            return Ok(None);
        }
        let object_path = shard_directory.join(&content_hash[2..]);
        match fs::symlink_metadata(&object_path) {
            Ok(metadata) => {
                self.validate_owned_file(&object_path, &metadata)?;
                Ok(Some(object_path))
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn existing_owned_directory(&self, path: &Path) -> io::Result<bool> {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                self.validate_owned_directory(path, &metadata)?;
                Ok(true)
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn validate_owned_directory(&self, path: &Path, metadata: &Metadata) -> io::Result<()> {
        if is_link_like(metadata) {
            return invalid_owned_path(path, "linked directory is forbidden");
        }
        ensure_real_directory(path, metadata)?;
        self.ensure_canonical_confinement(path)
    }

    fn validate_owned_file(&self, path: &Path, metadata: &Metadata) -> io::Result<()> {
        if is_link_like(metadata) {
            return invalid_owned_path(path, "linked object is forbidden");
        }
        if !metadata.is_file() {
            return invalid_owned_path(path, "content-addressed object is not a file");
        }
        self.ensure_canonical_confinement(path)
    }

    fn open_exact_owned_file(&self, path: &Path) -> io::Result<File> {
        #[cfg(test)]
        let _body_scope = {
            let hash = path.parent().and_then(Path::file_name).and_then(|name| name.to_str())
                .zip(path.file_name().and_then(|name| name.to_str()))
                .map(|(prefix, tail)| format!("{prefix}{tail}"))
                .filter(|hash| validate_content_hash(hash).is_ok()
                    && path == self.repository_root.join(object_physical_key(hash)));
            hash.as_deref().map(super::body_io::object_scope)
        };
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        #[cfg(windows)]
        {
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
            };
            options
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE);
        }
        let file = options.open(path);
        #[cfg(test)]
        super::body_io::open_result(
            if path.starts_with(self.repository_root.join("assets").join("objects")) { "managed" } else { "owned" },
            &file,
        );
        let file = match file {
            #[cfg(unix)]
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                return invalid_owned_path(path, "linked object is forbidden");
            }
            result => result?,
        };
        let metadata = file.metadata()?;
        self.validate_owned_file(path, &metadata)?;
        Ok(file)
    }

    fn ensure_canonical_confinement(&self, path: &Path) -> io::Result<()> {
        let canonical = fs::canonicalize(path)?;
        if !canonical.starts_with(&self.repository_root) {
            return invalid_owned_path(path, "path escapes the repository root");
        }
        Ok(())
    }

    fn verify_existing_object(
        &self,
        path: &Path,
        expected_hash: &str,
        expected_size: u64,
        physical_key: &str,
    ) -> io::Result<()> {
        #[cfg(test)]
        let _body_scope = super::body_io::object_scope(expected_hash);
        let mut file = self.open_exact_owned_file(path)?;
        if file.metadata()?.len() != expected_size {
            return collision_or_corruption(physical_key);
        }
        if hash_open_file(&mut file, &|| false)? != expected_hash {
            return collision_or_corruption(physical_key);
        }
        Ok(())
    }
}

impl PayloadCasReadScan {
    #[allow(dead_code)]
    pub(crate) fn read_object(&self, content_hash: &str) -> io::Result<Option<Vec<u8>>> {
        self.cas.read_object(content_hash)
    }

    pub(crate) fn stat_objects<'a>(
        &self,
        content_hashes: impl IntoIterator<Item = &'a str>,
    ) -> io::Result<Vec<Option<u64>>> {
        #[cfg(test)]
        super::body_io::batch_stat_request();
        let content_hashes = content_hashes.into_iter().collect::<Vec<_>>();
        for content_hash in &content_hashes {
            validate_content_hash(content_hash)?;
        }

        let objects_directory = self.checked_objects_directory()?;
        let Some(objects_directory) = objects_directory else {
            if self.checked_objects_directory()?.is_some() {
                return exact_scan_hierarchy_changed();
            }
            return Ok(vec![None; content_hashes.len()]);
        };
        let shards = content_hashes
            .iter()
            .map(|hash| &hash[..2])
            .collect::<BTreeSet<_>>();
        let mut shard_presence = BTreeMap::new();
        for shard in shards {
            let shard_directory = objects_directory.join(shard);
            shard_presence.insert(
                shard,
                self.cas.existing_owned_directory(&shard_directory)?,
            );
        }

        let mut sizes = Vec::with_capacity(content_hashes.len());
        for content_hash in &content_hashes {
            if !shard_presence[&content_hash[..2]] {
                sizes.push(None);
                continue;
            }
            let object_path = objects_directory
                .join(&content_hash[..2])
                .join(&content_hash[2..]);
            match fs::symlink_metadata(&object_path) {
                Ok(metadata) => {
                    self.cas.validate_owned_file(&object_path, &metadata)?;
                    sizes.push(Some(fs::symlink_metadata(&object_path)?.len()));
                }
                Err(error) if error.kind() == ErrorKind::NotFound => sizes.push(None),
                Err(error) => return Err(error),
            }
        }

        if self.checked_objects_directory()?.as_ref() != Some(&objects_directory) {
            return exact_scan_hierarchy_changed();
        }
        for (shard, present) in shard_presence {
            let shard_directory = objects_directory.join(shard);
            if self.cas.existing_owned_directory(&shard_directory)? != present {
                return exact_scan_hierarchy_changed();
            }
        }
        Ok(sizes)
    }

    fn checked_objects_directory(&self) -> io::Result<Option<PathBuf>> {
        self.cas.ensure_repository_root()?;
        let assets_directory = self.cas.repository_root.join("assets");
        if !self.cas.existing_owned_directory(&assets_directory)? {
            return Ok(None);
        }
        let objects_directory = assets_directory.join("objects");
        if !self.cas.existing_owned_directory(&objects_directory)? {
            return Ok(None);
        }
        Ok(Some(objects_directory))
    }
}

fn create_staging_file(path: &Path, parent: &Path) -> io::Result<(File, StagingFile)> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let staging = StagingFile {
        path: path.to_path_buf(),
        parent: parent.to_path_buf(),
        owned: true,
    };
    Ok((file, staging))
}

fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn reject_link_components(path: &Path) -> io::Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::CurDir
        ) {
            continue;
        }
        let metadata = fs::symlink_metadata(&current)?;
        if is_link_like(&metadata) {
            return invalid_owned_path(&current, "linked path component is forbidden");
        }
    }
    Ok(())
}

fn ensure_real_directory(path: &Path, metadata: &Metadata) -> io::Result<()> {
    if is_link_like(metadata) {
        return invalid_owned_path(path, "linked directory is forbidden");
    }
    if !metadata.is_dir() {
        return invalid_owned_path(path, "repository path is not a directory");
    }
    Ok(())
}

fn invalid_owned_path<T>(path: &Path, reason: &str) -> io::Result<T> {
    Err(io::Error::new(
        ErrorKind::InvalidData,
        format!("{reason}: {}", path.display()),
    ))
}

fn exact_scan_hierarchy_changed<T>() -> io::Result<T> {
    Err(io::Error::new(
        ErrorKind::InvalidData,
        "content-addressed object hierarchy changed during scan",
    ))
}

fn validate_content_hash(content_hash: &str) -> io::Result<()> {
    if is_lower_hex_256(content_hash) {
        return Ok(());
    }
    Err(io::Error::new(
        ErrorKind::InvalidInput,
        "content hash must be 64 lowercase hexadecimal characters",
    ))
}

pub(crate) fn object_physical_key(content_hash: &str) -> String {
    format!(
        "assets/objects/{}/{}",
        &content_hash[..2],
        &content_hash[2..]
    )
}

fn collision_or_corruption<T>(physical_key: &str) -> io::Result<T> {
    Err(io::Error::new(
        ErrorKind::InvalidData,
        format!("payload collision or corruption at {physical_key}"),
    ))
}

fn hash_open_file(file: &mut File, cancelled: &dyn Fn() -> bool) -> io::Result<String> {
    let mut hasher = Sha256::new();
    #[cfg(test)]
    crate::persistent_store::hash_work::begin("cas_existing_verify");
    #[cfg(test)]
    let mut observed_hash = super::body_io::PendingBodyHash::new("cas_existing_verify");
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    loop {
        if cancelled() {
            return Err(io::Error::other("payload verification cancelled"));
        }
        let read = file.read(&mut buffer);
        #[cfg(test)]
        super::body_io::read_result("managed", &read);
        let read = read?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        #[cfg(test)]
        crate::persistent_store::hash_work::update("cas_existing_verify", read);
        #[cfg(test)]
        observed_hash.update(read);
    }
    let content_hash = hex::encode(hasher.finalize());
    #[cfg(test)]
    observed_hash.verified(&content_hash);
    Ok(content_hash)
}

fn exact_object_changed<T>() -> io::Result<T> {
    Err(io::Error::new(
        ErrorKind::InvalidData,
        "exact canonical object changed while it was being verified for deletion",
    ))
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExactFileIdentity {
    device: u64,
    inode: u64,
    byte_size: u64,
    mode: u32,
    links: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(unix)]
impl ExactFileIdentity {
    fn byte_size(self) -> u64 {
        self.byte_size
    }
}

#[cfg(unix)]
pub(crate) fn exact_file_identity(file: &File) -> io::Result<ExactFileIdentity> {
    let metadata = file.metadata()?;
    Ok(ExactFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        byte_size: metadata.len(),
        mode: metadata.mode(),
        links: metadata.nlink(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    })
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExactFileIdentity {
    volume: u32,
    file_index: u64,
    byte_size: u64,
    links: u32,
    attributes: u32,
    creation_time: u64,
    modified_time: u64,
}

#[cfg(windows)]
impl ExactFileIdentity {
    fn byte_size(self) -> u64 {
        self.byte_size
    }
}

#[cfg(windows)]
pub(crate) fn exact_file_identity(file: &File) -> io::Result<ExactFileIdentity> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ExactFileIdentity {
        volume: information.dwVolumeSerialNumber,
        file_index: (u64::from(information.nFileIndexHigh) << 32)
            | u64::from(information.nFileIndexLow),
        byte_size: (u64::from(information.nFileSizeHigh) << 32)
            | u64::from(information.nFileSizeLow),
        links: information.nNumberOfLinks,
        attributes: information.dwFileAttributes,
        creation_time: (u64::from(information.ftCreationTime.dwHighDateTime) << 32)
            | u64::from(information.ftCreationTime.dwLowDateTime),
        modified_time: (u64::from(information.ftLastWriteTime.dwHighDateTime) << 32)
            | u64::from(information.ftLastWriteTime.dwLowDateTime),
    })
}

#[cfg(not(any(unix, windows)))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExactFileIdentity {
    byte_size: u64,
    modified: Option<std::time::SystemTime>,
}

#[cfg(not(any(unix, windows)))]
impl ExactFileIdentity {
    fn byte_size(self) -> u64 {
        self.byte_size
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn exact_file_identity(file: &File) -> io::Result<ExactFileIdentity> {
    let metadata = file.metadata()?;
    Ok(ExactFileIdentity {
        byte_size: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

#[cfg(test)]
mod tests {
    use super::{create_staging_file, ExactObjectUnlink, PayloadCas};
    use sha2::Digest;
    use std::io::Cursor;

    #[test]
    fn staged_copy_allows_repository_work_and_stays_invisible_until_publication() {
        let directory = tempfile::tempdir().unwrap();
        let cas = std::sync::Arc::new(PayloadCas::new(directory.path()).unwrap());
        let existing = cas.prepare_bytes(b"synthetic available media").unwrap();
        let bytes = vec![7_u8; super::COPY_BUFFER_BYTES * 3 + 1];
        let hash = hex::encode(sha2::Sha256::digest(&bytes));
        let (blocked_tx, blocked_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        struct Reader {
            source: Cursor<Vec<u8>>,
            blocked: Option<std::sync::mpsc::Sender<()>>,
            resume: std::sync::mpsc::Receiver<()>,
        }
        impl std::io::Read for Reader {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.source.position() > 0 {
                    if let Some(blocked) = self.blocked.take() {
                        blocked.send(()).unwrap();
                        self.resume.recv().unwrap();
                    }
                }
                std::io::Read::read(&mut self.source, buffer)
            }
        }
        let staging_worker = {
            let cas = cas.clone();
            let bytes = bytes.clone();
            let hash = hash.clone();
            std::thread::spawn(move || {
                cas.stage_reader_expected(
                    &mut Reader {
                        source: Cursor::new(bytes.clone()),
                        blocked: Some(blocked_tx),
                        resume: resume_rx,
                    },
                    &hash,
                    bytes.len() as u64,
                ).unwrap()
            })
        };
        blocked_rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        assert!(cas.stat_object(&hash).unwrap().is_none());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(10));
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let readers: Vec<_> = (0..9).map(|_| {
            let cas = cas.clone();
            let hash = existing.content_hash.clone();
            let barrier = barrier.clone();
            let done = done_tx.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let _guard = crate::asset_repository::coordinator::lock_repository_mutation().unwrap();
                let expected = b"synthetic available media";
                assert_eq!(cas.read_object_range(&hash, 0, expected.len() as u64).unwrap().unwrap(), expected);
                done.send(()).unwrap();
            })
        }).collect();
        barrier.wait();
        let completed = (0..9).all(|_| done_rx.recv_timeout(std::time::Duration::from_secs(10)).is_ok());
        assert!(cas.stat_object(&hash).unwrap().is_none());
        resume_tx.send(()).unwrap();
        let staged = staging_worker.join().unwrap();
        for reader in readers { reader.join().unwrap(); }
        assert!(completed, "repository readers must finish while staging is blocked");
        assert!(cas.stat_object(&hash).unwrap().is_none());
        let mut file = {
            let _guard = crate::asset_repository::coordinator::lock_repository_mutation().unwrap();
            cas.publish_staged(staged).unwrap();
            cas.open_object(&hash).unwrap().unwrap()
        };
        let mut actual = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut actual).unwrap();
        assert_eq!(actual, bytes);
        assert_eq!(std::fs::read_dir(directory.path().join("assets/staging")).unwrap().count(), 0);
    }

    #[test]
    fn staged_drop_failure_and_wrong_repository_remove_only_owned_temporaries() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let cas = PayloadCas::new(first.path()).unwrap();
        let other = PayloadCas::new(second.path()).unwrap();
        let bytes = b"synthetic staged bytes";
        let hash = hex::encode(sha2::Sha256::digest(bytes));
        let stage = || cas.stage_reader_expected(&mut Cursor::new(bytes), &hash, bytes.len() as u64).unwrap();
        let unrelated = first.path().join("assets/staging/unrelated.tmp");
        let staged = stage();
        std::fs::write(&unrelated, b"synthetic unrelated staging").unwrap();
        drop(staged);
        let error = other.publish_staged(stage()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(error.to_string(), "staged payload belongs to another repository");
        assert!(cas.stage_reader_expected(&mut Cursor::new(bytes), &hash, bytes.len() as u64 + 1).is_err());
        assert!(cas.stage_reader_expected(&mut Cursor::new(bytes), &"0".repeat(64), bytes.len() as u64).is_err());
        struct Fails(bool);
        impl std::io::Read for Fails {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.0 { return Err(std::io::Error::other("synthetic read failure")); }
                self.0 = true;
                buffer[0] = 1;
                Ok(1)
            }
        }
        assert!(cas.stage_reader_expected(&mut Fails(false), &hash, bytes.len() as u64).is_err());
        assert!(cas.stat_object(&hash).unwrap().is_none());
        assert!(other.stat_object(&hash).unwrap().is_none());
        assert_eq!(std::fs::read_dir(first.path().join("assets/staging")).unwrap().count(), 1);
        assert_eq!(std::fs::read(unrelated).unwrap(), b"synthetic unrelated staging");
        assert!(!second.path().join("assets").exists());
    }

    #[test]
    fn staged_duplicate_publication_verifies_same_size_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let bytes = b"synthetic staged duplicate";
        let hash = hex::encode(sha2::Sha256::digest(bytes));
        let stage = || cas.stage_reader_expected(&mut Cursor::new(bytes), &hash, bytes.len() as u64).unwrap();
        let prepared = cas.publish_staged(stage()).unwrap();
        assert!(!prepared.deduplicated);
        assert!(cas.publish_staged(stage()).unwrap().deduplicated);
        let object = directory.path().join(&prepared.physical_key);
        let corrupted = vec![b'x'; bytes.len()];
        std::fs::write(&object, &corrupted).unwrap();
        let error = cas.publish_staged(stage()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("payload collision or corruption"));
        assert_eq!(std::fs::read(object).unwrap(), corrupted);
        assert_eq!(std::fs::read_dir(directory.path().join("assets/staging")).unwrap().count(), 0);
    }

    #[test]
    fn a_staged_batch_publishes_every_payload_and_leaves_no_staging() {
        let directory = tempfile::tempdir().unwrap();
        let other_directory = tempfile::tempdir().unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let other = PayloadCas::new(other_directory.path()).unwrap();
        let bodies: [&[u8]; 3] = [b"synthetic batch one", b"synthetic batch two", b"synthetic batch three"];
        let stage = |bytes: &[u8]| {
            let hash = hex::encode(sha2::Sha256::digest(bytes));
            cas.stage_reader_expected(&mut Cursor::new(bytes), &hash, bytes.len() as u64).unwrap()
        };
        cas.publish_staged(stage(bodies[0])).unwrap();
        let published = cas.publish_staged_batch(bodies.iter().map(|bytes| stage(bytes)).collect()).unwrap();
        assert_eq!(published.iter().map(|payload| payload.deduplicated).collect::<Vec<_>>(), [true, false, false]);
        for (bytes, payload) in bodies.iter().zip(&published) {
            assert_eq!(cas.read_object(&payload.content_hash).unwrap().unwrap(), *bytes);
        }
        assert_eq!(std::fs::read_dir(directory.path().join("assets/staging")).unwrap().count(), 0);
        assert!(cas.publish_staged_batch(Vec::new()).unwrap().is_empty());
        let error = other.publish_staged_batch(vec![stage(b"synthetic foreign batch")]).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(std::fs::read_dir(directory.path().join("assets/staging")).unwrap().count(), 0);
        assert!(!other_directory.path().join("assets").exists());
    }

    #[test]
    fn staged_publication_rejects_replaced_file_identity() {
        let directory = tempfile::tempdir().unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let bytes = b"synthetic staged identity";
        let hash = hex::encode(sha2::Sha256::digest(bytes));
        let staged = cas.stage_reader_expected(&mut Cursor::new(bytes), &hash, bytes.len() as u64).unwrap();
        let staging_path = staged.staging.path.clone();
        std::fs::remove_file(&staging_path).unwrap();
        std::fs::write(&staging_path, vec![b'x'; bytes.len()]).unwrap();
        let error = cas.publish_staged(staged).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(cas.stat_object(&hash).unwrap().is_none());
        assert_eq!(std::fs::read_dir(directory.path().join("assets/staging")).unwrap().count(), 0);
    }

    #[test]
    fn read_scan_rejects_a_linked_object() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let outside = tempfile::NamedTempFile::new().expect("outside object");
        std::fs::write(outside.path(), b"outside bytes").unwrap();
        let cas = PayloadCas::new(directory.path()).expect("open repository");
        let prepared = cas.prepare_bytes(b"scan candidate").unwrap();
        let object_path = directory.path().join(&prepared.physical_key);
        std::fs::remove_file(&object_path).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), &object_path).unwrap();
        #[cfg(windows)]
        if let Err(error) = std::os::windows::fs::symlink_file(outside.path(), &object_path) {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                return;
            }
            panic!("create reparse fixture: {error}");
        }

        let scan = cas.into_read_scan();
        let error = scan
            .stat_objects([prepared.content_hash.as_str()])
            .expect_err("linked scan object must fail closed");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(outside.path()).unwrap(), b"outside bytes");
    }

    #[test]
    fn read_scan_rejects_a_repository_root_replaced_after_creation() {
        let parent = tempfile::tempdir().expect("temporary parent");
        let external = tempfile::tempdir().expect("external repository");
        let repository_root = parent.path().join("repository");
        let original_root = parent.path().join("original-repository");
        std::fs::create_dir(&repository_root).expect("create repository root");
        let scan = PayloadCas::new(&repository_root)
            .expect("open repository")
            .into_read_scan();
        std::fs::rename(&repository_root, &original_root).expect("move repository root");
        #[cfg(unix)]
        std::os::unix::fs::symlink(external.path(), &repository_root).unwrap();
        #[cfg(windows)]
        if let Err(error) = std::os::windows::fs::symlink_dir(external.path(), &repository_root) {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                return;
            }
            panic!("create reparse fixture: {error}");
        }

        let error = scan
            .stat_objects(["00a89e1d795e102f7b58e92f5448fba1f6f74d79b5b7da0aaac5c45c7e695916"])
            .expect_err("replaced scan root must fail closed");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn duplicate_publication_verifies_existing_bytes_and_cleans_staging() {
        let directory = tempfile::tempdir().unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let original = b"original synthetic object";
        let prepared = cas.prepare_bytes(original).unwrap();
        assert!(!prepared.deduplicated);
        assert!(cas.prepare_bytes(original).unwrap().deduplicated);
        let object = directory.path().join(&prepared.physical_key);
        let corrupted = vec![b'x'; original.len()];
        std::fs::write(&object, &corrupted).unwrap();
        let error = cas.prepare_bytes(original).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error
            .to_string()
            .contains("payload collision or corruption"));
        assert_eq!(std::fs::read(object).unwrap(), corrupted);
        assert_eq!(
            std::fs::read_dir(directory.path().join("assets/staging"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn concurrent_publication_never_replaces_the_winning_object() {
        let directory = tempfile::tempdir().unwrap();
        let cas = std::sync::Arc::new(PayloadCas::new(directory.path()).unwrap());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let cas = cas.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    cas.prepare_bytes(b"concurrent synthetic object").unwrap()
                })
            })
            .collect();
        let published: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(
            published
                .iter()
                .filter(|object| !object.deduplicated)
                .count(),
            1
        );
        assert_eq!(
            cas.read_object(&published[0].content_hash)
                .unwrap()
                .unwrap(),
            b"concurrent synthetic object"
        );
        assert_eq!(
            std::fs::read_dir(directory.path().join("assets/staging"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn existing_directory_from_a_crash_window_resyncs_its_parent_before_acceptance() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let cas = PayloadCas::new(directory.path()).expect("open repository");
        let existing = directory.path().join("assets");
        std::fs::create_dir(&existing).expect("simulate concurrent directory creation");
        let mut directory_entries_synced = true;
        let mut synced_parent = None;

        let accepted = cas
            .ensure_directory_with_sync(
                &cas.repository_root,
                "assets",
                &mut directory_entries_synced,
                |parent| {
                    synced_parent = Some(parent.to_path_buf());
                    Ok(false)
                },
            )
            .expect("accept existing directory");

        assert_eq!(accepted, cas.repository_root.join("assets"));
        assert_eq!(
            synced_parent.as_deref(),
            Some(cas.repository_root.as_path())
        );
        assert!(!directory_entries_synced);
    }

    #[test]
    fn failed_create_new_does_not_own_or_remove_a_preexisting_staging_file() {
        let directory = tempfile::tempdir().expect("temporary staging directory");
        let staging_path = directory.path().join("collision.tmp");
        std::fs::write(&staging_path, b"preexisting").expect("seed staging collision");

        let error = match create_staging_file(&staging_path, directory.path()) {
            Ok(_) => panic!("staging collision must fail"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(staging_path).expect("preexisting staging file remains"),
            b"preexisting"
        );
    }

    #[test]
    fn expected_prepare_rejects_mismatch_before_canonical_publication() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let cas = PayloadCas::new(directory.path()).expect("open repository");
        let expected = b"expected payload";
        let wrong = b"unexpected data";
        let expected_hash = hex::encode(sha2::Sha256::digest(expected));
        let wrong_hash = hex::encode(sha2::Sha256::digest(wrong));

        for _ in 0..3 {
            let error = cas
                .prepare_reader_expected(
                    &mut Cursor::new(wrong),
                    &expected_hash,
                    expected.len() as u64,
                )
                .expect_err("wrong payload must be rejected");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
            assert_eq!(cas.stat_object(&wrong_hash).unwrap(), None);
            assert_eq!(
                std::fs::read_dir(directory.path().join("assets").join("staging"))
                    .unwrap()
                    .count(),
                0
            );
        }

        let prepared = cas
            .prepare_reader_expected(
                &mut Cursor::new(expected),
                &expected_hash,
                expected.len() as u64,
            )
            .expect("exact payload is published");
        assert_eq!(prepared.content_hash, expected_hash);
        assert_eq!(prepared.byte_size, expected.len() as u64);

        assert!(cas
            .prepare_reader_expected(
                &mut Cursor::new(wrong),
                &expected_hash,
                expected.len() as u64,
            )
            .is_err());
        assert_eq!(cas.read_object(&expected_hash).unwrap().unwrap(), expected);
        assert_eq!(cas.stat_object(&wrong_hash).unwrap(), None);
    }

    #[test]
    fn exact_object_unlink_rejects_path_and_size_mismatch_without_touching_bytes() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let cas = PayloadCas::new(directory.path()).expect("open repository");
        let prepared = cas.prepare_bytes(b"exact deletion candidate").unwrap();

        let wrong_path = cas
            .unlink_exact_object(
                &prepared.content_hash,
                prepared.byte_size,
                "assets/objects/00/not-the-canonical-object",
            )
            .expect_err("mismatched physical key must fail closed");
        assert_eq!(wrong_path.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            cas.read_object(&prepared.content_hash).unwrap(),
            Some(b"exact deletion candidate".to_vec())
        );

        let wrong_size = cas
            .unlink_exact_object(
                &prepared.content_hash,
                prepared.byte_size + 1,
                &prepared.physical_key,
            )
            .expect_err("mismatched byte size must fail closed");
        assert_eq!(wrong_size.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            cas.stat_object(&prepared.content_hash).unwrap(),
            Some(prepared.byte_size)
        );
    }

    #[test]
    fn exact_object_unlink_removes_only_the_canonical_regular_file() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let cas = PayloadCas::new(directory.path()).expect("open repository");
        let prepared = cas.prepare_bytes(b"unlink me exactly").unwrap();

        let outcome = cas
            .unlink_exact_object(
                &prepared.content_hash,
                prepared.byte_size,
                &prepared.physical_key,
            )
            .expect("unlink exact object");

        assert!(matches!(
            outcome,
            ExactObjectUnlink::Removed {
                directory_entries_synced: _
            }
        ));
        assert_eq!(cas.stat_object(&prepared.content_hash).unwrap(), None);
        assert_eq!(
            cas.unlink_exact_object(
                &prepared.content_hash,
                prepared.byte_size,
                &prepared.physical_key,
            )
            .unwrap(),
            ExactObjectUnlink::Missing
        );
    }

    #[test]
    fn exact_object_unlink_rejects_a_symlink_or_reparse_object() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let outside = tempfile::tempdir().expect("temporary outside directory");
        let cas = PayloadCas::new(directory.path()).expect("open repository");
        let prepared = cas.prepare_bytes(b"linked deletion candidate").unwrap();
        let object_path = directory.path().join(&prepared.physical_key);
        let target = outside.path().join("target.bin");
        std::fs::write(&target, b"outside bytes").unwrap();
        std::fs::remove_file(&object_path).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &object_path).unwrap();
        #[cfg(windows)]
        if let Err(error) = std::os::windows::fs::symlink_file(&target, &object_path) {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                return;
            }
            panic!("create reparse fixture: {error}");
        }

        let error = cas
            .unlink_exact_object(
                &prepared.content_hash,
                prepared.byte_size,
                &prepared.physical_key,
            )
            .expect_err("linked object must fail closed");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(&target).unwrap(), b"outside bytes");
        assert!(std::fs::symlink_metadata(object_path)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn exact_object_unlink_rejects_same_size_replacement_after_streamed_hashing() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let cas = PayloadCas::new(directory.path()).expect("open repository");
        let original = vec![0x51; super::COPY_BUFFER_BYTES * 3 + 17];
        let replacement = vec![0x72; original.len()];
        let prepared = cas.prepare_bytes(&original).unwrap();
        let object_path = directory.path().join(&prepared.physical_key);

        let error = cas
            .unlink_exact_object_with_hash_hook(
                &prepared.content_hash,
                prepared.byte_size,
                &prepared.physical_key,
                |_| {
                    std::fs::remove_file(&object_path)?;
                    std::fs::write(&object_path, &replacement)
                },
            )
            .expect_err("same-size replacement after hashing must fail closed");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(object_path).unwrap(), replacement);
    }

    #[test]
    fn exact_object_check_stops_between_reads_once_cancelled() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let cas = PayloadCas::new(directory.path()).expect("open repository");
        let bytes = vec![0x5a; super::COPY_BUFFER_BYTES * 4 + 9];
        let prepared = cas.prepare_bytes(&bytes).unwrap();
        let asked = std::cell::Cell::new(0);
        // The third read is refused, after two chunks of five were hashed.
        let cancelled = || {
            asked.set(asked.get() + 1);
            asked.get() >= 3
        };
        assert!(cas
            .holds_exact_object(&prepared.content_hash, prepared.byte_size, &cancelled)
            .is_err());
        assert_eq!(asked.get(), 3);
        assert_eq!(
            std::fs::read(directory.path().join(&prepared.physical_key)).unwrap(),
            bytes
        );
        assert!(cas
            .holds_exact_object(&prepared.content_hash, prepared.byte_size, &|| false)
            .unwrap());
    }
}

#[cfg(test)]
mod hash_work_tests {
    use super::*;
    use crate::persistent_store::hash_work::{reset_hash_work, take_hash_work, DomainWork};

    #[test]
    fn stream_accounting_counts_only_executed_updates_on_read_failure() {
        struct FailingReader { first: bool }
        impl Read for FailingReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if !self.first { return Err(io::Error::other("synthetic failure")); }
                self.first = false; buffer[..7].copy_from_slice(b"partial"); Ok(7)
            }
        }
        let directory = tempfile::tempdir().unwrap(); let cas = PayloadCas::new(directory.path()).unwrap();
        reset_hash_work();
        assert!(cas.prepare_reader(&mut FailingReader { first: true }).is_err());
        let work = take_hash_work();
        assert_eq!(work.domains["cas_stage"], DomainWork { calls: 1, bytes: 7 });
        assert!(work.incomplete.is_empty());
    }

    #[test]
    fn adoption_and_existing_file_verification_are_separate_hash_passes() {
        let directory = tempfile::tempdir().unwrap(); let cas = PayloadCas::new(directory.path()).unwrap();
        let staging = directory.path().join("native-file-jobs/jobs/synthetic"); fs::create_dir_all(&staging).unwrap();
        let path = staging.join("body.payload"); let bytes = vec![7; COPY_BUFFER_BYTES + 13]; fs::write(&path, &bytes).unwrap();
        let expected = hex::encode(Sha256::digest(&bytes));
        reset_hash_work();
        let prepared = cas.adopt_import_payload(&path, &expected, bytes.len() as u64, &|| false).unwrap();
        let work = take_hash_work();
        assert_eq!(work.domains["cas_import_adopt"], DomainWork { calls: 1, bytes: bytes.len() as u64 });
        assert!(!work.domains.contains_key("cas_stage"));
        let mut file = File::open(directory.path().join(&prepared.physical_key)).unwrap();
        reset_hash_work();
        hash_open_file(&mut file, &|| false).unwrap();
        assert_eq!(take_hash_work().domains["cas_existing_verify"], DomainWork { calls: 1, bytes: bytes.len() as u64 });
        reset_hash_work();
        cas.stat_object(&expected).unwrap();
        assert!(take_hash_work().domains.is_empty());
    }
}


#[cfg(test)]
mod body_io_tests {
    use super::*;
    use std::collections::BTreeMap;
    use crate::asset_repository::body_io::{register_object_purpose, reset_body_io, take_body_io, BodyPurpose, BodyWork};

    fn fixture(bytes: &[u8]) -> (tempfile::TempDir, PayloadCas, PreparedPayload) {
        let directory = tempfile::tempdir().unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let prepared = cas.prepare_bytes(bytes).unwrap();
        (directory, cas, prepared)
    }

    #[test]
    fn actual_destination_writes_links_and_sha_conserve_control_asset_and_mixed_roles() {
        use crate::asset_repository::body_io::{BodyShaWork, BodyWork};
        let directory=tempfile::tempdir().unwrap(); let cas=PayloadCas::new(directory.path()).unwrap();
        let bodies=[vec![31;13],vec![32;COPY_BUFFER_BYTES+17],vec![33;5]];
        let hashes=bodies.iter().map(|body|hex::encode(Sha256::digest(body))).collect::<Vec<_>>();
        reset_body_io(); crate::persistent_store::hash_work::reset_hash_work();
        register_object_purpose(&hashes[0],BodyPurpose::Control);
        register_object_purpose(&hashes[1],BodyPurpose::Asset);
        register_object_purpose(&hashes[2],BodyPurpose::Control); register_object_purpose(&hashes[2],BodyPurpose::Asset);
        for (body,hash) in bodies.iter().zip(&hashes) {
            cas.prepare_reader_expected(&mut body.as_slice(),hash,body.len() as u64).unwrap();
        }
        let work=take_body_io(); let native=crate::persistent_store::hash_work::take_hash_work();
        assert!(work.complete()); assert!(native.incomplete.is_empty());
        let mut sum=BodyWork::default();
        for (body,hash) in bodies.iter().zip(&hashes) {
            let object=&work.objects[hash]; assert_eq!(object.work,BodyWork::default());
            sum.add(&object.owned_work);
            assert_eq!(object.owned_work.staging_written_bytes,body.len() as u64);
            assert_eq!(object.owned_work.staging_requested_bytes,body.len() as u64);
            assert_eq!(object.owned_work.publication_attempts,1); assert_eq!(object.owned_work.publications,1);
            assert_eq!(object.owned_work.publication_object_bytes,body.len() as u64);
            assert_eq!(object.owned_work.body_sha["cas_stage"],BodyShaWork{calls:1,bytes:body.len() as u64});
            #[cfg(not(target_os="android"))]
            assert_eq!(object.owned_work.publication_kinds["hard-link"].successes,1);
            #[cfg(target_os="android")]
            assert_eq!(object.owned_work.publication_kinds["rename"].successes,1);
        }
        assert_eq!(sum,work.domains["owned"]);
        assert_eq!(sum.body_sha["cas_stage"].calls,native.domains["cas_stage"].calls);
        assert_eq!(sum.body_sha["cas_stage"].bytes,native.domains["cas_stage"].bytes);
        assert_eq!(work.control_work(),work.objects[&hashes[0]].owned_work);
        let mut assets=work.objects[&hashes[1]].owned_work.clone(); assets.add(&work.objects[&hashes[2]].owned_work);
        assert_eq!(work.asset_work(),assets); assert_eq!(work.unknown_work(),BodyWork::default());
    }

    #[test]
    fn existing_destination_reports_actual_link_result_and_separate_verified_sha() {
        let bytes=b"synthetic deduplicated destination";
        let (_directory,cas,prepared)=fixture(bytes);
        reset_body_io(); register_object_purpose(&prepared.content_hash,BodyPurpose::Asset);
        crate::persistent_store::hash_work::reset_hash_work();
        assert!(cas.prepare_reader_expected(&mut bytes.as_slice(),&prepared.content_hash,bytes.len() as u64).unwrap().deduplicated);
        let work=take_body_io(); let native=crate::persistent_store::hash_work::take_hash_work();
        assert!(work.complete()); let object=&work.objects[&prepared.content_hash];
        assert_eq!(object.owned_work.staging_written_bytes,bytes.len() as u64);
        assert_eq!(object.owned_work.publication_attempts,1); assert_eq!(object.owned_work.publications,0);
        assert_eq!(object.owned_work.publication_already_exists,1); assert_eq!(object.owned_work.publication_failures,0);
        assert_eq!(object.owned_work.publication_object_bytes,0);
        assert_eq!(object.work.body_sha["cas_existing_verify"].calls,1);
        assert_eq!(object.work.body_sha["cas_existing_verify"].bytes,bytes.len() as u64);
        assert_eq!(object.work.body_sha["cas_existing_verify"].bytes,native.domains["cas_existing_verify"].bytes);
        assert_eq!(object.owned_work.body_sha["cas_stage"].bytes,native.domains["cas_stage"].bytes);
    }

    #[test]
    fn actual_import_adoption_attributes_sha_only_after_computed_identity_validation() {
        let bytes=b"synthetic private import asset";
        let hash=hex::encode(Sha256::digest(bytes));
        for valid in [true,false] {
            let directory=tempfile::tempdir().unwrap(); let cas=PayloadCas::new(directory.path()).unwrap();
            let jobs=directory.path().join("native-file-jobs").join("jobs").join("synthetic-import");
            fs::create_dir_all(&jobs).unwrap(); let path=jobs.join("asset.payload"); fs::write(&path,bytes).unwrap();
            let expected=if valid {hash.clone()} else {"ad".repeat(32)};
            reset_body_io(); register_object_purpose(&expected,BodyPurpose::Asset);
            crate::persistent_store::hash_work::reset_hash_work();
            let result=cas.adopt_import_payload(&path,&expected,bytes.len() as u64,&||false);
            assert_eq!(result.is_ok(),valid);
            let work=take_body_io(); let native=crate::persistent_store::hash_work::take_hash_work();
            assert_eq!(work.complete(),valid);
            assert_eq!(work.domains["owned"].body_sha["cas_import_adopt"].bytes,native.domains["cas_import_adopt"].bytes);
            if valid {
                let object=&work.objects[&hash].owned_work;
                assert_eq!(object.body_sha["cas_import_adopt"].bytes,bytes.len() as u64);
                assert_eq!(object.publication_attempts,1); assert_eq!(object.publications,1);
                assert_eq!(object.staging_write_attempts,0); assert_eq!(*object,work.asset_work());
                #[cfg(any(target_os="android",windows))]
                assert_eq!(object.publication_kinds["rename"].successes,1);
                #[cfg(not(any(target_os="android",windows)))]
                assert_eq!(object.publication_kinds["hard-link"].successes,1);
            } else {
                assert_eq!(work.unattributed_owned.body_sha["cas_import_adopt"].bytes,bytes.len() as u64);
                assert_eq!(work.unattributed_owned.publication_attempts,0);
                assert!(path.exists()); assert!(cas.stat_object(&expected).unwrap().is_none());
            }
        }
    }

    #[test]
    fn failed_stream_retains_actual_copy_and_sha_prefix_without_claiming_identity() {
        struct Prefix(bool);
        impl Read for Prefix { fn read(&mut self,buffer:&mut[u8])->io::Result<usize> {
            if self.0 { return Err(io::Error::other("synthetic reader failure")); }
            self.0=true; buffer[..3].copy_from_slice(b"abc"); Ok(3)
        }}
        let directory=tempfile::tempdir().unwrap(); let cas=PayloadCas::new(directory.path()).unwrap();
        reset_body_io(); crate::persistent_store::hash_work::reset_hash_work();
        assert!(cas.prepare_reader(&mut Prefix(false)).is_err());
        let work=take_body_io(); let native=crate::persistent_store::hash_work::take_hash_work();
        assert!(!work.complete()); assert!(work.objects.is_empty());
        assert_eq!(work.unattributed_owned.staging_write_attempts,1);
        assert_eq!(work.unattributed_owned.staging_written_bytes,3);
        assert_eq!(work.unattributed_owned.body_sha["cas_stage"].bytes,3);
        assert_eq!(work.unattributed_owned.body_sha["cas_stage"].bytes,native.domains["cas_stage"].bytes);
        assert_eq!(work.unattributed_owned,work.domains["owned"]);
        assert_eq!(work.unattributed_owned.publication_attempts,0);
    }

    #[test]
    fn explicit_worker_scope_aggregates_actual_cas_work_and_native_worker_receipt() {
        use crate::asset_repository::body_io::{capture_body_io_scope,with_body_io_scope};
        let directory=tempfile::tempdir().unwrap(); let cas=PayloadCas::new(directory.path()).unwrap();
        let bytes=b"synthetic worker destination"; let hash=hex::encode(Sha256::digest(bytes));
        reset_body_io(); register_object_purpose(&hash,BodyPurpose::Asset);
        crate::persistent_store::hash_work::reset_hash_work();
        let scope=capture_body_io_scope();
        let native=std::thread::spawn(move ||with_body_io_scope(scope,|| {
            crate::persistent_store::hash_work::reset_hash_work();
            cas.prepare_reader_expected(&mut bytes.as_slice(),&hash,bytes.len() as u64).unwrap();
            crate::persistent_store::hash_work::take_hash_work()
        })).join().unwrap();
        let work=take_body_io(); assert!(work.complete());
        assert_eq!(work.worker_scopes_started,1); assert_eq!(work.worker_scopes_settled,1);
        assert_eq!(work.pending_worker_scopes,0); assert_eq!(work.worker_threads.len(),1);
        assert_eq!(work.asset_work().staging_written_bytes,bytes.len() as u64);
        assert_eq!(work.asset_work().body_sha["cas_stage"].bytes,native.domains["cas_stage"].bytes);
        assert!(native.incomplete.is_empty());
        assert!(crate::persistent_store::hash_work::take_hash_work().domains.is_empty());
    }

    #[test]
    fn outstanding_worker_scope_reset_and_late_work_cannot_report_complete_zero() {
        use crate::asset_repository::body_io::{capture_body_io_scope,with_body_io_scope};
        let directory=tempfile::tempdir().unwrap(); let cas=PayloadCas::new(directory.path()).unwrap();
        let bytes=b"synthetic late worker"; let hash=hex::encode(Sha256::digest(bytes));
        reset_body_io(); register_object_purpose(&hash,BodyPurpose::Asset);
        let scope=capture_body_io_scope(); let old=scope.scope.clone();
        let before=take_body_io(); assert!(!before.complete()); assert_eq!(before.pending_worker_scopes,1);
        std::thread::spawn(move ||with_body_io_scope(scope,|| {
            cas.prepare_reader_expected(&mut bytes.as_slice(),&hash,bytes.len() as u64).unwrap();
        })).join().unwrap();
        assert!(!take_body_io().complete());
        let late=old.lock().unwrap(); assert!(!late.complete()); assert!(late.scope_violations>0);
        assert_eq!(late.asset_work().staging_written_bytes,bytes.len() as u64);
        assert_eq!(late.worker_scopes_started,late.worker_scopes_settled);
    }

    #[test]
    fn staging_provenance_conserves_owned_opens_and_asset_classification() {
        for purpose in [BodyPurpose::Control, BodyPurpose::Asset] {
            let directory = tempfile::tempdir().unwrap();
            let cas = PayloadCas::new(directory.path()).unwrap();
            let bytes = b"synthetic staged body";
            let hash = risunest_sync_wire::hash(bytes);
            reset_body_io(); register_object_purpose(&hash, purpose);
            let staged = cas.stage_reader_expected(&mut bytes.as_slice(), &hash, bytes.len() as u64).unwrap();
            cas.publish_staged(staged).unwrap();
            let work = take_body_io(); assert!(work.complete());
            assert_eq!(work.domains["owned"], BodyWork { open_attempts: 2, opens: 2,
                identity_metadata_open_attempts: 2, identity_metadata_opens: 2,
                verified_identity_metadata_opens: 2,
                staging_write_attempts: 1, staging_writes: 1, staging_requested_bytes: bytes.len() as u64,
                staging_written_bytes: bytes.len() as u64, publication_attempts: 1, publications: 1,
                publication_object_bytes: bytes.len() as u64,
                publication_kinds: BTreeMap::from([(if cfg!(target_os="android") {"rename"} else {"hard-link"}, crate::asset_repository::body_io::PublicationWork {
                    attempts:1,successes:1,object_bytes:bytes.len() as u64,..Default::default()
                })]),
                body_sha: BTreeMap::from([("cas_stage",crate::asset_repository::body_io::BodyShaWork {calls:1,bytes:bytes.len() as u64})]),
                ..Default::default() });
            assert_eq!(work.objects[&hash].owned_work, work.domains["owned"]);
            assert_eq!(work.objects[&hash].work, BodyWork::default());
            assert_eq!(work.unknown_work(), BodyWork::default());
            let classified = if purpose == BodyPurpose::Control { work.control_work() } else { work.asset_work() };
            assert_eq!(classified, work.domains["owned"]);
        }
    }

    #[test]
    fn two_control_stagings_conserve_four_verified_identity_metadata_opens() {
        let directory = tempfile::tempdir().unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let bodies = [b"first synthetic control".as_slice(), b"second synthetic control".as_slice()];
        let hashes = bodies.map(risunest_sync_wire::hash);
        reset_body_io();
        for (bytes, hash) in bodies.into_iter().zip(&hashes) {
            register_object_purpose(hash, BodyPurpose::Control);
            let mut reader = bytes;
            let staged = cas.stage_reader_expected(&mut reader, hash, bytes.len() as u64).unwrap();
            cas.publish_staged(staged).unwrap();
        }
        let work = take_body_io(); assert!(work.complete());
        let owned = work.domains["owned"].clone();
        assert_eq!(owned, BodyWork { open_attempts: 4, opens: 4,
            identity_metadata_open_attempts: 4, identity_metadata_opens: 4,
            verified_identity_metadata_opens: 4,
            staging_write_attempts: 2, staging_writes: 2, staging_requested_bytes: bodies.iter().map(|body|body.len() as u64).sum(),
            staging_written_bytes: bodies.iter().map(|body|body.len() as u64).sum(), publication_attempts: 2, publications: 2,
            publication_object_bytes: bodies.iter().map(|body|body.len() as u64).sum(),
            publication_kinds: BTreeMap::from([(if cfg!(target_os="android") {"rename"} else {"hard-link"}, crate::asset_repository::body_io::PublicationWork {
                attempts:2,successes:2,object_bytes:bodies.iter().map(|body|body.len() as u64).sum(),..Default::default()
            })]),
            body_sha: BTreeMap::from([("cas_stage",crate::asset_repository::body_io::BodyShaWork {calls:2,bytes:bodies.iter().map(|body|body.len() as u64).sum()})]),
            ..Default::default() });
        assert_eq!(work.control_work(), owned); assert_eq!(work.asset_work(), BodyWork::default());
        for hash in hashes { assert_eq!(work.objects[&hash].owned_work.verified_identity_metadata_opens, 2); }
    }

    #[test]
    fn failed_or_unclassified_staging_never_acquires_a_control_default() {
        let directory = tempfile::tempdir().unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let bytes = b"synthetic staged body";
        let hash = risunest_sync_wire::hash(bytes);
        reset_body_io();
        cas.prepare_bytes(bytes).unwrap();
        let unknown = take_body_io(); assert!(!unknown.complete());
        assert_eq!(unknown.unknown_work(), unknown.domains["owned"]);
        assert_eq!(unknown.control_work(), BodyWork::default());
        assert_eq!(unknown.unknown_work().verified_identity_metadata_opens, 2);
        register_object_purpose(&hash, BodyPurpose::Control);
        assert!(cas.stage_reader_expected(&mut bytes.as_slice(), &hash, 0).is_err());
        let failed = take_body_io(); assert!(!failed.complete());
        assert_eq!(failed.unattributed_owned.opens, 1);
        assert_eq!(failed.unattributed_owned, failed.domains["owned"]);
        assert_eq!(failed.unattributed_owned.verified_identity_metadata_opens, 1);
        reset_body_io();
        assert!(cas.open_exact_owned_file(&directory.path().join("missing.tmp")).is_err());
        let missing = take_body_io(); assert!(!missing.complete());
        assert_eq!(missing.unattributed_owned.failed_opens, 1);
        assert_eq!(missing.unattributed_owned.identity_metadata_open_attempts, 0);
    }

    #[test]
    fn identity_metadata_proof_excludes_nonidentity_owned_opens_and_mixed_assets() {
        let directory = tempfile::tempdir().unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let bytes = b"synthetic owned identity discriminator";
        let hash = risunest_sync_wire::hash(bytes);
        let path = directory.path().join("known-control.tmp");
        std::fs::write(&path, bytes).unwrap();
        reset_body_io(); register_object_purpose(&hash, BodyPurpose::Control);
        {
            let _scope = super::super::body_io::object_scope(&hash);
            drop(cas.open_exact_owned_file(&path).unwrap());
        }
        let work = take_body_io(); assert!(work.complete());
        assert_eq!(work.control_work().opens, 1);
        assert_eq!(work.control_work().identity_metadata_open_attempts, 0);
        assert_eq!(work.control_work().verified_identity_metadata_opens, 0);
        assert_eq!(work.objects[&hash].owned_work, work.domains["owned"]);
        register_object_purpose(&hash, BodyPurpose::Control);
        register_object_purpose(&hash, BodyPurpose::Asset);
        cas.prepare_bytes(bytes).unwrap();
        let mixed = take_body_io(); assert!(mixed.complete());
        assert_eq!(mixed.control_work(), BodyWork::default());
        assert_eq!(mixed.asset_work(), mixed.domains["owned"]);
        assert_eq!(mixed.asset_work().verified_identity_metadata_opens, 2);
        register_object_purpose(&hash, BodyPurpose::Control);
        drop(cas.open_object(&hash).unwrap().unwrap());
        let raw = take_body_io(); assert!(!raw.complete());
        assert_eq!(raw.domains["managed"].escaped_handles, 1);
        assert_eq!(raw.domains["managed"].verified_identity_metadata_opens, 0);
    }

    #[test]
    fn identity_metadata_open_failure_and_identity_mismatch_never_become_verified() {
        for missing in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let cas = PayloadCas::new(directory.path()).unwrap();
            let bytes = b"synthetic rejected staging identity";
            let hash = risunest_sync_wire::hash(bytes);
            reset_body_io(); register_object_purpose(&hash, BodyPurpose::Control);
            let staged = cas.stage_reader_expected(&mut bytes.as_slice(), &hash, bytes.len() as u64).unwrap();
            let path = staged.staging.path.clone();
            std::fs::remove_file(&path).unwrap();
            if !missing { std::fs::write(&path, b"replacement has a different identity and length").unwrap(); }
            assert!(cas.publish_staged(staged).is_err());
            let work = take_body_io(); assert!(work.complete());
            let owned = work.domains["owned"].clone();
            assert_eq!(owned.identity_metadata_open_attempts, 2);
            assert_eq!(owned.verified_identity_metadata_opens, 1);
            assert_eq!(owned.identity_metadata_opens, if missing { 1 } else { 2 });
            assert_eq!(owned.identity_metadata_failed_opens, if missing { 1 } else { 0 });
            assert_eq!(owned.open_attempts, owned.identity_metadata_open_attempts);
            assert_eq!(owned.opens, owned.identity_metadata_opens);
            assert_eq!(owned.failed_opens, owned.identity_metadata_failed_opens);
            assert_eq!(work.objects[&hash].owned_work, owned);
            assert_eq!(work.control_work(), owned);
        }
    }

    #[test]
    fn staged_owned_identity_cannot_cross_reset_or_worker_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let bytes = b"synthetic staged body";
        let hash = risunest_sync_wire::hash(bytes);
        reset_body_io(); register_object_purpose(&hash, BodyPurpose::Control);
        let staged = cas.stage_reader_expected(&mut bytes.as_slice(), &hash, bytes.len() as u64).unwrap();
        let live = take_body_io(); assert!(!live.complete());
        assert_eq!(live.control_work().outstanding_readers, 1);
        assert_eq!(live.control_work().verified_identity_metadata_opens, 1);
        drop(staged); assert!(!take_body_io().complete());
        reset_body_io(); register_object_purpose(&hash, BodyPurpose::Control);
        let staged = cas.stage_reader_expected(&mut bytes.as_slice(), &hash, bytes.len() as u64).unwrap();
        let worker = std::thread::spawn(move || {
            reset_body_io(); cas.publish_staged(staged).unwrap(); take_body_io()
        }).join().unwrap();
        let owner = take_body_io(); assert!(!owner.complete()); assert!(!worker.complete());
        assert_eq!(owner.control_work().opens, 1); assert_eq!(worker.unknown_work().opens, 1);
        assert_eq!(owner.control_work().verified_identity_metadata_opens, 1);
        assert_eq!(worker.unknown_work().verified_identity_metadata_opens, 1);
        assert_eq!(owner.domains["owned"].outstanding_readers, 0);
    }

    #[test]
    fn catalog_and_missing_body_checks_do_not_open_payloads() {
        let (directory, cas, prepared) = fixture(b"synthetic body");
        let missing = "a".repeat(64);
        let scan = PayloadCas::new(directory.path()).unwrap().into_read_scan();
        reset_body_io();
        assert_eq!(cas.stat_object(&prepared.content_hash).unwrap(), Some(14));
        assert_eq!(cas.stat_object(&missing).unwrap(), None);
        assert!(cas.read_object(&missing).unwrap().is_none());
        assert!(cas.open_object(&missing).unwrap().is_none());
        assert_eq!(scan.stat_objects([prepared.content_hash.as_str(),missing.as_str()]).unwrap(), vec![Some(14),None]);
        let work = take_body_io();
        assert!(work.domains.is_empty());
        assert_eq!(work.stat_requests, 2);
        assert_eq!(work.batch_stat_requests, 1);
        assert_eq!(work.presence_queries, 4);
        assert!(work.complete());
    }

    #[test]
    fn full_and_range_reads_record_actual_opens_and_exact_bytes() {
        let body = b"synthetic body";
        let (_directory, cas, prepared) = fixture(body);
        reset_body_io();
        register_object_purpose(&prepared.content_hash, BodyPurpose::Asset);
        assert_eq!(cas.read_object(&prepared.content_hash).unwrap().unwrap(), body);
        assert_eq!(cas.read_object_range(&prepared.content_hash, 2, 7).unwrap().unwrap(), &body[2..7]);
        let work = take_body_io();
        assert_eq!(work.domains["managed"], BodyWork {
            open_attempts: 2, opens: 2, read_operations: 2, read_bytes: body.len() as u64 + 5,
            ..Default::default()
        });
        assert!(work.complete());
        assert_eq!(take_body_io().domains.len(), 0);
        reset_body_io();
        assert!(take_body_io().domains.is_empty());
    }

    #[test]
    fn failed_actual_open_is_counted_without_inventing_a_body_read() {
        let (_directory, cas, _prepared) = fixture(b"synthetic body");
        let missing = cas.repository_root.join(object_physical_key(&"a".repeat(64)));
        reset_body_io();
        register_object_purpose(&"a".repeat(64), BodyPurpose::Asset);
        assert!(cas.open_exact_owned_file(&missing).is_err());
        assert!(cas.read_object_range(&"a".repeat(64), 3, 2).is_err());
        let work = take_body_io();
        assert_eq!(work.domains["managed"], BodyWork { open_attempts: 1, failed_opens: 1, ..Default::default() });
        assert_eq!(work.presence_queries, 0);
        assert!(work.complete());
    }

    #[test]
    fn escaped_handles_mark_later_external_read_bytes_incomplete() {
        let (_directory, cas, prepared) = fixture(b"synthetic body");
        reset_body_io();
        register_object_purpose(&prepared.content_hash, BodyPurpose::Asset);
        let mut file = cas.open_object(&prepared.content_hash).unwrap().unwrap();
        let mut bytes = Vec::new(); file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"synthetic body");
        let work = take_body_io();
        assert_eq!(work.domains["managed"], BodyWork { open_attempts: 1, opens: 1, escaped_handles: 1, ..Default::default() });
        assert!(!work.complete());
        reset_body_io();
        register_object_purpose(&prepared.content_hash, BodyPurpose::Asset);
        let path = cas.object_path(&prepared.content_hash).unwrap().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"synthetic body");
        let work = take_body_io();
        assert_eq!(work.domains["managed"], BodyWork { escaped_paths: 1, ..Default::default() });
        assert!(!work.complete());
    }

    #[test]
    fn verification_reads_count_only_executed_bytes_before_cancellation() {
        let bytes = vec![7; COPY_BUFFER_BYTES * 2 + 13];
        let (_directory, cas, prepared) = fixture(&bytes);
        let checks = std::cell::Cell::new(0);
        reset_body_io();
        register_object_purpose(&prepared.content_hash, BodyPurpose::Asset);
        assert!(cas.holds_exact_object(&prepared.content_hash, bytes.len() as u64, &|| {
            let count = checks.get(); checks.set(count + 1); count > 0
        }).is_err());
        let work = take_body_io();
        assert_eq!(work.domains["managed"], BodyWork {
            open_attempts: 1, opens: 1, read_operations: 1, read_bytes: COPY_BUFFER_BYTES as u64,
            body_sha:BTreeMap::from([("cas_existing_verify",crate::asset_repository::body_io::BodyShaWork {calls:1,bytes:COPY_BUFFER_BYTES as u64})]),
            ..Default::default()
        });
        assert_eq!(work.asset_work().read_bytes,COPY_BUFFER_BYTES as u64);
        assert_eq!(work.unattributed_managed.body_sha["cas_existing_verify"].bytes,COPY_BUFFER_BYTES as u64);
        assert!(!work.complete());
    }

    #[test]
    fn worker_io_is_collected_on_that_thread_and_never_inferred_on_caller() {
        let (_directory, cas, prepared) = fixture(b"synthetic body");
        let cas = std::sync::Arc::new(cas);
        reset_body_io();
        let caller = std::thread::current().id();
        let worker = std::thread::spawn(move || {
            reset_body_io();
            register_object_purpose(&prepared.content_hash, BodyPurpose::Asset);
            cas.read_object(&prepared.content_hash).unwrap().unwrap();
            take_body_io()
        }).join().unwrap();
        assert_ne!(worker.thread, caller);
        assert_eq!(worker.domains["managed"].opens, 1);
        assert_eq!(worker.domains["managed"].read_bytes, 14);
        assert!(worker.complete());
        let parent = take_body_io();
        assert_eq!(parent.thread, caller);
        assert!(parent.domains.is_empty());
        assert!(parent.complete());
    }

    #[test]
    fn tracked_reads_classify_actual_objects_and_count_exact_bytes() {
        let bytes = b"synthetic body";
        let (_directory, cas, prepared) = fixture(bytes);
        reset_body_io();
        register_object_purpose(&prepared.content_hash, BodyPurpose::Control);
        let mut reader = cas.open_object_tracked(&prepared.content_hash).unwrap().unwrap();
        assert_eq!(reader.len().unwrap(), bytes.len() as u64);
        reader.seek(SeekFrom::Start(2)).unwrap();
        let mut part = [0; 5]; reader.read_exact(&mut part).unwrap();
        assert_eq!(&part, &bytes[2..7]);
        drop(reader);
        let work = take_body_io();
        assert!(work.complete());
        assert_eq!(work.control_work(), BodyWork { open_attempts: 1, opens: 1, read_operations: 1, read_bytes: 5, ..Default::default() });
        assert_eq!(work.asset_work(), BodyWork::default());
        assert_eq!(work.unknown_work(), BodyWork::default());
        assert_eq!(work.objects[&prepared.content_hash].work, work.domains["managed"]);
    }

    #[test]
    fn tracked_unread_drop_and_full_read_are_observed_without_guessed_bytes() {
        let bytes = b"synthetic body";
        let (_directory, cas, prepared) = fixture(bytes);
        reset_body_io(); register_object_purpose(&prepared.content_hash, BodyPurpose::Asset);
        drop(cas.open_object_tracked(&prepared.content_hash).unwrap().unwrap());
        let work = take_body_io(); assert!(work.complete());
        assert_eq!(work.asset_work(), BodyWork { open_attempts: 1, opens: 1, ..Default::default() });
        register_object_purpose(&prepared.content_hash, BodyPurpose::Asset);
        let mut reader = cas.open_object_tracked(&prepared.content_hash).unwrap().unwrap();
        let mut result = Vec::new(); reader.read_to_end(&mut result).unwrap(); drop(reader);
        let work = take_body_io(); assert!(work.complete()); assert_eq!(result, bytes);
        assert_eq!(work.asset_work().read_bytes, bytes.len() as u64);
        assert!(work.asset_work().read_operations >= 2);
        assert_eq!(work.asset_work().escaped_handles, 0);
    }

    #[test]
    fn unknown_role_is_incomplete_and_mixed_roles_count_as_assets_across_roots() {
        let (_directory, cas, prepared) = fixture(b"synthetic body");
        let (_other_directory, other, other_prepared) = fixture(b"synthetic body");
        assert_eq!(prepared.content_hash, other_prepared.content_hash);
        reset_body_io(); drop(cas.open_object_tracked(&prepared.content_hash).unwrap());
        let unknown = take_body_io(); assert!(!unknown.complete());
        assert_eq!(unknown.unknown_work().opens, 1); assert_eq!(unknown.asset_work().opens, 0);
        register_object_purpose(&prepared.content_hash, BodyPurpose::Control);
        drop(cas.open_object_tracked(&prepared.content_hash).unwrap());
        register_object_purpose(&prepared.content_hash, BodyPurpose::Asset);
        drop(other.open_object_tracked(&prepared.content_hash).unwrap());
        let mixed = take_body_io(); assert!(mixed.complete());
        assert_eq!(mixed.asset_work().opens, 2); assert_eq!(mixed.control_work(), BodyWork::default());
        assert_eq!(mixed.objects[&prepared.content_hash].purposes.len(), 2);
    }

    #[test]
    fn live_reader_and_reset_boundary_fail_closed_without_erasing_old_scope() {
        let (_directory, cas, prepared) = fixture(b"synthetic body");
        reset_body_io(); register_object_purpose(&prepared.content_hash, BodyPurpose::Control);
        let mut reader = cas.open_object_tracked(&prepared.content_hash).unwrap().unwrap();
        let live = take_body_io(); assert!(!live.complete());
        assert_eq!(live.control_work().outstanding_readers, 1);
        let mut byte = [0]; assert_eq!(reader.read(&mut byte).unwrap(), 1); drop(reader);
        let stale = take_body_io(); assert!(!stale.complete()); assert!(stale.scope_violations > 0);
        assert_eq!(stale.asset_work().read_bytes, 0);
        reset_body_io(); assert!(take_body_io().complete());
        register_object_purpose(&prepared.content_hash, BodyPurpose::Control);
        let reader = cas.open_object_tracked(&prepared.content_hash).unwrap().unwrap();
        reset_body_io(); drop(reader); assert!(!take_body_io().complete());
    }

    #[test]
    fn tracked_reader_cross_thread_use_invalidates_owner_and_worker_scopes() {
        let (_directory, cas, prepared) = fixture(b"synthetic body");
        reset_body_io(); register_object_purpose(&prepared.content_hash, BodyPurpose::Asset);
        let mut reader = cas.open_object_tracked(&prepared.content_hash).unwrap().unwrap();
        let worker = std::thread::spawn(move || {
            reset_body_io(); let mut bytes = [0; 3]; reader.read_exact(&mut bytes).unwrap(); drop(reader);
            take_body_io()
        }).join().unwrap();
        let owner = take_body_io();
        assert!(!owner.complete()); assert!(!worker.complete());
        assert_eq!(owner.asset_work().read_bytes, 3); assert_eq!(owner.asset_work().outstanding_readers, 0);
        assert!(owner.scope_violations > 0); assert!(worker.scope_violations > 0);
        assert_eq!(worker.asset_work().read_bytes, 0);
    }
}
