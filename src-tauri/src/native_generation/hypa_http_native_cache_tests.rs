use super::*;
use crate::persistent_store::{PersistentStore, commands::{PersistentStoreState, composition_cache, active_cache_segments}};
use std::sync::atomic::AtomicBool;
use tauri::{Listener, test::MockRuntime};

type Opened = Arc<dyn Fn(&mut PersistentStore) + Send + Sync>;
type Committed = Arc<dyn Fn() + Send + Sync>;

struct StoreFixture {
    directory: tempfile::TempDir,
    state: Arc<PersistentStoreState>,
    notifications: Arc<AtomicUsize>,
    notify: Arc<dyn Fn() + Send + Sync>,
    _app: tauri::App<MockRuntime>,
}
impl StoreFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(PersistentStoreState::with_test_store(PersistentStore::open(directory.path()).unwrap()));
        let app = tauri::test::mock_builder().build(tauri::test::mock_context(tauri::test::noop_assets())).unwrap();
        let notifications = Arc::new(AtomicUsize::new(0));
        let count = notifications.clone();
        let observed = state.clone();
        app.listen(crate::server_sync::events::DEVICE_CHANGED_EVENT, move |_| {
            assert_eq!(active_cache_segments(&observed).unwrap(), 0, "notification retained store admission");
            let admitted = observed.admit_native_job_operation().unwrap();
            let store = observed.open_admitted_native_job_store(&admitted).unwrap();
            assert!(store.device_store().unwrap().revision().unwrap() > 0);
            count.fetch_add(1, Ordering::SeqCst);
        });
        let handle = app.handle().clone();
        let notify = Arc::new(move || crate::server_sync::events::notify_device_changed(&handle));
        Self { directory, state, notifications, notify, _app: app }
    }
    fn cache(&self, opened: Option<Opened>, committed: Option<Committed>) -> impl ImmediateCache + Send + 'static {
        composition_cache(self.state.clone(), self.notify.clone(), opened, committed)
    }
    fn store<T>(&self, action: impl FnOnce(&mut PersistentStore) -> T) -> T {
        let admitted = self.state.admit_native_job_operation().unwrap();
        let mut store = self.state.open_admitted_native_job_store(&admitted).unwrap();
        action(&mut store)
    }
    fn snapshot(&self) -> Value {
        self.store(|store| {
            let device = store.device_store().unwrap();
            let rows = |sql| device.connection().prepare(sql).unwrap().query_map([], |row| row.get::<_, String>(0)).unwrap()
                .map(|row| serde_json::from_str::<Value>(&row.unwrap()).unwrap()).collect::<Vec<_>>();
            json!({
                "revision": device.revision().unwrap(),
                "rows": rows("SELECT json_array(cache_key,producer,model,endpoint,preprocess_version,dimensions,hex(vector),tombstone) FROM hypa_embeddings ORDER BY cache_key"),
                "units": rows("SELECT json_array(key,stamp,value,version,identity) FROM lww_units ORDER BY key"),
                "outbox": rows("SELECT json_array(key,stamp,value,version,identity,authority) FROM lww_outbox ORDER BY key"),
            })
        })
    }
    fn reopen(&self) {
        assert_eq!(active_cache_segments(&self.state).unwrap(), 0);
        let maintenance = self.state.acquire_device_maintenance().unwrap();
        self.state.reopen_under_maintenance(&maintenance, self.directory.path()).unwrap();
        drop(maintenance);
    }
}

fn target(url: &str, budget: &Budget) -> EmbeddingTarget {
    EmbeddingTarget::capture(&Text::from_str("custom", budget).unwrap(), None,
        &Text::from_str(url, budget).unwrap(), &Text::from_str("old-cache-identity", budget).unwrap(), budget).unwrap()
}
fn owner<C: ImmediateCache + Send + 'static, F: Fn(&Budget) -> Result<CapturedConfig, Failure> + Send + 'static>(
    url: &str, cache: C, capture: F, operation: Operation, batch: usize,
) -> NativeRetrieval<HypaV1Retrieval<C, Adapter<F>>> {
    let diagnostic = SharedFailure::new(&operation).unwrap();
    let candidates = CandidateCount::new(&operation).unwrap();
    let adapter = Adapter::new(capture, diagnostic.clone(), candidates.clone());
    let retrieval = HypaV1Retrieval::new(target(url, operation.budget()), cache, adapter, batch).unwrap();
    NativeRetrieval::new(retrieval, Executor::new(operation, limits(), ORIGIN.into()).unwrap(), diagnostic, candidates)
}
fn defaults() -> Arc<Mutex<Settings>> { Arc::new(Mutex::new(Settings::default())) }
fn documents(budget: &Budget, count: usize) -> Vec<Text> {
    (0..count).map(|n| Text::from_str(&format!("document {n}"), budget).unwrap()).collect()
}
fn scores(output: &Observed<Result<RetrievalResult, StageError>>) -> Vec<(String, u64)> {
    let RetrievalOutput::Scored(rows) = &output.value.as_ref().unwrap().output else { panic!("expected scores") };
    rows.iter().map(|row| (row.document.as_ref().unwrap().as_unicode().unwrap().to_owned(), row.score.to_bits())).collect()
}
async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !ready() { tokio::task::yield_now().await; }
    }).await.expect("owned worker condition");
}
async fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}
#[derive(Default)]
struct Gate {
    entered: AtomicBool,
    released: Mutex<bool>,
    wake: Condvar,
}
impl Gate {
    fn hold(&self) {
        self.entered.store(true, Ordering::SeqCst);
        let released = self.released.lock().unwrap();
        let (released, timeout) = self.wake.wait_timeout_while(released, Duration::from_secs(10), |released| !*released).unwrap();
        assert!(!timeout.timed_out() && *released, "synthetic gate was not released");
    }
    fn release(&self) { *self.released.lock().unwrap() = true; self.wake.notify_all(); }
}
struct Release(Arc<Gate>);
impl Drop for Release { fn drop(&mut self) { self.0.release(); } }

#[tokio::test]
async fn real_store_cold_reopened_warm_and_mixed_duplicates_preserve_bits_identity_and_effects() {
    let fixture = StoreFixture::new();
    let mut held = Reply::plain(200, &vectors(50)); held.held_headers = true;
    let mut server = Loopback::new(vec![held, Reply::plain(200, &vectors(1)), Reply::plain(200, &vectors(1)),
        Reply::plain(200, &vectors(1)), Reply::plain(200, &vectors(1)), Reply::plain(200, &vectors(1))]).await;
    let url = format!("{}/v1", server.url);
    let settings = Arc::new(Mutex::new(Settings { model: Some("first-model".into()), key: None }));
    let (operation, budget, slots) = operation();
    let mut retrieval = owner(&url, fixture.cache(None, None), capture(settings.clone()), operation, 7);
    let docs = documents(&budget, 51);
    let mut expected_keys = docs.iter().map(|doc| String::from_utf8(target(&url, &budget).cache_key(doc, &budget).unwrap().hex().to_vec()).unwrap()).collect::<Vec<_>>();
    expected_keys.sort();
    let mut preparing = Box::pin(retrieval.prepare(docs));
    let first = tokio::select! { request = server.captured() => request, _ = &mut preparing => panic!("held HTTP completed") };
    assert_eq!(active_cache_segments(&fixture.state).unwrap(), 0, "cache segment crossed HTTP suspension");
    let state = fixture.state.clone();
    tokio::task::spawn_blocking(move || {
        drop(state.acquire_raw_capture().unwrap());
        let admitted = state.admit_native_job_operation().unwrap();
        state.open_admitted_native_job_store(&admitted).unwrap().device_store_mut().unwrap().write_setting("accountst", &json!(true)).unwrap();
    }).await.unwrap();
    *settings.lock().unwrap() = Settings { model: Some("second-model".into()), key: Some("second-key".into()) };
    server.release.notify_one();
    let prepared = preparing.await;
    assert!(prepared.value.is_ok(), "{:?}", prepared.value.as_ref().err()); drop(prepared);
    let second = server.captured().await;
    assert_eq!(serde_json::from_slice::<Value>(&first.body).unwrap()["input"].as_array().unwrap().len(), 50);
    assert_eq!(serde_json::from_slice::<Value>(&second.body).unwrap()["input"].as_array().unwrap().len(), 1);
    assert_eq!(serde_json::from_slice::<Value>(&first.body).unwrap()["model"], "first-model");
    assert_eq!(serde_json::from_slice::<Value>(&second.body).unwrap()["model"], "second-model");
    assert!(!first.headers.contains_key("authorization"));
    assert_eq!(second.headers["authorization"], ["Bearer second-key"]);
    assert_eq!(fixture.notifications.load(Ordering::SeqCst), 8);
    let snapshot = fixture.snapshot();
    let rows = snapshot["rows"].as_array().unwrap();
    assert_eq!(rows.iter().map(|row| row[0].as_str().unwrap()).collect::<Vec<_>>(), expected_keys.iter().map(String::as_str).collect::<Vec<_>>());
    for row in rows {
        assert_eq!(&row.as_array().unwrap()[1..], &json!(["hypa-v1-text","custom:old-cache-identity",url,1,2,"0000803F00000040",0]).as_array().unwrap()[..]);
    }
    let query = retrieval.query(Text::from_str("query", &budget).unwrap()).await;
    server.captured().await;
    let fresh = (1.00000001f64 * 1.00000001 + 4.0).to_bits();
    assert_eq!(scores(&query), (0..51).map(|n| (format!("document {n}"), fresh)).collect::<Vec<_>>());
    drop(retrieval);
    assert_eq!(slots.available_permits(), 0); drop(query);
    assert_eq!(slots.available_permits(), 1);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());

    fixture.reopen();
    assert_eq!(fixture.snapshot(), snapshot);
    let (operation, budget, slots) = self::operation();
    let mut warm = owner(&url, fixture.cache(None, None), capture(settings.clone()), operation, 7);
    let prepared = warm.prepare(documents(&budget, 51)).await;
    assert!(prepared.value.is_ok()); drop(prepared);
    assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
    let query = warm.query(Text::from_str("warm query", &budget).unwrap()).await;
    assert!(serde_json::from_slice::<Value>(&server.captured().await.body).unwrap()["input"].is_string());
    let rounded = (1.00000001f64 + 4.0).to_bits();
    assert_ne!(fresh, rounded);
    assert_eq!(scores(&query), (0..51).map(|n| (format!("document {n}"), rounded)).collect::<Vec<_>>());
    assert_eq!(fixture.snapshot(), snapshot);
    assert_eq!(fixture.notifications.load(Ordering::SeqCst), 8);
    drop(warm); drop(query);
    assert_eq!(slots.available_permits(), 1); assert_eq!(budget.owned_bytes(), Budget::context_bytes());

    let (operation, budget, _) = self::operation();
    let mut mixed = owner(&url, fixture.cache(None, None), capture(settings), operation, 7);
    let prepared = mixed.prepare(["document 0","document 0","new document"].into_iter().map(|value| Text::from_str(value, &budget).unwrap()).collect()).await;
    assert!(prepared.value.is_ok()); drop(prepared);
    assert_eq!(serde_json::from_slice::<Value>(&server.captured().await.body).unwrap()["input"], json!(["new document"]));
    let query = mixed.query(Text::from_str("mixed query", &budget).unwrap()).await;
    server.captured().await;
    assert_eq!(scores(&query), [("new document".into(),fresh),("document 0".into(),rounded),("document 0".into(),rounded)]);
    assert_eq!(fixture.snapshot()["rows"].as_array().unwrap().len(), 52);
    assert_eq!(fixture.notifications.load(Ordering::SeqCst), 9);
    drop(mixed); drop(query); assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[tokio::test]
async fn committed_rows_survive_owned_query_status_json_decode_and_transport_failures() {
    for kind in 0..4 {
        let fixture = StoreFixture::new();
        let reply = match kind {
            0 => Reply::plain(429, br#"{"error":"synthetic","details":{"code":17}}"#),
            1 => Reply::plain(200, b"not json"),
            2 => Reply::encoded(b"invalid gzip".to_vec(), "gzip"),
            _ => { let mut reply = Reply::plain(200, b"{"); reply.held = true; reply },
        };
        let mut server = Loopback::new(vec![Reply::plain(200, &vectors(1)), reply]).await;
        let (operation, budget, slots) = operation();
        let mut retrieval = owner(&format!("{}/v1", server.url), fixture.cache(None, None), capture(defaults()), operation, 1);
        let prepared = retrieval.prepare(documents(&budget, 1)).await;
        assert!(prepared.value.is_ok()); drop(prepared); server.captured().await;
        let before = fixture.snapshot();
        let output = if kind == 3 {
            let mut query = Box::pin(retrieval.query(Text::from_str("query", &budget).unwrap()));
            tokio::select! { _ = server.captured() => {}, _ = &mut query => panic!("held transport completed") }
            server.worker.abort(); query.await
        } else {
            let result = retrieval.query(Text::from_str("query", &budget).unwrap()).await;
            server.captured().await; result
        };
        assert!(output.value.is_err());
        match kind {
            0 => {
                let Some(Failure::Remote { status: 429, diagnostic }) = &output.diagnostic else { panic!("remote diagnostic") };
                assert_eq!(diagnostic.as_unicode(), Some(r#"{"error":"synthetic","details":{"code":17}}"#));
            }
            1 => assert!(matches!(output.diagnostic, Some(Failure::Decode { .. }))),
            2 => assert!(matches!(output.value, Err(StageError::Invalid(_)))),
            _ => assert!(matches!(output.diagnostic, Some(Failure::Transport { .. }))),
        }
        assert_eq!(fixture.snapshot(), before); assert_eq!(fixture.notifications.load(Ordering::SeqCst), 1);
        drop(retrieval); assert_eq!(slots.available_permits(), 0); drop(output);
        assert_eq!(slots.available_permits(), 1); assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}

#[tokio::test]
async fn later_document_failure_writes_nothing_and_later_sql_batch_failure_keeps_earlier_commits() {
    let fixture = StoreFixture::new();
    let mut server = Loopback::new(vec![Reply::plain(200, &vectors(50)), Reply::plain(500, br#"{"error":"synthetic"}"#)]).await;
    let (operation, budget, slots) = operation();
    let mut retrieval = owner(&format!("{}/v1", server.url), fixture.cache(None, None), capture(defaults()), operation, 2);
    let before = fixture.snapshot();
    let output = retrieval.prepare(documents(&budget, 51)).await;
    assert!(matches!(output.diagnostic, Some(Failure::Remote { status: 500, .. })));
    server.captured().await; server.captured().await;
    assert_eq!(fixture.snapshot(), before); assert_eq!(fixture.notifications.load(Ordering::SeqCst), 0);
    drop(retrieval); drop(output); assert_eq!(slots.available_permits(), 1); assert_eq!(budget.owned_bytes(), Budget::context_bytes());

    let fixture = StoreFixture::new();
    let mut server = Loopback::new(vec![Reply::plain(200, &vectors(5)), Reply::plain(200, &vectors(1))]).await;
    let url = format!("{}/v1", server.url);
    let (operation, budget, slots) = self::operation();
    let docs = documents(&budget, 5);
    let bad = String::from_utf8(target(&url, &budget).cache_key(&docs[3], &budget).unwrap().hex().to_vec()).unwrap();
    let opened: Opened = Arc::new(move |store| {
        store.device_store().unwrap().connection().execute_batch(&format!(
            "CREATE TEMP TRIGGER synthetic_composition_failure BEFORE INSERT ON main.hypa_embeddings WHEN NEW.cache_key='{bad}' BEGIN SELECT RAISE(ABORT,'synthetic batch failure'); END;")).unwrap();
    });
    let mut retrieval = owner(&url, fixture.cache(Some(opened), None), capture(defaults()), operation, 2);
    let output = retrieval.prepare(docs).await;
    assert!(output.value.is_ok(), "ordinary cache unavailability preserves fresh retrieval");
    drop(output); server.captured().await;
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot["rows"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot["units"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot["outbox"].as_array().unwrap().len(), 2);
    assert_eq!(fixture.notifications.load(Ordering::SeqCst), 1);
    let query = retrieval.query(Text::from_str("query", &budget).unwrap()).await; server.captured().await;
    assert_eq!(scores(&query).len(), 5);
    assert_eq!(fixture.snapshot(), snapshot);
    drop(retrieval); drop(query); assert_eq!(slots.available_permits(), 1); assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[tokio::test]
async fn ordinary_read_unavailable_uses_http_but_budget_and_cancellation_refuse_dispatch() {
    for mode in 0..3 {
        let fixture = StoreFixture::new();
        let mut server = Loopback::new(vec![Reply::plain(200, &vectors(1))]).await;
        let (operation, budget, slots) = operation();
        let cancel = operation.cancel().clone();
        let denied = Arc::new(Mutex::new(None::<Charge>));
        let held = denied.clone();
        let b = budget.clone();
        let c = cancel.clone();
        let opened: Opened = Arc::new(move |_| match mode {
            0 => {},
            1 => { *held.lock().unwrap() = Some(b.reserve_owned(b.limits().max_owned_bytes - b.owned_bytes()).unwrap()); }
            _ => c.cancel(),
        });
        let url = format!("{}/v1", server.url);
        if mode == 0 {
            let key = String::from_utf8(target(&url, &budget).cache_key(&Text::from_str("document 0", &budget).unwrap(), &budget).unwrap().hex().to_vec()).unwrap();
            fixture.store(|store| store.device_store().unwrap().connection().execute(
                "INSERT INTO hypa_embeddings(cache_key,producer,model,preprocess_version,dimensions,vector,tombstone,write_clock,writer_id) VALUES(?1,'hypa-v1-text','custom:old-cache-identity',1,1,?2,0,'0','')",
                rusqlite::params![key, vec![1u8,2,3,4,5]],
            ).unwrap());
        }
        let mut retrieval = owner(&url, fixture.cache(Some(opened), None), capture(defaults()), operation, 1);
        let output = retrieval.prepare(documents(&budget, 1)).await;
        if mode == 0 {
            assert!(output.value.is_ok()); server.captured().await;
        } else {
            if mode == 1 { assert!(matches!(output.value, Err(StageError::Limit(_)))); }
            else { assert!(matches!(output.value, Err(StageError::Cancelled))); }
            assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        }
        assert_eq!(fixture.notifications.load(Ordering::SeqCst), usize::from(mode == 0));
        drop(retrieval); drop(output); denied.lock().unwrap().take();
        assert_eq!(slots.available_permits(), 1); assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}

#[tokio::test]
async fn running_prepare_query_and_reply_workers_settle_cancellation_and_abandonment() {
    for turn in 0..3 {
        for abandoned in [false, true] {
            let fixture = StoreFixture::new();
            let mut server = Loopback::new(vec![Reply::plain(200, &vectors(1)), Reply::plain(200, &vectors(1))]).await;
            let (operation, budget, slots) = operation();
            let cancel = operation.cancel().clone();
            let gate = Arc::new(Gate::default());
            let _release = Release(gate.clone());
            let caller = std::thread::current().id();
            let opens = Arc::new(AtomicUsize::new(0));
            let count = opens.clone(); let hold = gate.clone();
            let opened: Opened = Arc::new(move |_| {
                assert_ne!(std::thread::current().id(), caller, "cache work ran on the async owner");
                let index = count.fetch_add(1, Ordering::SeqCst);
                if (turn == 0 && index == 0) || (turn == 2 && index == 1) { hold.hold(); }
            });
            let captures = Arc::new(AtomicUsize::new(0));
            let count = captures.clone(); let hold = gate.clone();
            let actual = capture(defaults());
            let captured = move |b: &Budget| {
                assert_ne!(std::thread::current().id(), caller, "configuration capture ran on the async owner");
                if turn == 1 && count.fetch_add(1, Ordering::SeqCst) == 1 { hold.hold(); }
                actual(b)
            };
            let mut retrieval = owner(&format!("{}/v1", server.url), fixture.cache(Some(opened), None), captured, operation, 1);
            if turn == 1 {
                let prepared = retrieval.prepare(documents(&budget, 1)).await;
                assert!(prepared.value.is_ok()); drop(prepared); server.captured().await;
            }
            let mut pending: Pin<Box<dyn Future<Output = Observed<Result<RetrievalResult, StageError>>> + '_>> = if turn == 1 {
                Box::pin(retrieval.query(Text::from_str("query", &budget).unwrap()))
            } else { Box::pin(retrieval.prepare(documents(&budget, 1))) };
            tokio::select! { _ = &mut pending => panic!("worker escaped its gate"), _ = until(|| gate.entered.load(Ordering::SeqCst)) => {} }
            let progressed = Arc::new(AtomicBool::new(false));
            let marker = progressed.clone();
            tokio::spawn(async move { marker.store(true, Ordering::SeqCst); }).await.unwrap();
            assert!(progressed.load(Ordering::SeqCst));
            assert_eq!(slots.available_permits(), 0);
            assert!(budget.owned_bytes() > Budget::context_bytes());
            if turn == 2 { server.captured().await; }
            if abandoned {
                drop(pending);
                assert!(cancel.is_cancelled(), "abandonment did not cancel the worker");
                assert_eq!(cancel.origin(), decode::CancelOrigin::Owner);
                assert!(retrieval.session.is_none());
                drop(retrieval);
                assert_eq!(slots.available_permits(), 0, "running worker lost its operation");
                gate.release();
                until(|| slots.available_permits() == 1).await;
            } else {
                cancel.cancel();
                assert!(std::future::poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx))).await.is_pending(), "cancel reported before worker settled");
                assert_eq!(slots.available_permits(), 0);
                gate.release();
                let output = pending.await;
                assert!(matches!(output.value, Err(StageError::Cancelled)));
                drop(retrieval); assert_eq!(slots.available_permits(), 0); drop(output);
                assert_eq!(slots.available_permits(), 1);
            }
            assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
            assert_eq!(active_cache_segments(&fixture.state).unwrap(), 0);
            assert_eq!(fixture.snapshot()["rows"].as_array().unwrap().len(), if turn == 1 { 1 } else { 0 });
            assert_eq!(fixture.notifications.load(Ordering::SeqCst), if turn == 1 { 1 } else { 0 });
            assert_eq!(budget.owned_bytes(), Budget::context_bytes());
        }
    }
}

#[tokio::test]
async fn cancellation_after_commit_joins_notification_and_preserves_committed_rows() {
    let fixture = StoreFixture::new();
    let mut server = Loopback::new(vec![Reply::plain(200, &vectors(1))]).await;
    let (operation, budget, slots) = operation();
    let cancel = operation.cancel().clone();
    let gate = Arc::new(Gate::default()); let _release = Release(gate.clone()); let hold = gate.clone();
    let committed: Committed = Arc::new(move || hold.hold());
    let mut retrieval = owner(&format!("{}/v1", server.url), fixture.cache(None, Some(committed)), capture(defaults()), operation, 1);
    let mut pending = Box::pin(retrieval.prepare(documents(&budget, 1)));
    tokio::select! { _ = &mut pending => panic!("commit gate escaped"), _ = until(|| gate.entered.load(Ordering::SeqCst)) => {} }
    server.captured().await;
    assert_eq!(active_cache_segments(&fixture.state).unwrap(), 0);
    assert_eq!(fixture.snapshot()["rows"].as_array().unwrap().len(), 1);
    assert_eq!(fixture.notifications.load(Ordering::SeqCst), 0);
    cancel.cancel();
    assert!(poll_once(pending.as_mut()).await.is_pending());
    gate.release();
    let output = pending.await;
    assert!(matches!(output.value, Err(StageError::Cancelled)));
    assert_eq!(fixture.notifications.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.snapshot()["rows"].as_array().unwrap().len(), 1);
    drop(retrieval); drop(output); assert_eq!(slots.available_permits(), 1); assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[test]
fn queued_first_prepare_and_query_keep_handoffs_until_actual_join_or_abandoned_drop() {
    for query in [false, true] {
        for abandoned in [false, true] {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().max_blocking_threads(1).build().unwrap();
            runtime.block_on(async {
                let fixture = StoreFixture::new();
                let mut server = Loopback::new(vec![Reply::plain(200, &vectors(1)), Reply::plain(200, &vectors(1))]).await;
                let (operation, budget, slots) = operation();
                let cancel = operation.cancel().clone();
                let mut retrieval = owner(&format!("{}/v1", server.url), fixture.cache(None, None), capture(defaults()), operation, 1);
                if query {
                    let prepared = retrieval.prepare(documents(&budget, 1)).await;
                    assert!(prepared.value.is_ok()); drop(prepared); server.captured().await;
                }
                let gate = Arc::new(Gate::default()); let _release = Release(gate.clone()); let hold = gate.clone();
                let blocker = tokio::task::spawn_blocking(move || hold.hold());
                until(|| gate.entered.load(Ordering::SeqCst)).await;
                let mut query_text = query.then(|| Text::from_str("query", &budget).unwrap());
                let mut docs = (!query).then(|| {
                    let mut docs = Vec::with_capacity(128);
                    docs.push(Text::from_str("document", &budget).unwrap());
                    docs
                });
                let expected = retrieval.handoff_bytes().unwrap() + if query {
                    retrieval.candidates.rows.load(Ordering::Relaxed) * size_of::<risunest_memory::hanurai::ScoredDocument>()
                } else { docs.as_ref().unwrap().capacity() * size_of::<Text>() };
                let before = budget.owned_bytes();
                let mut pending: Pin<Box<dyn Future<Output = Observed<Result<RetrievalResult, StageError>>> + '_>> = if query {
                    Box::pin(retrieval.query(query_text.take().unwrap()))
                } else { Box::pin(retrieval.prepare(docs.take().unwrap())) };
                assert!(std::future::poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx))).await.is_pending());
                assert_eq!(budget.owned_bytes(), before + expected, "queued application payload lost its handoff lease");
                if abandoned {
                    drop(pending);
                    assert!(cancel.is_cancelled()); assert!(retrieval.session.is_none());
                    drop(retrieval);
                    assert_eq!(slots.available_permits(), 0);
                    gate.release(); blocker.await.unwrap();
                    until(|| slots.available_permits() == 1).await;
                } else {
                    cancel.cancel();
                    assert!(std::future::poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx))).await.is_pending());
                    assert_eq!(slots.available_permits(), 0);
                    gate.release(); blocker.await.unwrap();
                    let output = pending.await;
                    assert!(matches!(output.value, Err(StageError::Cancelled)));
                    drop(retrieval); drop(output); assert_eq!(slots.available_permits(), 1);
                }
                assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
                assert_eq!(fixture.snapshot()["rows"].as_array().unwrap().len(), usize::from(query));
                assert_eq!(fixture.notifications.load(Ordering::SeqCst), usize::from(query));
                assert_eq!(budget.owned_bytes(), Budget::context_bytes());
            });
        }
    }
}

#[tokio::test]
async fn outer_input_and_worker_handoff_admission_have_low_and_ample_controls() {
    for mode in 0..3 {
        let fixture = StoreFixture::new();
        let mut server = Loopback::new(vec![Reply::plain(200, &vectors(1))]).await;
        let (operation, budget, slots) = operation();
        let mut retrieval = owner(&format!("{}/v1", server.url), fixture.cache(None, None), capture(defaults()), operation, 1);
        let mut docs = Vec::with_capacity(128);
        docs.push(Text::from_str("document", &budget).unwrap());
        let needed = docs.capacity() * size_of::<Text>();
        let allowed = if mode == 0 { needed - 1 } else { needed + retrieval.handoff_bytes().unwrap() - 1 };
        let held = if mode < 2 { Some(budget.reserve_owned(budget.limits().max_owned_bytes - budget.owned_bytes() - allowed).unwrap()) } else { None };
        let output = retrieval.prepare(docs).await;
        if mode < 2 {
            assert!(matches!(output.value, Err(StageError::Limit(_))));
            assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
            assert!(fixture.snapshot()["rows"].as_array().unwrap().is_empty());
        } else {
            assert!(output.value.is_ok()); server.captured().await;
            assert_eq!(fixture.snapshot()["rows"].as_array().unwrap().len(), 1);
        }
        drop(retrieval); drop(output); drop(held);
        assert_eq!(slots.available_permits(), 1); assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}

#[tokio::test]
async fn real_admission_wait_stays_off_async_owner_and_settles_before_cancelled_publication() {
    for abandoned in [false, true] {
        let fixture = StoreFixture::new();
        let mut server = Loopback::new(vec![Reply::plain(200, &vectors(1))]).await;
        let held_admission = fixture.state.admit_native_job_operation().unwrap();
        let state = fixture.state.clone();
        let reset = tokio::task::spawn_blocking(move || state.reset_renderer_session().unwrap());
        until(|| fixture.state.maintenance_active()).await;
        let (operation, budget, slots) = operation();
        let cancel = operation.cancel().clone();
        let mut retrieval = owner(&format!("{}/v1", server.url), fixture.cache(None, None), capture(defaults()), operation, 1);
        let mut pending = Box::pin(retrieval.prepare(documents(&budget, 1)));
        tokio::select! { _ = &mut pending => panic!("admission did not hold the worker"), _ = until(|| fixture.state.native_job_waiting_for_reset()) => {} }
        let progressed = tokio::spawn(async { 17 }).await.unwrap();
        assert_eq!(progressed, 17);
        assert_eq!(slots.available_permits(), 0);
        assert!(budget.owned_bytes() > Budget::context_bytes());
        if abandoned {
            drop(pending); assert!(cancel.is_cancelled()); drop(retrieval);
            assert_eq!(slots.available_permits(), 0);
            drop(held_admission); reset.await.unwrap();
            until(|| slots.available_permits() == 1).await;
        } else {
            cancel.cancel();
            assert!(poll_once(pending.as_mut()).await.is_pending());
            assert!(fixture.state.native_job_waiting_for_reset());
            drop(held_admission); reset.await.unwrap();
            let output = pending.await;
            assert!(matches!(output.value, Err(StageError::Cancelled)));
            drop(retrieval); drop(output); assert_eq!(slots.available_permits(), 1);
        }
        assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        assert!(fixture.snapshot()["rows"].as_array().unwrap().is_empty());
        assert_eq!(fixture.notifications.load(Ordering::SeqCst), 0);
        assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}

#[tokio::test]
async fn result_reservation_and_worker_handoff_refuse_before_query_http_with_ample_control() {
    for limited in [true, false] {
        let fixture = StoreFixture::new();
        let mut server = Loopback::new(vec![Reply::plain(200, &vectors(1)), Reply::plain(200, &vectors(1))]).await;
        let (operation, budget, slots) = operation();
        let mut retrieval = owner(&format!("{}/v1", server.url), fixture.cache(None, None), capture(defaults()), operation, 1);
        let prepared = retrieval.prepare(documents(&budget, 1)).await;
        assert!(prepared.value.is_ok()); drop(prepared); server.captured().await;
        let query = Text::from_str("query", &budget).unwrap();
        let needed = retrieval.handoff_bytes().unwrap() + retrieval.candidates.rows.load(Ordering::Relaxed) * size_of::<risunest_memory::hanurai::ScoredDocument>();
        let held = if limited { Some(budget.reserve_owned(budget.limits().max_owned_bytes - budget.owned_bytes() - needed + 1).unwrap()) } else { None };
        let output = retrieval.query(query).await;
        if limited {
            assert!(matches!(output.value, Err(StageError::Limit(_))));
            assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        } else {
            assert_eq!(scores(&output).len(), 1); server.captured().await;
        }
        assert_eq!(fixture.snapshot()["rows"].as_array().unwrap().len(), 1);
        assert_eq!(fixture.notifications.load(Ordering::SeqCst), 1);
        drop(held); drop(retrieval); assert_eq!(slots.available_permits(), 0); drop(output);
        assert_eq!(slots.available_permits(), 1); assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}

#[tokio::test]
async fn unreceived_worker_outputs_keep_pending_request_or_scored_payload_until_drop() {
    for scored in [false, true] {
        let fixture = StoreFixture::new();
        let mut server = Loopback::new(vec![Reply::plain(200, &vectors(1)), Reply::plain(200, &vectors(1)), Reply::plain(200, &vectors(1))]).await;
        let (operation, budget, slots) = operation();
        let mut retrieval = owner(&format!("{}/v1", server.url), fixture.cache(None, None), capture(defaults()), operation, 1);
        let flight = Flight::new(&retrieval.operation);
        let (turn, reservation, handoff) = if scored {
            let prepared = retrieval.prepare(documents(&budget, 1)).await;
            assert!(prepared.value.is_ok()); drop(prepared); server.captured().await;
            let reservation = retrieval.candidates.reserve_result().unwrap();
            let handoff = retrieval.handoff().unwrap();
            let mut output = retrieval.worker(Turn::Query(Some(Text::from_str("query", &budget).unwrap())), Some(reservation), handoff).await.unwrap();
            retrieval.session = output.session.take();
            let Ok(WorkerResult::Http(request)) = output.result.take().unwrap() else { panic!("query HTTP") };
            let reply = retrieval.session.as_mut().unwrap().executor.execute(request).await.unwrap();
            server.captured().await;
            (Turn::ResumeHttp(Some(reply)), output.reservation.take(), output.handoff.take().unwrap())
        } else {
            let docs = documents(&budget, 1);
            retrieval.candidates.record(docs.len()).unwrap();
            let charge = budget.reserve_owned(docs.capacity() * size_of::<Text>()).unwrap();
            (Turn::Prepare(PrepareInput { documents: Some(docs), _charge: Some(charge) }), None, retrieval.handoff().unwrap())
        };
        let gate = Arc::new(Gate::default()); let _release = Release(gate.clone()); let hold = gate.clone();
        let task = WorkerTask { turn, session: retrieval.session.take(), reservation, handoff: Some(handoff),
            operation: retrieval.operation.clone(), diagnostic: retrieval.diagnostic.clone() };
        let handle = tokio::task::spawn_blocking(move || {
            let output = task.run();
            match output.result.as_ref().unwrap() {
                Ok(WorkerResult::Done(result)) if scored => {
                    let RetrievalOutput::Scored(rows) = &result.output else { panic!("scored output") };
                    assert_eq!(rows.len(), 1);
                }
                Ok(WorkerResult::Http(request)) if !scored => assert!(!request.body.is_empty()),
                _ => panic!("unexpected worker output"),
            }
            hold.hold();
            output
        });
        until(|| gate.entered.load(Ordering::SeqCst)).await;
        assert!(budget.owned_bytes() > Budget::context_bytes());
        assert_eq!(slots.available_permits(), 0);
        drop(handle); drop(flight); drop(retrieval);
        assert_eq!(slots.available_permits(), 0, "unreceived output lost session or result ownership");
        gate.release();
        until(|| slots.available_permits() == 1).await;
        assert_eq!(budget.owned_bytes(), Budget::context_bytes());
        assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)), "unreceived output dispatched late HTTP");
        assert_eq!(fixture.snapshot()["rows"].as_array().unwrap().len(), usize::from(scored));
        assert_eq!(fixture.notifications.load(Ordering::SeqCst), usize::from(scored));
    }
}

#[tokio::test]
async fn worker_panic_is_terminal_and_releases_real_cache_admission_and_payloads() {
    let fixture = StoreFixture::new();
    let mut server = Loopback::new(vec![Reply::plain(200, &vectors(1))]).await;
    let (operation, budget, slots) = operation();
    let capture = |_: &Budget| -> Result<CapturedConfig, Failure> { panic!("synthetic configuration capture panic") };
    let mut retrieval = owner(&format!("{}/v1", server.url), fixture.cache(None, None), capture, operation, 1);
    let output = retrieval.prepare(documents(&budget, 1)).await;
    assert!(matches!(&output.value, Err(StageError::Invalid(kind)) if kind == "native-http-worker-join"));
    assert!(retrieval.session.is_none());
    assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
    assert_eq!(active_cache_segments(&fixture.state).unwrap(), 0);
    assert!(fixture.snapshot()["rows"].as_array().unwrap().is_empty());
    assert_eq!(fixture.notifications.load(Ordering::SeqCst), 0);
    drop(retrieval); assert_eq!(slots.available_permits(), 0); drop(output);
    assert_eq!(slots.available_permits(), 1); assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}
