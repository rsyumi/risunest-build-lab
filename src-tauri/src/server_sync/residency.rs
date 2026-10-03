//! Device-local payload custody. This is physical residency metadata, not a
//! logical record or a synchronized preference. Credentials remain OS-protected.
use super::{credentials::StoredConfig, Result, SyncError};
use risunest_sync_wire::{hash, validate_hash, RemoteHead, Sequence};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

#[cfg(test)]
thread_local! {
    pub(crate) static OPEN_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// Closing the last connection tears down the write-ahead log, and opens that
// race the teardown fail with SQLITE_PROTOCOL. While armed, one idle
// connection stays open so per-use connections are never the last one.
struct Anchor {
    root: PathBuf,
    db: Option<Connection>,
}

static ANCHOR: Mutex<Option<Anchor>> = Mutex::new(None);

fn anchor() -> MutexGuard<'static, Option<Anchor>> {
    ANCHOR.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Keeps a connection to `root`'s store open after it next opens. Arming does
/// not create the store.
pub(crate) fn arm_anchor(root: &Path) {
    *anchor() = Some(Anchor {
        root: root.to_path_buf(),
        db: None,
    });
}

#[cfg(any(mobile, test))]
pub(crate) fn release_anchor() {
    anchor().take();
}

fn hold_anchor(root: &Path, path: &Path) {
    let mut anchor = anchor();
    let Some(anchor) = anchor.as_mut().filter(|anchor| anchor.root == root) else {
        return;
    };
    if anchor.db.is_some() {
        return;
    }
    // Only a connection that has read holds the shared lock that keeps the
    // log alive. Failure leaves the next open to retry.
    anchor.db = Connection::open(path)
        .and_then(|db| {
            db.query_row("PRAGMA user_version", [], |_| Ok(()))?;
            Ok(db)
        })
        .ok();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum AssetPolicy {
    Full,
    Remote,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RetainedObject {
    pub hash: String,
    pub size: Sequence,
    pub retention_id: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RemoteObject {
    pub context: String,
    pub hash: String,
    pub size: u64,
    pub retention_id: String,
    pub device_id: String,
    pub config: StoredConfig,
}

pub(crate) struct Residency {
    db: Connection,
    root: PathBuf,
}

impl Residency {
    pub fn path(root: &Path) -> PathBuf {
        root.join("server-sync").join("asset-residency.sqlite")
    }
    pub fn exists(root: &Path) -> bool {
        Self::path(root).exists()
    }
    pub fn open(root: &Path) -> Result<Self> {
        #[cfg(test)]
        OPEN_COUNT.with(|count| count.set(count.get() + 1));
        let directory = root.join("server-sync");
        std::fs::create_dir_all(&directory)?;
        let metadata = std::fs::symlink_metadata(&directory)?;
        if !metadata.is_dir() || crate::trust_boundary::is_link_like(&metadata) {
            return Err(SyncError::new("unsafe-residency-path", 409));
        }
        let path = std::fs::canonicalize(&directory)?.join("asset-residency.sqlite");
        for suffix in ["", "-wal", "-shm"] {
            let target = PathBuf::from(format!("{}{suffix}", path.display()));
            match std::fs::symlink_metadata(target) {
                Ok(metadata)
                    if !metadata.is_file() || crate::trust_boundary::is_link_like(&metadata) =>
                {
                    return Err(SyncError::new("unsafe-residency-path", 409))
                }
                Ok(_) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        let mut db = Connection::open(&path)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version == 0 {
            let tx = db.transaction()?;
            let count: i64 =
                tx.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
            if count != 0 {
                return Err(SyncError::new("invalid-residency-store", 409));
            }
            tx.execute_batch("CREATE TABLE contexts(id TEXT PRIMARY KEY,library_id TEXT NOT NULL,device_id TEXT NOT NULL,epoch TEXT NOT NULL,config TEXT NOT NULL);
                CREATE TABLE objects(context TEXT NOT NULL REFERENCES contexts(id),hash TEXT NOT NULL,size INTEGER NOT NULL CHECK(size>=0),retention_id TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('active','releasing','released')),PRIMARY KEY(context,hash));
                CREATE INDEX objects_hash ON objects(hash);
                PRAGMA user_version=1;")?;
            tx.commit()?;
        } else if version != 1 {
            return Err(SyncError::new("incompatible-residency-store", 409));
        }
        hold_anchor(root, &path);
        Ok(Self { db, root: root.to_path_buf() })
    }
    /// A fresh registration restores access to the same library's historical
    /// custody. Keep its original owner ID for eventual retention release.
    pub fn replace_access_config(&self, config: &StoredConfig) -> Result<()> {
        self.db.execute(
            "UPDATE contexts SET config=?1 WHERE library_id=?2 AND config<>?1",
            params![
                serde_json::to_string(config)
                    .map_err(|_| SyncError::new("invalid-retention-config", 409))?,
                config.library_id
            ],
        )?;
        Ok(())
    }
    pub fn context_id(config: &StoredConfig, epoch: &str) -> String {
        #[cfg(test)]
        super::hash_metrics::record("c_context_identity", format!("{}\0{}\0{}", config.library_id, config.device_id, epoch).len());
        hash(format!("{}\0{}\0{}", config.library_id, config.device_id, epoch).as_bytes())
    }
    pub fn confirm(
        &mut self,
        config: &StoredConfig,
        head: &RemoteHead,
        objects: &[RetainedObject],
    ) -> Result<()> {
        head.validate()?;
        if config.library_id != head.library_id {
            return Err(SyncError::new("retention-library-mismatch", 409));
        }
        let context = Self::context_id(config, &head.epoch);
        let tx = self.db.transaction()?;
        tx.execute("INSERT INTO contexts VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET config=excluded.config",params![context,config.library_id,config.device_id,head.epoch,serde_json::to_string(config).map_err(|_|SyncError::new("invalid-retention-config",409))?])?;
        for object in objects {
            validate_hash(&object.hash)?;
            validate_hash(&object.retention_id)?;
            let size = object
                .size
                .as_str()
                .parse::<i64>()
                .map_err(|_| SyncError::new("invalid-retention-size", 409))?;
            tx.execute("INSERT INTO objects VALUES(?1,?2,?3,?4,'active') ON CONFLICT(context,hash) DO UPDATE SET size=excluded.size,retention_id=excluded.retention_id,state='active'",params![context,object.hash,size,object.retention_id])?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Persist a positively acknowledged custody grant before a dependency can
    /// be activated without bytes or an existing local file can be evicted.
    pub fn retain(
        &mut self,
        client: &super::client::ServerClient,
        config: &StoredConfig,
        head: &RemoteHead,
        objects: &[(String, Option<u64>)],
    ) -> Result<()> {
        for page in objects.chunks(1024) {
            let requests=page.iter().map(|(hash,size)|serde_json::json!({"hash":hash,"size":size.map(|size|size.to_string())})).collect::<Vec<_>>();
            let (_, retained): (_, Vec<RetainedObject>) = client.json(
                reqwest::Method::POST,
                "objects/retention",
                &[],
                Some(&serde_json::json!({"epoch":head.epoch,"objects":requests})),
                &[],
            )?;
            let mut expected = page
                .iter()
                .cloned()
                .collect::<std::collections::BTreeMap<_, _>>();
            if expected.len() != page.len() || retained.len() != page.len() {
                return Err(SyncError::new("invalid-retention-response", 502));
            }
            for object in &retained {
                let Some(size) = expected.remove(&object.hash) else {
                    return Err(SyncError::new("invalid-retention-response", 502));
                };
                if size.is_some_and(|size| object.size != size.into()) {
                    return Err(SyncError::new("invalid-retention-response", 502));
                }
            }
            self.confirm(config, head, &retained)?;
        }
        Ok(())
    }
    pub fn object(&self, digest: &str, context: Option<&str>) -> Result<Option<RemoteObject>> {
        self.lookup(digest, context, true)
    }
    pub fn release_object(&self, digest: &str, context: &str) -> Result<Option<RemoteObject>> {
        self.lookup(digest, Some(context), false)
    }
    fn lookup(
        &self,
        digest: &str,
        context: Option<&str>,
        active: bool,
    ) -> Result<Option<RemoteObject>> {
        validate_hash(digest)?;
        let value:Option<(String,i64,String,String,String)>=self.db.query_row("SELECT o.context,o.size,o.retention_id,c.device_id,c.config FROM objects o JOIN contexts c ON c.id=o.context WHERE o.hash=?1 AND o.state!='released' AND (NOT ?3 OR o.state='active') AND (?2 IS NULL OR o.context=?2) ORDER BY o.rowid DESC LIMIT 1",params![digest,context,active],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        value
            .map(|(context, size, retention_id, device_id, config)| {
                Ok(RemoteObject {
                    context,
                    hash: digest.to_owned(),
                    size: u64::try_from(size)
                        .map_err(|_| SyncError::new("invalid-retention-size", 409))?,
                    retention_id,
                    device_id,
                    config: serde_json::from_str(&config)
                        .map_err(|_| SyncError::new("invalid-retention-config", 409))?,
                })
            })
            .transpose()
    }
    pub fn confirms(&self, digest: &str, size: Option<u64>, context: &str) -> Result<bool> {
        Ok(self
            .object(digest, Some(context))?
            .is_some_and(|object| size.is_none_or(|size| object.size == size)))
    }
    pub fn gc_size(&self, digest: &str) -> std::io::Result<Option<u64>> {
        self.lookup(digest, None, false)
            .map(|object| object.map(|object| object.size))
            .map_err(|error| std::io::Error::other(error.code))
            .and_then(|size| match size { Some(size) => Ok(Some(size)), None => crate::external_storage::lww_residency::stat(&self.root, digest) })
    }
    pub fn page(&self, after: &str) -> Result<Vec<(String, String, u64)>> {
        let mut statement=self.db.prepare("SELECT context||'/'||hash,hash,size FROM objects WHERE state!='released' AND context||'/'||hash>?1 ORDER BY context,hash LIMIT 128")?;
        let rows = statement
            .query_map([after], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(cursor, hash, size)| {
                Ok((
                    cursor,
                    hash,
                    u64::try_from(size)
                        .map_err(|_| SyncError::new("invalid-retention-size", 409))?,
                ))
            })
            .collect()
    }
    pub fn begin_release(&self, object: &RemoteObject) -> Result<bool> {
        Ok(self.db.execute("UPDATE objects SET state='releasing' WHERE context=?1 AND hash=?2 AND retention_id=?3 AND state IN ('active','releasing')",params![object.context,object.hash,object.retention_id])?==1)
    }
    pub fn begin_latest_release(
        &mut self,
        digest: &str,
        context: &str,
    ) -> Result<Option<RemoteObject>> {
        validate_hash(digest)?;
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let value: Option<(i64, String, String, String)> = tx
            .query_row(
                "SELECT o.size,o.retention_id,c.device_id,c.config FROM objects o JOIN contexts c ON c.id=o.context WHERE o.hash=?1 AND o.context=?2 AND o.state!='released'",
                params![digest, context],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((size, retention_id, device_id, config)) = value else {
            tx.commit()?;
            return Ok(None);
        };
        let updated = tx.execute(
            "UPDATE objects SET state='releasing' WHERE context=?1 AND hash=?2 AND retention_id=?3 AND state IN ('active','releasing')",
            params![context, digest, retention_id],
        )?;
        tx.commit()?;
        if updated != 1 {
            return Ok(None);
        }
        Ok(Some(RemoteObject {
            context: context.to_owned(),
            hash: digest.to_owned(),
            size: u64::try_from(size).map_err(|_| SyncError::new("invalid-retention-size", 409))?,
            retention_id,
            device_id,
            config: serde_json::from_str(&config)
                .map_err(|_| SyncError::new("invalid-retention-config", 409))?,
        }))
    }
    pub fn finish_release(&self, object: &RemoteObject) -> Result<()> {
        self.db.execute("UPDATE objects SET state='released' WHERE context=?1 AND hash=?2 AND retention_id=?3 AND state='releasing'",params![object.context,object.hash,object.retention_id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;

/// Stands in for the Sync download in tests. A body a test serves for one
/// repository still goes through custody and the checked promotion into its
/// CAS; only the network transfer is skipped.
#[cfg(test)]
pub(crate) mod test_remote {
    use std::{
        collections::BTreeMap,
        path::{Path, PathBuf},
        sync::Mutex,
    };

    static SERVED: Mutex<BTreeMap<PathBuf, (BTreeMap<String, Vec<u8>>, usize)>> =
        Mutex::new(BTreeMap::new());

    pub(crate) fn serve(root: &Path, digest: &str, body: Vec<u8>) {
        let root = std::fs::canonicalize(root).unwrap();
        SERVED.lock().unwrap().entry(root).or_default().0.insert(digest.into(), body);
    }

    /// Records custody for these bodies, as a confirmed remote head does.
    pub(crate) fn hold(root: &Path, bodies: &[(&str, u64)]) {
        let config: super::StoredConfig = serde_json::from_value(serde_json::json!({
            "endpoint":"http://127.0.0.1:9/", "libraryId":"library", "deviceId":"device",
            "credentialId":"00000000-0000-4000-8000-000000000000"
        }))
        .unwrap();
        let head = super::RemoteHead {
            head_id: super::hash(b"head"),
            ..super::RemoteHead::genesis("library".into(), "epoch".into()).unwrap()
        };
        let objects = bodies
            .iter()
            .map(|(digest, size)| super::RetainedObject {
                hash: (*digest).into(),
                size: (*size).into(),
                retention_id: super::hash(digest.as_bytes()),
            })
            .collect::<Vec<_>>();
        super::Residency::open(root)
            .unwrap()
            .confirm(&config, &head, &objects)
            .unwrap();
    }

    /// How many bodies this repository fetched.
    pub(crate) fn fetched(root: &Path) -> usize {
        let root = std::fs::canonicalize(root).unwrap();
        SERVED.lock().unwrap().get(&root).map_or(0, |(_, fetched)| *fetched)
    }

    pub(super) fn body(root: &Path, digest: &str) -> Option<Vec<u8>> {
        let mut served = SERVED.lock().unwrap();
        let (bodies, fetched) = served.get_mut(root)?;
        let body = bodies.get(digest)?.clone();
        *fetched += 1;
        Some(body)
    }
}

/// Byte consumers (exports, AI attachments, plugins) need verified bytes. The
/// temporary transfer CAS is discarded after promotion, including chunk files.
/// Display URLs never call this path.
pub(crate) fn open_or_hydrate(root: &Path, digest: &str) -> Result<Option<std::fs::File>> {
    open_or_hydrate_with_check(root, digest, &|| Ok(()))
}

pub(crate) fn open_or_hydrate_with_check(
    root: &Path,
    digest: &str,
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<std::fs::File>> {
    HydrationSession::new(root, None)?.open(digest, check)
}

static TRANSFER_BUDGET: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) struct TransientBody {
    body: super::cache::Body,
    _directory: Option<tempfile::TempDir>,
}
impl TransientBody {
    pub(crate) fn len(&self) -> std::io::Result<u64> {
        self.body.len()
    }
}
impl std::io::Read for TransientBody {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.body, buffer)
    }
}

pub(crate) fn open_transient_with_check(
    root: &Path,
    scratch_root: &Path,
    digest: &str,
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<TransientBody>> {
    open_transient_inner(root, scratch_root, digest, check, true)
}
pub(crate) fn open_transient_server_with_check(
    root: &Path,
    scratch_root: &Path,
    digest: &str,
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<TransientBody>> {
    open_transient_inner(root, scratch_root, digest, check, false)
}
fn open_transient_inner(
    root: &Path,
    scratch_root: &Path,
    digest: &str,
    check: &dyn Fn() -> Result<()>,
    allow_external: bool,
) -> Result<Option<TransientBody>> {
    check()?;
    validate_hash(digest)?;
    let cas = crate::asset_repository::PayloadCas::new(root)?;
    #[cfg(test)]
    let local = cas.open_object_tracked(digest)?.map(super::cache::Body::Tracked);
    #[cfg(not(test))]
    let local = cas.open_object(digest)?.map(super::cache::Body::File);
    if let Some(body) = local {
        return Ok(Some(TransientBody { body, _directory: None }));
    }
    let Some(proof) = Residency::open(root)?.object(digest, None)? else {
        if !allow_external { return Ok(None); }
        return Ok(crate::external_storage::lww_residency::hydrate(root, digest, check)?.map(|file|TransientBody {body:super::cache::Body::File(file),_directory:None}));
    };
    open_transient_server_proof_with_check(root, scratch_root, &proof, check)
}
pub(crate) fn open_transient_server_proof_with_check(
    root: &Path,
    scratch_root: &Path,
    proof: &RemoteObject,
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<TransientBody>> {
    check()?;
    let digest = proof.hash.as_str();
    validate_hash(digest)?;
    let _budget = lock_with_check(&TRANSFER_BUDGET, check)?;
    #[cfg(test)]
    if let Some(bytes) = test_remote::body(&std::fs::canonicalize(root)?, digest) {
        if bytes.len() > super::cache::SMALL_OBJECT_BYTES {
            return Err(SyncError::new("transient-fixture-body-too-large", 409));
        }
        if bytes.len() as u64 != proof.size {
            return Err(SyncError::new("hydration-size-mismatch", 409));
        }
        let directory = tempfile::Builder::new().prefix("asset-transient-").tempdir_in(scratch_root)?;
        let cache = super::cache::Cache::open(directory.path())?;
        check()?;
        if cache.put(&bytes)? != digest {
            return Err(SyncError::new("transfer-target-mismatch", 502));
        }
        cache.verify(digest, check)?;
        if cache.stat_object(digest)? != Some(proof.size) {
            return Err(SyncError::new("hydration-size-mismatch", 409));
        }
        let body = cache.open_object(digest)?.ok_or_else(|| SyncError::new("hydration-incomplete", 502))?;
        check()?;
        return Ok(Some(TransientBody { body, _directory: Some(directory) }));
    }
    let client = super::client::ServerClient::new(proof.config.resolve(root)?)?;
    #[cfg(test)]
    let client = { let mut client = client; super::client::attach_test_io(root, &mut client); client };
    client.resolve_identity()?;
    check()?;
    let directory = tempfile::Builder::new().prefix("asset-transient-").tempdir_in(scratch_root)?;
    let cache = super::cache::Cache::open(directory.path())?;
    super::transfer::Transfer::new(&client, &cache)?.with_check(check)
        .download(&[digest.to_owned()], &[])?;
    cache.verify(digest, check)?;
    if cache.stat_object(digest)? != Some(proof.size) {
        return Err(SyncError::new("hydration-size-mismatch", 409));
    }
    let body = cache.open_object(digest)?.ok_or_else(|| SyncError::new("hydration-incomplete", 502))?;
    check()?;
    Ok(Some(TransientBody { body, _directory: Some(directory) }))
}

pub(crate) struct HydrationSession {
    root: PathBuf,
    residency: Option<Residency>,
    clients: std::collections::BTreeMap<String, super::client::ServerClient>,
    cache: Option<super::cache::Cache>,
    directory: Option<tempfile::TempDir>,
    cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HydrationOutcome { AlreadyLocal, Downloaded }
impl HydrationSession {
    pub(crate) fn new(root: &Path, cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) -> Result<Self> {
        Ok(Self { root: std::fs::canonicalize(root)?, residency: None, clients: Default::default(), cache: None, directory: None, cancellation })
    }
    pub(crate) fn open(&mut self, digest: &str, check: &dyn Fn() -> Result<()>) -> Result<Option<std::fs::File>> {
        if !self.hydrate_many(&[digest.to_owned()], check)?.is_empty() { return Ok(None); }
        check()?;
        Ok(crate::asset_repository::PayloadCas::new(&self.root)?.open_object(digest)?)
    }
    pub(crate) fn hydrate_many(&mut self, digests: &[String], check: &dyn Fn() -> Result<()>) -> Result<Vec<String>> {
        self.hydrate_many_outcomes(digests, check, |_, _| {})
    }
    pub(crate) fn hydrate_many_observed(&mut self, digests: &[String], check: &dyn Fn() -> Result<()>, on_object_done: &dyn Fn()) -> Result<Vec<String>> {
        self.hydrate_many_outcomes(digests, check, |_, _| on_object_done())
    }
    pub(crate) fn hydrate_many_outcomes(&mut self, digests: &[String], check: &dyn Fn() -> Result<()>, opened: impl FnMut(&str, HydrationOutcome)) -> Result<Vec<String>> {
        self.hydrate_many_outcomes_prioritized(digests, &Default::default(), check, opened)
    }
    pub(crate) fn hydrate_many_outcomes_prioritized(&mut self, digests: &[String], priority: &std::collections::BTreeSet<String>, check: &dyn Fn() -> Result<()>, mut opened: impl FnMut(&str, HydrationOutcome)) -> Result<Vec<String>> {
        let mut unavailable = Vec::new();
        let mut external = std::collections::BTreeSet::new();
        let mut server_pages = Vec::new();
        let cas = crate::asset_repository::PayloadCas::new(&self.root)?;
        for page in digests.chunks(64) {
            check()?;
            let hashes = page.iter().collect::<std::collections::BTreeSet<_>>();
            let locks = hashes.iter().map(|hash| hydration_lock(&self.root, hash)).collect::<Result<Vec<_>>>()?;
            let guards = locks.iter().map(|lock| lock_with_check(lock, check)).collect::<Result<Vec<_>>>()?;
            let mut objects = Vec::new();
            for hash in hashes {
                check()?;
                validate_hash(hash)?;
                {
                    let _guard = crate::asset_repository::coordinator::lock_repository_mutation()?;
                    if cas.stat_object(hash)?.is_some() {
                        opened(hash, HydrationOutcome::AlreadyLocal);
                        continue;
                    }
                }
                if self.residency.is_none() && Residency::exists(&self.root) { self.residency = Some(Residency::open(&self.root)?); }
                let Some(proof) = self.residency.as_ref().map(|residency| residency.object(hash, None)).transpose()?.flatten() else {
                    external.insert(hash.clone());
                    continue;
                };
                objects.push(proof);
            }
            server_pages.push(objects);
            drop(guards);
            std::thread::yield_now();
        }
        let external = digests.iter().filter(|hash| external.remove(hash.as_str())).cloned().collect::<Vec<_>>();
        for selected in [true, false] {
            for page in &server_pages {
                let objects = page.iter().filter(|object| {
                    if priority.is_empty() { selected } else { priority.contains(&object.hash) == selected }
                }).collect::<Vec<_>>();
                if objects.is_empty() { continue; }
                check()?;
                let hashes = objects.iter().map(|object| &object.hash).collect::<std::collections::BTreeSet<_>>();
                let locks = hashes.iter().map(|hash| hydration_lock(&self.root, hash)).collect::<Result<Vec<_>>>()?;
                let guards = locks.iter().map(|lock| lock_with_check(lock, check)).collect::<Result<Vec<_>>>()?;
                let mut groups = std::collections::BTreeMap::<String, Vec<RemoteObject>>::new();
                for proof in objects {
                    let hash = &proof.hash;
                    check()?;
                    {
                        let _guard = crate::asset_repository::coordinator::lock_repository_mutation()?;
                        if cas.stat_object(hash)?.is_some() {
                            opened(hash, HydrationOutcome::AlreadyLocal);
                            continue;
                        }
                    }
                    #[cfg(test)]
                    if let Some(body) = test_remote::body(&self.root, hash) {
                        let staged = super::transfer::stage_checked(&cas, &mut body.as_slice(), hash, proof.size, check)?;
                        check()?;
                        let _guard = crate::asset_repository::coordinator::lock_repository_mutation()?;
                        check()?;
                        let outcome = cas.publish_staged(staged);
                        check()?;
                        outcome?;
                        opened(hash, HydrationOutcome::Downloaded);
                        continue;
                    }
                    let key = format!("{}:{}", proof.context, serde_json::to_string(&proof.config)
                        .map_err(|_| SyncError::new("invalid-server-config", 409))?);
                    groups.entry(key).or_default().push(proof.clone());
                }
                for (key, objects) in groups {
                    let _budget = lock_with_check(&TRANSFER_BUDGET, check)?;
                    if !self.clients.contains_key(&key) {
                        let client = super::client::ServerClient::with_cancellation(objects[0].config.resolve(&self.root)?, self.cancellation.clone())?;
                        #[cfg(test)]
                        let client = { let mut client = client; super::client::attach_test_io(&self.root, &mut client); client };
                        client.resolve_identity()?;
                        self.clients.insert(key.clone(), client);
                    }
                    if self.cache.is_none() {
                        let directory = tempfile::Builder::new().prefix("asset-hydration-").tempdir_in(&self.root)?;
                        self.cache = Some(super::cache::Cache::open(directory.path())?);
                        self.directory = Some(directory);
                    }
                    let cache = self.cache.as_ref().unwrap();
                    super::transfer::Transfer::new(self.clients.get(&key).unwrap(), cache)?.with_check(check)
                        .download(&objects.iter().map(|object| object.hash.clone()).collect::<Vec<_>>(), &[])?;
                    for object in objects {
                        let mut body = cache.open_derived(&object.hash)?.ok_or_else(|| SyncError::new("hydration-incomplete", 502))?;
                        let staged = super::transfer::stage_checked(&cas, &mut body, &object.hash, object.size, check)?;
                        check()?;
                        {
                            let _guard = crate::asset_repository::coordinator::lock_repository_mutation()?;
                            check()?;
                            let outcome = cas.publish_staged(staged);
                            check()?;
                            outcome?;
                            opened(&object.hash, HydrationOutcome::Downloaded);
                        }
                        drop(body);
                        cache.remove_derived(&object.hash)?;
                    }
                }
                drop(guards);
                std::thread::yield_now();
            }
            if selected && !external.is_empty() {
                unavailable.extend(crate::external_storage::lww_residency::hydrate_registered_many(
                    &self.root, &external, priority, self.cancellation.clone(), check, &mut opened,
                )?);
            }
        }
        Ok(unavailable)
    }
}

pub(crate) fn with_hydration_lock<T>(
    root: &Path,
    digest: &str,
    check: &dyn Fn() -> Result<()>,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let lock = hydration_lock(root, digest)?;
    let _guard = lock_with_check(&lock, check)?;
    operation()
}

fn hydration_lock(root: &Path, digest: &str) -> Result<std::sync::Arc<std::sync::Mutex<()>>> {
    use std::sync::{Arc, Mutex, OnceLock, Weak};
    static IN_FLIGHT: OnceLock<Mutex<std::collections::HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    validate_hash(digest)?;
    let mut locks = IN_FLIGHT.get_or_init(Default::default).lock()
        .map_err(|_| SyncError::new("hydration-unavailable", 503))?;
    locks.retain(|_, lock| lock.strong_count() > 0);
    let key = root.join(digest);
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) { return Ok(lock); }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    Ok(lock)
}

fn lock_with_check<'a>(
    lock: &'a std::sync::Mutex<()>,
    check: &dyn Fn() -> Result<()>,
) -> Result<std::sync::MutexGuard<'a, ()>> {
    loop {
        check()?;
        match lock.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(SyncError::new("hydration-unavailable", 503));
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
}

/// Explicit byte-consumer adapter. Ordinary PayloadCas::stat_object continues
/// to describe physical storage and never performs a network request.
pub(crate) trait RemotePayloadAccess {
    fn open_available_object(&self, hash: &str) -> std::io::Result<Option<std::fs::File>>;
    fn stat_available_object(&self, hash: &str) -> std::io::Result<Option<u64>> {
        self.open_available_object(hash)?
            .map(|file| file.metadata().map(|metadata| metadata.len()))
            .transpose()
    }
}
impl RemotePayloadAccess for crate::asset_repository::PayloadCas {
    fn open_available_object(&self, hash: &str) -> std::io::Result<Option<std::fs::File>> {
        open_or_hydrate(self.repository_root(), hash)
            .map_err(|error| std::io::Error::other(error.code))
    }
}
