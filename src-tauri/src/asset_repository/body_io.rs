//! Scoped CAS body-I/O observations for verification entries only.
//!
//! Reset/run/take must execute on the same thread. This sample covers CAS opens,
//! reads, staging writes and publication inside payload_cas, not all app I/O.
//! Returned raw File handles and paths make subsequent coverage incomplete.
//! Register purposes from actual object provenance after reset. Domains are
//! totals; object rows are a classification of those totals, not extra work.
//! Tracked readers retain their owner scope. Crossing threads or reset/take
//! boundaries invalidates that scope. Worker aggregation requires an explicit
//! captured scope, kept alive until its worker has completed.
#![cfg(test)]

use std::{cell::RefCell, collections::{BTreeMap, BTreeSet}, fs::File, io::{self, Read, Seek, SeekFrom}, sync::{Arc, Mutex}, thread::ThreadId};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct BodyShaWork { pub calls: u64, pub bytes: u64 }

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PublicationWork {
    pub attempts: u64,
    pub successes: u64,
    pub already_exists: u64,
    pub failures: u64,
    pub object_bytes: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct BodyWork {
    pub open_attempts: u64,
    pub opens: u64,
    pub failed_opens: u64,
    pub unknown_open_results: u64,
    pub read_operations: u64,
    pub read_bytes: u64,
    pub incomplete_reads: u64,
    pub escaped_handles: u64,
    pub escaped_paths: u64,
    pub outstanding_readers: u64,
    pub identity_metadata_open_attempts: u64,
    pub identity_metadata_opens: u64,
    pub identity_metadata_failed_opens: u64,
    pub verified_identity_metadata_opens: u64,
    pub staging_write_attempts: u64,
    pub staging_writes: u64,
    pub staging_failed_writes: u64,
    pub staging_requested_bytes: u64,
    pub staging_written_bytes: u64,
    pub incomplete_writes: u64,
    pub publication_attempts: u64,
    pub publications: u64,
    pub publication_already_exists: u64,
    pub publication_failures: u64,
    // The identity size named by a successful link/rename, not copied bytes.
    pub publication_object_bytes: u64,
    pub publication_kinds: BTreeMap<&'static str, PublicationWork>,
    // A per-object attribution of native hash_work, not additional SHA work.
    pub body_sha: BTreeMap<&'static str, BodyShaWork>,
}

impl BodyWork {
    pub(super) fn add(&mut self, other: &Self) {
        self.open_attempts += other.open_attempts; self.opens += other.opens;
        self.failed_opens += other.failed_opens; self.unknown_open_results += other.unknown_open_results;
        self.read_operations += other.read_operations; self.read_bytes += other.read_bytes;
        self.incomplete_reads += other.incomplete_reads; self.escaped_handles += other.escaped_handles;
        self.escaped_paths += other.escaped_paths; self.outstanding_readers += other.outstanding_readers;
        self.identity_metadata_open_attempts += other.identity_metadata_open_attempts;
        self.identity_metadata_opens += other.identity_metadata_opens;
        self.identity_metadata_failed_opens += other.identity_metadata_failed_opens;
        self.verified_identity_metadata_opens += other.verified_identity_metadata_opens;
        self.staging_write_attempts += other.staging_write_attempts; self.staging_writes += other.staging_writes;
        self.staging_failed_writes += other.staging_failed_writes;
        self.staging_requested_bytes += other.staging_requested_bytes; self.staging_written_bytes += other.staging_written_bytes;
        self.incomplete_writes += other.incomplete_writes;
        self.publication_attempts += other.publication_attempts; self.publications += other.publications;
        self.publication_already_exists += other.publication_already_exists; self.publication_failures += other.publication_failures;
        self.publication_object_bytes += other.publication_object_bytes;
        for (kind, source) in &other.publication_kinds {
            let target = self.publication_kinds.entry(kind).or_default();
            target.attempts += source.attempts; target.successes += source.successes;
            target.already_exists += source.already_exists; target.failures += source.failures; target.object_bytes += source.object_bytes;
        }
        for (domain, source) in &other.body_sha {
            let target = self.body_sha.entry(domain).or_default(); target.calls += source.calls; target.bytes += source.bytes;
        }
    }
    fn observed_completely(&self) -> bool {
        self.unknown_open_results == 0 && self.incomplete_reads == 0 && self.incomplete_writes == 0
            && self.escaped_handles == 0 && self.escaped_paths == 0 && self.outstanding_readers == 0
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum BodyPurpose { Control, Asset }

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ObjectBodyWork {
    pub purposes: BTreeSet<BodyPurpose>,
    pub work: BodyWork,
    pub owned_work: BodyWork,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BodyIo {
    pub thread: ThreadId,
    // managed: canonical assets/objects bodies. owned: CAS staging/import files.
    pub domains: BTreeMap<&'static str, BodyWork>,
    pub objects: BTreeMap<String, ObjectBodyWork>,
    pub unattributed_managed: BodyWork,
    pub unattributed_owned: BodyWork,
    pub pending_owned_identities: u64,
    pub pending_body_hashes: u64,
    pub pending_worker_scopes: u64,
    pub worker_scopes_started: u64,
    pub worker_scopes_settled: u64,
    pub worker_threads: BTreeSet<String>,
    pub scope_violations: u64,
    closed: bool,
    // API requests/queries, not OS metadata syscall counts.
    pub stat_requests: u64,
    pub batch_stat_requests: u64,
    pub presence_queries: u64,
}

impl Default for BodyIo {
    fn default() -> Self {
        Self {
            thread: std::thread::current().id(), domains: BTreeMap::new(), objects: BTreeMap::new(),
            unattributed_managed: BodyWork::default(), unattributed_owned: BodyWork::default(),
            pending_owned_identities: 0, pending_body_hashes: 0, pending_worker_scopes: 0,
            worker_scopes_started: 0, worker_scopes_settled: 0, worker_threads: BTreeSet::new(),
            scope_violations: 0, closed: false,
            stat_requests: 0, batch_stat_requests: 0, presence_queries: 0,
        }
    }
}

impl BodyIo {
    pub(crate) fn complete(&self) -> bool {
        self.scope_violations == 0 && self.pending_owned_identities == 0 && self.pending_body_hashes == 0
            && self.pending_worker_scopes == 0 && self.worker_scopes_started == self.worker_scopes_settled
            && self.domains.values().all(BodyWork::observed_completely)
            && self.unknown_work() == BodyWork::default()
    }
    pub(crate) fn asset_work(&self) -> BodyWork {
        self.classified_work(|row| row.purposes.contains(&BodyPurpose::Asset))
    }
    pub(crate) fn control_work(&self) -> BodyWork {
        self.classified_work(|row| row.purposes.contains(&BodyPurpose::Control) && !row.purposes.contains(&BodyPurpose::Asset))
    }
    pub(crate) fn unknown_work(&self) -> BodyWork {
        let mut work = self.classified_work(|row| row.purposes.is_empty());
        work.add(&self.unattributed_managed); work.add(&self.unattributed_owned); work
    }
    fn classified_work(&self, include: impl Fn(&ObjectBodyWork) -> bool) -> BodyWork {
        let mut work = BodyWork::default();
        for row in self.objects.values().filter(|row| include(row)) { work.add(&row.work); work.add(&row.owned_work); }
        work
    }
}

thread_local! {
    static WORK: RefCell<Arc<Mutex<BodyIo>>> = RefCell::new(Arc::new(Mutex::new(BodyIo::default())));
    static OBJECT: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static PENDING_OWNED: RefCell<Vec<(Arc<Mutex<BodyIo>>, Arc<Mutex<BodyWork>>)>> = const { RefCell::new(Vec::new()) };
}

fn current_scope() -> Arc<Mutex<BodyIo>> { WORK.with(|work| work.borrow().clone()) }

pub(crate) fn reset_body_io() {
    let previous = current_scope();
    let mut previous = previous.lock().unwrap(); previous.closed = true;
    let mut next = BodyIo::default();
    if previous.pending_worker_scopes > 0 { previous.scope_violations += 1; next.scope_violations += 1; }
    WORK.with(|work| *work.borrow_mut() = Arc::new(Mutex::new(next)));
}

pub(crate) fn take_body_io() -> BodyIo {
    let scope = current_scope();
    let work = scope.lock().unwrap().clone();
    reset_body_io(); work
}

pub(crate) struct BodyIoScope { pub(super) scope: Arc<Mutex<BodyIo>>, entered: bool }

pub(crate) fn capture_body_io_scope() -> BodyIoScope {
    let scope = current_scope();
    { let mut work = scope.lock().unwrap(); if work.closed { work.scope_violations += 1; } work.pending_worker_scopes += 1; }
    BodyIoScope { scope, entered: false }
}

pub(crate) fn with_body_io_scope<T>(mut scope: BodyIoScope, operation: impl FnOnce() -> T) -> T {
    let previous = current_scope();
    {
        let mut work = scope.scope.lock().unwrap();
        if work.closed { work.scope_violations += 1; }
        work.worker_scopes_started += 1;
        work.worker_threads.insert(format!("{:?}", std::thread::current().id()));
    }
    scope.entered = true;
    WORK.with(|work| *work.borrow_mut() = scope.scope.clone());
    struct Restore(Arc<Mutex<BodyIo>>);
    impl Drop for Restore { fn drop(&mut self) { WORK.with(|work| *work.borrow_mut() = self.0.clone()); } }
    let _restore = Restore(previous);
    operation()
}

impl Drop for BodyIoScope {
    fn drop(&mut self) {
        let mut work = self.scope.lock().unwrap(); work.pending_worker_scopes -= 1;
        if self.entered { work.worker_scopes_settled += 1; }
        else { work.scope_violations += 1; }
        if work.closed || std::thread::panicking() { work.scope_violations += 1; }
    }
}

pub(crate) fn register_object_purpose(hash: &str, purpose: BodyPurpose) {
    current_scope().lock().unwrap().objects.entry(hash.to_owned()).or_default().purposes.insert(purpose);
}

pub(crate) struct ObjectScope;
pub(crate) fn object_scope(hash: &str) -> ObjectScope {
    OBJECT.with(|object| object.borrow_mut().push(hash.to_owned())); ObjectScope
}
impl Drop for ObjectScope {
    fn drop(&mut self) { OBJECT.with(|object| { object.borrow_mut().pop(); }); }
}

fn record(work: &mut BodyIo, kind: &'static str, hash: Option<&str>, update: impl Fn(&mut BodyWork)) {
    if work.closed { work.scope_violations += 1; }
    update(work.domains.entry(kind).or_default());
    if kind == "managed" || kind == "owned" {
        match hash {
            Some(hash) => {
                let row = work.objects.entry(hash.to_owned()).or_default();
                update(if kind == "managed" { &mut row.work } else { &mut row.owned_work });
            }
            None => update(if kind == "managed" { &mut work.unattributed_managed } else { &mut work.unattributed_owned }),
        }
    }
}
fn domain(kind: &'static str, update: impl Fn(&mut BodyWork)) {
    if kind == "owned" {
        if let Some((scope, pending)) = PENDING_OWNED.with(|pending| pending.borrow().last().cloned()) {
            let current = current_scope();
            if scope.lock().unwrap().closed || !Arc::ptr_eq(&scope, &current) {
                scope.lock().unwrap().scope_violations += 1;
                current.lock().unwrap().scope_violations += 1;
            }
            update(scope.lock().unwrap().domains.entry(kind).or_default());
            update(&mut pending.lock().unwrap());
            return;
        }
    }
    let hash = OBJECT.with(|object| object.borrow().last().cloned());
    record(&mut current_scope().lock().unwrap(), kind, hash.as_deref(), update);
}

// Staging opens precede final stream validation. Associate their observed work
// only after that validation supplies the actual immutable content identity.
pub(super) struct PendingOwnedIdentity {
    scope: Arc<Mutex<BodyIo>>,
    work: Arc<Mutex<BodyWork>>,
    hash: Option<String>,
    owner: ThreadId,
}
impl PendingOwnedIdentity {
    pub(super) fn new() -> Self {
        let scope = current_scope();
        scope.lock().unwrap().pending_owned_identities += 1;
        let work = Arc::new(Mutex::new(BodyWork::default()));
        PENDING_OWNED.with(|pending| pending.borrow_mut().push((scope.clone(), work.clone())));
        Self { scope, work, hash: None, owner: std::thread::current().id() }
    }
    pub(super) fn verified(&mut self, hash: &str) { self.hash = Some(hash.to_owned()); }
}
impl Drop for PendingOwnedIdentity {
    fn drop(&mut self) {
        PENDING_OWNED.with(|pending| { pending.borrow_mut().pop(); });
        let current = current_scope();
        let changed = self.scope.lock().unwrap().closed || self.owner != std::thread::current().id() || !Arc::ptr_eq(&self.scope, &current);
        let mut scope = self.scope.lock().unwrap();
        scope.pending_owned_identities -= 1;
        if changed { scope.scope_violations += 1; }
        let work = self.work.lock().unwrap();
        match &self.hash {
            Some(hash) => scope.objects.entry(hash.clone()).or_default().owned_work.add(&work),
            None => scope.unattributed_owned.add(&work),
        }
        drop(scope);
        if changed && !Arc::ptr_eq(&self.scope, &current) { current.lock().unwrap().scope_violations += 1; }
    }
}

pub(super) struct OwnedIdentity {
    scope: Arc<Mutex<BodyIo>>,
    hash: String,
    owner: ThreadId,
}
impl OwnedIdentity {
    pub(super) fn new(hash: &str) -> Self {
        let scope = current_scope();
        record(&mut scope.lock().unwrap(), "owned", Some(hash), |work| work.outstanding_readers += 1);
        Self { scope, hash: hash.to_owned(), owner: std::thread::current().id() }
    }
    pub(super) fn check_scope(&self) {
        let current = current_scope();
        if self.scope.lock().unwrap().closed || self.owner != std::thread::current().id() || !Arc::ptr_eq(&self.scope, &current) {
            self.scope.lock().unwrap().scope_violations += 1;
            if !Arc::ptr_eq(&self.scope, &current) { current.lock().unwrap().scope_violations += 1; }
        }
    }
}
impl Drop for OwnedIdentity {
    fn drop(&mut self) {
        self.check_scope();
        record(&mut self.scope.lock().unwrap(), "owned", Some(&self.hash), |work| work.outstanding_readers -= 1);
    }
}

pub(super) fn stat_request() { current_scope().lock().unwrap().stat_requests += 1; }
pub(super) fn batch_stat_request() { current_scope().lock().unwrap().batch_stat_requests += 1; }
pub(super) fn presence_query() { current_scope().lock().unwrap().presence_queries += 1; }

pub(crate) fn open_result<T>(kind: &'static str, result: &io::Result<T>) {
    domain(kind, |work| {
        work.open_attempts += 1;
        if result.is_ok() { work.opens += 1; } else { work.failed_opens += 1; }
    });
}

pub(super) fn identity_metadata_open_result<T>(result: &io::Result<T>) {
    domain("owned", |work| {
        work.identity_metadata_open_attempts += 1;
        if result.is_ok() { work.identity_metadata_opens += 1; }
        else { work.identity_metadata_failed_opens += 1; }
    });
}

pub(super) fn verified_identity_metadata_open() {
    domain("owned", |work| work.verified_identity_metadata_opens += 1);
}

pub(super) fn staging_write_result(result: &io::Result<()>, bytes: usize) {
    domain("owned", |work| {
        work.staging_write_attempts += 1; work.staging_requested_bytes += bytes as u64;
        if result.is_ok() { work.staging_writes += 1; work.staging_written_bytes += bytes as u64; }
        else { work.staging_failed_writes += 1; work.incomplete_writes += 1; }
    });
}

pub(super) fn publication_result(kind: &'static str, result: &io::Result<()>, bytes: u64) {
    domain("owned", |work| {
        work.publication_attempts += 1;
        let detail = work.publication_kinds.entry(kind).or_default(); detail.attempts += 1;
        match result {
            Ok(()) => { work.publications += 1; work.publication_object_bytes += bytes; detail.successes += 1; detail.object_bytes += bytes; }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => { work.publication_already_exists += 1; detail.already_exists += 1; }
            Err(_) => { work.publication_failures += 1; detail.failures += 1; }
        }
    });
}

pub(super) fn body_sha_begin(kind: &'static str, name: &'static str) {
    domain(kind, |work| work.body_sha.entry(name).or_default().calls += 1);
}
pub(super) fn body_sha_update(kind: &'static str, name: &'static str, bytes: usize) {
    domain(kind, |work| work.body_sha.entry(name).or_default().bytes += bytes as u64);
}

pub(super) struct PendingBodyHash { scope: Arc<Mutex<BodyIo>>, work: BodyWork, name: &'static str, hash: Option<String>, owner: ThreadId }
impl PendingBodyHash {
    pub(super) fn new(name: &'static str) -> Self {
        let scope = current_scope();
        let mut work = BodyWork::default(); work.body_sha.entry(name).or_default().calls = 1;
        { let mut sample = scope.lock().unwrap(); sample.pending_body_hashes += 1; sample.domains.entry("managed").or_default().add(&work); }
        Self { scope, work, name, hash: None, owner: std::thread::current().id() }
    }
    pub(super) fn update(&mut self, bytes: usize) {
        self.work.body_sha.entry(self.name).or_default().bytes += bytes as u64;
        self.scope.lock().unwrap().domains.entry("managed").or_default().body_sha.entry(self.name).or_default().bytes += bytes as u64;
    }
    pub(super) fn verified(&mut self, hash: &str) {
        if OBJECT.with(|object| object.borrow().last().is_some_and(|expected| expected == hash)) { self.hash = Some(hash.to_owned()); }
    }
}
impl Drop for PendingBodyHash {
    fn drop(&mut self) {
        let current = current_scope();
        let mut sample = self.scope.lock().unwrap(); sample.pending_body_hashes -= 1;
        let changed = sample.closed || self.owner != std::thread::current().id() || !Arc::ptr_eq(&self.scope, &current);
        if changed { sample.scope_violations += 1; }
        match &self.hash {
            Some(hash) => sample.objects.entry(hash.clone()).or_default().work.add(&self.work),
            None => sample.unattributed_managed.add(&self.work),
        }
        drop(sample);
        if changed && !Arc::ptr_eq(&self.scope,&current) { current.lock().unwrap().scope_violations += 1; }
    }
}

pub(super) fn escaped_handle(kind: &'static str) {
    domain(kind, |work| work.escaped_handles += 1);
}

pub(super) fn escaped_path(kind: &'static str) {
    domain(kind, |work| work.escaped_paths += 1);
}

// fs::read performs one read-open and returns all successfully read bytes.
// Its error does not expose whether opening or a later read failed.
pub(super) fn read_file_result(result: &io::Result<Vec<u8>>) {
    domain("managed", |work| {
        work.open_attempts += 1;
        work.read_operations += 1;
        match result {
            Ok(bytes) => { work.opens += 1; work.read_bytes += bytes.len() as u64; }
            Err(_) => { work.unknown_open_results += 1; work.incomplete_reads += 1; }
        }
    });
}

// Operations count observed Read/read_exact/fs::read boundaries, not syscalls.
pub(super) fn read_result(kind: &'static str, result: &io::Result<usize>) {
    domain(kind, |work| {
        work.read_operations += 1;
        match result {
            Ok(bytes) => work.read_bytes += *bytes as u64,
            Err(_) => work.incomplete_reads += 1,
        }
    });
}

pub(super) fn read_exact_result(kind: &'static str, result: &io::Result<()>, bytes: usize) {
    domain(kind, |work| {
        work.read_operations += 1;
        if result.is_ok() { work.read_bytes += bytes as u64; } else { work.incomplete_reads += 1; }
    });
}

pub(crate) struct TrackedBodyFile {
    file: File,
    hash: String,
    scope: Arc<Mutex<BodyIo>>,
    owner: ThreadId,
}

impl TrackedBodyFile {
    pub(crate) fn new(file: File, hash: &str) -> Self {
        let scope = current_scope();
        record(&mut scope.lock().unwrap(), "managed", Some(hash), |work| work.outstanding_readers += 1);
        Self { file, hash: hash.to_owned(), scope, owner: std::thread::current().id() }
    }
    fn check_scope(&self) {
        let current = current_scope();
        if self.scope.lock().unwrap().closed || self.owner != std::thread::current().id() || !Arc::ptr_eq(&self.scope, &current) {
            self.scope.lock().unwrap().scope_violations += 1;
            if !Arc::ptr_eq(&self.scope, &current) { current.lock().unwrap().scope_violations += 1; }
        }
    }
    pub(crate) fn len(&self) -> io::Result<u64> {
        self.check_scope(); self.file.metadata().map(|metadata| metadata.len())
    }
}

impl Read for TrackedBodyFile {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.check_scope();
        let result = self.file.read(bytes);
        record(&mut self.scope.lock().unwrap(), "managed", Some(&self.hash), |work| {
            work.read_operations += 1;
            match &result {
                Ok(bytes) => work.read_bytes += *bytes as u64,
                Err(_) => work.incomplete_reads += 1,
            }
        });
        result
    }
}

impl Seek for TrackedBodyFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.check_scope(); self.file.seek(position)
    }
}

impl Drop for TrackedBodyFile {
    fn drop(&mut self) {
        self.check_scope();
        record(&mut self.scope.lock().unwrap(), "managed", Some(&self.hash), |work| work.outstanding_readers -= 1);
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delegated_file_read_error_preserves_unknown_outcome() {
        let directory = tempfile::tempdir().unwrap();
        reset_body_io();
        let result = std::fs::read(directory.path().join("missing"));
        assert!(result.is_err());
        read_file_result(&result);
        let work = take_body_io();
        assert_eq!(work.domains["managed"], BodyWork {
            open_attempts: 1, read_operations: 1, unknown_open_results: 1, incomplete_reads: 1,
            ..Default::default()
        });
        assert!(!work.complete());
    }

    #[test]
    fn actual_tracked_read_error_records_failure_without_guessing_partial_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let hash = "a".repeat(64);
        reset_body_io(); register_object_purpose(&hash, BodyPurpose::Control);
        let scope = object_scope(&hash);
        let file = File::create(directory.path().join("write-only")).unwrap();
        open_result("managed", &Ok::<_, io::Error>(&file));
        let mut reader = TrackedBodyFile::new(file, &hash);
        drop(scope);
        assert!(reader.read(&mut [0; 4]).is_err()); drop(reader);
        let work = take_body_io();
        assert!(!work.complete());
        assert_eq!(work.control_work(), BodyWork { open_attempts: 1, opens: 1, read_operations: 1, incomplete_reads: 1, ..Default::default() });
        assert_eq!(work.unknown_work(), BodyWork::default());
    }

    #[test]
    fn unattributed_managed_work_never_disappears_into_a_control_default() {
        reset_body_io(); register_object_purpose(&"a".repeat(64), BodyPurpose::Control);
        open_result("managed", &Err::<File, _>(io::Error::other("synthetic failed open")));
        let work = take_body_io();
        assert!(!work.complete());
        assert_eq!(work.unknown_work(), BodyWork { open_attempts: 1, failed_opens: 1, ..Default::default() });
        assert_eq!(work.control_work(), BodyWork::default());
    }

    #[test]
    fn failed_actual_write_retains_requested_bytes_without_inventing_partial_bytes() {
        use std::io::Write;
        let directory=tempfile::tempdir().unwrap(); let path=directory.path().join("read-only");
        std::fs::write(&path,b"synthetic unchanged").unwrap(); let mut file=File::open(&path).unwrap();
        reset_body_io(); let hash="ad".repeat(32); register_object_purpose(&hash,BodyPurpose::Asset);
        let identity=object_scope(&hash);
        let bytes=b"synthetic attempted write";
        let result=file.write_all(bytes);
        staging_write_result(&result,bytes.len()); assert!(result.is_err()); drop(identity);
        let work=take_body_io(); assert!(!work.complete()); let asset=work.asset_work();
        assert_eq!(asset.staging_write_attempts,1); assert_eq!(asset.staging_failed_writes,1);
        assert_eq!(asset.staging_requested_bytes,bytes.len() as u64); assert_eq!(asset.staging_written_bytes,0);
        assert_eq!(asset.incomplete_writes,1); assert_eq!(asset,work.domains["owned"]);
    }

    #[test]
    fn an_unentered_or_panicking_worker_scope_invalidates_the_parent_receipt() {
        reset_body_io(); drop(capture_body_io_scope()); assert!(!take_body_io().complete());
        reset_body_io(); let scope=capture_body_io_scope();
        assert!(std::thread::spawn(move ||with_body_io_scope(scope,||panic!("synthetic worker failure"))).join().is_err());
        let work=take_body_io(); assert!(!work.complete()); assert!(work.scope_violations>0);
        assert_eq!(work.pending_worker_scopes,0); assert_eq!(work.worker_scopes_started,work.worker_scopes_settled);
    }

    #[test]
    fn actual_failed_link_records_the_attempt_and_keeps_unknown_roles_incomplete() {
        let directory=tempfile::tempdir().unwrap();
        reset_body_io();
        let source=directory.path().join("absent"); let destination=directory.path().join("destination");
        let result=std::fs::hard_link(&source,&destination); assert!(result.is_err());
        publication_result("hard-link",&result,0);
        let work=take_body_io(); assert!(!work.complete());
        assert_eq!(work.unattributed_owned.publication_attempts,1);
        assert_eq!(work.unattributed_owned.publication_failures,1);
        assert_eq!(work.unattributed_owned.publications,0);
        assert_eq!(work.unattributed_owned.publication_object_bytes,0);
        assert_eq!(work.unattributed_owned.publication_kinds["hard-link"].failures,1);
        assert_eq!(work.unattributed_owned,work.domains["owned"]);
    }
}
