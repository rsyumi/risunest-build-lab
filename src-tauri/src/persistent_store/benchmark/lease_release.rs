//! Isolated release measurements. SQL tracing observes requests, not filesystem syncs.
use super::*;
use std::{ffi::{c_int, c_uint, c_void, CStr}, sync::Mutex};

const COMMITS: usize = 1_000;
const EXTERNAL_BATCH: usize = 50;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckpointEvent {
    mode: String,
    busy: Option<i64>,
    log_frames: Option<i64>,
    checkpointed_frames: Option<i64>,
}

struct Trace {
    database: *mut ffi::sqlite3,
    events: Box<Mutex<Vec<CheckpointEvent>>>,
}
impl Trace {
    fn install(connection: &Connection) -> Self {
        let mut events = Box::new(Mutex::new(Vec::new()));
        // This test owns the connection. The box stays put, and Drop unregisters
        // the callback before the store is dropped, including during unwinding.
        let database = unsafe { connection.handle() };
        let result = unsafe { ffi::sqlite3_trace_v2(database,
            (ffi::SQLITE_TRACE_STMT | ffi::SQLITE_TRACE_ROW) as c_uint,
            Some(checkpoint_trace), (&mut *events as *mut Mutex<Vec<CheckpointEvent>>).cast()) };
        assert_eq!(result, ffi::SQLITE_OK);
        Self { database, events }
    }
    fn take(&self) -> Vec<CheckpointEvent> {
        std::mem::take(&mut *self.events.lock().expect("checkpoint trace lock"))
    }
}
impl Drop for Trace {
    fn drop(&mut self) { unsafe { ffi::sqlite3_trace_v2(self.database, 0, None, std::ptr::null_mut()); } }
}

unsafe extern "C" fn checkpoint_trace(event: c_uint, context: *mut c_void, statement: *mut c_void, _: *mut c_void) -> c_int {
    // No unwinding may cross SQLite's C callback boundary.
    let _ = std::panic::catch_unwind(|| {
        let statement = statement.cast::<ffi::sqlite3_stmt>();
        let sql = unsafe { ffi::sqlite3_sql(statement) };
        if sql.is_null() { return; }
        let sql = unsafe { CStr::from_ptr(sql) }.to_bytes();
        let Some(mode) = sql.strip_prefix(b"PRAGMA wal_checkpoint(").and_then(|rest| rest.strip_suffix(b")")) else { return; };
        let mode = match mode { b"TRUNCATE" => "truncate", b"PASSIVE" => "passive", _ => return };
        let events = unsafe { &*context.cast::<Mutex<Vec<CheckpointEvent>>>() };
        let Ok(mut events) = events.lock() else { return; };
        if event == ffi::SQLITE_TRACE_STMT as c_uint {
            events.push(CheckpointEvent { mode: mode.into(), busy: None, log_frames: None, checkpointed_frames: None });
        } else if event == ffi::SQLITE_TRACE_ROW as c_uint {
            if let Some(last) = events.last_mut() {
                last.busy = Some(unsafe { ffi::sqlite3_column_int64(statement, 0) });
                last.log_frames = Some(unsafe { ffi::sqlite3_column_int64(statement, 1) });
                last.checkpointed_frames = Some(unsafe { ffi::sqlite3_column_int64(statement, 2) });
            }
        }
    });
    0
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseSample {
    release_us: u64,
    wal_before_bytes: u64,
    wal_after_bytes: u64,
    checkpoint_events: Vec<CheckpointEvent>,
}

fn mutate(store: &mut PersistentStore, run: usize) -> u64 {
    let input: WorkingSetCommit = serde_json::from_value(json!({
        "expectedRevision":store.revision().unwrap(),
        "pluginStorage":[{"type":"set","owner":"lease-release-benchmark","key":"value","value":format!("{run}:{}", "s".repeat(256))}]
    })).unwrap();
    let started = Instant::now();
    let result = store.commit(&input).expect("synthetic small commit");
    let duration = elapsed_us(started);
    assert_eq!(result.revision, input.expected_revision + 1);
    duration
}

fn fixture() -> (tempfile::TempDir, PersistentStore) {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let database = contract_fixture();
    let staging = store.replace_begin().unwrap();
    store.replace_put_root(&staging.staging_id, &root_without_characters(&database)).unwrap();
    store.replace_put_presets(&staging.staging_id, database["botPresets"].as_array().unwrap()).unwrap();
    stage_in_public_batches(&mut store, &staging.staging_id, database["characters"].as_array().unwrap());
    store.replace_commit(&staging.staging_id, Some(0)).unwrap();
    mutate(&mut store, usize::MAX);
    store.checkpoint(CheckpointMode::Truncate).unwrap();
    (directory, store)
}

fn measure_release(store: &mut PersistentStore, lease: &str, trace: Option<&Trace>) -> ReleaseSample {
    if let Some(trace) = trace { trace.take(); }
    let wal_before_bytes = wal_bytes(store);
    let started = Instant::now();
    store.release_revision(lease).expect("release synthetic revision");
    let release_us = elapsed_us(started);
    ReleaseSample { release_us, wal_before_bytes, wal_after_bytes: wal_bytes(store),
        checkpoint_events: trace.map(Trace::take).unwrap_or_default() }
}

fn run_case(name: &str, traced: bool) -> Value {
    let (_directory, mut store) = fixture();
    let page_size = u64::try_from(store.connection.query_row("PRAGMA page_size", [], |row| row.get::<_, i64>(0)).unwrap()).expect("synthetic page size must be nonnegative");
    let autocheckpoint_pages = u64::try_from(store.connection.query_row("PRAGMA wal_autocheckpoint", [], |row| row.get::<_, i64>(0)).unwrap()).expect("synthetic autocheckpoint pages must be nonnegative");
    let busy_timeout_ms = u64::try_from(store.connection.query_row("PRAGMA busy_timeout", [], |row| row.get::<_, i64>(0)).unwrap()).expect("synthetic busy timeout must be nonnegative");
    let journal_size_limit: i64 = store.connection.query_row("PRAGMA journal_size_limit", [], |row| row.get(0)).unwrap();
    let start_revision = store.revision().unwrap();
    let anchor = (name == "registered-reader-growing-wal").then(|| store.acquire_revision(start_revision).unwrap());
    let trace = traced.then(|| Trace::install(&store.connection));
    let mut commits = Vec::new();
    let mut samples = Vec::new();
    let mut peak_wal = wal_bytes(&store);
    let mut external = None;
    let mut batch_lease = None;
    for run in 0..COMMITS {
        if name == "external-reader" && run % EXTERNAL_BATCH == 0 {
            let reader = crate::sqlite_open::open_with_flags(&store.database_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            reader.execute_batch("BEGIN").unwrap();
            let _: i64 = reader.query_row("SELECT count(*) FROM characters", [], |row| row.get(0)).unwrap();
            external = Some(reader);
            batch_lease = Some(store.acquire_revision(store.revision().unwrap()).unwrap());
        }
        if name == "external-reader" {
            commits.push(mutate(&mut store, run));
            peak_wal = peak_wal.max(wal_bytes(&store));
            if (run + 1) % EXTERNAL_BATCH == 0 {
                let lease = batch_lease.take().unwrap();
                samples.push(measure_release(&mut store, &lease.lease, trace.as_ref()));
                external.take().unwrap().execute_batch("ROLLBACK").unwrap();
                // Keep each blocked-reader trial independent. This explicit
                // cleanup is outside the release interval and trace samples.
                store.checkpoint(CheckpointMode::Truncate).unwrap();
            }
            continue;
        }
        let lease = store.acquire_revision(store.revision().unwrap()).unwrap();
        if name != "empty-wal" { commits.push(mutate(&mut store, run)); }
        store.read_root(Some(&lease.lease)).unwrap();
        peak_wal = peak_wal.max(wal_bytes(&store));
        samples.push(measure_release(&mut store, &lease.lease, trace.as_ref()));
    }
    assert_eq!(store.revision().unwrap() - start_revision, commits.len() as i64);
    let requests = samples.iter().map(|sample| sample.checkpoint_events.len()).sum::<usize>();
    let busy = samples.iter().flat_map(|sample| &sample.checkpoint_events).filter(|event| event.busy == Some(1)).count();
    let inferred_candidates = samples.iter().filter(|sample| sample.wal_before_bytes > 0 &&
        sample.checkpoint_events.iter().any(|event| event.mode == "truncate" && event.busy == Some(0))).count();
    let result = json!({"scenario":name,"traced":traced,"commits":commits.len(),"releases":samples.len(),
        "pageSize":page_size,"autocheckpointPages":autocheckpoint_pages,"writerBusyTimeoutMs":busy_timeout_ms,
        "journalSizeLimitBytes":journal_size_limit,"exceededAutocheckpointWalBytes":peak_wal > 32 + autocheckpoint_pages * (page_size + 24),
        "autocheckpointWalBytes":32 + autocheckpoint_pages * (page_size + 24),"peakObservedWalBytes":peak_wal,
        "releaseUs":nearest_rank(&samples.iter().map(|sample|sample.release_us).collect::<Vec<_>>()),
        "commitUs":(!commits.is_empty()).then(|| nearest_rank(&commits)),
        "checkpointSqlRequests":traced.then_some(requests),"busyCheckpointResults":traced.then_some(busy),
        "inferredBackfillCandidates":traced.then_some(inferred_candidates),"samples":samples});
    drop(trace);
    if let Some(anchor) = anchor { store.release_revision(&anchor.lease).unwrap(); }
    result
}

#[test]
#[ignore = "release-only checkpoint attribution and latency measurement; external-reader case preserves the five-second timeout"]
fn lease_release_checkpoint_measurements() {
    assert!(!cfg!(debug_assertions), "run this measurement with --release");
    let mut cases = Vec::new();
    for name in ["empty-wal", "commit-read-release", "registered-reader-growing-wal", "external-reader"] {
        for traced in [false, true] { cases.push(run_case(name, traced)); }
    }
    let result = json!({"schemaVersion":1,"benchmark":"lease-release-checkpoints","synthetic":true,
        "sourceRevision":std::env::var("RISUNEST_LEASE_RELEASE_REVISION").ok(),
        "sqliteVersion":rusqlite::version(),"os":std::env::consts::OS,"arch":std::env::consts::ARCH,
        "latencyEvidence":"Untraced direct store calls; excludes IPC and lock-queue time. Traced pass is attribution only.",
        "countEvidence":"SQLite trace_v2 STMT and ROW on release checkpoint PRAGMAs. Autocheckpoints inside commit are not counted.",
        "backfillEvidence":"inferredBackfillCandidates counts successful TRUNCATE requests with a nonempty prior WAL. This is not a measured backfilling count: those frames may already be checkpointed. Returned frame counters are cumulative checkpoint results, not physical writes or fsyncs.",
        "externalReaderEvidence":"1000 commits in 20 batches, each holds an unregistered older snapshot through release; default writer timeout unchanged.",
        "cases":cases});
    let encoded=serde_json::to_string(&result).unwrap();
    if let Ok(path)=std::env::var("RISUNEST_LEASE_RELEASE_OUTPUT") { std::fs::write(path,&encoded).unwrap(); }
    println!("{encoded}");
}

#[test]
fn trace_counts_statements_and_rows_including_an_empty_checkpoint() {
    let (_directory, store)=fixture();
    let trace=Trace::install(&store.connection);
    store.checkpoint(CheckpointMode::Truncate).unwrap();
    let events=trace.take();
    assert_eq!(events.len(),1);
    assert_eq!(events[0].mode,"truncate");
    assert_eq!(events[0].busy,Some(0));
    assert_eq!(events[0].log_frames,Some(0));
    assert_eq!(events[0].checkpointed_frames,Some(0));
    assert_eq!(wal_bytes(&store),0);
    store.connection.query_row("SELECT 1", [], |row| row.get::<_, i64>(0)).unwrap();
    assert!(trace.take().is_empty(), "ordinary SQL must not become a checkpoint count");
    store.checkpoint(CheckpointMode::Passive).unwrap();
    let events=trace.take();
    assert_eq!(events.len(),1);
    assert_eq!(events[0].mode,"passive");
    assert_eq!(events[0].busy,Some(0));
}
