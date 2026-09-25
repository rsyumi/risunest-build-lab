//! Local canonical capture spool. The SQLite catalog contains only hashes and
//! lengths; unchanged record bodies are immutable shared files, never copied as
//! part of another small-edit capture. This module performs no remote requests.
use crate::{
    local_backup::{CancellationProbe, NeverCancelled},
    persistent_store::{
        content_capture::ContentCaptureSink, sync_selection::CaptureIdentity, StoreError,
    },
    trust_boundary::{is_link_like, sync_directory},
};
use super::content_store::ContentStore;
use risunest_external_storage_format::content_identity::{hash, hash_reader};
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, StoreError>;

fn invalid(message: &str) -> StoreError {
    StoreError::Validation {
        message: message.into(),
    }
}
fn checked_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    if is_link_like(&fs::symlink_metadata(path)?) {
        return Err(invalid("Capture directory must not be a link"));
    }
    Ok(())
}

pub(super) struct OwnedCaptureFile {
    path: PathBuf,
    directory: PathBuf,
    owned: bool,
}

impl OwnedCaptureFile {
    pub(super) fn new(path: PathBuf, directory: &Path) -> Self {
        Self {
            path,
            directory: directory.to_path_buf(),
            owned: true,
        }
    }

    fn keep(&mut self) {
        self.owned = false;
    }

    pub(super) fn remove_and_sync(&mut self) -> io::Result<bool> {
        if !self.owned {
            return Ok(true);
        }
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.owned = false;
        sync_directory(&self.directory)
    }
}

impl Drop for OwnedCaptureFile {
    fn drop(&mut self) {
        if self.owned && fs::remove_file(&self.path).is_ok() {
            let _ = sync_directory(&self.directory);
        }
    }
}

pub(crate) struct CaptureCatalog {
    pub(crate) db: Connection,
    path: PathBuf,
    content: ContentStore,
    identity: Option<CaptureIdentity>,
    pub(crate) rebuilt: bool,
    finalized: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DurableCaptureReference {
    pub capture_id: String,
    pub identity: CaptureIdentity,
    /// Relative to `<persistent root>/external-storage`.
    pub catalog_path: String,
    pub catalog_hash: String,
}

#[derive(Debug, Default)]
pub(crate) struct RegisteredCaptureRoots {
    pub assets: crate::asset_repository::migration_gc::AssetRootSet,
    pub catalogs: BTreeSet<PathBuf>,
    pub logical_records: BTreeSet<PathBuf>,
    /// Bodies the content store holds by identity rather than as files.
    pub content_objects: BTreeSet<String>,
}

/// Both paths are selected by the native owner. Previous catalogs must have
/// an authoritative PDS manifest hash; an unregistered receipt is not sufficient.
impl CaptureCatalog {
    pub(crate) fn reopen(
        path: &Path,
        external_root: &Path,
        expected_hash: &[u8; 32],
        expected_identity: &CaptureIdentity,
    ) -> Result<Self> {
        let mut file = crate::trust_boundary::open_regular_source(path)?;
        let size = file.metadata()?.len();
        if hash_reader(&mut file, size).map_err(|_| invalid("Capture catalog integrity failed"))?
            != *expected_hash
        {
            return Err(invalid("Capture catalog integrity failed"));
        }
        let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let encoded: String = db.query_row(
            "SELECT identity FROM capture_info WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        let identity: CaptureIdentity = serde_json::from_str(&encoded)?;
        if identity != *expected_identity {
            return Err(invalid("Capture catalog identity differs"));
        }
        Ok(Self {
            db,
            path: path.into(),
            content: ContentStore::open(external_root)?,
            identity: Some(identity),
            rebuilt: false,
            finalized: true,
        })
    }

    /// Where this capture's bodies live. A body staged by the current capture
    /// is readable here before the catalog commits; a second connection to the
    /// same store would not see it yet.
    pub(crate) fn content(&self) -> &ContentStore {
        &self.content
    }

    fn require_writing(&self) -> Result<()> {
        if self.identity.is_none() || self.finalized || self.db.is_autocommit() {
            return Err(invalid("Capture catalog is not writable"));
        }
        Ok(())
    }
    pub(crate) fn create(
        directory: &Path,
        external_root: &Path,
        previous: Option<(&Path, &[u8; 32])>,
    ) -> Result<Self> {
        checked_directory(directory)?;
        let content = ContentStore::open(external_root)?;
        let path = directory.join("capture.sqlite");
        // Reserve the destination before copying, never overwrite another job.
        let mut destination = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let mut destination_guard = OwnedCaptureFile::new(path.clone(), directory);
        if let Some((source, expected)) = previous {
            if is_link_like(&fs::symlink_metadata(source)?) {
                return Err(invalid("Capture catalog must not be a link"));
            }
            let mut source = File::open(source)?;
            let length = source.metadata()?.len();
            // Verify the bytes being copied, rather than verifying and reopening.
            let mut digest = sha2::Sha256::new();
            use sha2::Digest;
            use std::io::Read;
            let mut buffer = [0u8; 64 * 1024];
            let mut copied = 0u64;
            loop {
                let n = source.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                destination.write_all(&buffer[..n])?;
                digest.update(&buffer[..n]);
                copied += n as u64;
            }
            let actual: [u8; 32] = digest.finalize().into();
            if copied != length || actual != *expected {
                return Err(invalid("Previous capture catalog differs"));
            }
        }
        destination.sync_all()?;
        drop(destination);
        let db = Connection::open(&path)?;
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;")?;
        if previous.is_none() {
            db.execute_batch("CREATE TABLE capture_info(singleton INTEGER PRIMARY KEY CHECK(singleton=1),identity TEXT NOT NULL);
                CREATE TABLE records(key TEXT PRIMARY KEY,hash TEXT NOT NULL,bytes INTEGER NOT NULL);
                CREATE TABLE generated(hash TEXT PRIMARY KEY,bytes INTEGER NOT NULL);
                CREATE TABLE dependencies(record TEXT NOT NULL,hash TEXT NOT NULL,bytes INTEGER NOT NULL,PRIMARY KEY(record,hash));
                CREATE TABLE delta(key TEXT PRIMARY KEY);")?;
        }
        destination_guard.keep();
        Ok(Self {
            db,
            path,
            content,
            identity: None,
            rebuilt: false,
            finalized: false,
        })
    }

    pub(crate) fn manifest(&self) -> Result<([u8; 32], &Path, &CaptureIdentity)> {
        if !self.finalized {
            return Err(invalid("Capture catalog is not durable"));
        }
        let mut file = File::open(&self.path)?;
        let length = file.metadata()?.len();
        let digest = hash_reader(&mut file, length)
            .map_err(|_| invalid("Capture catalog integrity failed"))?;
        Ok((
            digest,
            &self.path,
            self.identity
                .as_ref()
                .ok_or_else(|| invalid("Capture identity missing"))?,
        ))
    }

    pub(crate) fn durable_reference(
        &self,
        capture_id: &str,
        repository_root: &Path,
    ) -> Result<DurableCaptureReference> {
        if capture_id.is_empty() || capture_id.len() > 1024 || capture_id.contains('\0') {
            return Err(invalid("Capture identity is invalid"));
        }
        let (hash, path, identity) = self.manifest()?;
        let root = repository_root.join("external-storage").canonicalize()?;
        if is_link_like(&fs::symlink_metadata(path)?) {
            return Err(invalid("Capture catalog must not be a link"));
        }
        let path = path.canonicalize()?;
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| invalid("Capture catalog escaped its native directory"))?;
        let relative = relative
            .components()
            .map(|component| match component {
                std::path::Component::Normal(value) => value
                    .to_str()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| invalid("Capture catalog path is invalid")),
                _ => Err(invalid("Capture catalog path is invalid")),
            })
            .collect::<Result<Vec<_>>>()?
            .join("/");
        if relative.is_empty() || relative.len() > 8192 {
            return Err(invalid("Capture catalog path is invalid"));
        }
        Ok(DurableCaptureReference {
            capture_id: capture_id.into(),
            identity: identity.clone(),
            catalog_path: relative,
            catalog_hash: hex::encode(hash),
        })
    }

    pub(crate) fn content_fingerprint(&self, scope: &[u8; 32]) -> Result<[u8; 32]> {
        if !self.finalized {
            return Err(invalid("Capture catalog is not durable"));
        }
        let mut digest = risunest_external_storage_format::format::FingerprintBuilder::new(scope);
        let mut query = self
            .db
            .prepare("SELECT key,hash FROM records ORDER BY key")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let hash: String = row.get(1)?;
            let hash: [u8; 32] = hex::decode(hash)
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(|| invalid("Capture record hash is invalid"))?;
            digest
                .push(&key, &hash)
                .map_err(|_| invalid("Capture records are not ordered"))?;
        }
        Ok(digest.finish())
    }
}

impl ContentCaptureSink for CaptureCatalog {
    fn begin(&mut self, identity: &CaptureIdentity, after: Option<i64>) -> Result<()> {
        if self.identity.is_some() {
            return Err(invalid("Capture catalog already started"));
        }
        if let Some(after) = after {
            let encoded: Option<String> = self
                .db
                .query_row(
                    "SELECT identity FROM capture_info WHERE singleton=1",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            let old: CaptureIdentity = serde_json::from_str(
                &encoded.ok_or_else(|| invalid("Capture cache requires rebuild"))?,
            )?;
            if old.store_id != identity.store_id
                || old.library_epoch != identity.library_epoch
                || old.generation != identity.generation
                || old.revision != after
            {
                return Err(invalid("Capture cache does not match its PDS cursor"));
            }
        }
        // Nothing staged from an abandoned attempt carries into this one.
        self.content.discard();
        self.db
            .execute_batch("BEGIN IMMEDIATE; DELETE FROM delta;")?;
        self.rebuilt = after.is_none();
        if self.rebuilt {
            self.db.execute_batch(
                "DELETE FROM records; DELETE FROM generated; DELETE FROM dependencies;",
            )?;
        }
        self.identity = Some(identity.clone());
        Ok(())
    }
    fn remove_record(&mut self, key: &str) -> Result<()> {
        self.require_writing()?;
        self.db
            .execute("INSERT OR IGNORE INTO delta VALUES(?1)", [key])?;
        self.db.execute("DELETE FROM records WHERE key=?1", [key])?;
        self.db
            .execute("DELETE FROM dependencies WHERE record=?1", [key])?;
        Ok(())
    }
    fn record(&mut self, key: &str, bytes: &[u8]) -> Result<()> {
        self.require_writing()?;
        let hash = hex::encode(hash(bytes));
        self.content.put(&hash, bytes)?;
        self.db.execute(
            "INSERT INTO records VALUES(?1,?2,?3)",
            params![key, hash, bytes.len() as i64],
        )?;
        Ok(())
    }
    fn object(&mut self, hash: &str, bytes: &[u8]) -> Result<()> {
        self.require_writing()?;
        self.content.put(hash, bytes)?;
        self.db.execute(
            "INSERT OR IGNORE INTO generated VALUES(?1,?2)",
            params![hash, bytes.len() as i64],
        )?;
        Ok(())
    }
    fn reference(&mut self, record: &str, hash: &str, size: u64) -> Result<()> {
        self.require_writing()?;
        let size = i64::try_from(size).map_err(|_| invalid("Capture payload length overflow"))?;
        self.db.execute(
            "INSERT OR IGNORE INTO dependencies VALUES(?1,?2,?3)",
            params![record, hash, size],
        )?;
        let stored: i64 = self.db.query_row(
            "SELECT bytes FROM dependencies WHERE record=?1 AND hash=?2",
            params![record, hash],
            |r| r.get(0),
        )?;
        if stored != size {
            return Err(invalid("Capture dependency length differs"));
        }
        Ok(())
    }
    fn finish(&mut self) -> Result<()> {
        self.require_writing()?;
        let identity = self
            .identity
            .as_ref()
            .ok_or_else(|| invalid("Capture not started"))?;
        self.db.execute("INSERT INTO capture_info VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET identity=excluded.identity",[serde_json::to_string(identity)?])?;
        self.db.execute(
            "DELETE FROM generated WHERE hash NOT IN (SELECT hash FROM dependencies)",
            [],
        )?;
        // Bodies before the rows that name them: the catalog only becomes
        // durable once every body it points at already is.
        self.content.commit()?;
        self.db.execute_batch("COMMIT")?;
        // FlushFileBuffers on Windows requires a handle opened for writing.
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)?
            .sync_all()?;
        self.content.sync_objects()?;
        sync_directory(self.path.parent().unwrap())?;
        self.finalized = true;
        Ok(())
    }
}

fn resolve_registered_catalog(
    repository_root: &Path,
    reference: &DurableCaptureReference,
) -> Result<(PathBuf, PathBuf)> {
    if reference.capture_id.is_empty()
        || reference.capture_id.len() > 1024
        || reference.capture_id.contains('\0')
        || !crate::trust_boundary::is_lower_hex_256(&reference.catalog_hash)
        || reference.catalog_path.is_empty()
        || reference.catalog_path.len() > 8192
    {
        return Err(invalid("Registered capture reference is invalid"));
    }
    let relative = Path::new(&reference.catalog_path);
    if relative
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(invalid("Registered capture path is invalid"));
    }
    let root = repository_root.join("external-storage").canonicalize()?;
    let path = root.join(relative);
    if is_link_like(&fs::symlink_metadata(&path)?) {
        return Err(invalid("Registered capture is a link"));
    }
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(&root) {
        return Err(invalid("Registered capture escaped its native directory"));
    }
    Ok((canonical, root))
}

fn capture_roots<'a>(
    references: impl IntoIterator<Item = &'a DurableCaptureReference>,
    repository_root: &Path,
    verify_contents: bool,
    recovery: bool,
    probe: &dyn CancellationProbe,
) -> Result<RegisteredCaptureRoots> {
    let cancelled = || probe.is_cancelled();
    let check = || {
        if cancelled() {
            Err(invalid("Capture validation cancelled"))
        } else {
            Ok(())
        }
    };
    let mut roots = RegisteredCaptureRoots::default();
    let payloads = if recovery {
        Some(crate::asset_repository::PayloadCas::new(repository_root)?)
    } else {
        None
    };
    for reference in references {
        check()?;
        let (path, external_root) = resolve_registered_catalog(repository_root, reference)?;
        let expected: [u8; 32] = hex::decode(&reference.catalog_hash)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| invalid("Registered capture hash is invalid"))?;
        let catalog =
            CaptureCatalog::reopen(&path, &external_root, &expected, &reference.identity)?;
        roots.catalogs.insert(path);

        let mut dependencies = catalog.db.prepare(
            "SELECT DISTINCT d.hash FROM dependencies d LEFT JOIN generated g ON g.hash=d.hash WHERE g.hash IS NULL",
        )?;
        for hash in dependencies.query_map([], |row| row.get::<_, String>(0))? {
            let hash = hash?;
            if !crate::trust_boundary::is_lower_hex_256(&hash) {
                return Err(invalid("Registered capture payload identity differs"));
            }
            roots.assets.object_hashes.insert(hash);
        }
        if let Some(payloads) = &payloads {
            let mut held = catalog.db.prepare(
                "SELECT DISTINCT d.hash,d.bytes FROM dependencies d LEFT JOIN generated g ON g.hash=d.hash WHERE g.hash IS NULL",
            )?;
            for payload in held.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })? {
                let (hash, bytes) = payload?;
                let bytes = u64::try_from(bytes)
                    .map_err(|_| invalid("Registered capture payload length is invalid"))?;
                check()?;
                // Recovery replaces a library with these bytes, so their
                // length is not proof of them.
                if !payloads.holds_exact_object(&hash, bytes, &cancelled)? {
                    return Err(invalid("Recovery capture payload is not held locally"));
                }
            }
        }

        let mut objects = catalog.db.prepare(
            "SELECT hash,bytes FROM records UNION SELECT hash,bytes FROM generated ORDER BY hash",
        )?;
        for object in objects.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (hash, bytes) = object?;
            if bytes < 0 || !crate::trust_boundary::is_lower_hex_256(&hash) {
                return Err(invalid("Registered capture object identity differs"));
            }
            check()?;
            catalog.content.validate_checked(&hash, bytes, verify_contents, &cancelled)?;
            // Protected in whichever store holds it: a file by its path, a
            // database body by its identity.
            match catalog.content.file_path(&hash)? {
                Some(path) => {
                    roots.logical_records.insert(path);
                }
                None => {
                    roots.content_objects.insert(hash);
                }
            }
        }
    }
    Ok(roots)
}

/// Resolves every physical input owned by durable capture references. Any
/// error is a conservative GC blocker for the caller. Record bodies are not
/// rehashed during inventory; source claims verify them before reading.
pub(crate) fn registered_capture_roots<'a>(
    references: impl IntoIterator<Item = &'a DurableCaptureReference>,
    repository_root: &Path,
) -> Result<RegisteredCaptureRoots> {
    capture_roots(references, repository_root, false, false, &NeverCancelled)
}

#[cfg(test)]
pub(crate) fn validate_capture_sources<'a>(
    references: impl IntoIterator<Item = &'a DurableCaptureReference>,
    repository_root: &Path,
) -> Result<RegisteredCaptureRoots> {
    capture_roots(references, repository_root, true, false, &NeverCancelled)
}

/// What `validate_capture_sources` checks, and that every payload the capture
/// names is held locally at its recorded length and content, so local recovery
/// never has to reach custody. Every body is read in bounded chunks, and
/// `probe` is asked between objects and chunks.
pub(crate) fn validate_recovery_sources<'a>(
    references: impl IntoIterator<Item = &'a DurableCaptureReference>,
    repository_root: &Path,
    probe: &dyn CancellationProbe,
) -> Result<RegisteredCaptureRoots> {
    capture_roots(references, repository_root, true, true, probe)
}

/// GC performs this inventory only when it actually needs roots. Every normal
/// edit/capture can reuse the durable index without scanning all asset aliases.
pub(crate) fn registered_roots(
    db: &Connection,
    repository_root: &Path,
) -> Result<crate::asset_repository::migration_gc::AssetRootSet> {
    let references = registered_references(db, repository_root)?;
    if references.is_empty() {
        return Ok(crate::asset_repository::migration_gc::AssetRootSet::default());
    }
    Ok(registered_capture_roots(&references, repository_root)?.assets)
}

/// Every registered capture catalog, whether or not a job still references it.
pub(crate) fn registered_references(
    db: &Connection,
    repository_root: &Path,
) -> Result<Vec<DurableCaptureReference>> {
    let mut references = Vec::new();
    let mut query=db.prepare("SELECT c.id,c.identity,f.catalog_path,f.file_hash FROM external_storage_capture_files f JOIN external_storage_captures c ON c.id=f.capture_id")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let original = PathBuf::from(row.get::<_, String>(2)?);
        if is_link_like(&fs::symlink_metadata(&original)?) {
            return Err(invalid("Registered capture is a link"));
        }
        let absolute = original.canonicalize()?;
        let root = repository_root.join("external-storage").canonicalize()?;
        let relative = absolute
            .strip_prefix(&root)
            .map_err(|_| invalid("Registered capture escaped its native directory"))?
            .components()
            .map(|component| match component {
                std::path::Component::Normal(value) => value
                    .to_str()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| invalid("Registered capture path is invalid")),
                _ => Err(invalid("Registered capture path is invalid")),
            })
            .collect::<Result<Vec<_>>>()?
            .join("/");
        references.push(DurableCaptureReference {
            capture_id: row.get(0)?,
            identity: serde_json::from_str(&row.get::<_, String>(1)?)?,
            catalog_path: relative,
            catalog_hash: row.get(3)?,
        });
    }
    Ok(references)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> CaptureIdentity {
        CaptureIdentity {
            store_id: "store".into(),
            library_epoch: "library".into(),
            generation: "generation".into(),
            selection_epoch: "selection".into(),
            revision: 1,
        }
    }

    #[test]
    fn failed_catalog_creation_releases_the_reserved_destination_for_retry() {
        let root = tempfile::tempdir().expect("create capture root");
        let source_path = root.path().join("previous.sqlite");
        let source = Connection::open(&source_path).expect("create previous catalog");
        source
            .execute_batch("CREATE TABLE synthetic(value TEXT);")
            .expect("initialize previous catalog");
        drop(source);
        let source_bytes = fs::read(&source_path).expect("read previous catalog");
        let expected = hash(&source_bytes);
        let capture_directory = root.path().join("capture");
        let external = root.path().join("external-storage");

        assert!(CaptureCatalog::create(
            &capture_directory,
            &external,
            Some((&source_path, &[0; 32])),
        )
        .is_err());
        assert!(!capture_directory.join("capture.sqlite").exists());

        let catalog = CaptureCatalog::create(
            &capture_directory,
            &external,
            Some((&source_path, &expected)),
        )
        .expect("retry catalog creation");
        assert!(catalog.path.is_file());
    }

    #[test]
    fn capture_object_publication_never_replaces_an_existing_identity() {
        let root = tempfile::tempdir().expect("create capture root");
        let capture_directory = root.path().join("capture");
        let object_directory = root.path().join("objects");
        let mut catalog = CaptureCatalog::create(&capture_directory, root.path(), None)
            .expect("create capture catalog");
        // Above the threshold, so publication still goes through the shared
        // immutable file layout this test is about.
        let payload = vec![7u8; super::super::content_store::SMALL_OBJECT_BYTES + 1];
        let payload = payload.as_slice();
        let expected = hex::encode(hash(payload));

        catalog
            .content
            .put(&expected, payload)
            .expect("publish capture object");
        catalog
            .content
            .put(&expected, payload)
            .expect("deduplicate capture object");
        assert_eq!(
            fs::read(object_directory.join(&expected)).expect("read published object"),
            payload
        );
        assert!(fs::read_dir(&object_directory)
            .expect("list object directory")
            .all(|entry| !entry
                .expect("read object entry")
                .file_name()
                .to_string_lossy()
            .ends_with(".partial")));
    }

    #[test]
    fn empty_registered_roots_do_not_require_an_external_storage_directory() {
        let root = tempfile::tempdir().unwrap();
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE external_storage_captures(id TEXT,identity TEXT);
             CREATE TABLE external_storage_capture_files(capture_id TEXT,catalog_path TEXT,file_hash TEXT);",
        )
        .unwrap();
        let roots = registered_roots(&db, root.path()).unwrap();
        assert!(roots.object_hashes.is_empty());
        assert!(!root.path().join("external-storage").exists());
    }

    #[test]
    fn durable_capture_roots_keep_catalog_records_and_cas_dependencies_distinct() {
        let root = tempfile::tempdir().unwrap();
        let external = root.path().join("external-storage");
        let capture_directory = external.join("captures/conflict");
        let object_directory = external.join("objects");
        let mut catalog = CaptureCatalog::create(&capture_directory, &external, None)
            .unwrap();
        let identity = identity();
        let record = b"synthetic logical record";
        let record_hash = hex::encode(hash(record));
        let asset_hash = "03".repeat(32);
        // One body of each size, so both protected sets are exercised.
        let generated = vec![9u8; super::super::content_store::SMALL_OBJECT_BYTES + 1];
        let generated_hash = hex::encode(hash(&generated));
        catalog.begin(&identity, None).unwrap();
        catalog.record("root", record).unwrap();
        catalog.object(&generated_hash, &generated).unwrap();
        catalog.reference("root", &generated_hash, generated.len() as u64).unwrap();
        catalog.reference("root", &asset_hash, 9).unwrap();
        catalog.finish().unwrap();
        let reference = catalog
            .durable_reference("capture", root.path())
            .unwrap();

        let roots = registered_capture_roots([&reference], root.path()).unwrap();
        let expected_catalog = capture_directory
            .join("capture.sqlite")
            .canonicalize()
            .unwrap();
        assert!(roots.catalogs.contains(&expected_catalog));
        assert!(roots.content_objects.contains(&record_hash));
        assert!(roots
            .logical_records
            .contains(&object_directory.join(&generated_hash).canonicalize().unwrap()));
        assert!(roots.assets.object_hashes.contains(&asset_hash));

        // A body that no longer matches its identity is found by the pass that
        // reads contents, not by the inventory that only counts roots.
        let store = rusqlite::Connection::open(external.join("content.sqlite")).unwrap();
        store
            .execute(
                "UPDATE small_objects SET body=?1 WHERE hash=?2",
                params![b"synthetic damaged record".to_vec(), record_hash],
            )
            .unwrap();
        drop(store);
        // The same length as the original, so only reading it can tell.
        assert!(catalog.content.put(&record_hash, record).is_err());
        assert!(registered_capture_roots([&reference], root.path()).is_ok());
        assert!(validate_capture_sources([&reference], root.path()).is_err());
    }

    #[test]
    fn corrupt_catalog_blocks_root_collection() {
        let root = tempfile::tempdir().unwrap();
        let external = root.path().join("external-storage");
        let capture_directory = external.join("captures/conflict");
        let object_directory = external.join("objects");
        let mut catalog = CaptureCatalog::create(&capture_directory, &external, None)
            .unwrap();
        catalog.begin(&identity(), None).unwrap();
        catalog.record("root", b"record").unwrap();
        catalog.finish().unwrap();
        let reference = catalog
            .durable_reference("capture", root.path())
            .unwrap();
        drop(catalog);
        let path = capture_directory.join("capture.sqlite");
        let mut bytes = fs::read(&path).unwrap();
        bytes[0] ^= 1;
        fs::write(path, bytes).unwrap();
        assert!(registered_capture_roots([&reference], root.path()).is_err());
    }
}
