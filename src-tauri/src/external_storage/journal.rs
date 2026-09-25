//! Reconstructible transfer receipts. Publication and activation authority stays
//! in PDS; reopening this journal requires its current PDS job identity.
use super::{contract::*, transfer::SpoolSource};
use risunest_external_storage_format::snapshot as wire;
use crate::persistent_store::sync_selection::CaptureIdentity;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn corrupt() -> ProviderError {
    ProviderError::new(ErrorKind::Corrupt)
}
fn storage(_: impl std::fmt::Display) -> ProviderError {
    ProviderError::new(ErrorKind::Transient)
}

/// Named beside the transfer receipts because it is the same job's durable
/// state and is read by the same directory scan.
const PARENT_FILE: &str = "parent.json";

/// The application-wide ceiling on sealed ciphertext that unfinished jobs are
/// holding. Paused and failed jobs keep theirs, so it is summed from the files
/// each job left rather than counted as work is admitted.
pub(crate) const TRANSFER_SPOOL_BUDGET: u64 = 512 * 1024 * 1024;

/// What one job directory is holding, counting only what a transfer owns.
/// An unreadable directory reads as empty, so this is only for reporting.
#[cfg(test)]
pub(crate) fn spool_bytes(directory: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|value| value == "spool"))
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .sum()
}

/// What one job directory is holding, for deciding whether more may be
/// written. A directory that cannot be read is unknown usage, not none.
pub(crate) fn held_spool_bytes(directory: &Path) -> Result<u64> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(storage(error)),
    };
    let mut total = 0u64;
    for entry in entries {
        let entry = entry.map_err(storage)?;
        if entry.path().extension().is_none_or(|value| value != "spool") {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            // Released between listing and reading it.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(storage(error)),
        };
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

/// Transfer spool writes this process has admitted and not yet finished, by
/// job. Once a file is on disk its directory answers for it instead.
static SPOOL_WRITES: std::sync::Mutex<std::collections::BTreeMap<String, u64>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Bytes admitted for `job_id` whose writes have not finished.
#[cfg(test)]
pub(crate) fn admitted_spool_writes(job_id: &str) -> u64 {
    SPOOL_WRITES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(job_id)
        .copied()
        .unwrap_or(0)
}

/// How many pack families may be active at once in this application. A family
/// is one pack from the moment its plaintext is allocated until its upload is
/// settled or its job stops: the plaintext, the ciphertext sealed from it, and
/// the wave that registers and sends it.
pub(crate) const ACTIVE_PACK_FAMILIES: usize = 2;

/// The admission every pack family of this application passes, whichever job
/// or connection produces it. The desktop app runs as one process per app
/// identifier and a mobile app as one process, so one value in this process
/// is the whole application.
static SHARED_FAMILIES: std::sync::LazyLock<Arc<PackFamilies>> =
    std::sync::LazyLock::new(|| PackFamilies::new(ACTIVE_PACK_FAMILIES));

#[derive(Default)]
struct FamilyState {
    active: usize,
    waiting: usize,
}

pub(crate) struct PackFamilies {
    limit: usize,
    state: std::sync::Mutex<FamilyState>,
    changed: tokio::sync::Notify,
}

/// One active pack family. Dropping it ends the family, whether its upload
/// settled or its job stopped and left the sealed bytes for a later attempt.
pub(crate) struct FamilyPermit(Arc<PackFamilies>);

impl Drop for FamilyPermit {
    fn drop(&mut self) {
        {
            let mut state = self.0.state();
            state.active = state.active.saturating_sub(1);
        }
        self.0.changed.notify_waiters();
    }
}

/// Counts a producer as waiting for as long as it does, including a wait
/// that is abandoned.
struct FamilyWaiter<'a>(&'a PackFamilies);

impl Drop for FamilyWaiter<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.waiting = state.waiting.saturating_sub(1);
    }
}

impl PackFamilies {
    pub(crate) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit: limit.max(1),
            state: Default::default(),
            changed: tokio::sync::Notify::new(),
        })
    }

    pub(crate) fn shared() -> Arc<Self> {
        SHARED_FAMILIES.clone()
    }

    /// The counts are only ever changed whole, so a panic elsewhere while the
    /// lock was held leaves them exact, and a family is never stranded by it.
    fn state(&self) -> std::sync::MutexGuard<'_, FamilyState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Starts a family now, or answers `None` when every family is active.
    pub(crate) fn try_acquire(self: &Arc<Self>) -> Result<Option<FamilyPermit>> {
        let mut state = self.state();
        if state.active >= self.limit {
            return Ok(None);
        }
        state.active += 1;
        Ok(Some(FamilyPermit(self.clone())))
    }

    /// Waits until a family can start. A job holding sealed packs sends them
    /// as soon as anybody waits here, so every active family ends without
    /// anything a waiter holds. At most one producer per running job waits.
    /// The wait stops at cancellation or when `abandoned` resolves.
    pub(crate) async fn acquire(
        self: &Arc<Self>,
        cancel: &Cancellation,
        abandoned: impl std::future::Future<Output = ()>,
    ) -> Result<FamilyPermit> {
        if let Some(permit) = self.try_acquire()? {
            return Ok(permit);
        }
        let waiter = {
            let mut state = self.state();
            state.waiting += 1;
            // One job's producer waiting on its own sealed packs is the usual
            // pace of a publication; several waiting at once is worth a line.
            if state.waiting > 1 {
                crate::nlog!(
                    "info",
                    "External packs wait for a family: {} active, {} waiting",
                    state.active,
                    state.waiting
                );
            }
            FamilyWaiter(self)
        };
        self.changed.notify_waiters();
        tokio::pin!(abandoned);
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut state = self.state();
                if state.active < self.limit {
                    state.active += 1;
                    drop(state);
                    drop(waiter);
                    return Ok(FamilyPermit(self.clone()));
                }
            }
            tokio::select! {
                _ = &mut changed => {}
                _ = cancel.cancelled() => return Err(ProviderError::new(ErrorKind::Cancelled)),
                _ = &mut abandoned => return Err(ProviderError::new(ErrorKind::Cancelled)),
            }
        }
    }

    /// Resolves once some producer waits for a family: the signal for a job
    /// holding sealed packs to send them rather than wait for more.
    pub(crate) async fn contended(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.state().waiting > 0 {
                return;
            }
            changed.await;
        }
    }

    /// Active families and producers waiting for one.
    #[cfg(test)]
    pub(crate) fn counts(&self) -> (usize, usize) {
        let state = self.state();
        (state.active, state.waiting)
    }
}

/// The application-wide transfer spool as one job sees it: every job that is
/// still holding sealed ciphertext, this one included, since what it already
/// holds is on the same disk as what it is about to write.
pub(crate) struct SpoolBudget {
    job_id: String,
    retaining: Box<dyn Fn() -> Result<Vec<(String, PathBuf)>> + Send + Sync>,
    limit: u64,
    families: Arc<PackFamilies>,
}

/// One admitted write. Dropping it hands the bytes back to the directory that
/// now holds them, or to nobody if the write failed.
pub(crate) struct SpoolReservation {
    job_id: String,
    bytes: u64,
}

impl Drop for SpoolReservation {
    fn drop(&mut self) {
        let mut writes = SPOOL_WRITES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(held) = writes.get_mut(&self.job_id) {
            *held = held.saturating_sub(self.bytes);
            if *held == 0 {
                writes.remove(&self.job_id);
            }
        }
    }
}

impl SpoolBudget {
    /// `retaining` names every job whose directory may still hold sealed
    /// ciphertext, with that directory.
    pub(crate) fn new(
        job_id: String,
        retaining: impl Fn() -> Result<Vec<(String, PathBuf)>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            job_id,
            retaining: Box::new(retaining),
            limit: TRANSFER_SPOOL_BUDGET,
            families: PackFamilies::shared(),
        }
    }
    /// A budget of its own, with pack families of its own unless the test
    /// shares them between the jobs it runs.
    #[cfg(test)]
    pub(crate) fn with_limit(mut self, limit: u64) -> Self {
        self.limit = limit;
        self.families = PackFamilies::new(ACTIVE_PACK_FAMILIES);
        self
    }
    #[cfg(test)]
    pub(crate) fn with_families(mut self, families: Arc<PackFamilies>) -> Self {
        self.families = families;
        self
    }
    #[cfg(test)]
    pub(crate) fn reserve(&self, bytes: u64) -> Result<SpoolReservation> {
        self.try_reserve(bytes)?
            .ok_or_else(|| ProviderError::new(ErrorKind::Transient))
    }
    /// Admits one more write of `bytes`, or answers `None` when the spool has
    /// no room for it until something held there is released.
    pub(crate) fn try_reserve(&self, bytes: u64) -> Result<Option<SpoolReservation>> {
        self.admit(bytes, false)
    }
    /// Charges a write that cannot wait for room because it is what lets
    /// room be released: the page registering a wave that is already sealed.
    fn charge(&self, bytes: u64) -> Result<SpoolReservation> {
        self.admit(bytes, true)?
            .ok_or_else(|| ProviderError::new(ErrorKind::Transient))
    }
    fn admit(&self, bytes: u64, always: bool) -> Result<Option<SpoolReservation>> {
        let mut writes = SPOOL_WRITES.lock().map_err(storage)?;
        let mut used = 0u64;
        let mut largest: Option<(String, u64)> = None;
        for (job, directory) in (self.retaining)()? {
            let held = held_spool_bytes(&directory)?
                .saturating_add(writes.get(&job).copied().unwrap_or(0));
            used = used.saturating_add(held);
            if largest.as_ref().is_none_or(|(_, previous)| held > *previous) {
                largest = Some((job, held));
            }
        }
        if used.saturating_add(bytes) > self.limit {
            if let Some((owner, held)) = largest {
                crate::nlog!(
                    "warn",
                    "External job {} waits for the transfer spool budget; job {owner} holds {held} bytes",
                    self.job_id
                );
            }
            if !always {
                return Ok(None);
            }
        }
        let held = writes.entry(self.job_id.clone()).or_insert(0);
        *held = held.saturating_add(bytes);
        Ok(Some(SpoolReservation {
            job_id: self.job_id.clone(),
            bytes,
        }))
    }
}

/// Whether a new sealed object may be written now.
pub(crate) enum SpoolAdmission {
    /// Charged to the budget, or written with no budget to charge.
    Admitted(Option<SpoolReservation>),
    /// Refused until this job or another releases what the spool holds.
    Full,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct JobIdentity {
    pub job_id: String,
    pub connection_id: String,
    pub repository_id: String,
    pub capture_id: String,
    pub capture: CaptureIdentity,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SealedResume {
    reference: String,
    confirmed_offset: u64,
    expires_at_ms: Option<u64>,
}
impl SealedResume {
    fn from_state(state: &ResumeState) -> Self {
        Self {
            reference: state.sealed_state.0.clone(),
            confirmed_offset: state.confirmed_offset,
            expires_at_ms: state.expires_at_ms,
        }
    }
    fn into_state(self) -> ResumeState {
        ResumeState {
            sealed_state: SecretRef(self.reference),
            confirmed_offset: self.confirmed_offset,
            expires_at_ms: self.expires_at_ms,
        }
    }
}

/// One object of a registration wave, with the plaintext identity its
/// inventory entry carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WaveMember {
    pub object_id: String,
    pub plaintext_length: u64,
    pub plaintext_sha256: String,
}

pub(crate) struct TransferRecord {
    pub intent: ObjectIntent,
    pub attempted: bool,
    pub resume: Option<ResumeState>,
    pub receipt: Option<ObjectReceipt>,
    /// The ciphertext is durable remotely and no longer kept here.
    pub released: bool,
    /// What this object was registered as, for a caller that rebuilt the same
    /// plaintext and needs no local ciphertext to prove it.
    pub plaintext: Option<(u64, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpoolCleanup {
    Retained,
    Removed { objects: u64, bytes: u64 },
}

pub(crate) struct TransferJournal {
    db: Connection,
    directory: PathBuf,
    identity: JobIdentity,
    pub(super) verification: super::packaging::AttemptVerification,
    spool_budget: Option<SpoolBudget>,
    families: Arc<PackFamilies>,
}
impl TransferJournal {
    /// Charges every sealed object this journal writes from now on to the
    /// application-wide transfer spool, and admits its packs among the
    /// application's active pack families.
    pub(crate) fn set_spool_budget(&mut self, budget: SpoolBudget) {
        self.families = budget.families.clone();
        self.spool_budget = Some(budget);
    }
    /// The pack families this journal's packs are admitted among. A journal
    /// without a budget still keeps its own packs to that many.
    pub(crate) fn families(&self) -> Arc<PackFamilies> {
        self.families.clone()
    }
    /// Admits a new sealed object of `bytes` before its file is created.
    pub(crate) fn reserve_spool(&self, bytes: u64) -> Result<SpoolAdmission> {
        let Some(budget) = &self.spool_budget else {
            return Ok(SpoolAdmission::Admitted(None));
        };
        Ok(match budget.try_reserve(bytes)? {
            Some(reservation) => SpoolAdmission::Admitted(Some(reservation)),
            None => SpoolAdmission::Full,
        })
    }
    /// Charges a page registering an already sealed wave. It is admitted even
    /// past the budget, because sending that wave is what releases its room;
    /// the members were admitted with room for it.
    pub(crate) fn charge_page(&self, bytes: u64) -> Result<Option<SpoolReservation>> {
        self.spool_budget
            .as_ref()
            .map(|budget| budget.charge(bytes))
            .transpose()
    }
    /// Reopens an existing journal after its worker has exited, then removes
    /// terminal spools only when the authoritative PDS owner can be released.
    /// Jobs without a transfer journal have no registered spool to remove.
    pub(crate) fn cleanup_terminal_spools_at(
        directory: &Path,
        job_id: &str,
        store: &mut crate::persistent_store::PersistentStore,
        format_repository_id: &str,
    ) -> Result<SpoolCleanup> {
        let path = directory.join("transfers.sqlite");
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SpoolCleanup::Retained);
            }
            Err(error) => return Err(storage(error)),
            Ok(_) => {}
        }
        if crate::trust_boundary::is_link_like(
            &std::fs::symlink_metadata(directory).map_err(storage)?,
        ) {
            return Err(corrupt());
        }
        crate::trust_boundary::open_regular_source(&path).map_err(storage)?;
        let db = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(storage)?;
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;")
            .map_err(storage)?;
        let encoded: String = db
            .query_row("SELECT value FROM identity WHERE singleton=1", [], |row| {
                row.get(0)
            })
            .map_err(|_| corrupt())?;
        if encoded.len() > 16 * 1024 {
            return Err(corrupt());
        }
        let identity: JobIdentity = serde_json::from_str(&encoded).map_err(|_| corrupt())?;
        if identity.job_id != job_id {
            return Err(corrupt());
        }
        Self { db, directory: directory.into(), identity, verification: Default::default(), spool_budget: None, families: PackFamilies::new(ACTIVE_PACK_FAMILIES) }
            .cleanup_terminal_spools(store, format_repository_id)
    }

    pub(crate) fn progress(directory: &Path, job_id: &str) -> Result<(u64, u64, u64, u64)> {
        let path = directory.join("transfers.sqlite");
        crate::trust_boundary::open_regular_source(&path).map_err(storage)?;
        let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(storage)?;
        let encoded: String = db
            .query_row("SELECT value FROM identity WHERE singleton=1", [], |row| {
                row.get(0)
            })
            .map_err(storage)?;
        let identity: JobIdentity = serde_json::from_str(&encoded).map_err(|_| corrupt())?;
        if identity.job_id != job_id {
            return Err(corrupt());
        }
        let values:(i64,i64,i64,i64)=db.query_row("SELECT COALESCE(SUM(CASE WHEN receipt IS NOT NULL THEN json_extract(intent,'$.byteLength') ELSE COALESCE(json_extract(resume,'$.confirmedOffset'),0) END),0),COALESCE(SUM(json_extract(intent,'$.byteLength')),0),COALESCE(SUM(receipt IS NOT NULL),0),COUNT(*) FROM objects",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).map_err(storage)?;
        Ok((
            values.0.try_into().map_err(|_| corrupt())?,
            values.1.try_into().map_err(|_| corrupt())?,
            values.2.try_into().map_err(|_| corrupt())?,
            values.3.try_into().map_err(|_| corrupt())?,
        ))
    }
    pub(crate) fn job_id(&self) -> &str {
        &self.identity.job_id
    }

    pub(crate) fn connection_id(&self) -> &str {
        &self.identity.connection_id
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    /// A previous receipt is historical once a fresh remote observation says
    /// the object is missing. Keep its immutable source and upload session.
    pub(crate) fn reopen_object(&mut self, object: &str) -> Result<()> {
        if self.db.execute("UPDATE objects SET receipt=NULL WHERE id=?1 AND released=0", [object])
            .map_err(storage)? != 1
        {
            return Err(corrupt());
        }
        Ok(())
    }

    /// What one job has already put in the repository, read without taking the
    /// journal over. A cleanup needs this because an unfinished job's fragments
    /// are named by nothing else.
    /// The selected parent graph, written before the first reuse decision so a
    /// cleanup that finds this job stopped still protects what it is reusing.
    pub(crate) fn record_parent(&self, roots: &[wire::StoredObject]) -> Result<()> {
        for root in roots {
            root.validate().map_err(|_| corrupt())?;
        }
        let encoded = serde_json::to_vec(roots).map_err(storage)?;
        if encoded.len() > 256 * 1024 {
            return Err(corrupt());
        }
        let mut file = tempfile::NamedTempFile::new_in(&self.directory).map_err(storage)?;
        std::io::Write::write_all(file.as_file_mut(), &encoded).map_err(storage)?;
        file.as_file_mut().sync_all().map_err(storage)?;
        file.persist(self.directory.join(PARENT_FILE))
            .map_err(storage)?
            .sync_all()
            .map_err(storage)?;
        Ok(())
    }

    pub(crate) fn parent(directory: &Path) -> Result<Vec<wire::StoredObject>> {
        let path = directory.join(PARENT_FILE);
        let encoded = match std::fs::read(&path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(storage(error)),
        };
        if encoded.len() > 256 * 1024 {
            return Err(corrupt());
        }
        let roots: Vec<wire::StoredObject> =
            serde_json::from_slice(&encoded).map_err(|_| corrupt())?;
        for root in &roots {
            root.validate().map_err(|_| corrupt())?;
        }
        Ok(roots)
    }

    pub(crate) fn uploaded(directory: &Path, job_id: &str) -> Result<Vec<ObjectReceipt>> {
        let path = directory.join("transfers.sqlite");
        crate::trust_boundary::open_regular_source(&path).map_err(storage)?;
        let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(storage)?;
        let encoded: String = db
            .query_row("SELECT value FROM identity WHERE singleton=1", [], |row| {
                row.get(0)
            })
            .map_err(storage)?;
        let identity: JobIdentity = serde_json::from_str(&encoded).map_err(|_| corrupt())?;
        if identity.job_id != job_id {
            return Err(corrupt());
        }
        let mut query = db
            .prepare("SELECT receipt FROM objects WHERE receipt IS NOT NULL")
            .map_err(storage)?;
        let rows = query
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage)?;
        let mut receipts = Vec::new();
        for row in rows {
            let encoded = row.map_err(storage)?;
            if encoded.len() > 64 * 1024 {
                return Err(corrupt());
            }
            receipts.push(serde_json::from_str(&encoded).map_err(|_| corrupt())?);
        }
        Ok(receipts)
    }
    pub fn open(directory: &Path, identity: JobIdentity) -> Result<Self> {
        if [
            &identity.job_id,
            &identity.connection_id,
            &identity.repository_id,
            &identity.capture_id,
        ]
        .iter()
        .any(|value| value.is_empty())
        {
            return Err(corrupt());
        }
        std::fs::create_dir_all(directory).map_err(storage)?;
        if crate::trust_boundary::is_link_like(
            &std::fs::symlink_metadata(directory).map_err(storage)?,
        ) {
            return Err(corrupt());
        }
        let path = directory.join("transfers.sqlite");
        let fresh = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                file.sync_all().map_err(storage)?;
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                crate::trust_boundary::open_regular_source(&path).map_err(storage)?;
                false
            }
            Err(error) => return Err(storage(error)),
        };
        let db = Connection::open(&path).map_err(storage)?;
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;")
            .map_err(storage)?;
        if fresh {
            db.execute_batch("BEGIN IMMEDIATE;
                CREATE TABLE identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1), value TEXT NOT NULL);
                CREATE TABLE objects(id TEXT PRIMARY KEY,intent TEXT NOT NULL,attempted INTEGER NOT NULL CHECK(attempted IN (0,1)),resume TEXT,receipt TEXT,page TEXT,plaintext_length INTEGER,plaintext_sha256 TEXT,released INTEGER NOT NULL DEFAULT 0 CHECK(released IN (0,1)));").map_err(storage)?;
            db.execute(
                "INSERT INTO identity VALUES(1,?1)",
                [serde_json::to_string(&identity).map_err(storage)?],
            )
            .map_err(storage)?;
            db.execute_batch("COMMIT").map_err(storage)?;
            crate::trust_boundary::sync_directory(directory).map_err(storage)?;
        }
        let encoded: String = db
            .query_row("SELECT value FROM identity WHERE singleton=1", [], |row| {
                row.get(0)
            })
            .map_err(|_| corrupt())?;
        if encoded.len() > 16 * 1024
            || serde_json::from_str::<JobIdentity>(&encoded).map_err(|_| corrupt())? != identity
        {
            return Err(corrupt());
        }
        let journal = Self {
            db,
            directory: directory.into(),
            identity,
            verification: Default::default(),
            spool_budget: None,
            families: PackFamilies::new(ACTIVE_PACK_FAMILIES),
        };
        journal.discard_released()?;
        Ok(journal)
    }

    pub fn spool_path(&self, object_id: &str) -> PathBuf {
        self.directory.join(format!(
            "{}.spool",
            hex::encode(risunest_external_storage_format::content_identity::hash(
                object_id.as_bytes()
            ))
        ))
    }

    /// The producer has already closed and fsynced the immutable ciphertext.
    pub fn register(&mut self, intent: &ObjectIntent) -> Result<()> {
        if intent.job_id != self.identity.job_id
            || intent.repository_id != self.identity.repository_id
            || intent.object_id.is_empty()
            || !crate::trust_boundary::is_lower_hex_256(&intent.sha256)
        {
            return Err(corrupt());
        }
        SpoolSource::verified(
            &self.spool_path(&intent.object_id),
            intent.byte_length,
            &intent.sha256,
        )?;
        crate::trust_boundary::sync_directory(&self.directory).map_err(storage)?;
        if let Some(existing) = self.record(&intent.object_id)? {
            return if existing.intent == *intent {
                Ok(())
            } else {
                Err(corrupt())
            };
        }
        self.db
            .execute(
                "INSERT INTO objects VALUES(?1,?2,0,NULL,NULL,NULL,NULL,NULL,0)",
                params![
                    intent.object_id,
                    serde_json::to_string(intent).map_err(storage)?
                ],
            )
            .map_err(storage)?;
        Ok(())
    }

    /// Fixes which page covers which objects before that page is uploaded. A
    /// resumed wave rebuilds its document from these rows rather than from
    /// whatever the caller gathers the second time, because a page that
    /// changed shape would upload different bytes under the same object id.
    pub fn cover(&mut self, page_object: &str, members: &[WaveMember]) -> Result<()> {
        if page_object.is_empty() || members.is_empty() {
            return Err(corrupt());
        }
        let transaction = self.db.unchecked_transaction().map_err(storage)?;
        for member in members {
            let row: Option<(Option<String>, Option<i64>, Option<String>)> = transaction
                .query_row(
                    "SELECT page,plaintext_length,plaintext_sha256 FROM objects WHERE id=?1",
                    params![member.object_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(storage)?;
            let Some((page, length, digest)) = row else {
                return Err(corrupt());
            };
            let plaintext_length =
                i64::try_from(member.plaintext_length).map_err(|_| corrupt())?;
            match page {
                Some(existing)
                    if existing == page_object
                        && length == Some(plaintext_length)
                        && digest.as_deref() == Some(member.plaintext_sha256.as_str()) => {}
                Some(_) => return Err(corrupt()),
                None => {
                    transaction
                        .execute(
                            "UPDATE objects SET page=?2,plaintext_length=?3,plaintext_sha256=?4
                             WHERE id=?1 AND page IS NULL",
                            params![
                                member.object_id,
                                page_object,
                                plaintext_length,
                                member.plaintext_sha256
                            ],
                        )
                        .map_err(storage)?;
                }
            }
        }
        transaction.commit().map_err(storage)
    }

    /// The exact membership a page was fixed with, in the order its document
    /// lists it.
    pub fn page_members(&self, page_object: &str) -> Result<Vec<(ObjectIntent, u64, String)>> {
        let mut statement = self
            .db
            .prepare(
                "SELECT intent,plaintext_length,plaintext_sha256 FROM objects
                 WHERE page=?1 ORDER BY id",
            )
            .map_err(storage)?;
        let rows = statement
            .query_map(params![page_object], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(storage)?;
        let mut members = Vec::new();
        for row in rows {
            let (intent, length, digest) = row.map_err(storage)?;
            members.push((
                serde_json::from_str(&intent).map_err(|_| corrupt())?,
                u64::try_from(length).map_err(|_| corrupt())?,
                digest,
            ));
        }
        Ok(members)
    }

    pub fn covered_by(&self, object: &str) -> Result<Option<String>> {
        self.db
            .query_row(
                "SELECT page FROM objects WHERE id=?1",
                params![object],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(storage)
            .map(Option::flatten)
    }

    pub fn record(&self, object: &str) -> Result<Option<TransferRecord>> {
        type Row = (
            String,
            bool,
            Option<String>,
            Option<String>,
            bool,
            Option<i64>,
            Option<String>,
        );
        let row: Option<Row> = self
            .db
            .query_row(
                "SELECT intent,attempted,resume,receipt,released,plaintext_length,plaintext_sha256
                 FROM objects WHERE id=?1",
                [object],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()
            .map_err(storage)?;
        let Some((intent, attempted, resume, receipt, released, length, digest)) = row else {
            return Ok(None);
        };
        // Only a durable receipt can have released the ciphertext.
        if released && receipt.is_none() {
            return Err(corrupt());
        }
        if intent.len() > 64 * 1024
            || resume.as_ref().is_some_and(|s| s.len() > 16 * 1024)
            || receipt.as_ref().is_some_and(|s| s.len() > 64 * 1024)
        {
            return Err(corrupt());
        }
        let intent: ObjectIntent = serde_json::from_str(&intent).map_err(|_| corrupt())?;
        if intent.object_id != object
            || intent.job_id != self.identity.job_id
            || intent.repository_id != self.identity.repository_id
        {
            return Err(corrupt());
        }
        let resume = resume
            .map(|s| {
                serde_json::from_str::<SealedResume>(&s)
                    .map(SealedResume::into_state)
                    .map_err(|_| corrupt())
            })
            .transpose()?;
        if resume
            .as_ref()
            .is_some_and(|r| r.confirmed_offset > intent.byte_length || r.sealed_state.0.is_empty())
        {
            return Err(corrupt());
        }
        let receipt = receipt
            .map(|s| serde_json::from_str(&s).map_err(|_| corrupt()))
            .transpose()?;
        let plaintext = match (length, digest) {
            (Some(length), Some(digest)) => {
                Some((u64::try_from(length).map_err(|_| corrupt())?, digest))
            }
            (None, None) => None,
            _ => return Err(corrupt()),
        };
        Ok(Some(TransferRecord {
            intent,
            attempted,
            resume,
            receipt,
            released,
            plaintext,
        }))
    }

    pub fn attempted(&mut self, object: &str, resume: Option<&ResumeState>) -> Result<()> {
        let resume = resume
            .map(|r| serde_json::to_string(&SealedResume::from_state(r)))
            .transpose()
            .map_err(storage)?;
        if self
            .db
            .execute(
                "UPDATE objects SET attempted=1,resume=?2 WHERE id=?1 AND receipt IS NULL",
                params![object, resume],
            )
            .map_err(storage)?
            != 1
        {
            return Err(corrupt());
        }
        Ok(())
    }

    pub fn complete(
        &mut self,
        intent: &ObjectIntent,
        repository: &RepositoryHandle,
        receipt: &ObjectReceipt,
    ) -> Result<()> {
        validate_receipt(intent, repository, receipt)?;
        if self
            .db
            .execute(
                "UPDATE objects SET receipt=?2,released=1 WHERE id=?1 AND attempted=1",
                params![
                    intent.object_id,
                    serde_json::to_string(receipt).map_err(storage)?
                ],
            )
            .map_err(storage)?
            != 1
        {
            return Err(corrupt());
        }
        self.discard_spool(&intent.object_id)
    }

    /// Removing the file after its release is durable, so a crash between the
    /// two leaves a file the next reopening sweeps rather than an object whose
    /// receipt says nothing about what is still on disk.
    fn discard_spool(&self, object: &str) -> Result<()> {
        let path = self.spool_path(object);
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(storage(error)),
            Ok(metadata) if crate::trust_boundary::is_link_like(&metadata) => {
                return Err(corrupt())
            }
            Ok(_) => {}
        }
        std::fs::remove_file(&path).map_err(storage)?;
        crate::trust_boundary::sync_directory(&self.directory).map_err(storage)?;
        Ok(())
    }

    fn discard_released(&self) -> Result<()> {
        let ids = {
            let mut query = self
                .db
                .prepare("SELECT id FROM objects WHERE released=1 ORDER BY id")
                .map_err(storage)?;
            let rows = query
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(storage)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(storage)?
        };
        for id in ids {
            self.discard_spool(&id)?;
        }
        Ok(())
    }

    /// The actual worker must have ended before handing its journal here. PDS
    /// completion is written only after the final remote root is confirmed;
    /// cancellation is terminal only after any unknown publication is resolved.
    /// This releases this job's capture reference, never another owner or the
    /// shared capture payload. Conflicts and exports can retain every spool.
    pub(crate) fn cleanup_terminal_spools(
        self,
        store: &mut crate::persistent_store::PersistentStore,
        format_repository_id: &str,
    ) -> Result<SpoolCleanup> {
        let job = store.external_job(&self.identity.job_id).map_err(storage)?
            .ok_or_else(corrupt)?;
        if job.id != self.identity.job_id || job.connection_id != self.identity.connection_id
            || job.repository_id != format_repository_id || job.capture_id != self.identity.capture_id
            || job.identity != self.identity.capture
        {
            return Err(corrupt());
        }
        if !matches!(job.phase.as_str(), "complete" | "cancelled") {
            return Ok(SpoolCleanup::Retained);
        }
        if !store.release_external_capture(&job.capture_id, &job.id).map_err(storage)? {
            return Ok(SpoolCleanup::Retained);
        }
        if crate::trust_boundary::is_link_like(
            &std::fs::symlink_metadata(&self.directory).map_err(storage)?,
        ) {
            return Err(corrupt());
        }
        let ids = {
            let mut query = self.db.prepare("SELECT id FROM objects ORDER BY id").map_err(storage)?;
            let rows = query.query_map([], |row| row.get::<_, String>(0)).map_err(storage)?;
            rows.collect::<std::result::Result<Vec<_>, _>>().map_err(storage)?
        };
        // Validate all registered files before the first unlink. Unregistered
        // partial files, receive caches and shared source paths are not swept.
        let mut files = Vec::new();
        for id in ids {
            self.record(&id)?.ok_or_else(corrupt)?;
            let path = self.spool_path(&id);
            match std::fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(storage(error)),
                Ok(_) => {}
            }
            let file = crate::trust_boundary::open_regular_source(&path).map_err(storage)?;
            let length = file.metadata().map_err(storage)?.len();
            files.push((path, length));
        }
        let mut objects = 0u64;
        let mut bytes = 0u64;
        for (path, length) in files {
            match std::fs::remove_file(path) {
                Ok(()) => {
                    objects = objects.checked_add(1).ok_or_else(corrupt)?;
                    bytes = bytes.checked_add(length).ok_or_else(corrupt)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(storage(error)),
            }
        }
        crate::trust_boundary::sync_directory(&self.directory).map_err(storage)?;
        Ok(SpoolCleanup::Removed { objects, bytes })
    }

    pub(crate) async fn release_completed_sessions(
        &mut self,
        vault: &dyn super::auth::SecretVault,
    ) -> Result<()> {
        let entries = {
            let mut query = self.db.prepare("SELECT id,resume FROM objects WHERE receipt IS NOT NULL AND resume IS NOT NULL").map_err(storage)?;
            let rows = query
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(storage)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(storage)?
        };
        for (id, encoded) in entries {
            let resume: SealedResume = serde_json::from_str(&encoded).map_err(|_| corrupt())?;
            vault.remove(&SecretRef(resume.reference)).await?;
            self.db
                .execute(
                    "UPDATE objects SET resume=NULL WHERE id=?1 AND receipt IS NOT NULL",
                    [id],
                )
                .map_err(storage)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        external_storage::fake,
        persistent_store::{external_storage_state, PersistentStore},
    };

    fn fixture() -> (tempfile::TempDir, PersistentStore, JobIdentity, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(root.path()).unwrap();
        let capture = store.external_identity().unwrap();
        // Seed only synthetic authority through the same capture registration
        // operation used by PDS tests. No user data or provider is involved.
        let mut db = Connection::open(root.path().join("persistent/persistent.sqlite")).unwrap();
        let tx = db.transaction().unwrap();
        external_storage_state::register_capture(
            &tx, "capture", &capture, "scope", "logical-v1", "", &"a".repeat(64), "connection",
        ).unwrap();
        tx.commit().unwrap();
        drop(db);
        store.external_prepare_backup("job", "connection", "format-repository", "capture", "point").unwrap();
        let identity = JobIdentity {
            job_id: "job".into(), connection_id: "connection".into(),
            repository_id: fake::repository().repository_id, capture_id: "capture".into(), capture,
        };
        let directory = root.path().join("job");
        let mut journal = TransferJournal::open(&directory, identity.clone()).unwrap();
        let bytes = b"synthetic sealed ciphertext";
        let intent = ObjectIntent {
            repository_id: identity.repository_id.clone(), job_id: identity.job_id.clone(),
            object_id: "pack".into(), role: ObjectRole::Pack,
            byte_length: bytes.len() as u64, sha256: risunest_sync_wire::hash(bytes),
        };
        std::fs::write(journal.spool_path("pack"), bytes).unwrap();
        journal.register(&intent).unwrap();
        drop(journal);
        (root, store, identity, directory)
    }

    /// Every holder counts toward one budget: another job's retained spool,
    /// the asking job's own, and what another job was admitted to write but
    /// has not finished writing. A holder that cannot be read defers the
    /// write rather than reading as empty.
    #[test]
    fn c_the_spool_budget_counts_every_holder_and_refuses_what_would_pass_it() {
        let root = tempfile::tempdir().unwrap();
        let own_id = format!("own-{}", uuid::Uuid::new_v4());
        let other_id = format!("other-{}", uuid::Uuid::new_v4());
        let own = root.path().join("own");
        let other = root.path().join("other");
        for directory in [&own, &other] {
            std::fs::create_dir_all(directory).unwrap();
        }
        let retained = std::fs::File::create(other.join("retained.spool")).unwrap();
        retained.set_len(300).unwrap();
        let resumed = std::fs::File::create(own.join("resumed.spool")).unwrap();
        resumed.set_len(100).unwrap();
        std::fs::write(own.join("transfers.sqlite"), vec![0; 4096]).unwrap();
        let holders = vec![(own_id.clone(), own.clone()), (other_id.clone(), other.clone())];
        let budget = |job: &str| {
            let holders = holders.clone();
            SpoolBudget::new(job.into(), move || Ok(holders.clone())).with_limit(500)
        };
        let writing = budget(&own_id).reserve(100).unwrap();
        assert!(
            budget(&other_id).reserve(1).is_err(),
            "a write another job was admitted to was not counted"
        );
        drop(writing);
        drop(budget(&other_id).reserve(100).unwrap());
        resumed.set_len(200).unwrap();
        assert!(
            budget(&own_id).reserve(1).is_err(),
            "the asking job's own spool was not counted"
        );
        let unreadable = root.path().join("not-a-directory");
        std::fs::write(&unreadable, b"synthetic").unwrap();
        let broken = SpoolBudget::new(own_id.clone(), move || {
            Ok(vec![("broken".into(), unreadable.clone())])
        });
        assert_eq!(
            broken.reserve(1).err().map(|error| error.kind),
            Some(ErrorKind::Transient)
        );
        assert!(held_spool_bytes(&root.path().join("absent")).unwrap() == 0);
    }

    /// Families past the limit wait rather than fail. A waiter is what
    /// `contended` reports, a released family is what it receives, and a
    /// wait that is abandoned or cancelled leaves nobody counted as waiting.
    #[test]
    fn c_pack_families_admit_up_to_their_limit_and_count_who_waits() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let families = PackFamilies::new(ACTIVE_PACK_FAMILIES);
            let first = families.try_acquire().unwrap().unwrap();
            let second = families.try_acquire().unwrap().unwrap();
            assert!(families.try_acquire().unwrap().is_none());
            assert_eq!(families.counts(), (2, 0));

            let abandoned = families.acquire(&Cancellation::default(), async {}).await;
            assert_eq!(abandoned.err().map(|error| error.kind), Some(ErrorKind::Cancelled));
            let cancel = Cancellation::default();
            cancel.cancel();
            let cancelled = families.acquire(&cancel, std::future::pending()).await;
            assert_eq!(cancelled.err().map(|error| error.kind), Some(ErrorKind::Cancelled));
            assert_eq!(families.counts(), (2, 0));

            let never = Cancellation::default();
            let (third, ()) = tokio::join!(
                families.acquire(&never, std::future::pending()),
                async {
                    families.contended().await;
                    assert_eq!(families.counts(), (2, 1));
                    drop(first);
                },
            );
            assert_eq!(families.counts(), (2, 0));
            drop((second, third.unwrap()));
            assert_eq!(families.counts(), (0, 0));
        });
    }

    /// A panic while the family counts were locked leaves them exact, so
    /// every family and wait that ends is still counted out, and a new family
    /// can still start.
    #[test]
    fn c_a_poisoned_family_lock_still_ends_every_family() {
        let families = PackFamilies::new(1);
        let permit = families.try_acquire().unwrap().unwrap();
        let poisoner = families.clone();
        std::thread::spawn(move || {
            let _held = poisoner.state.lock().unwrap();
            panic!("the family counts are poisoned on purpose");
        })
        .join()
        .unwrap_err();
        assert!(families.state.is_poisoned());
        assert!(families.try_acquire().unwrap().is_none());
        let abandoned = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(families.acquire(&Cancellation::default(), async {}));
        assert_eq!(abandoned.err().map(|error| error.kind), Some(ErrorKind::Cancelled));
        assert_eq!(families.counts(), (1, 0));
        drop(permit);
        assert_eq!(families.counts(), (0, 0));
        let again = families.try_acquire().unwrap().unwrap();
        assert_eq!(families.counts(), (1, 0));
        drop(again);
        assert_eq!(families.counts(), (0, 0));
    }

    #[test]
    fn c_terminal_spool_cleanup_preserves_failed_work_and_conflict_or_export_owners() {
        let (root, mut store, identity, directory) = fixture();
        let journal = TransferJournal::open(&directory, identity.clone()).unwrap();
        let spool = journal.spool_path("pack");
        assert_eq!(journal.cleanup_terminal_spools(&mut store, "format-repository").unwrap(), SpoolCleanup::Retained);
        assert!(spool.is_file());
        store.retain_external_capture("capture", "conflict-owner").unwrap();
        store.retain_external_capture("capture", "export-owner").unwrap();
        store.external_finish_backup("job", "point", "bundle", "authenticated-synthetic-observation").unwrap();
        let journal = TransferJournal::open(&directory, identity.clone()).unwrap();
        assert_eq!(journal.cleanup_terminal_spools(&mut store, "format-repository").unwrap(), SpoolCleanup::Retained);
        assert!(spool.is_file());
        assert!(!store.release_external_capture("capture", "conflict-owner").unwrap());
        let journal = TransferJournal::open(&directory, identity.clone()).unwrap();
        assert_eq!(journal.cleanup_terminal_spools(&mut store, "format-repository").unwrap(), SpoolCleanup::Retained);
        assert!(store.release_external_capture("capture", "export-owner").unwrap());
        let shared = root.path().join("shared-source.spool");
        std::fs::write(&shared, b"shared source").unwrap();
        assert_eq!(TransferJournal::cleanup_terminal_spools_at(
            &directory, "job", &mut store, "format-repository",
        ).unwrap(),
            SpoolCleanup::Removed { objects: 1, bytes: b"synthetic sealed ciphertext".len() as u64 });
        assert!(!spool.exists());
        assert_eq!(std::fs::read(shared).unwrap(), b"shared source");
        assert_eq!(TransferJournal::cleanup_terminal_spools_at(
            &directory, "job", &mut store, "format-repository",
        ).unwrap(),
            SpoolCleanup::Removed { objects: 0, bytes: 0 });
        assert_eq!(TransferJournal::cleanup_terminal_spools_at(
            &root.path().join("no-journal"), "job", &mut store, "format-repository",
        ).unwrap(), SpoolCleanup::Retained);
    }


    /// The receipt and the release are one durable step, so the file can only
    /// outlive it by a crash, which the next reopening finishes.
    #[test]
    fn c_a_release_follows_its_receipt_and_is_finished_by_the_next_reopening() {
        let (_root, mut store, identity, directory) = fixture();
        let mut journal = TransferJournal::open(&directory, identity.clone()).unwrap();
        let spool = journal.spool_path("pack");
        let intent = journal.record("pack").unwrap().unwrap().intent;
        journal.attempted("pack", None).unwrap();
        let attempted = journal.record("pack").unwrap().unwrap();
        assert!(attempted.attempted && attempted.receipt.is_none() && !attempted.released);
        assert!(spool.is_file());
        let receipt = ObjectReceipt {
            locator: RemoteLocator {
                connection_identity: fake::repository().connection_identity,
                collection: None,
                object: "pack".into(),
            },
            byte_length: intent.byte_length,
            version: None,
            checksum: None,
            complete: true,
        };
        journal.complete(&intent, &fake::repository(), &receipt).unwrap();
        let released = journal.record("pack").unwrap().unwrap();
        assert!(released.released && released.receipt.is_some());
        assert!(!spool.exists());
        // Nothing may reopen an object whose ciphertext it no longer holds.
        assert_eq!(journal.reopen_object("pack").unwrap_err().kind, ErrorKind::Corrupt);
        assert!(journal.record("pack").unwrap().unwrap().receipt.is_some());

        std::fs::write(&spool, b"synthetic sealed ciphertext").unwrap();
        drop(journal);
        let journal = TransferJournal::open(&directory, identity).unwrap();
        assert!(!spool.exists());
        store.external_finish_backup("job", "point", "bundle", "authenticated-synthetic-observation").unwrap();
        assert_eq!(
            journal.cleanup_terminal_spools(&mut store, "format-repository").unwrap(),
            SpoolCleanup::Removed { objects: 0, bytes: 0 }
        );
    }
    #[test]
    fn c_only_authoritatively_cancelled_matching_jobs_can_discard_their_spools() {
        let (_root, mut store, identity, directory) = fixture();
        let journal = TransferJournal::open(&directory, identity.clone()).unwrap();
        let spool = journal.spool_path("pack");
        assert_eq!(journal.cleanup_terminal_spools(&mut store, "wrong-repository").unwrap_err().kind, ErrorKind::Corrupt);
        assert!(spool.exists());
        store.external_cancel_prepared("job").unwrap();
        let journal = TransferJournal::open(&directory, identity).unwrap();
        assert!(matches!(journal.cleanup_terminal_spools(&mut store, "format-repository").unwrap(),
            SpoolCleanup::Removed { objects: 1, .. }));
        assert!(!spool.exists());
    }
}

pub(crate) fn validate_receipt(
    intent: &ObjectIntent,
    repository: &RepositoryHandle,
    receipt: &ObjectReceipt,
) -> Result<()> {
    intent.validate(repository)?;
    receipt.locator.validate_for(repository)?;
    if !receipt.complete
        || receipt.byte_length != intent.byte_length
        || receipt
            .checksum
            .as_ref()
            .is_some_and(|c| c.algorithm.eq_ignore_ascii_case("sha256") && c.value != intent.sha256)
    {
        return Err(corrupt());
    }
    Ok(())
}
