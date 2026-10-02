//! Direct-locator snapshot download and full verification into native staging.
//! No PDS generation is activated here; the caller owns the subsequent staged
//! apply and eventual cleanup of this durable verified directory.
#[cfg(test)]
use super::worker_observation::spawn_blocking;
#[cfg(not(test))]
use tokio::task::spawn_blocking;
use super::{
    content_store::{ContentStore, ObjectSource},
    package_cache::CatalogRange,
    contract::{
        Cancellation, ErrorKind, ObjectRole, Provider, ProviderError, ReadReceipt,
        RepositoryHandle, Result,
    },
    packaging::{cpu_permit, RemoteObject},
    phase_progress::PhaseProgress,
    sections::{CapturedSection, SectionSource},
    transfer::SpoolSink,
};
use crate::asset_repository::PayloadCas;
use risunest_external_storage_format::{
    content_identity::hash_reader, crypto::derive_key, pack, snapshot as wire,
};
use sha2::{Digest, Sha256};
use rusqlite::OptionalExtension;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

/// What one staging root had to fetch, and how much of it was packs.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct TestReads {
    pub network: u64,
    pub network_bytes: u64,
    pub packs: u64,
    pub pack_bytes: u64,
    /// Packs whose stored ciphertext is at or below the small-pack size.
    pub small_packs: u64,
}

/// The stored ciphertext length below which a pack counts as small.
#[cfg(test)]
pub(super) const SMALL_PACK_BYTES: u64 = 256 * 1024;

#[cfg(test)]
static TEST_READ_COUNTS: std::sync::LazyLock<std::sync::Mutex<BTreeMap<PathBuf, TestReads>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(BTreeMap::new()));

#[cfg(test)]
pub(super) fn reset_test_read_counts(staging_root: &Path) {
    TEST_READ_COUNTS
        .lock()
        .unwrap()
        .insert(staging_root.to_path_buf(), TestReads::default());
}

#[cfg(test)]
pub(super) fn take_test_read_counts(staging_root: &Path) -> TestReads {
    TEST_READ_COUNTS
        .lock()
        .unwrap()
        .remove(staging_root)
        .expect("test read counter root was registered")
}

/// The most pack copies one staging root held at once while it turned packs
/// over.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct TestTurnover {
    pub ciphertexts: usize,
    pub plaintexts: usize,
}

#[cfg(test)]
static TEST_TURNOVER: std::sync::LazyLock<std::sync::Mutex<BTreeMap<PathBuf, TestTurnover>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(BTreeMap::new()));

#[cfg(test)]
pub(super) fn reset_test_turnover(staging_root: &Path) {
    TEST_TURNOVER
        .lock()
        .unwrap()
        .insert(staging_root.to_path_buf(), TestTurnover::default());
}

#[cfg(test)]
pub(super) fn take_test_turnover(staging_root: &Path) -> TestTurnover {
    TEST_TURNOVER
        .lock()
        .unwrap()
        .remove(staging_root)
        .expect("test turnover root was registered")
}

#[cfg(test)]
fn record_turnover(staging_root: &Path, packs: &[RemoteObject]) {
    let mut turnover = TEST_TURNOVER.lock().unwrap();
    let Some(counts) = turnover.get_mut(staging_root) else {
        return;
    };
    let ciphertexts = packs
        .iter()
        .filter(|pack| {
            staging_root
                .join("downloads")
                .join(format!("{}.cipher", pack.ciphertext_sha256))
                .exists()
        })
        .count();
    let plaintexts = packs
        .iter()
        .filter(|pack| {
            staging_root
                .join("plaintext")
                .join(&pack.plaintext_sha256)
                .exists()
        })
        .count();
    counts.ciphertexts = counts.ciphertexts.max(ciphertexts);
    counts.plaintexts = counts.plaintexts.max(plaintexts);
}

#[cfg(test)]
fn record_test_read(downloads: &Path, pack: bool, byte_length: u64) {
    let staging_root = downloads
        .parent()
        .expect("snapshot downloads directory has a staging root");
    let mut reads = TEST_READ_COUNTS.lock().unwrap();
    if let Some(counts) = reads.get_mut(staging_root) {
        counts.network += 1;
        counts.network_bytes += byte_length;
        if pack {
            counts.packs += 1;
            counts.pack_bytes += byte_length;
            if byte_length <= SMALL_PACK_BYTES {
                counts.small_packs += 1;
            }
        }
    }
}

fn corrupt(_: impl std::fmt::Display) -> ProviderError {
    ProviderError::new(ErrorKind::Corrupt)
}
fn transient(error: impl std::fmt::Display + 'static) -> ProviderError {
    super::packaging::transient(error)
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedRecord {
    pub key: String,
    pub content_hash: String,
    pub byte_length: u64,
    pub source: ObjectSource,
}
#[derive(Clone, Debug)]
pub(crate) struct PreparedObject {
    pub content_hash: String,
    pub byte_length: u64,
    pub source: ObjectSource,
}
#[derive(Clone, Debug)]
pub(crate) struct PreparedRemoteSnapshot {
    pub snapshot_id: String,
    pub repository_id: String,
    pub fingerprint: String,
    pub library_fingerprint: String,
    pub logical_revision: u64,
    pub staging_root: PathBuf,
    pub records: Vec<PreparedRecord>,
    pub objects: Vec<PreparedObject>,
    /// The device whose own values the sections are, when there is one.
    pub captured_by_device: Option<String>,
}

/// Where a downloaded snapshot's record bodies live. The apply attaches to
/// this same directory beside the staging root.
fn content_store(staging_root: &Path) -> Result<ContentStore> {
    ContentStore::open(&staging_root.join("external-storage")).map_err(transient)
}

#[cfg(test)]
impl PreparedRemoteSnapshot {
    /// A prepared body, wherever the plan put it: a record in the content store
    /// beside the staging root, an asset in a staging file.
    pub(super) fn body(&self, source: &ObjectSource) -> Vec<u8> {
        match source {
            ObjectSource::File(path) => fs::read(path).expect("prepared staging file"),
            ObjectSource::Captured(digest) => content_store(&self.staging_root)
                .expect("prepared content store")
                .read_all(digest)
                .expect("prepared record body"),
            ObjectSource::Library(_) => panic!("a library body is not read through here"),
        }
    }
    pub(super) fn record_body(&self, index: usize) -> Vec<u8> {
        self.body(&self.records[index].source)
    }
    /// Drop every record body this plan prepared, so a run over the same
    /// staging directory has to build them again.
    pub(super) fn discard_record_bodies(&self) {
        let mut content = content_store(&self.staging_root).expect("prepared content store");
        for record in &self.records {
            if let Some(path) = content
                .file_path(&record.content_hash)
                .expect("prepared record path")
            {
                fs::remove_file(path).expect("discard prepared record file");
            }
        }
        let digests = self
            .records
            .iter()
            .map(|record| record.content_hash.as_str())
            .collect::<Vec<_>>();
        content
            .delete(&digests)
            .expect("discard prepared record bodies");
    }
}

fn ensure_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(transient)?;
    if crate::trust_boundary::is_link_like(&fs::symlink_metadata(path).map_err(transient)?) {
        return Err(corrupt("snapshot staging directory is a link"));
    }
    Ok(())
}
pub(crate) fn verify_body_file(path:&Path,length:u64,sha256:&str)->Result<bool> {verify(path,length,sha256)}
fn verify(path: &Path, length: u64, sha256: &str) -> Result<bool> {
    let mut file = match crate::trust_boundary::open_regular_source(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(corrupt(error)),
    };
    if file.metadata().map_err(corrupt)?.len() != length {
        return Ok(false);
    }
    let hashed=hash_reader(&mut file,length);
    #[cfg(test)]
    match &hashed {
        Ok(_)=>crate::persistent_store::hash_work::observe("external_restore_file_verify",length as usize),
        Err(_)=>crate::persistent_store::hash_work::incomplete("external_restore_file_verify"),
    }
    Ok(hex::encode(hashed.map_err(corrupt)?) == sha256)
}

fn publish_verified(
    partial: &Path,
    destination: &Path,
    expected_length: u64,
    expected_sha256: &str,
) -> Result<()> {
    if destination.exists() {
        if verify(destination, expected_length, expected_sha256)? {
            fs::remove_file(partial).map_err(transient)?;
            return Ok(());
        }
        let metadata = fs::symlink_metadata(destination).map_err(corrupt)?;
        if crate::trust_boundary::is_link_like(&metadata) || !metadata.file_type().is_file() {
            return Err(corrupt(
                "snapshot staging destination is not a regular file",
            ));
        }
        fs::remove_file(destination).map_err(transient)?;
    }
    #[cfg(target_os = "android")]
    let result = crate::trust_boundary::rename_without_replace(partial, destination);
    #[cfg(not(target_os = "android"))]
    let result = fs::hard_link(partial, destination);
    match result {
        Ok(()) => {
            #[cfg(not(target_os = "android"))]
            fs::remove_file(partial).map_err(transient)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if !verify(destination, expected_length, expected_sha256)? {
                return Err(corrupt(
                    "snapshot staging publication raced with corrupt data",
                ));
            }
            fs::remove_file(partial).map_err(transient)?;
        }
        Err(error) => return Err(transient(error)),
    }
    crate::trust_boundary::sync_directory(destination.parent().unwrap()).map_err(transient)?;
    Ok(())
}

async fn download_ciphertext(
    object: &RemoteObject,
    downloads: &Path,
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    cancel: &Cancellation,
) -> Result<PathBuf> {
    object.stored(repository)?;
    let destination = downloads.join(format!("{}.cipher", object.ciphertext_sha256));
    if verify(
        &destination,
        object.receipt.byte_length,
        &object.ciphertext_sha256,
    )? {
        return Ok(destination);
    }
    if destination.exists() {
        fs::remove_file(&destination).map_err(transient)?;
    }
    let partial = downloads.join(format!("{}.partial", object.ciphertext_sha256));
    if partial.exists() {
        fs::remove_file(&partial).map_err(transient)?;
    }
    let mut sink = SpoolSink::create(&partial, object.receipt.byte_length)?;
    #[cfg(test)]
    record_test_read(
        downloads,
        object.role == ObjectRole::Pack,
        object.receipt.byte_length,
    );
    let receipt = provider
        .read_object(repository, &object.receipt.locator, None, &mut sink, cancel)
        .await?;
    let ReadReceipt::Body(receipt) = receipt else {
        return Err(corrupt("unexpected not-modified response"));
    };
    if !sink.is_verified() || !receipt.complete || receipt.byte_length != object.receipt.byte_length
    {
        return Err(corrupt("incomplete snapshot object"));
    }
    if !verify(
        &partial,
        object.receipt.byte_length,
        &object.ciphertext_sha256,
    )? {
        return Err(corrupt("snapshot ciphertext hash differs"));
    }
    publish_verified(
        &partial,
        &destination,
        object.receipt.byte_length,
        &object.ciphertext_sha256,
    )?;
    Ok(destination)
}

pub(super) async fn open_object(
    object: &RemoteObject,
    root_key: &[u8; 32],
    staging_root: &Path,
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    cancel: &Cancellation,
) -> Result<PathBuf> {
    let downloads = staging_root.join("downloads");
    let plaintext = staging_root.join("plaintext");
    ensure_directory(&downloads)?;
    ensure_directory(&plaintext)?;
    let destination = plaintext.join(&object.plaintext_sha256);
    if verify(
        &destination,
        object.plaintext_length,
        &object.plaintext_sha256,
    )? {
        return Ok(destination);
    }
    if destination.exists() {
        fs::remove_file(&destination).map_err(transient)?;
    }
    let ciphertext = download_ciphertext(object, &downloads, provider, repository, cancel).await?;
    let partial = plaintext.join(format!("{}.partial", object.plaintext_sha256));
    if partial.exists() {
        fs::remove_file(&partial).map_err(transient)?;
    }
    let object_copy = object.clone();
    let repository_id = object.repository_id.clone();
    let root = *root_key;
    let cipher_path = ciphertext.clone();
    let partial_path = partial.clone();
    let cpu = cpu_permit().await?;
    spawn_blocking(move || {
        let mut input =
            crate::trust_boundary::open_regular_source(&cipher_path).map_err(corrupt)?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&partial_path)
            .map_err(transient)?;
        let purpose = if object_copy.role == ObjectRole::Pack {
            "data"
        } else {
            "metadata"
        };
        let key = derive_key(&root, &repository_id, purpose).map_err(corrupt)?;
        let header =
            wire::open_envelope(&mut input, &mut output, &key, object_copy.plaintext_length)
                .map_err(super::packaging::format_error)?;
        output.sync_all().map_err(transient)?;
        drop(output);
        let expected_role = match object_copy.role {
            ObjectRole::Segment => return Err(ProviderError::new(ErrorKind::Unsupported)),
            ObjectRole::Snapshot => wire::ObjectRole::SyncState,
            ObjectRole::Descriptor => wire::ObjectRole::Descriptor,
            ObjectRole::Pack => wire::ObjectRole::Pack,
            ObjectRole::Catalog => wire::ObjectRole::Catalog,
            ObjectRole::SyncState => wire::ObjectRole::SyncState,
            ObjectRole::BackupBundle => wire::ObjectRole::BackupBundle,
            ObjectRole::BackupPoint => wire::ObjectRole::BackupPoint,
            ObjectRole::InventoryPage => wire::ObjectRole::InventoryPage,
            ObjectRole::Lease => wire::ObjectRole::Lease,
        };
        let expected = wire::PublicObjectHeader::new(
            repository_id,
            object_copy.object_id.clone(),
            expected_role,
            object_copy.plaintext_length,
        )
        .map_err(corrupt)?;
        if header != expected
            || !verify(
                &partial_path,
                object_copy.plaintext_length,
                &object_copy.plaintext_sha256,
            )?
        {
            return Err(corrupt("snapshot plaintext differs"));
        }
        Ok(())
    })
    .await
    .map_err(transient)??;
    drop(cpu);
    publish_verified(
        &partial,
        &destination,
        object.plaintext_length,
        &object.plaintext_sha256,
    )?;
    Ok(destination)
}

/// Removes what one opened object left behind. A check proves a body and has
/// no use for it afterwards, so the staging directory never grows past the
/// object being read.
pub(super) fn discard_object(staging_root: &Path, object: &RemoteObject) {
    let _ = fs::remove_file(staging_root.join("plaintext").join(&object.plaintext_sha256));
    let _ = fs::remove_file(
        staging_root
            .join("downloads")
            .join(format!("{}.cipher", object.ciphertext_sha256)),
    );
}

pub(super) fn read_bytes(path: &Path, max: usize) -> Result<Vec<u8>> {
    let mut file = crate::trust_boundary::open_regular_source(path).map_err(corrupt)?;
    let length = file.metadata().map_err(corrupt)?.len();
    if length > max as u64 {
        return Err(corrupt("metadata limit exceeded"));
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.read_to_end(&mut bytes).map_err(corrupt)?;
    Ok(bytes)
}

#[derive(Clone)]
pub(super) struct CompleteEntry {
    pub(super) kind: wire::CatalogEntryKind,
    pub(super) key: String,
    pub(super) content_sha256: [u8; 32],
    pub(super) byte_length: u64,
    pub(super) chunks: Vec<wire::StoredChunk>,
}

/// Keep what this download already read, so the publication that follows it
/// does not ask the provider for the same catalog again. A cache that cannot
/// be written costs that, and nothing else, so it does not fail the download.
fn record_read(
    record_into: Option<&Path>,
    label: &super::packaging::CatalogRoot,
    root: &RemoteObject,
    entries: &[CompleteEntry],
    packs: &BTreeMap<String, RemoteObject>,
    nodes: &[(RemoteObject, CatalogRange)],
    repository: &RepositoryHandle,
) {
    let Some(cache_root) = record_into else {
        return;
    };
    let _ = super::cache_hydration::record_read_catalog(
        cache_root, label, root, entries, packs, nodes, repository,
    );
}

/// Everything one catalog names: its complete entries, the packs they point
/// at, and the nodes the walk passed through with the key range each covered.
/// A restore only needs the first two, and rebuilding reuse metadata needs all
/// three. Nodes arrive in depth-first order, which keeps the nodes of one
/// level in the order that level was published in.
pub(super) async fn read_catalog(
    root: &RemoteObject,
    expected_kind: wire::CatalogKind,
    root_key: &[u8; 32],
    staging_root: &Path,
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    cancel: &Cancellation,
) -> Result<(
    Vec<CompleteEntry>,
    BTreeMap<String, RemoteObject>,
    Vec<(RemoteObject, CatalogRange)>,
)> {
    let mut stack = vec![root.clone()];
    let mut nodes = Vec::new();
    let mut visited = BTreeSet::new();
    let mut fragments: BTreeMap<(u8, String), Vec<wire::CatalogEntryFragment>> = BTreeMap::new();
    let mut packs = BTreeMap::new();
    while let Some(object) = stack.pop() {
        cancel.check()?;
        if object.role != ObjectRole::Catalog
            || object.repository_id != root.repository_id
            || !visited.insert(object.object_id.clone())
        {
            return Err(corrupt("duplicate or invalid catalog node"));
        }
        let path = open_object(
            &object,
            root_key,
            staging_root,
            provider,
            repository,
            cancel,
        )
        .await?;
        let bytes = read_bytes(&path, wire::MAX_METADATA_BYTES)?;
        let document = wire::CatalogDocument::decode(
            &bytes,
            usize::try_from(object.plaintext_length).map_err(corrupt)?,
        )
        .map_err(corrupt)?;
        if document.kind != expected_kind {
            return Err(corrupt("catalog kind differs"));
        }
        nodes.push((
            object.clone(),
            CatalogRange {
                level: document.level,
                first_key: document.first_key.clone(),
                last_key: document.last_key.clone(),
            },
        ));
        if document.level == 0 {
            for pack in document.packs {
                let remote = RemoteObject::from_stored(&pack, repository)?;
                if remote.role != ObjectRole::Pack || remote.repository_id != root.repository_id {
                    return Err(corrupt("catalog references non-pack"));
                }
                if let Some(old) = packs.insert(remote.object_id.clone(), remote.clone()) {
                    if old != remote {
                        return Err(corrupt("conflicting pack reference"));
                    }
                }
            }
            for fragment in document.entries {
                let valid_kind = match expected_kind {
                    wire::CatalogKind::Records => matches!(
                        fragment.kind,
                        wire::CatalogEntryKind::Record | wire::CatalogEntryKind::Object
                    ),
                    wire::CatalogKind::Assets => fragment.kind == wire::CatalogEntryKind::Object,
                    wire::CatalogKind::Section => matches!(
                        fragment.kind,
                        wire::CatalogEntryKind::SectionEntry
                            | wire::CatalogEntryKind::SectionObject
                    ),
                };
                if !valid_kind {
                    return Err(corrupt("catalog entry kind differs"));
                }
                let tag = match fragment.kind {
                    wire::CatalogEntryKind::Record => 0,
                    wire::CatalogEntryKind::Object => 1,
                    wire::CatalogEntryKind::SectionEntry => 2,
                    wire::CatalogEntryKind::SectionObject => 3,
                };
                fragments
                    .entry((tag, fragment.key.clone()))
                    .or_default()
                    .push(fragment);
            }
        } else {
            for child in document.children.into_iter().rev() {
                stack.push(RemoteObject::from_stored(&child.object, repository)?);
            }
        }
    }
    let mut result = Vec::new();
    for ((_tag, key), mut parts) in fragments {
        parts.sort_by_key(|part| part.fragment_index);
        let first = parts
            .first()
            .ok_or_else(|| corrupt("missing catalog fragment"))?;
        let expected_count = usize::try_from(first.fragment_count).map_err(corrupt)?;
        if parts.len() != expected_count {
            return Err(corrupt("catalog fragment missing"));
        }
        let kind = first.kind;
        let content_sha256 = first.content_sha256;
        let byte_length = first.byte_length;
        let mut chunks = Vec::new();
        for (index, part) in parts.into_iter().enumerate() {
            if part.fragment_index as usize != index
                || part.fragment_count as usize != expected_count
                || part.kind != kind
                || part.key != key
                || part.content_sha256 != content_sha256
                || part.byte_length != byte_length
            {
                return Err(corrupt("conflicting catalog fragments"));
            }
            chunks.extend(part.chunks);
        }
        let total = chunks.iter().try_fold(0u64, |sum, chunk| {
            sum.checked_add(chunk.plaintext_length)
                .ok_or_else(|| corrupt("entry length overflow"))
        })?;
        if total != byte_length {
            return Err(corrupt("catalog entry length differs"));
        }
        result.push(CompleteEntry {
            kind,
            key,
            content_sha256,
            byte_length,
            chunks,
        });
    }
    Ok((result, packs, nodes))
}

async fn open_packs(
    packs: &BTreeMap<String, RemoteObject>,
    root_key: &[u8; 32],
    staging_root: &Path,
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    progress: &PhaseProgress,
    cancel: &Cancellation,
) -> Result<BTreeMap<String, PathBuf>> {
    // What each catalog needs is known before any of it is fetched, and a
    // later catalog adds to the same total rather than restarting it.
    progress.plan(
        packs.len() as u64,
        packs.values().map(|pack| pack.receipt.byte_length).sum(),
    );
    let mut paths = BTreeMap::new();
    for (id, pack) in packs {
        cancel.check()?;
        paths.insert(
            id.clone(),
            open_object(pack, root_key, staging_root, provider, repository, cancel).await?,
        );
        progress.completed(pack.receipt.byte_length);
    }
    Ok(paths)
}

/// What a resolver may accept as a source, and what the consumer's repository
/// is. A body is only offered as a library source when the job that receives
/// the plan writes into that same repository, which a scratch store does not.
#[derive(Clone, Copy, Debug)]
pub(crate) enum SourceTrust<'a> {
    /// Nothing local answers. Every body comes out of a pack.
    Downloaded,
    /// The repository at this root may answer, and each body it answers with
    /// is read and proved against the identity the catalog names first.
    ProvenLibrary(&'a Path),
    /// The repository at this root may answer on identity and length alone,
    /// which is what it was already admitted under.
    AdmittedLibrary(&'a Path),
}

/// The repository a plan may take bodies from, and whether each one has to be
/// read before it counts as a source.
struct LocalLibrary {
    root: PathBuf,
    prove: bool,
}

/// One catalog entry and where its content is going to come from. An entry
/// that is already here names its source; the rest name the chunks that have
/// to be assembled, and only those decide which packs are opened.
struct ResolvedEntry {
    entry: CompleteEntry,
    digest: String,
    /// Where an object's body is going to be written. A record has none,
    /// because the content store decides where its body belongs.
    destination: Option<PathBuf>,
    source: Option<ObjectSource>,
}

/// Decides per entry whether anything has to be read out of a pack. Content
/// this staging directory already holds and can prove is not assembled again.
fn resolve_entries(
    entries: Vec<CompleteEntry>,
    staging_root: &Path,
    content: &ContentStore,
    library: Option<LocalLibrary>,
    cancel: &Cancellation,
) -> Result<Vec<ResolvedEntry>> {
    let prove = library.as_ref().is_some_and(|library| library.prove);
    let library = library
        .map(|library| PayloadCas::new(&library.root))
        .transpose()
        .map_err(transient)?;
    let object_root = staging_root.join("objects");
    ensure_directory(&object_root)?;
    let mut plan = Vec::with_capacity(entries.len());
    for entry in entries {
        cancel.check()?;
        let digest = hex::encode(entry.content_sha256);
        let (destination, source) = match entry.kind {
            // A record already in the content store is proved the same way a
            // staging file was, by its length and then by its content.
            wire::CatalogEntryKind::Record => {
                let length = i64::try_from(entry.byte_length).map_err(corrupt)?;
                let held = content.validate(&digest, length, true).is_ok();
                (None, held.then(|| ObjectSource::Captured(digest.clone())))
            }
            wire::CatalogEntryKind::Object => {
                let destination = object_root.join(&digest);
                let mut source = verify(&destination, entry.byte_length, &digest)?
                    .then(|| ObjectSource::File(destination.clone()));
                if source.is_none() {
                    if let Some(cas) = library.as_ref() {
                        source = library_body(cas, &digest, entry.byte_length, prove)?;
                    }
                }
                (Some(destination), source)
            }
            wire::CatalogEntryKind::SectionEntry | wire::CatalogEntryKind::SectionObject => {
                return Err(corrupt("section entry in library catalog"));
            }
        };
        plan.push(ResolvedEntry {
            entry,
            digest,
            destination,
            source,
        });
    }
    Ok(plan)
}

/// A body the library already holds under this exact identity and length. The
/// length is asked for as well, because a truncated file carries the name of
/// the content it no longer is.
fn library_body(
    cas: &PayloadCas,
    digest: &str,
    byte_length: u64,
    prove: bool,
) -> Result<Option<ObjectSource>> {
    let Some(path) = cas.object_path(digest).map_err(transient)? else {
        return Ok(None);
    };
    let mut file = match crate::trust_boundary::open_regular_source(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(corrupt(error)),
    };
    if file.metadata().map_err(corrupt)?.len() != byte_length {
        return Ok(None);
    }
    // Proving it costs one read of a body that is going to be consumed anyway,
    // which is what a job that answers for everything it consumes owes. A body
    // that fails is not a source, so the pack that carries it is read instead.
    if prove {
        let hashed=hash_reader(&mut file,byte_length);
        #[cfg(test)]
        match &hashed {
            Ok(_)=>crate::persistent_store::hash_work::observe("external_restore_local_source_verify",byte_length as usize),
            Err(_)=>crate::persistent_store::hash_work::incomplete("external_restore_local_source_verify"),
        }
        if hex::encode(hashed.map_err(corrupt)?) != digest {return Ok(None);}
    }
    Ok(Some(ObjectSource::Library(digest.to_owned())))
}

/// One entry that still has to be put together out of packs.
struct Pending {
    resolved: ResolvedEntry,
    /// Where each chunk starts in the assembled body.
    offsets: Vec<u64>,
    /// The packs its chunks lie in. An entry in one pack is put together while
    /// that pack is open; one spread over several is written into its own
    /// assembly file a chunk at a time, as each of its packs arrives.
    packs: BTreeSet<String>,
}

impl Pending {
    fn new(resolved: ResolvedEntry) -> Result<Self> {
        let mut offsets = Vec::with_capacity(resolved.entry.chunks.len());
        let mut length = 0u64;
        for chunk in &resolved.entry.chunks {
            offsets.push(length);
            length = length
                .checked_add(chunk.plaintext_length)
                .ok_or_else(|| corrupt("restored entry length overflow"))?;
        }
        if length != resolved.entry.byte_length {
            return Err(corrupt("restored entry length differs from its chunks"));
        }
        let packs = resolved
            .entry
            .chunks
            .iter()
            .map(|chunk| chunk.pack_id.clone())
            .collect();
        Ok(Self {
            resolved,
            offsets,
            packs,
        })
    }

    fn spread(&self) -> bool {
        self.packs.len() > 1
    }
}

/// What a pack's placement leaves in `turnover/`: its bytes are in the
/// assembly files of the entries it carries, so it is not read again.
fn marker_path(staging_root: &Path, pack_id: &str) -> Result<PathBuf> {
    if pack_id.is_empty()
        || !pack_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(corrupt("catalog pack identity is not a file name"));
    }
    Ok(staging_root.join("turnover").join(pack_id))
}

fn remove_marker(staging_root: &Path, pack_id: &str) -> Result<()> {
    match fs::remove_file(marker_path(staging_root, pack_id)?) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(transient(error)),
        _ => Ok(()),
    }
}

fn assembly_path(staging_root: &Path, digest: &str) -> PathBuf {
    staging_root.join("assembly").join(digest)
}

/// Readies what an earlier attempt left for another pass. An assembly file
/// that is not the length it was created at no longer holds what the markers
/// of its packs say it does, and an entry placed whole out of one pack is
/// only here once its pack is read again.
fn prepare_turnover(staging_root: &Path, pending: &[Pending]) -> Result<()> {
    ensure_directory(&staging_root.join("assembly"))?;
    ensure_directory(&staging_root.join("turnover"))?;
    for item in pending {
        if !item.spread() {
            for pack in &item.packs {
                remove_marker(staging_root, pack)?;
            }
            continue;
        }
        let path = assembly_path(staging_root, &item.resolved.digest);
        let intact = match fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.is_file()
                    && !crate::trust_boundary::is_link_like(&metadata)
                    && metadata.len() == item.resolved.entry.byte_length =>
            {
                true
            }
            Ok(_) => {
                fs::remove_file(&path).map_err(transient)?;
                false
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(transient(error)),
        };
        if !intact {
            for pack in &item.packs {
                remove_marker(staging_root, pack)?;
            }
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .map_err(transient)?;
            file.set_len(item.resolved.entry.byte_length)
                .map_err(transient)?;
        }
    }
    Ok(())
}

fn read_chunk(
    input: &mut fs::File,
    input_length: u64,
    chunk: &wire::StoredChunk,
) -> Result<Vec<u8>> {
    if chunk
        .offset
        .checked_add(chunk.stored_length)
        .is_none_or(|end| end > input_length)
    {
        return Err(corrupt("chunk escaped pack"));
    }
    input.seek(SeekFrom::Start(chunk.offset)).map_err(corrupt)?;
    let mut limited = input.take(chunk.stored_length);
    let decoded = pack::read_entry(&mut limited, pack::MAX_CHUNK_BYTES);
    #[cfg(test)]
    match &decoded {
        Ok(decoded)=>crate::persistent_store::hash_work::observe("native_external_pack_decode",decoded.bytes.len()),
        Err(_)=>crate::persistent_store::hash_work::incomplete("native_external_pack_decode"),
    }
    let decoded=decoded.map_err(corrupt)?;
    if limited.limit() != 0
        || decoded.hash != chunk.plaintext_sha256
        || decoded.bytes.len() as u64 != chunk.plaintext_length
    {
        return Err(corrupt("pack chunk differs"));
    }
    Ok(decoded.bytes)
}

/// Puts an entry that lies in one pack (or in none, when it is empty) where it
/// belongs: a record into the content store, an object into its file.
fn place_whole(
    mut input: Option<(&mut fs::File, u64)>,
    item: &Pending,
    content: &mut ContentStore,
) -> Result<()> {
    let ResolvedEntry {
        entry,
        digest,
        destination,
        ..
    } = &item.resolved;
    let mut next = |chunk: &wire::StoredChunk| match input.as_mut() {
        Some((file, length)) => read_chunk(file, *length, chunk),
        None => Err(corrupt("catalog pack is missing")),
    };
    let Some(destination) = destination else {
        // The store takes the body and decides where it belongs, so a record
        // never becomes a plaintext file of its own that the apply has to
        // copy into another store. The identity the catalog named is proved
        // by the store.
        let mut bytes =
            Vec::with_capacity(usize::try_from(entry.byte_length).map_err(corrupt)?);
        for chunk in &entry.chunks {
            bytes.extend_from_slice(&next(chunk)?);
        }
        if bytes.len() as u64 != entry.byte_length {
            return Err(corrupt("restored entry integrity failed"));
        }
        return content.put(digest, &bytes).map_err(corrupt);
    };
    let partial = destination.with_extension("partial");
    if partial.exists() {
        fs::remove_file(&partial).map_err(transient)?;
    }
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&partial)
        .map_err(transient)?;
    let mut output_hash = Sha256::new();
    #[cfg(test)]
    crate::persistent_store::hash_work::begin("external_restore_entry_assembly");
    let mut written = 0u64;
    for chunk in &entry.chunks {
        let bytes = next(chunk)?;
        output.write_all(&bytes).map_err(transient)?;
        #[cfg(test)]
        crate::persistent_store::hash_work::update("external_restore_entry_assembly",bytes.len());
        output_hash.update(&bytes);
        written = written
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| corrupt("restored entry length overflow"))?;
    }
    output.sync_all().map_err(transient)?;
    drop(output);
    // The bytes were hashed as they were written and the length is what was
    // written, so reading the file back to learn the same two things is a
    // pass with no evidence in it.
    let actual: [u8; 32] = output_hash.finalize().into();
    if actual != entry.content_sha256 || written != entry.byte_length {
        return Err(corrupt("restored entry integrity failed"));
    }
    publish_verified(&partial, destination, entry.byte_length, digest)
}

/// Packs placed since the last flush. Their markers wait until what they
/// placed is durable, so an interruption costs at most a group's packs read
/// again, while small packs do not each pay for a sync and a commit.
struct PlacedGroup {
    packs: Vec<String>,
    bytes: u64,
    /// Assembly files written since the last flush, by content hash.
    touched: BTreeSet<String>,
}

const PLACED_GROUP_PACKS: usize = 32;
const PLACED_GROUP_BYTES: u64 = 64 * 1024 * 1024;

impl PlacedGroup {
    fn full(&self) -> bool {
        self.packs.len() >= PLACED_GROUP_PACKS || self.bytes >= PLACED_GROUP_BYTES
    }

    /// Makes what the group placed durable, then marks its packs done. The
    /// apply reads the content store on its own connection, and a marked pack
    /// is not read again.
    fn flush(&mut self, staging_root: &Path, content: &mut ContentStore) -> Result<()> {
        for digest in std::mem::take(&mut self.touched) {
            let file = OpenOptions::new()
                .write(true)
                .open(assembly_path(staging_root, &digest))
                .map_err(transient)?;
            file.sync_all().map_err(transient)?;
        }
        content.commit().map_err(transient)?;
        for pack in self.packs.drain(..) {
            fs::write(marker_path(staging_root, &pack)?, b"").map_err(transient)?;
        }
        self.bytes = 0;
        Ok(())
    }
}

/// Places everything one open pack carries for `users`. A pack already
/// fetched is placed whole; cancellation is honored between packs.
fn place_pack(
    staging_root: &Path,
    pack_id: &str,
    plaintext: &Path,
    pending: &[Pending],
    users: &[usize],
    content: &mut ContentStore,
    touched: &mut BTreeSet<String>,
) -> Result<()> {
    let mut input = crate::trust_boundary::open_regular_source(plaintext).map_err(corrupt)?;
    let input_length = input.metadata().map_err(corrupt)?.len();
    for &index in users {
        let item = &pending[index];
        if !item.spread() {
            place_whole(Some((&mut input, input_length)), item, content)?;
            continue;
        }
        let path = assembly_path(staging_root, &item.resolved.digest);
        let metadata = fs::symlink_metadata(&path).map_err(transient)?;
        if crate::trust_boundary::is_link_like(&metadata) || !metadata.is_file() {
            return Err(corrupt("snapshot assembly file is not a regular file"));
        }
        let mut output = OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(transient)?;
        for (chunk, offset) in item.resolved.entry.chunks.iter().zip(&item.offsets) {
            if chunk.pack_id != pack_id {
                continue;
            }
            let bytes = read_chunk(&mut input, input_length, chunk)?;
            output.seek(SeekFrom::Start(*offset)).map_err(transient)?;
            output.write_all(&bytes).map_err(transient)?;
        }
        touched.insert(item.resolved.digest.clone());
    }
    Ok(())
}

/// Proves every assembly file whole and gives it to every entry that shares
/// it: a record's store and an object's file are separate places even when
/// the bytes are the same. One that is not what its catalog names takes the
/// markers of all its entries' packs with it, so the next attempt reads
/// exactly those packs again.
fn finish_assemblies(staging_root: &Path, pending: &[Pending]) -> Result<()> {
    let mut content = content_store(staging_root)?;
    let mut shared: BTreeMap<&str, Vec<&Pending>> = BTreeMap::new();
    for item in pending {
        if item.packs.is_empty() {
            place_whole(None, item, &mut content)?;
        } else if item.spread() {
            shared
                .entry(item.resolved.digest.as_str())
                .or_default()
                .push(item);
        }
    }
    for (digest, items) in shared {
        let entry = &items[0].resolved.entry;
        if items
            .iter()
            .any(|item| item.resolved.entry.byte_length != entry.byte_length)
        {
            return Err(corrupt("entries of one content hash differ in length"));
        }
        let record = items.iter().any(|item| item.resolved.destination.is_none());
        // Every object of one hash is written to the same file.
        let file = items
            .iter()
            .find_map(|item| item.resolved.destination.as_deref());
        let path = assembly_path(staging_root, digest);
        let intact = if record {
            // A record goes into the store in memory, so it is proved from
            // the same read.
            let mut source = crate::trust_boundary::open_regular_source(&path).map_err(corrupt)?;
            let mut bytes =
                Vec::with_capacity(usize::try_from(entry.byte_length).map_err(corrupt)?);
            source.read_to_end(&mut bytes).map_err(transient)?;
            #[cfg(test)]
            crate::persistent_store::hash_work::observe("external_restore_record_verify",bytes.len());
            let actual: [u8; 32] = Sha256::digest(&bytes).into();
            (actual == entry.content_sha256 && bytes.len() as u64 == entry.byte_length)
                .then_some(Some(bytes))
        } else {
            verify(&path, entry.byte_length, digest)?.then_some(None)
        };
        let Some(bytes) = intact else {
            let _ = fs::remove_file(&path);
            for pack in items.iter().flat_map(|item| &item.packs) {
                remove_marker(staging_root, pack)?;
            }
            return Err(transient("restored entry integrity failed"));
        };
        if let Some(bytes) = bytes {
            content.put(digest, &bytes).map_err(corrupt)?;
        }
        match file {
            Some(destination) => {
                publish_verified(&path, destination, entry.byte_length, digest)?
            }
            None => fs::remove_file(&path).map_err(transient)?,
        }
    }
    content.commit().map_err(transient)?;
    Ok(())
}

/// Reads the packs a plan still needs one at a time, placing what each one
/// carries and releasing it before the next, so at most one pack's
/// ciphertext and plaintext are on disk however many packs the snapshot has
/// or one entry spans. A pack an interrupted attempt already placed is not
/// read again.
#[allow(clippy::too_many_arguments)]
async fn turn_over_packs(
    plan: Vec<ResolvedEntry>,
    packs: &BTreeMap<String, RemoteObject>,
    root_key: &[u8; 32],
    staging_root: &Path,
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    progress: &PhaseProgress,
    cancel: &Cancellation,
) -> Result<(
    BTreeMap<String, PreparedRecord>,
    BTreeMap<String, PreparedObject>,
)> {
    let mut records = BTreeMap::new();
    let mut objects = BTreeMap::new();
    let mut pending = Vec::new();
    for resolved in plan {
        match resolved.source.clone() {
            Some(source) => insert_prepared(
                resolved.entry,
                resolved.digest,
                source,
                &mut records,
                &mut objects,
            )?,
            None => pending.push(Pending::new(resolved)?),
        }
    }
    // Packs in the order the plan first needs them, each with the entries it
    // carries. A pack no remaining chunk names is never opened.
    let mut order: Vec<String> = Vec::new();
    let mut users: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, item) in pending.iter().enumerate() {
        for chunk in &item.resolved.entry.chunks {
            let entries = users.entry(chunk.pack_id.clone()).or_insert_with(|| {
                order.push(chunk.pack_id.clone());
                Vec::new()
            });
            if entries.last() != Some(&index) {
                entries.push(index);
            }
        }
    }
    let mut required = Vec::with_capacity(order.len());
    for id in &order {
        required.push(
            packs
                .get(id)
                .cloned()
                .ok_or_else(|| corrupt("catalog pack is missing"))?,
        );
    }
    let pending = std::sync::Arc::new(pending);
    let prepared_root = staging_root.to_path_buf();
    let prepared_pending = pending.clone();
    let cpu = cpu_permit().await?;
    spawn_blocking(move || prepare_turnover(&prepared_root, &prepared_pending))
        .await
        .map_err(transient)??;
    drop(cpu);
    // What each catalog needs is known before any of it is fetched, and a
    // later catalog adds to the same total rather than restarting it.
    progress.plan(
        required.len() as u64,
        required.iter().map(|pack| pack.receipt.byte_length).sum(),
    );
    let opened_root = staging_root.to_path_buf();
    let mut content = Some(
        spawn_blocking(move || content_store(&opened_root))
            .await
            .map_err(transient)??,
    );
    let mut group = Some(PlacedGroup {
        packs: Vec::new(),
        bytes: 0,
        touched: BTreeSet::new(),
    });
    let turned = async {
        for (id, pack) in order.iter().zip(&required) {
            cancel.check()?;
            if marker_path(staging_root, id)?.is_file() {
                progress.completed(pack.receipt.byte_length);
                continue;
            }
            let plaintext =
                open_object(pack, root_key, staging_root, provider, repository, cancel).await?;
            #[cfg(test)]
            record_turnover(staging_root, &required);
            // Decrypting was all the ciphertext was for.
            match fs::remove_file(
                staging_root
                    .join("downloads")
                    .join(format!("{}.cipher", pack.ciphertext_sha256)),
            ) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(transient(error));
                }
                _ => {}
            }
            let place_root = staging_root.to_path_buf();
            let place_id = id.clone();
            let place_pending = pending.clone();
            let place_users = users.remove(id).unwrap_or_default();
            let mut place_content = content.take().ok_or_else(|| transient("content store"))?;
            let mut place_group = group.take().ok_or_else(|| transient("placed group"))?;
            let plaintext_length = pack.plaintext_length;
            let cpu = cpu_permit().await?;
            let (returned_content, returned_group, placed) = spawn_blocking(move || {
                let placed = place_pack(
                    &place_root,
                    &place_id,
                    &plaintext,
                    &place_pending,
                    &place_users,
                    &mut place_content,
                    &mut place_group.touched,
                )
                .and_then(|()| {
                    place_group.packs.push(place_id);
                    place_group.bytes = place_group.bytes.saturating_add(plaintext_length);
                    if place_group.full() {
                        place_group.flush(&place_root, &mut place_content)?;
                    }
                    Ok(())
                });
                (place_content, place_group, placed)
            })
            .await
            .map_err(transient)?;
            drop(cpu);
            content = Some(returned_content);
            group = Some(returned_group);
            placed?;
            discard_object(staging_root, pack);
            #[cfg(test)]
            record_turnover(staging_root, &required);
            progress.completed(pack.receipt.byte_length);
        }
        Ok(())
    }
    .await;
    // Packs placed before a cancellation or a failure are kept for the next
    // attempt, which is what makes it resume rather than restart.
    if let (Some(mut flush_content), Some(mut flush_group)) = (content.take(), group.take()) {
        let flush_root = staging_root.to_path_buf();
        let flushed = spawn_blocking(move || {
            flush_group.flush(&flush_root, &mut flush_content)
        })
        .await
        .map_err(transient)?;
        turned?;
        flushed?;
    } else {
        turned?;
    }
    let finish_root = staging_root.to_path_buf();
    let finish_pending = pending.clone();
    let cpu = cpu_permit().await?;
    spawn_blocking(move || finish_assemblies(&finish_root, &finish_pending))
        .await
        .map_err(transient)??;
    drop(cpu);
    // Every entry is where it belongs, so nothing an interrupted attempt
    // would resume from is left to keep.
    let _ = fs::remove_dir_all(staging_root.join("assembly"));
    let _ = fs::remove_dir_all(staging_root.join("turnover"));
    let pending = std::sync::Arc::try_unwrap(pending)
        .map_err(|_| transient("snapshot placement is still running"))?;
    for item in pending {
        let ResolvedEntry {
            entry,
            digest,
            destination,
            ..
        } = item.resolved;
        let source = match destination {
            None => ObjectSource::Captured(digest.clone()),
            Some(destination) => ObjectSource::File(destination),
        };
        insert_prepared(entry, digest, source, &mut records, &mut objects)?;
    }
    Ok((records, objects))
}

/// File one materialized entry under the kind its catalog gave it.
fn insert_prepared(
    entry: CompleteEntry,
    digest: String,
    source: ObjectSource,
    records: &mut BTreeMap<String, PreparedRecord>,
    objects: &mut BTreeMap<String, PreparedObject>,
) -> Result<()> {
    match entry.kind {
        wire::CatalogEntryKind::Record => {
            if records
                .insert(
                    entry.key.clone(),
                    PreparedRecord {
                        key: entry.key,
                        content_hash: digest,
                        byte_length: entry.byte_length,
                        source,
                    },
                )
                .is_some()
            {
                return Err(corrupt("duplicate logical record key"));
            }
        }
        wire::CatalogEntryKind::Object => {
            let prepared = PreparedObject {
                content_hash: digest.clone(),
                byte_length: entry.byte_length,
                source,
            };
            if let Some(old) = objects.insert(digest, prepared.clone()) {
                if old.byte_length != prepared.byte_length {
                    return Err(corrupt("conflicting content object"));
                }
            }
        }
        wire::CatalogEntryKind::SectionEntry | wire::CatalogEntryKind::SectionObject => {
            return Err(corrupt("section entry in library catalog"));
        }
    }
    Ok(())
}

fn validate_section_lengths(entries: &[CompleteEntry]) -> Result<()> {
    use risunest_external_storage_format::section::{MAX_SECTION_ENTRY_BYTES, MAX_SECTION_OBJECT_BYTES};
    for entry in entries {
        let limit = match entry.kind {
            wire::CatalogEntryKind::SectionEntry => MAX_SECTION_ENTRY_BYTES,
            wire::CatalogEntryKind::SectionObject => MAX_SECTION_OBJECT_BYTES,
            _ => return Err(corrupt("non-section entry in section catalog")),
        };
        if entry.byte_length > limit as u64 {
            return Err(corrupt("section source exceeds its codec limit"));
        }
    }
    Ok(())
}

fn assemble(entry: &CompleteEntry, packs: &BTreeMap<String, PathBuf>) -> Result<Vec<u8>> {
    validate_section_lengths(std::slice::from_ref(entry))?;
    let mut bytes = Vec::with_capacity(usize::try_from(entry.byte_length).map_err(corrupt)?);
    let mut digest = Sha256::new();
    #[cfg(test)]
    crate::persistent_store::hash_work::begin("external_restore_section_assembly");
    for chunk in &entry.chunks {
        let pack_path = packs
            .get(&chunk.pack_id)
            .ok_or_else(|| corrupt("catalog pack is missing"))?;
        let mut input = crate::trust_boundary::open_regular_source(pack_path).map_err(corrupt)?;
        let input_length = input.metadata().map_err(corrupt)?.len();
        let decoded = read_chunk(&mut input, input_length, chunk)?;
        #[cfg(test)]
        crate::persistent_store::hash_work::update("external_restore_section_assembly",decoded.len());
        digest.update(&decoded);
        bytes.extend_from_slice(&decoded);
    }
    let actual: [u8; 32] = digest.finalize().into();
    if actual != entry.content_sha256 || bytes.len() as u64 != entry.byte_length {
        return Err(corrupt("received section entry integrity failed"));
    }
    Ok(bytes)
}

fn materialize_section_entries(
    entries: Vec<CompleteEntry>,
    packs: &BTreeMap<String, PathBuf>,
    staging_root: &Path,
    cancel: &Cancellation,
) -> Result<Vec<SectionSource>> {
    let spool = staging_root.join("section-spool");
    ensure_directory(&spool)?;
    let mut sources = Vec::with_capacity(entries.len());
    for entry in entries {
        cancel.check()?;
        let bytes = assemble(&entry, packs)?;
        let digest = hex::encode(entry.content_sha256);
        let destination = spool.join(&digest);
        if !verify(&destination, entry.byte_length, &digest)? {
            let partial = spool.join(format!(".section-{}.partial", uuid::Uuid::new_v4()));
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&partial)
                .map_err(transient)?;
            output.write_all(&bytes).map_err(transient)?;
            output.sync_all().map_err(transient)?;
            drop(output);
            if !verify(&partial, entry.byte_length, &digest)? {
                return Err(corrupt("received section spool integrity failed"));
            }
            publish_verified(&partial, &destination, entry.byte_length, &digest)?;
        }
        drop(bytes);
        sources.push(SectionSource {
            kind: entry.kind,
            key: entry.key,
            content_sha256: digest,
            byte_length: entry.byte_length,
            path: destination,
            offset: None,
        });
    }
    sources.sort_by(|a, b| (a.kind as u8, &a.key).cmp(&(b.kind as u8, &b.key)));
    Ok(sources)
}

/// Downloads only the sections the caller asked for. A section left out of
/// `wanted` is never read, so its values never reach this device.
pub(crate) async fn download_sections(
    snapshot: &RemoteObject,
    wanted: &BTreeSet<String>,
    staging_root: &Path,
    root_key: &[u8; 32],
    record_into: Option<&Path>,
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    progress: &PhaseProgress,
    cancel: &Cancellation,
) -> Result<Vec<CapturedSection>> {
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    ensure_directory(staging_root)?;
    let root_path = open_object(
        snapshot,
        root_key,
        staging_root,
        provider,
        repository,
        cancel,
    )
    .await?;
    let root_bytes = read_bytes(&root_path, wire::MAX_METADATA_BYTES)?;
    let wire_role = match snapshot.role {
        ObjectRole::SyncState => wire::ObjectRole::SyncState,
        ObjectRole::BackupBundle => wire::ObjectRole::BackupBundle,
        _ => return Err(corrupt("snapshot root role")),
    };
    let document =
        super::control::SnapshotView::read(&root_bytes, wire_role, &snapshot.repository_id)?;
    let mut prepared = Vec::new();
    for (id, reference) in &document.sections {
        if !wanted.contains(id) {
            continue;
        }
        let entries_root = RemoteObject::from_stored(&reference.entries_root, repository)?;
        if entries_root.repository_id != snapshot.repository_id {
            return Err(corrupt("section catalog repository differs"));
        }
        let (complete, packs, nodes) = read_catalog(
            &entries_root,
            wire::CatalogKind::Section,
            root_key,
            staging_root,
            provider,
            repository,
            cancel,
        )
        .await?;
        record_read(
            record_into,
            &super::packaging::CatalogRoot::Section(id.clone()),
            &entries_root,
            &complete,
            &packs,
            &nodes,
            repository,
        );
        validate_section_lengths(&complete)?;
        let pack_paths =
            open_packs(&packs, root_key, staging_root, provider, repository, progress, cancel)
                .await?;
        let cpu = cpu_permit().await?;
        let section_cancel = cancel.clone();
        let section_staging = staging_root.to_path_buf();
        let sources = spawn_blocking(move || {
            materialize_section_entries(complete, &pack_paths, &section_staging, &section_cancel)
        })
        .await
        .map_err(transient)??;
        drop(cpu);
        prepared.push(CapturedSection {
            kind: reference.kind,
            generation: reference.generation.clone(),
            gc_floor: reference.gc_floor.clone(),
            max_write_clock: reference.max_write_clock.clone(),
            content_fingerprint: reference.content_fingerprint,
            sources,
        });
    }
    Ok(prepared)
}

pub(crate) async fn download_checkpoint_data(
    root: &RemoteObject, staging_root: &Path, root_key: &[u8;32],
    provider: &dyn Provider, repository: &RepositoryHandle, cancel: &Cancellation,
) -> Result<(Vec<PreparedRecord>, Vec<PreparedObject>)> {
    ensure_directory(staging_root)?;
    let (entries,packs,_) = read_catalog(root,wire::CatalogKind::Records,root_key,staging_root,provider,repository,cancel).await?;
    let content=content_store(staging_root)?;
    let plan=resolve_entries(entries,staging_root,&content,None,cancel)?;
    let (records,objects)=turn_over_packs(plan,&packs,root_key,staging_root,provider,repository,&PhaseProgress::silent(),cancel).await?;
    Ok((records.into_values().collect(),objects.into_values().collect()))
}

pub(crate) async fn download_control_catalogs(
    catalogs:&[wire::StoredObject],staging_root:&Path,root_key:&[u8;32],
    provider:&dyn Provider,repository:&RepositoryHandle,cancel:&Cancellation,
) -> Result<Vec<PreparedObject>> {
    let mut objects=BTreeMap::new();let mut roots=BTreeMap::new();
    for catalog in catalogs {
        cancel.check()?;
        if catalog.header.repository_id!=repository.repository_id || catalog.header.role!=wire::ObjectRole::Catalog {
            return Err(corrupt("control catalog scope differs"));
        }
        if let Some(previous)=roots.insert(catalog.header.object_id.clone(),catalog.clone()) {
            if previous!=*catalog {return Err(corrupt("control catalog identity differs"));}
            continue;
        }
        let remote=RemoteObject::from_stored(catalog,repository)?;
        let stage=staging_root.join(&catalog.header.object_id);
        let (records,controls)=download_checkpoint_data(&remote,&stage,root_key,provider,repository,cancel).await?;
        if !records.is_empty() {return Err(corrupt("control catalog contains unit records"));}
        for mut control in controls {
            if let ObjectSource::Captured(hash)=&control.source {
                let content=content_store(&stage)?;let mut reader=content.open_body(hash).map_err(transient)?;
                let path=stage.join(format!("{hash}.control"));let mut file=std::fs::File::create(&path).map_err(transient)?;
                if std::io::copy(&mut reader,&mut file).map_err(transient)?!=control.byte_length {return Err(corrupt("control file length differs"));}
                file.sync_all().map_err(transient)?;control.source=ObjectSource::File(path);
            }
            if let Some(previous)=objects.insert(control.content_hash.clone(),control.clone()) {
                if previous.byte_length!=control.byte_length {return Err(corrupt("control body length differs"));}
            }
        }
    }
    Ok(objects.into_values().collect())
}

pub(crate) async fn revalidate_catalog(
    catalog:&wire::StoredObject,kind:wire::CatalogKind,root_key:&[u8;32],provider:&dyn Provider,
    repository:&RepositoryHandle,cancel:&Cancellation,
) -> Result<()> {
    if !matches!(kind,wire::CatalogKind::Records|wire::CatalogKind::Assets)
        || catalog.header.repository_id!=repository.repository_id || catalog.header.role!=wire::ObjectRole::Catalog {return Err(corrupt("dependency catalog scope differs"));}
    let stage=tempfile::tempdir().map_err(transient)?;
    let remote=RemoteObject::from_stored(catalog,repository)?;
    let (entries,packs,_)=read_catalog(&remote,kind,root_key,stage.path(),provider,repository,cancel).await?;
    if entries.iter().any(|entry|entry.kind!=wire::CatalogEntryKind::Object) {return Err(corrupt("dependency catalog contains unit records"));}
    for pack in packs.values() {
        cancel.check()?;
        let body=tempfile::tempdir_in(stage.path()).map_err(transient)?;
        open_object(pack,root_key,body.path(),provider,repository,cancel).await?;
    }
    Ok(())
}

pub(crate) async fn admit_data_catalogs(
    catalogs:&[wire::StoredObject],store:&mut crate::persistent_store::PersistentStore,target:&str,root_key:&[u8;32],
    provider:&dyn Provider,repository:&RepositoryHandle,cancel:&Cancellation,
) -> Result<Vec<String>> {
    if catalogs.is_empty() {return Ok(Vec::new());}
    let stage=tempfile::tempdir().map_err(transient)?;
    let mut hashes=BTreeSet::new();let mut roots=BTreeMap::new();
    for catalog in catalogs {
        cancel.check()?;
        if catalog.header.repository_id!=repository.repository_id || catalog.header.role!=wire::ObjectRole::Catalog {return Err(corrupt("control catalog scope differs"));}
        if let Some(previous)=roots.insert(catalog.header.object_id.clone(),catalog.clone()) {
            if previous!=*catalog {return Err(corrupt("control catalog identity differs"));}continue;
        }
        if let Some(known)=store.external_lww_verified_data_catalog(target,catalog).map_err(corrupt)? {
            let mut complete=true;
            for hash in &known {if !store.lww_verified_object_present(hash).map_err(corrupt)? {complete=false;break;}}
            if complete {hashes.extend(known);continue;}
        }
        let remote=RemoteObject::from_stored(catalog,repository)?;
        let directory=stage.path().join(&catalog.header.object_id);ensure_directory(&directory)?;
        let (entries,packs,_)=read_catalog(&remote,wire::CatalogKind::Records,root_key,&directory,provider,repository,cancel).await?;
        let mut required=BTreeSet::new();let mut missing=Vec::new();
        for entry in entries {
            if entry.kind!=wire::CatalogEntryKind::Object {return Err(corrupt("control catalog contains unit records"));}
            let hash=hex::encode(entry.content_sha256);required.insert(hash.clone());
            if let Some(size)=store.external_lww_verified_control_size(&hash).map_err(corrupt)? {
                if size!=entry.byte_length {return Err(corrupt("known control body length differs"));}
            } else {missing.push(entry);}
        }
        let content=content_store(&directory)?;
        let plan=resolve_entries(missing,&directory,&content,None,cancel)?;
        let (records,controls)=turn_over_packs(plan,&packs,root_key,&directory,provider,repository,&PhaseProgress::silent(),cancel).await?;
        if !records.is_empty() {return Err(corrupt("control catalog contains unit records"));}
        for control in controls.into_values() {
            cancel.check()?;
            let ObjectSource::File(path)=control.source else {return Err(corrupt("control file is unavailable"));};
            let bytes=read_bytes(&path,usize::try_from(control.byte_length).map_err(corrupt)?)?;
            if bytes.len() as u64!=control.byte_length || crate::persistent_store::external_capture::hash_backup_body(&bytes,"native_data_catalog_control_identity")!=control.content_hash {return Err(corrupt("control body identity differs"));}
            if bytes.len()>risunest_sync_wire::MAX_METADATA_BYTES {crate::persistent_store::external_capture::verified_large_message_page(&bytes).map_err(corrupt)?;}
            store.lww_put_object(&control.content_hash,&bytes).map_err(corrupt)?;
        }
        let required=required.into_iter().collect::<Vec<_>>();
        store.external_lww_witness_data_catalog(target,catalog,&required).map_err(corrupt)?;
        hashes.extend(required);
    }
    Ok(hashes.into_iter().collect())
}

pub(crate) async fn download_packed_body(hash:&str,length:u64,chunks:Vec<wire::StoredChunk>,packs:Vec<wire::StoredObject>,stage:&Path,key:&[u8;32],provider:&dyn Provider,repository:&RepositoryHandle,cancel:&Cancellation)->Result<Vec<u8>> {
    let path=download_packed_body_file(hash,length,chunks,packs,stage,key,provider,repository,cancel).await?;
    std::fs::read(path).map_err(transient)
}
pub(crate) async fn download_packed_body_files(
    sources:&[super::lww_residency::PackedSource],staging_root:&Path,root_key:&[u8;32],
    provider:&dyn Provider,repository:&RepositoryHandle,cancel:&Cancellation,
)->Result<BTreeMap<String,PathBuf>> {
    ensure_directory(staging_root)?;
    let mut packs=BTreeMap::new();let mut entries=Vec::new();let mut hashes=BTreeSet::new();
    for source in sources {
        cancel.check()?;super::lww_residency::validate_packed_source(source,repository)?;
        if !hashes.insert(source.hash.clone()) {return Err(corrupt("duplicate body"));}
        for stored in &source.packs {
            let pack=RemoteObject::from_stored(stored,repository)?;
            if let Some(previous)=packs.insert(pack.object_id.clone(),pack.clone()) {
                if previous!=pack {return Err(corrupt("conflicting pack"));}
            }
        }
        entries.push(CompleteEntry {kind:wire::CatalogEntryKind::Object,key:format!("object/{}",source.hash),content_sha256:hex::decode(&source.hash).map_err(corrupt)?.try_into().map_err(|_|corrupt("body hash"))?,byte_length:source.byte_length,chunks:source.chunks.clone()});
    }
    let content=content_store(staging_root)?;
    let mut plan=resolve_entries(entries,staging_root,&content,None,cancel)?;
    for resolved in &mut plan {resolved.destination=Some(staging_root.join(format!("{}.payload",resolved.digest)));}
    let (_,objects)=turn_over_packs(plan,&packs,root_key,staging_root,provider,repository,&PhaseProgress::silent(),cancel).await?;
    objects.into_iter().map(|(hash,object)|match object.source {ObjectSource::File(path)=>Ok((hash,path)),_=>Err(corrupt("body source"))}).collect()
}

pub(crate) async fn download_packed_body_file(
    hash:&str, length:u64, chunks:Vec<wire::StoredChunk>, stored_packs:Vec<wire::StoredObject>, staging_root:&Path, root_key:&[u8;32],
    provider:&dyn Provider, repository:&RepositoryHandle,cancel:&Cancellation,
) -> Result<PathBuf> {
    ensure_directory(staging_root)?;
    let mut packs=BTreeMap::new();
    for stored in &stored_packs { let pack=RemoteObject::from_stored(stored,repository)?; packs.insert(pack.object_id.clone(),pack); }
    let entry=CompleteEntry {kind:wire::CatalogEntryKind::Object,key:format!("object/{hash}"),content_sha256:hex::decode(hash).map_err(corrupt)?.try_into().map_err(|_|corrupt("body hash"))?,byte_length:length,chunks};
    let content=content_store(staging_root)?;
    let plan=resolve_entries(vec![entry],staging_root,&content,None,cancel)?;
    let (_,objects)=turn_over_packs(plan,&packs,root_key,staging_root,provider,repository,&PhaseProgress::silent(),cancel).await?;
    let body=objects.get(hash).ok_or_else(||corrupt("body missing"))?;
    match &body.source { ObjectSource::File(path)=>Ok(path.clone()), _=>Err(corrupt("body source")) }
}

pub(crate) async fn download_original_backup_units(
    root: &RemoteObject,
    staging_root: &Path,
    key: &[u8; 32],
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    cancel: &Cancellation,
) -> Result<BTreeMap<risunest_sync_wire::unit::UnitKey, risunest_sync_wire::unit::UnitValue>> {
    let (records, objects) = download_checkpoint_data(
        root, staging_root, key, provider, repository, cancel,
    ).await?;
    if !objects.is_empty() { return Err(corrupt("original unit catalog contains non-unit controls")); }
    let content = content_store(staging_root)?;
    let mut units = BTreeMap::new();
    for record in records {
        cancel.check()?;
        if record.byte_length > risunest_sync_wire::MAX_METADATA_BYTES as u64 {
            return Err(corrupt("original unit control exceeds its bound"));
        }
        let mut source=content.open_body(&record.content_hash).map_err(transient)?;
        let mut bytes=Vec::new();
        source.by_ref().take(risunest_sync_wire::MAX_METADATA_BYTES as u64+1).read_to_end(&mut bytes).map_err(transient)?;
        if bytes.len() as u64 != record.byte_length {
            return Err(corrupt("original unit control length"));
        }
        let unit: super::packaging::OriginalBackupUnit = risunest_sync_wire::canonical::decode(
            &bytes, risunest_sync_wire::MAX_METADATA_BYTES,
        ).map_err(corrupt)?;
        if unit.schema != "risunest.backup-unit/v1"
            || record.key != format!("original-unit/{}", hex::encode(unit.key.as_str()))
        {
            return Err(corrupt("original unit catalog key differs from its control"));
        }
        #[cfg(test)]
        crate::persistent_store::hash_work::validation(&unit.value);
        unit.value.validate().map_err(corrupt)?;
        if units.insert(unit.key, unit.value).is_some() {
            return Err(corrupt("duplicate original unit"));
        }
    }
    Ok(units)
}

pub(crate) async fn download_backup_original_units(
    snapshot:&RemoteObject, staging_root:&Path, key:&[u8;32], provider:&dyn Provider,
    repository:&RepositoryHandle, cancel:&Cancellation,
) -> Result<BTreeMap<risunest_sync_wire::unit::UnitKey,risunest_sync_wire::unit::UnitValue>> {
    if snapshot.role != ObjectRole::BackupBundle {return Err(corrupt("full backup is required"))}
    let root=open_object(snapshot,key,staging_root,provider,repository,cancel).await?;
    let document=super::control::SnapshotView::read(&read_bytes(&root,wire::MAX_METADATA_BYTES)?,wire::ObjectRole::BackupBundle,&snapshot.repository_id)?;
    if snapshot.object_id != format!("snapshot-{}",document.snapshot_id) {return Err(corrupt("backup identity differs"))}
    let original=RemoteObject::from_stored(document.original_units.as_ref().ok_or_else(|| corrupt("complete original unit root is required"))?,repository)?;
    download_original_backup_units(&original,staging_root,key,provider,repository,cancel).await
}

pub(crate) struct DatabaseFirstSnapshot {
    pub snapshot:PreparedRemoteSnapshot,
    pub original_units:BTreeMap<risunest_sync_wire::unit::UnitKey,risunest_sync_wire::unit::UnitValue>,
    pub required:BTreeSet<String>,
    pub present:BTreeSet<String>,
    pub missing:BTreeSet<String>,
    pub sources:Vec<super::lww_residency::PackedSource>,
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn admit_asset_catalogs(
    catalogs:&[wire::StoredObject],store:&mut crate::persistent_store::PersistentStore,target:&str,
    library_id:&str,protected_segment:&str,connection_id:&str,connection_root:&Path,root_key:&[u8;32],
    provider:&dyn Provider,repository:&RepositoryHandle,cancel:&Cancellation,
) -> Result<Vec<String>> {
    let scratch=super::leftovers::managed_scratch(store.repository_root(),"asset-catalog-")?;
    let metadata=rusqlite::Connection::open_with_flags(store.repository_root().join("persistent/persistent.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(transient)?;
    let cas=PayloadCas::new(store.repository_root()).map_err(transient)?;
    let mut planned=BTreeMap::<String,super::lww_residency::PackedSource>::new();let mut proofs=Vec::new();
    for catalog in catalogs {
        if catalog.header.repository_id!=repository.repository_id || catalog.header.role!=wire::ObjectRole::Catalog {
            return Err(corrupt("asset catalog repository or role differs"));
        }
        let root=RemoteObject::from_stored(catalog,repository)?;
        let (entries,packs,_)=read_catalog(&root,wire::CatalogKind::Assets,root_key,scratch.path(),provider,repository,cancel).await?;
        let mut witnessed=Vec::new();
        for entry in entries {
            cancel.check()?;
            if entry.kind!=wire::CatalogEntryKind::Object {return Err(corrupt("asset catalog kind differs"));}
            let hash=hex::encode(entry.content_sha256);
            let catalogued:Option<i64>=metadata.query_row("SELECT byte_size FROM asset_objects WHERE object_hash=?1",[&hash],|row|row.get(0)).optional().map_err(transient)?;
            let catalogued=catalogued.map(u64::try_from).transpose().map_err(corrupt)?;
            if catalogued.is_some_and(|size|size!=entry.byte_length)
                || cas.stat_object(&hash).map_err(transient)?.is_some_and(|size|size!=entry.byte_length) {
                return Err(corrupt("immutable asset size differs"));
            }
            let ids=entry.chunks.iter().map(|chunk|chunk.pack_id.as_str()).collect::<BTreeSet<_>>();
            let references=packs.iter().filter(|(id,_)|ids.contains(id.as_str())).map(|(_,pack)|pack.stored(repository)).collect::<Result<Vec<_>>>()?;
            let source=super::lww_residency::PackedSource{hash:hash.clone(),byte_length:entry.byte_length,
                library_id:library_id.into(),connection_id:connection_id.into(),connection_root:connection_root.into(),
                protected_snapshot:protected_segment.into(),catalog:catalog.clone(),chunks:entry.chunks,packs:references};
            super::lww_residency::validate_packed_source(&source,repository)?;
            witnessed.push((hash.clone(),source.byte_length));
            if let Some(previous)=planned.get(&hash) {
                if previous.byte_length!=source.byte_length {return Err(corrupt("conflicting asset catalog body"));}
            } else {planned.insert(hash,source);}
        }
        proofs.push((catalog,witnessed));
    }
    cancel.check()?;
    for (catalog,entries) in proofs {store.external_lww_witness_asset_catalog(target,catalog,&entries).map_err(corrupt)?;}
    let registrations=planned.values().map(|source|crate::persistent_store::asset_object_catalog::AssetObjectRegistration{object_hash:source.hash.clone(),byte_size:source.byte_length}).collect::<Vec<_>>();
    for batch in registrations.chunks(crate::persistent_store::asset_object_catalog::ASSET_OBJECT_CATALOG_MAX_PAGE as usize) {
        store.asset_object_catalog().register(batch,super::runtime::now_ms() as i64).map_err(transient)?;
    }
    for source in planned.values() {
        cancel.check()?;
        super::lww_residency::register_packed(store.repository_root(),source,repository)?;
    }
    Ok(planned.into_keys().collect())
}
pub(crate) async fn download_snapshot_database_first(snapshot:&RemoteObject,staging_root:&Path,library_root:&Path,connection_root:&Path,connection_id:&str,key:&[u8;32],provider:&dyn Provider,repository:&RepositoryHandle,cancel:&Cancellation)->Result<DatabaseFirstSnapshot> {
    if snapshot.role!=ObjectRole::BackupBundle {return Err(corrupt("full backup required"));}
    ensure_directory(staging_root)?;
    let root_path=open_object(snapshot,key,staging_root,provider,repository,cancel).await?;
    let document=super::control::SnapshotView::read(&read_bytes(&root_path,wire::MAX_METADATA_BYTES)?,wire::ObjectRole::BackupBundle,&snapshot.repository_id)?;
    if snapshot.object_id!=format!("snapshot-{}",document.snapshot_id) {return Err(corrupt("backup identity"));}
    let original_root = document.original_units.as_ref()
        .ok_or_else(|| corrupt("full backup original unit root is required"))?;
    let original_root = RemoteObject::from_stored(original_root, repository)?;
    let original_units = download_original_backup_units(
        &original_root, staging_root, key, provider, repository, cancel,
    ).await?;
    let records_root=RemoteObject::from_stored(&document.library.record_catalog,repository)?;
    let (records,mut objects)=download_checkpoint_data(&records_root,staging_root,key,provider,repository,cancel).await?;
    let assets_root=RemoteObject::from_stored(&document.library.asset_catalog,repository)?;
    let (entries,packs,_)=read_catalog(&assets_root,wire::CatalogKind::Assets,key,staging_root,provider,repository,cancel).await?;
    let cas=PayloadCas::new(library_root).map_err(transient)?;
    let metadata = rusqlite::Connection::open_with_flags(
        library_root.join("persistent/persistent.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ).map_err(transient)?;
    let mut required=BTreeSet::new();let mut present=BTreeSet::new();let mut missing=BTreeSet::new();
    let mut sources=Vec::new();
    for entry in entries {
        if entry.kind!=wire::CatalogEntryKind::Object {return Err(corrupt("asset catalog kind"));}
        let hash=hex::encode(entry.content_sha256);
        if !required.insert(hash.clone()) {return Err(corrupt("duplicate asset body"));}
        let catalogued: Option<i64> = metadata.query_row(
            "SELECT byte_size FROM asset_objects WHERE object_hash=?1", [&hash], |row| row.get(0),
        ).optional().map_err(transient)?;
        let catalogued = catalogued.map(u64::try_from).transpose().map_err(corrupt)?;
        if catalogued.is_some_and(|size| size != entry.byte_length) {
            return Err(corrupt("local immutable body size differs from backup"));
        }
        let local_size=cas.stat_object(&hash).map_err(transient)?;
        if local_size.is_some_and(|size|size!=entry.byte_length) {
            return Err(corrupt("local immutable body size differs from backup"));
        }
        if local_size==Some(entry.byte_length) {present.insert(hash.clone());} else {
            missing.insert(hash.clone());
        }
        let ids=entry.chunks.iter().map(|chunk|chunk.pack_id.as_str()).collect::<BTreeSet<_>>();
        let references=packs.iter().filter(|(id,_)|ids.contains(id.as_str())).map(|(_,pack)|pack.stored(repository)).collect::<Result<Vec<_>>>()?;
        let source=super::lww_residency::PackedSource {hash:hash.clone(),byte_length:entry.byte_length,library_id:snapshot.repository_id.clone(),connection_id:connection_id.into(),connection_root:connection_root.into(),protected_snapshot:snapshot.object_id.clone(),catalog:document.library.asset_catalog.clone(),chunks:entry.chunks,packs:references};
        super::lww_residency::validate_packed_source(&source,repository)?;
        sources.push(source);
        objects.push(PreparedObject{content_hash:hash.clone(),byte_length:entry.byte_length,source:ObjectSource::Library(hash)});
    }
    Ok(DatabaseFirstSnapshot{snapshot:PreparedRemoteSnapshot{snapshot_id:document.snapshot_id,repository_id:snapshot.repository_id.clone(),fingerprint:hex::encode(document.library.content_fingerprint),library_fingerprint:hex::encode(document.library.content_fingerprint),logical_revision:document.revision.parse().map_err(corrupt)?,staging_root:staging_root.into(),records,objects,captured_by_device:document.captured_by_device},original_units,required,present,missing,sources})
}
pub(crate) async fn download_snapshot(
    snapshot: &RemoteObject,
    staging_root: &Path,
    root_key: &[u8; 32],
    record_into: Option<&Path>,
    trust: SourceTrust<'_>,
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    progress: &PhaseProgress,
    cancel: &Cancellation,
) -> Result<PreparedRemoteSnapshot> {
    if !matches!(
        snapshot.role,
        ObjectRole::SyncState | ObjectRole::BackupBundle
    ) {
        return Err(corrupt("snapshot root role"));
    }
    ensure_directory(staging_root)?;
    let root_path = open_object(
        snapshot,
        root_key,
        staging_root,
        provider,
        repository,
        cancel,
    )
    .await?;
    let root_bytes = read_bytes(&root_path, wire::MAX_METADATA_BYTES)?;
    let wire_role = match snapshot.role {
        ObjectRole::SyncState => wire::ObjectRole::SyncState,
        _ => wire::ObjectRole::BackupBundle,
    };
    let document =
        super::control::SnapshotView::read(&root_bytes, wire_role, &snapshot.repository_id)?;
    if snapshot.object_id != format!("snapshot-{}", document.snapshot_id) {
        return Err(corrupt("snapshot identity differs"));
    }
    let record_catalog = RemoteObject::from_stored(&document.library.record_catalog, repository)?;
    let asset_catalog = RemoteObject::from_stored(&document.library.asset_catalog, repository)?;
    if record_catalog.repository_id != snapshot.repository_id
        || asset_catalog.repository_id != snapshot.repository_id
    {
        return Err(corrupt("snapshot catalog repository differs"));
    }
    let (record_entries, mut record_packs, record_nodes) = read_catalog(
        &record_catalog,
        wire::CatalogKind::Records,
        root_key,
        staging_root,
        provider,
        repository,
        cancel,
    )
    .await?;
    record_read(
        record_into,
        &super::packaging::CatalogRoot::Records,
        &record_catalog,
        &record_entries,
        &record_packs,
        &record_nodes,
        repository,
    );
    let (asset_entries, asset_packs, asset_nodes) = read_catalog(
        &asset_catalog,
        wire::CatalogKind::Assets,
        root_key,
        staging_root,
        provider,
        repository,
        cancel,
    )
    .await?;
    record_read(
        record_into,
        &super::packaging::CatalogRoot::Assets,
        &asset_catalog,
        &asset_entries,
        &asset_packs,
        &asset_nodes,
        repository,
    );
    for (id, object) in asset_packs {
        if let Some(old) = record_packs.insert(id, object.clone()) {
            if old != object {
                return Err(corrupt("conflicting snapshot pack"));
            }
        }
    }
    let library_staging = staging_root.to_path_buf();
    let library_cancel = cancel.clone();
    let library = match trust {
        SourceTrust::Downloaded => None,
        SourceTrust::ProvenLibrary(root) => Some(LocalLibrary {
            root: root.to_path_buf(),
            prove: true,
        }),
        SourceTrust::AdmittedLibrary(root) => {
            Some(LocalLibrary {
                root: root.to_path_buf(),
                prove: false,
            })
        }
    };
    let cpu = cpu_permit().await?;
    let plan = spawn_blocking(move || {
        let content = content_store(&library_staging)?;
        let mut data=resolve_entries(record_entries,&library_staging,&content,None,&library_cancel)?;
        let assets=resolve_entries(asset_entries,&library_staging,&content,library,&library_cancel)?;
        data.extend(assets);
        Ok::<_,ProviderError>(data)
    })
    .await
    .map_err(transient)??;
    drop(cpu);
    let (records, objects) = turn_over_packs(
        plan,
        &record_packs,
        root_key,
        staging_root,
        provider,
        repository,
        progress,
        cancel,
    )
    .await?;
    Ok(PreparedRemoteSnapshot {
        snapshot_id: document.snapshot_id,
        repository_id: snapshot.repository_id.clone(),
        fingerprint: hex::encode(document.library.content_fingerprint),
        library_fingerprint: hex::encode(document.library.content_fingerprint),
        logical_revision: document.revision.parse().unwrap_or_default(),
        staging_root: staging_root.to_path_buf(),
        records: records.into_values().collect(),
        objects: objects.into_values().collect(),
        captured_by_device: document.captured_by_device,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_section_is_rejected_before_pack_access_or_allocation() {
        let entry = CompleteEntry {
            kind: wire::CatalogEntryKind::SectionObject,
            key: "object/oversized".into(),
            content_sha256: [0; 32],
            byte_length: u64::MAX,
            chunks: Vec::new(),
        };
        assert_eq!(assemble(&entry, &BTreeMap::new()).unwrap_err().kind, ErrorKind::Corrupt);
        assert!(validate_section_lengths(&[entry]).is_err());
    }


    #[test]
    fn test_read_counts_are_scoped_to_registered_staging_root() {
        let root_a = Path::new("snapshot-restore-read-count-root-a");
        let root_b = Path::new("snapshot-restore-read-count-root-b");
        reset_test_read_counts(root_a);

        record_test_read(&root_a.join("downloads"), false, 4);
        record_test_read(&root_a.join("downloads"), true, SMALL_PACK_BYTES);
        record_test_read(&root_a.join("downloads"), true, SMALL_PACK_BYTES + 1);
        record_test_read(&root_b.join("downloads"), true, 8);

        let counts = take_test_read_counts(root_a);
        assert_eq!(
            counts,
            TestReads {
                network: 3,
                network_bytes: SMALL_PACK_BYTES * 2 + 5,
                packs: 2,
                pack_bytes: SMALL_PACK_BYTES * 2 + 1,
                small_packs: 1,
            }
        );
    }
}
