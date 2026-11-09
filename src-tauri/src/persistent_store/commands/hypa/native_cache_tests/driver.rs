use super::*;
use super::contracts::*;
use risunest_ir::stage::{EmbedRequest, HostReply, HostRequest, Step};
use risunest_memory::{hanurai::{Retrieval, RetrievalOutput}, hypa_v1::*};
use std::sync::{atomic::{AtomicBool, AtomicUsize, Ordering}, Condvar, Mutex};
use std::time::Duration;

struct Service {
    requests: Arc<Mutex<Vec<(EmbeddingPurpose, usize)>>>,
    pending: Option<EmbeddingBatch>,
    document_count: Option<usize>,
    yielding: bool,
}
impl Service {
    fn new() -> Self { Self { requests: Arc::new(Mutex::new(Vec::new())), pending: None, document_count: None, yielding: true } }
}
impl EmbeddingService for Service {
    fn embed(&mut self, request: EmbeddingRequest, b: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        let length = match &request.input { EmbeddingInput::Scalar(_) => 1, EmbeddingInput::Batch(values) => values.as_slice().len() };
        self.requests.lock().unwrap().push((request.purpose, length));
        let length = if request.purpose == EmbeddingPurpose::Document { self.document_count.unwrap_or(length) } else { length };
        let mut output = OwnedVec::new(b)?;
        for index in 0..length {
            let mut vector = OwnedVec::new(b)?;
            vector.extend(&[if request.purpose == EmbeddingPurpose::Query { 1.0 } else { index as f64 + 1.0 }])?;
            output.push(Some(EmbeddingVector::F64(vector)))?;
        }
        if !self.yielding { return Ok(Step::Done(output)); }
        self.pending = Some(output);
        Ok(Step::NeedsHost(HostRequest::Embed(EmbedRequest { model: text("synthetic-native-cache-leaf", b), texts: Vec::new() })))
    }
    fn resume(&mut self, reply: HostReply, _: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        match reply {
            HostReply::Embeddings(_) => Ok(Step::Done(self.pending.take().expect("pending synthetic embeddings"))),
            HostReply::Failed { message } => { self.pending = None; Err(StageError::Host(message.units().collect())) }
            HostReply::Cancelled => { self.pending = None; Err(StageError::Cancelled) }
            _ => Err(StageError::Invalid("synthetic-native-cache-reply".into())),
        }
    }
}
type Session = HypaV1Retrieval<NativeCache, Service>;
enum Action { Prepare(Vec<Text>), Query(Text), Resume(HostReply) }

async fn worker_step(mut session: Session, action: Action, budget: Budget) -> Result<(Session, Result<Step<RetrievalOutput>, StageError>), StageError> {
    // Cancellation is a budget signal, not a reason to drop this still-owned worker.
    let handle = tokio::task::spawn_blocking(move || {
        let result = match action {
            Action::Prepare(documents) => session.prepare(documents, &budget),
            Action::Query(query) => session.query(query, &budget),
            Action::Resume(reply) => session.resume(reply, &budget),
        };
        (session, result)
    });
    handle.await.map_err(|_| StageError::Invalid("native-cache-worker-join".into()))
}
async fn complete(mut session: Session, action: Action, b: &Budget, fixture: &Fixture) -> (Session, Result<RetrievalOutput, StageError>) {
    let (next, mut step) = worker_step(session, action, b.clone()).await.unwrap();
    session = next;
    loop {
        match step {
            Err(error) => return (session, Err(error)),
            Ok(Step::Done(output)) => return (session, Ok(output)),
            Ok(Step::NeedsHost(HostRequest::Embed(request))) => {
                assert_eq!(request.model.as_unicode(), Some("synthetic-native-cache-leaf"));
                // Maintenance and another real admitted store operation both work across the yield.
                assert_eq!(fixture.state.renderer_gate.state.lock().unwrap().operations, 0, "cache admission crossed an embedding yield");
                drop(fixture.state.try_acquire_renderer_maintenance(false).unwrap().unwrap());
                fixture.store(|store| { store.device_store_mut().unwrap().write_setting("accountst", &serde_json::json!(true)).unwrap(); });
                let (next, resumed) = worker_step(session, Action::Resume(HostReply::Embeddings(Vec::new())), b.clone()).await.unwrap();
                session = next;
                step = resumed;
            }
            _ => panic!("unexpected embedding leaf"),
        }
    }
}
fn runtime() -> tokio::runtime::Runtime { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap() }
fn documents(b: &Budget, count: usize) -> Vec<Text> { (0..count).map(|index| text(&format!("document {index}"), b)).collect() }
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !condition() { tokio::task::yield_now().await; }
    }).await.expect("deterministic worker boundary");
}
#[derive(Default)]
struct Gate { entered: AtomicBool, released: Mutex<bool>, wake: Condvar }
impl Gate {
    fn hold(&self) {
        self.entered.store(true, Ordering::SeqCst);
        let mut released = self.released.lock().unwrap();
        while !*released { released = self.wake.wait(released).unwrap(); }
    }
    fn release(&self) { *self.released.lock().unwrap() = true; self.wake.notify_all(); }
}
struct ReleaseGate(Arc<Gate>);
impl Drop for ReleaseGate { fn drop(&mut self) { self.0.release(); } }

#[test]
fn cold_and_warm_retrieval_release_store_admission_across_embedding_yields() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let b = ample();
        let service = Service::new();
        let requests = service.requests.clone();
        let stage = HypaV1Retrieval::new(target(&b), fixture.cache(), service, 1).unwrap();
        let (stage, result) = complete(stage, Action::Prepare(documents(&b, 2)), &b, &fixture).await;
        assert!(matches!(result, Ok(RetrievalOutput::Prepared)));
        assert_eq!(fixture.notifications.load(Ordering::SeqCst), 2);
        let (_, result) = complete(stage, Action::Query(text("query", &b)), &b, &fixture).await;
        let RetrievalOutput::Scored(scores) = result.unwrap() else { panic!() };
        assert_eq!(scores.iter().map(|row| row.score).collect::<Vec<_>>(), [2.0, 1.0]);
        assert_eq!(*requests.lock().unwrap(), [(EmbeddingPurpose::Document, 2), (EmbeddingPurpose::Query, 1)]);

        let service = Service::new();
        let requests = service.requests.clone();
        let stage = HypaV1Retrieval::new(target(&b), fixture.cache(), service, 1).unwrap();
        let (stage, result) = complete(stage, Action::Prepare(documents(&b, 2)), &b, &fixture).await;
        assert!(matches!(result, Ok(RetrievalOutput::Prepared)));
        assert!(requests.lock().unwrap().is_empty());
        let (_, result) = complete(stage, Action::Query(text("query", &b)), &b, &fixture).await;
        assert!(matches!(result, Ok(RetrievalOutput::Scored(_))));
        assert_eq!(*requests.lock().unwrap(), [(EmbeddingPurpose::Query, 1)]);
        assert_eq!(fixture.notifications.load(Ordering::SeqCst), 2);
    });
}

#[test]
fn later_read_failure_discards_earlier_hits_before_fresh_embedding() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let b = ample();
        let target = target(&b);
        let docs = documents(&b, 2);
        fixture.raw(target.cache_key(&docs[0], &b).unwrap(), 1, &99f32.to_bits().to_le_bytes(), false);
        fixture.raw(target.cache_key(&docs[1], &b).unwrap(), 1, &[1,2,3,4,5], false);
        let service = Service::new();
        let requests = service.requests.clone();
        let stage = HypaV1Retrieval::new(target, fixture.cache(), service, 1).unwrap();
        let (stage, result) = complete(stage, Action::Prepare(docs), &b, &fixture).await;
        assert!(matches!(result, Ok(RetrievalOutput::Prepared)));
        assert_eq!(*requests.lock().unwrap(), [(EmbeddingPurpose::Document, 2)]);
        let (_, result) = complete(stage, Action::Query(text("query", &b)), &b, &fixture).await;
        let RetrievalOutput::Scored(rows) = result.unwrap() else { panic!() };
        assert_eq!(rows.iter().map(|row| row.score).collect::<Vec<_>>(), [2.0,1.0]);
    });
}

#[test]
fn later_sql_failure_or_missing_key_keeps_earlier_batches_and_fresh_retrieval() {
    for sql_failure in [true, false] {
        runtime().block_on(async {
            let fixture = Fixture::new();
            let b = ample();
            let target = target(&b);
            let docs = documents(&b, if sql_failure { 5 } else { 3 });
            let mut cache = fixture.cache();
            if sql_failure {
                let bad = key_name(target.cache_key(&docs[3], &b).unwrap());
                cache.opened = Some(Arc::new(move |store| { store.device_store().unwrap().connection().execute_batch(&format!(
                    "CREATE TEMP TRIGGER synthetic_hypa_failure BEFORE INSERT ON main.hypa_embeddings WHEN NEW.cache_key='{bad}' BEGIN SELECT RAISE(ABORT,'synthetic cache write failure'); END;")).unwrap(); }));
            }
            let mut service = Service::new();
            if !sql_failure { service.document_count = Some(4); }
            let stage = HypaV1Retrieval::new(target, cache, service, 2).unwrap();
            let (stage, result) = complete(stage, Action::Prepare(docs), &b, &fixture).await;
            assert!(matches!(result, Ok(RetrievalOutput::Prepared)));
            let snapshot = fixture.snapshot();
            assert_eq!(snapshot["rows"].as_array().unwrap().len(), 2);
            assert_eq!(snapshot["units"].as_array().unwrap().len(), 2);
            assert_eq!(snapshot["outbox"].as_array().unwrap().len(), 2);
            assert_eq!(fixture.notifications.load(Ordering::SeqCst), 1);
            let (stage, result) = complete(stage, Action::Query(text("query", &b)), &b, &fixture).await;
            let RetrievalOutput::Scored(rows) = result.unwrap() else { panic!() };
            assert_eq!(rows.len(), if sql_failure { 5 } else { 4 });
            let (stage, step) = worker_step(stage, Action::Query(text("failed query", &b)), b.clone()).await.unwrap();
            assert!(matches!(step, Ok(Step::NeedsHost(_))));
            let (_, failed) = worker_step(stage, Action::Resume(HostReply::Failed { message: text("synthetic query failure", &b) }), b.clone()).await.unwrap();
            assert!(matches!(failed, Err(StageError::Host(_))));
            // Yield-side setting writes may advance revision; query failure does not change the cache.
            assert_eq!(fixture.snapshot()["rows"], snapshot["rows"]);
            assert_eq!(fixture.snapshot()["outbox"], snapshot["outbox"]);
            assert_eq!(fixture.notifications.load(Ordering::SeqCst), 1);
        });
    }
}

#[test]
fn reset_wait_stays_off_executor_and_cancellation_waits_for_worker_settlement() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let cancel = Arc::new(AtomicBool::new(false));
        let b = budget(32 << 20, 10_000_000, cancel.clone());
        let stage = HypaV1Retrieval::new(target(&b), fixture.cache(), Service::new(), 1).unwrap();
        let reset = fixture.state.try_acquire_renderer_maintenance(false).unwrap().unwrap();
        let mut pending = Box::pin(worker_step(stage, Action::Prepare(documents(&b, 1)), b.clone()));
        tokio::select! { _ = &mut pending => panic!("reset did not hold the cache"), _ = until(|| fixture.state.native_job_waiting_for_reset()) => {} }
        let progressed = Arc::new(AtomicBool::new(false));
        let marker = progressed.clone();
        tokio::spawn(async move { marker.store(true, Ordering::SeqCst); }).await.unwrap();
        assert!(progressed.load(Ordering::SeqCst));
        cancel.store(true, Ordering::SeqCst);
        tokio::select! { _ = &mut pending => panic!("reported cancellation before admitted worker settled"), _ = tokio::task::yield_now() => {} }
        assert!(fixture.state.native_job_waiting_for_reset());
        drop(reset);
        let (stage, result) = pending.await.unwrap();
        assert!(matches!(result, Err(StageError::Cancelled)));
        drop(stage);
        assert_eq!(b.owned_bytes(), Budget::context_bytes());
        assert_eq!(fixture.snapshot()["revision"], 0);
        assert_eq!(fixture.notifications.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.state.renderer_gate.state.lock().unwrap().operations, 0);
    });
}

static SQLITE_BUSY: AtomicBool = AtomicBool::new(false);
#[test]
fn sqlite_wait_settles_committed_write_and_notification_before_cancellation_result() {
    runtime().block_on(async {
        SQLITE_BUSY.store(false, Ordering::SeqCst);
        let fixture = Fixture::new();
        let cancel = Arc::new(AtomicBool::new(false));
        let b = budget(32 << 20, 10_000_000, cancel.clone());
        let gate = Arc::new(Gate::default());
        let _release = ReleaseGate(gate.clone());
        let held_gate = gate.clone();
        let opened = Arc::new(AtomicUsize::new(0));
        let mut cache = fixture.cache();
        cache.opened = Some(Arc::new(move |store| {
            if opened.fetch_add(1, Ordering::SeqCst) == 1 {
                store.device_store().unwrap().connection().busy_handler(Some(|_| {
                    SQLITE_BUSY.store(true, Ordering::SeqCst);
                    std::thread::yield_now();
                    true
                })).unwrap();
                held_gate.hold();
            }
        }));
        let mut service = Service::new();
        service.yielding = false;
        let stage = HypaV1Retrieval::new(target(&b), cache, service, 1).unwrap();
        let mut pending = Box::pin(worker_step(stage, Action::Prepare(documents(&b, 1)), b.clone()));
        tokio::select! { _ = &mut pending => panic!("write boundary not held"), _ = until(|| gate.entered.load(Ordering::SeqCst)) => {} }
        let path = fixture.store(|store| store.device_store().unwrap().connection().path().unwrap().to_owned());
        let blocker = crate::sqlite_open::open(path).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        gate.release();
        tokio::select! { _ = &mut pending => panic!("SQLite write was not blocked"), _ = until(|| SQLITE_BUSY.load(Ordering::SeqCst)) => {} }
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(fixture.notifications.load(Ordering::SeqCst), 0);
        tokio::select! { _ = &mut pending => panic!("cancelled worker detached before SQLite settled"), _ = tokio::task::yield_now() => {} }
        blocker.execute_batch("ROLLBACK").unwrap();
        let (stage, result) = pending.await.unwrap();
        assert!(matches!(result, Err(StageError::Cancelled)));
        drop(stage);
        assert_eq!(b.owned_bytes(), Budget::context_bytes());
        assert_eq!(fixture.snapshot()["rows"].as_array().unwrap().len(), 1);
        assert_eq!(fixture.notifications.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn device_maintenance_refusal_remains_an_ordinary_cache_boundary() {
    let fixture = Fixture::new();
    let maintenance = fixture.state.acquire_raw_capture().unwrap();
    assert!(matches!(fixture.cache().read_batch(&[key(1)], &ample()), Err(CacheError::Unavailable)));
    drop(maintenance);
    let rows = cache_ok(fixture.cache().read_batch(&[key(1)], &ample()));
    assert!(rows.as_slice()[0].is_none());
}

#[test]
fn mixed_cache_embeds_only_the_missing_document_and_keeps_cached_scores() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let b = ample();
        let target = target(&b);
        let docs = documents(&b, 2);
        fixture.raw(target.cache_key(&docs[0], &b).unwrap(), 1, &99f32.to_bits().to_le_bytes(), false);
        let service = Service::new();
        let requests = service.requests.clone();
        let stage = HypaV1Retrieval::new(target, fixture.cache(), service, 1).unwrap();
        let (stage, result) = complete(stage, Action::Prepare(docs), &b, &fixture).await;
        assert!(matches!(result, Ok(RetrievalOutput::Prepared)));
        assert_eq!(*requests.lock().unwrap(), [(EmbeddingPurpose::Document, 1)]);
        let (_, result) = complete(stage, Action::Query(text("query", &b)), &b, &fixture).await;
        let RetrievalOutput::Scored(rows) = result.unwrap() else { panic!() };
        assert_eq!(rows.iter().map(|row| (row.document.as_ref().unwrap().as_unicode().unwrap(), row.score)).collect::<Vec<_>>(),
            [("document 0",99.0),("document 1",1.0)]);
        assert_eq!(fixture.notifications.load(Ordering::SeqCst), 1);
    });
}
