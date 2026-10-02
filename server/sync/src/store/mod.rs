mod backup;
mod connection;
mod lww;
#[cfg(test)]
mod lww_tests;
pub(crate) mod management;
mod registration_claims;
pub use backup::BackupManifest;
pub use management::{ManagedDevice, ManagementConnection};
mod descriptors;
mod descriptors_index;
mod maintenance;
mod media;
pub use media::MediaResponse;
mod retention;
pub use retention::{ObjectIdentity, RetainedObject, RetentionPage, RetentionRelease};
mod objects;
pub use objects::Body;
mod schema;
mod stream_transfers;
pub use stream_transfers::DeltaProgress;
mod transfers;
mod upload_jobs;
mod uploads;
pub use transfers::TransferRequest;
pub use uploads::{UploadManifest, UploadProgress, UPLOAD_CHUNK_BYTES};

use crate::{Error, Result};
use risunest_sync_wire::{canonical, hash, validate_id, RemoteHead, Sequence};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

pub struct Store {
    root: PathBuf,
    db: Mutex<Connection>,
    read_db: Mutex<Connection>,
    objects_gate: Mutex<()>,
    upload_job_gate: Mutex<()>,
    download_job_gate: Mutex<()>,
    connection_gate: Mutex<()>,
    trash_cursors: Mutex<(i64, i64)>,
    orphan_cursor: Mutex<maintenance::OrphanScan>,
    temporary_paths: Mutex<BTreeSet<PathBuf>>,
    media_signer: risunest_sync_connect::media::MediaSigner,
    heads: tokio::sync::watch::Sender<u64>,
    _owner: OwnerLock,
}

struct OwnerLock(File);

impl Drop for OwnerLock {
    fn drop(&mut self) {
        // Duplicated or inherited descriptors must not retain ownership after close.
        let _ = self.0.unlock();
    }
}

struct StagingTemp<'a> {
    file: tempfile::NamedTempFile,
    store: &'a Store,
}
impl std::ops::Deref for StagingTemp<'_> {
    type Target = tempfile::NamedTempFile;
    fn deref(&self) -> &Self::Target {
        &self.file
    }
}
impl std::ops::DerefMut for StagingTemp<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.file
    }
}
impl Drop for StagingTemp<'_> {
    fn drop(&mut self) {
        if let Ok(mut paths) = self.store.temporary_paths.lock() {
            paths.remove(self.file.path());
        }
    }
}

/// Tokens are emitted once by the administration CLI, never by a sync route.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCredential {
    pub device_id: String,
    pub library_id: String,
    pub token: String,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSession {
    pub head: RemoteHead,
    pub device_id: String,
    pub operation_watermark: Sequence,
    pub operation_pending: bool,
    pub protocol_id: &'static str,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableWork {
    pub commit_jobs: u64,
    pub upload_jobs: u64,
    pub download_jobs: u64,
    pub staged_changes: u64,
    pub uploads: u64,
}

pub(super) fn random_id() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|_| Error::new("entropy-unavailable", 503))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub(super) fn json<T: Serialize>(value: &T) -> Result<String> {
    Ok(String::from_utf8(canonical::encode(value)?).unwrap())
}
pub(super) fn parse<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T> {
    canonical::decode(value.as_bytes(), risunest_sync_wire::MAX_METADATA_BYTES)
        .map_err(|_| Error::new("corrupt-metadata", 503))
}
impl Store {
    fn staging_temp(&self) -> Result<StagingTemp<'_>> {
        let mut paths = self
            .temporary_paths
            .lock()
            .map_err(|_| Error::new("storage-unavailable", 503))?;
        let file = tempfile::Builder::new()
            .prefix(".risunest-tmp-")
            .tempfile_in(self.root.join("staging"))?;
        paths.insert(file.path().to_owned());
        Ok(StagingTemp { file, store: self })
    }
    pub fn durable_work(&self) -> Result<DurableWork> {
        let db = self.reader()?;
        let count = |table: &str| -> Result<u64> {
            let value: i64 = db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })?;
            u64::try_from(value).map_err(|_| Error::new("corrupt-metadata", 503))
        };
        Ok(DurableWork {
            commit_jobs: 0,
            upload_jobs: count("upload_jobs")?,
            download_jobs: count("download_deltas")?,
            staged_changes: 0,
            uploads: count("uploads")?,
        })
    }
    #[cfg(test)]
    pub(crate) fn source_observer_body_work(
        &self,
    ) -> Result<crate::source_observer::PendingBodyWork> {
        let db = self.reader()?;
        let (upload_jobs,download_jobs,upload_sessions):(i64,i64,i64)=db.query_row(
            "SELECT (SELECT count(*) FROM upload_jobs WHERE terminal=0), (SELECT count(*) FROM download_deltas WHERE state IN ('queued','working')), (SELECT count(*) FROM uploads WHERE state IN ('open','queued','finalizing') AND expires>unixepoch())",
            [],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
        Ok(crate::source_observer::PendingBodyWork {
            upload_jobs: upload_jobs
                .try_into()
                .map_err(|_| Error::new("corrupt-metadata", 503))?,
            download_jobs: download_jobs
                .try_into()
                .map_err(|_| Error::new("corrupt-metadata", 503))?,
            upload_sessions: upload_sessions
                .try_into()
                .map_err(|_| Error::new("corrupt-metadata", 503))?,
        })
    }

    pub fn init(root: &Path) -> Result<Self> {
        Self::open_inner(root, true)
    }
    pub fn open(root: &Path) -> Result<Self> {
        Self::open_inner(root, false)
    }
    fn open_inner(root: &Path, create: bool) -> Result<Self> {
        if !root.is_absolute() {
            return Err(Error::new("absolute-data-dir-required", 400));
        }
        if create {
            fs::create_dir_all(root.parent().ok_or(Error::new("invalid-data-dir", 400))?)?;
        }
        let root = crate::resolve_data_root(root)?;
        if create {
            fs::create_dir_all(&root)?;
        }
        for name in [
            "owner.lock",
            "metadata.sqlite",
            "metadata.sqlite-wal",
            "metadata.sqlite-shm",
            "objects",
            "staging",
        ] {
            objects::check_path(&root.join(name))?;
        }
        let owner = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("owner.lock"))?;
        owner
            .try_lock()
            .map_err(|_| Error::new("data-dir-busy", 409))?;
        let owner = OwnerLock(owner);
        // No publisher from this daemon exists while the owner is opening.
        for directory in [&root, &root.join("staging")] {
            let entries = match fs::read_dir(directory) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let mut changed = false;
            for entry in entries.take(4096) {
                let entry = entry?;
                if !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".risunest-tmp-")
                {
                    continue;
                }
                let meta = entry.metadata()?;
                if meta.is_file()
                    && (directory != &root
                        || meta
                            .modified()?
                            .elapsed()
                            .is_ok_and(|age| age.as_secs() >= 3600))
                {
                    fs::remove_file(entry.path())?;
                    changed = true;
                }
            }
            if changed {
                objects::sync_directory(directory)?;
            }
        }
        let db_path = root.join("metadata.sqlite");
        if create && db_path.exists() {
            return Err(Error::new("already-initialized", 409));
        }
        if !create && !db_path.is_file() {
            return Err(Error::new("not-initialized", 404));
        }
        if create {
            for name in ["objects", "staging"] {
                fs::create_dir_all(root.join(name))?;
            }
            let staged = tempfile::Builder::new()
                .prefix(".risunest-tmp-")
                .tempfile_in(root.join("staging"))?;
            let mut db = Connection::open(staged.path())?;
            db.execute_batch(
                "PRAGMA auto_vacuum=INCREMENTAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
            )?;
            let tx = db.transaction()?;
            tx.execute_batch(schema::SCHEMA)?;
            risunest_small_object_store::initialize(&tx)
                .map_err(|_| Error::new("metadata-storage", 503))?;
            let head = RemoteHead::genesis(random_id()?, random_id()?)?;
            tx.execute("INSERT INTO library VALUES(1,?1)", [json(&head)?])?;
            tx.execute(
                "INSERT INTO media_secret VALUES(1,?1)",
                [risunest_sync_connect::media::generate_key()?.as_slice()],
            )?;
            tx.commit()?;
            drop(db);
            staged.as_file().sync_all()?;
            staged
                .persist_noclobber(&db_path)
                .map_err(|_| Error::new("initialization-publication-failed", 503))?;
            objects::sync_directory(&root)?;
        }
        let db = Connection::open(&db_path)?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version != schema::VERSION {
            return Err(Error::new("incompatible-store", 409));
        }
        schema::verify(&db)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000; PRAGMA journal_size_limit=67108864;")?;
        db.execute(
            "UPDATE uploads SET state=CASE WHEN id IN (SELECT upload FROM upload_jobs) THEN 'queued' ELSE 'open' END WHERE state='finalizing'",
            [],
        )?;
        db.execute(
            "UPDATE download_deltas SET state='queued' WHERE state='working'",
            [],
        )?;
        let read_db = Connection::open_with_flags(
            root.join("metadata.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        read_db.busy_timeout(std::time::Duration::from_secs(5))?;
        let media_key: Vec<u8> =
            db.query_row("SELECT key FROM media_secret WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
        let media_signer = risunest_sync_connect::media::MediaSigner::new(&media_key)?;
        let store = Self {
            root,
            db: Mutex::new(db),
            read_db: Mutex::new(read_db),
            objects_gate: Mutex::new(()),
            upload_job_gate: Mutex::new(()),
            download_job_gate: Mutex::new(()),
            connection_gate: Mutex::new(()),
            trash_cursors: Mutex::new((0, 0)),
            orphan_cursor: Mutex::new(maintenance::OrphanScan::default()),
            temporary_paths: Mutex::new(BTreeSet::new()),
            media_signer,
            heads: tokio::sync::watch::Sender::new(0),
            _owner: owner,
        };
        store
            .head()?
            .validate()
            .map_err(|_| Error::new("corrupt-metadata", 503))?;
        Ok(store)
    }
    pub(super) fn db(&self) -> Result<MutexGuard<'_, Connection>> {
        self.db
            .lock()
            .map_err(|_| Error::new("writer-unavailable", 503))
    }
    pub(super) fn reader(&self) -> Result<MutexGuard<'_, Connection>> {
        self.read_db
            .lock()
            .map_err(|_| Error::new("reader-unavailable", 503))
    }
    pub fn head(&self) -> Result<RemoteHead> {
        Self::read_head(&*self.reader()?)
    }
    pub fn device_head(&self, device: &Device) -> Result<RemoteHead> {
        let db = self.reader()?;
        Self::require_device(&db, device)?;
        Self::read_head(&db)
    }
    /// Observes announcements that the head may have moved. A reader still
    /// confirms the head itself: an announcement is never the state.
    pub fn head_announcements(&self) -> tokio::sync::watch::Receiver<u64> {
        self.heads.subscribe()
    }
    pub(super) fn announce_head(&self) {
        self.heads
            .send_modify(|value| *value = value.wrapping_add(1));
    }
    pub fn device_session(&self, device: &Device) -> Result<DeviceSession> {
        let mut connection = self.reader()?;
        let db = connection.transaction()?;
        Self::require_device(&db, device)?;
        Ok(DeviceSession {
            head: Self::read_head(&db)?,
            device_id: device.id.clone(),
            operation_watermark: 0.into(),
            operation_pending: false,
            protocol_id: crate::PROTOCOL_ID,
        })
    }
    pub fn device_active(&self, actor: &Device, id: &str) -> Result<bool> {
        validate_id(id)?;
        let db = self.reader()?;
        Self::require_device(&db, actor)?;
        Ok(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM devices WHERE id=?1 AND revoked=0)",
            [id],
            |r| r.get(0),
        )?)
    }
    pub(super) fn read_head(db: &Connection) -> Result<RemoteHead> {
        parse(&db.query_row::<String, _, _>(
            "SELECT head FROM library WHERE singleton=1",
            [],
            |r| r.get(0),
        )?)
    }
    pub fn add_device(&self) -> Result<DeviceCredential> {
        self.add_named_device("", None)
    }
    pub(super) fn add_named_device(
        &self,
        name: &str,
        request: Option<&str>,
    ) -> Result<DeviceCredential> {
        let token = random_id()?;
        let device_id = random_id()?;
        let db = self.db()?;
        if let Some(request) = request {
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM devices WHERE registration_request=?1)",
                [request],
                |r| r.get(0),
            )?;
            if exists {
                return Err(Error::new("registration-already-issued", 409));
            }
        }
        let library_id = Self::read_head(&db)?.library_id;
        db.execute(
            "INSERT INTO devices(id,verifier,name,registration_request) VALUES(?1,?2,?3,?4)",
            params![device_id, hash(token.as_bytes()), name, request],
        )?;
        Ok(DeviceCredential {
            device_id,
            library_id,
            token,
        })
    }
    pub fn revoke_device(&self, id: &str) -> Result<()> {
        validate_id(id)?;
        if self
            .db()?
            .execute("UPDATE devices SET revoked=1 WHERE id=?1", [id])?
            == 0
        {
            return Err(Error::new("device-not-found", 404));
        }
        self.announce_head();
        Ok(())
    }
    pub fn authenticate(&self, library: &str, token: &str) -> Result<Device> {
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::new("unauthorized", 401));
        }
        let db = self.reader()?;
        if Self::read_head(&db)?.library_id != library {
            return Err(Error::new("unauthorized", 401));
        }
        let id = db
            .query_row(
                "SELECT id FROM devices WHERE verifier=?1 AND revoked=0",
                [hash(token.as_bytes())],
                |r| r.get(0),
            )
            .optional()?;
        id.map(|id| Device { id })
            .ok_or(Error::new("unauthorized", 401))
    }
    pub(super) fn require_device(db: &Connection, device: &Device) -> Result<()> {
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM devices WHERE id=?1 AND revoked=0)",
            [&device.id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::new("unauthorized", 401));
        }
        Ok(())
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;

    #[test]
    fn closing_store_releases_ownership_while_a_duplicate_descriptor_remains() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::init(root.path()).unwrap();
        let duplicate = store._owner.0.try_clone().unwrap();
        let head = store.head().unwrap();
        let error = Store::open(root.path())
            .err()
            .expect("active owner must be rejected");
        assert_eq!((error.code, error.status), ("data-dir-busy", 409));
        drop(store);

        let reopened = Store::open(root.path()).unwrap();
        assert_eq!(reopened.head().unwrap(), head);
        drop(duplicate);
        let error = Store::open(root.path())
            .err()
            .expect("new active owner must be rejected");
        assert_eq!((error.code, error.status), ("data-dir-busy", 409));
        drop(reopened);
        assert_eq!(Store::open(root.path()).unwrap().head().unwrap(), head);
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;

    #[test]
    fn exported_store_format_matches_the_initialized_schema() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::init(&root.path().join("sync")).unwrap();
        let version: i64 = store
            .reader()
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::VERSION);
        assert_eq!(
            crate::STORE_FORMAT_ID,
            format!("risunest-sync-store/v{version}")
        );
    }
}
