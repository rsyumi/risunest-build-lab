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
// The app's listener holds the state, and a mock app never drops its listeners.
impl Drop for StoreFixture { fn drop(&mut self) { self.state.close_for_test(); } }
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
    NativeRetrieval::new(retrieval, Executor::new(operation, limits()).unwrap(), diagnostic, candidates)
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
    if isolate_loopback_test(concat!(module_path!(), "::real_store_cold_reopened_warm_and_mixed_duplicates_preserve_bits_identity_and_effects")) { return; }
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
    if isolate_loopback_test(concat!(module_path!(), "::committed_rows_survive_owned_query_status_json_decode_and_transport_failures")) { return; }
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
            2 => { assert!(matches!(output.value, Err(StageError::Host(_)))); assert!(matches!(output.diagnostic, Some(Failure::Transport { .. }))); },
            _ => assert!(matches!(output.diagnostic, Some(Failure::Transport { .. }))),
        }
        assert_eq!(fixture.snapshot(), before); assert_eq!(fixture.notifications.load(Ordering::SeqCst), 1);
        drop(retrieval); assert_eq!(slots.available_permits(), 0); drop(output);
        assert_eq!(slots.available_permits(), 1); assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}

#[tokio::test]
async fn later_document_failure_writes_nothing_and_later_sql_batch_failure_keeps_earlier_commits() {
    if isolate_loopback_test(concat!(module_path!(), "::later_document_failure_writes_nothing_and_later_sql_batch_failure_keeps_earlier_commits")) { return; }
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
    if isolate_loopback_test(concat!(module_path!(), "::ordinary_read_unavailable_uses_http_but_budget_and_cancellation_refuse_dispatch")) { return; }
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
    if isolate_loopback_test(concat!(module_path!(), "::running_prepare_query_and_reply_workers_settle_cancellation_and_abandonment")) { return; }
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
    if isolate_loopback_test(concat!(module_path!(), "::cancellation_after_commit_joins_notification_and_preserves_committed_rows")) { return; }
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
    if isolate_loopback_test(concat!(module_path!(), "::queued_first_prepare_and_query_keep_handoffs_until_actual_join_or_abandoned_drop")) { return; }
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
    if isolate_loopback_test(concat!(module_path!(), "::outer_input_and_worker_handoff_admission_have_low_and_ample_controls")) { return; }
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
    if isolate_loopback_test(concat!(module_path!(), "::real_admission_wait_stays_off_async_owner_and_settles_before_cancelled_publication")) { return; }
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
    if isolate_loopback_test(concat!(module_path!(), "::result_reservation_and_worker_handoff_refuse_before_query_http_with_ample_control")) { return; }
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
    if isolate_loopback_test(concat!(module_path!(), "::unreceived_worker_outputs_keep_pending_request_or_scored_payload_until_drop")) { return; }
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
    if isolate_loopback_test(concat!(module_path!(), "::worker_panic_is_terminal_and_releases_real_cache_admission_and_payloads")) { return; }
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


use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::stage::{Effect, MemoryInput, MemoryOutput, MemoryStage, TokenCounter, TokenInput};
use risunest_memory::hypa_v2::{HypaV2, HypaV2Config};
use risunest_memory::hypa_v3::{HypaV3, HypaV3Config};
use risunest_memory::summary::Summary;
use risunest_cbs_experimental::cbs::processor::{Processor, Snapshot};
use risunest_ir::rng::Tape;

struct HostTokens;
impl TokenCounter for HostTokens {
    fn count(&mut self, input: TokenInput<'_>, budget: &Budget) -> Result<Step<u32>, StageError> {
        let TokenInput::Message(message) = input else { panic!("memory token input") };
        Ok(Step::NeedsHost(HostRequest::PluginTokenizer { text: message.content.copy(budget)? }))
    }
    fn resume(&mut self, reply: HostReply, _: &Budget) -> Result<Step<u32>, StageError> {
        let HostReply::Tokens(tokens) = reply else { panic!("token reply") };
        Ok(Step::Done(tokens.len() as u32))
    }
}
fn policy_history(budget: &Budget, length: usize, current: f64, maximum: f64) -> MemoryInput {
    let chats = (0..length).map(|index| {
        let text = Text::from_str(&format!("chat {index}"), budget).unwrap();
        let mut message = PromptMessage::new(Role::User, text);
        message.memo = Some(Text::from_str(&format!("memo {index}"), budget).unwrap());
        message
    }).collect();
    MemoryInput { chats, current_tokens: current, max_context_tokens: maximum }
}
fn summary_reply(number: usize, budget: &Budget) -> HostReply {
    use risunest_ir::provider::{OutputShape, ProviderOutput};
    HostReply::Llm(Ok(ProviderOutput { texts: vec![Text::from_str(&format!("new summary {number}"), budget).unwrap()],
        shape: OutputShape::Text, thoughts: Vec::new(), tool_calls: Vec::new(), usage: None, finish: None }))
}
enum MemoryCall { Start(MemoryInput), Resume(HostReply) }
async fn memory_call<M: MemoryStage + Send + 'static>(mut stage: M, call: MemoryCall, budget: &Budget)
    -> (M, Result<Step<MemoryOutput>, StageError>)
{
    let budget = budget.clone();
    tokio::task::spawn_blocking(move || {
        let result = match call { MemoryCall::Start(input) => stage.run(input, &budget), MemoryCall::Resume(reply) => stage.resume(reply, &budget) };
        (stage, result)
    }).await.unwrap()
}
struct PolicyHttp(Option<product_http::Executor>);
impl PolicyHttp {
    fn new(operation: Operation, limits: Limits) -> Result<Self, Failure> { Ok(Self(Some(product_http::Executor::new(operation, limits)?))) }
}
impl std::ops::Deref for PolicyHttp { type Target = product_http::Executor; fn deref(&self) -> &Self::Target { self.0.as_ref().unwrap() } }
impl std::ops::DerefMut for PolicyHttp { fn deref_mut(&mut self) -> &mut Self::Target { self.0.as_mut().unwrap() } }
async fn memory_http_call<M: MemoryStage + Send + 'static>(mut stage: M, owner: &mut PolicyHttp, reply: ReplyEnvelope, budget: &Budget)
    -> (M, Result<Step<MemoryOutput>, StageError>)
{
    let mut executor = owner.0.take().unwrap();
    let budget = budget.clone();
    let (stage, executor, result) = tokio::task::spawn_blocking(move || {
        let result = executor.resume(reply, |reply| stage.resume(reply, &budget)).map_err(product_retrieval::terminal_failure).and_then(|result| result);
        (stage, executor, result)
    }).await.unwrap();
    owner.0 = Some(executor);
    (stage, result)
}
async fn retrieval_http_call<R: Retrieval + Send + 'static>(mut retrieval: R, owner: &mut PolicyHttp, reply: ReplyEnvelope, budget: &Budget)
    -> (R, Result<Step<RetrievalOutput>, StageError>)
{
    let mut executor = owner.0.take().unwrap();
    let budget = budget.clone();
    let (retrieval, executor, result) = tokio::task::spawn_blocking(move || {
        let result = executor.resume(reply, |reply| retrieval.resume(reply, &budget)).map_err(product_retrieval::terminal_failure).and_then(|result| result);
        (retrieval, executor, result)
    }).await.unwrap();
    owner.0 = Some(executor);
    (retrieval, result)
}
fn assert_new_v3_memory(output: &MemoryOutput, budget: &Budget, minimum: usize) {
    let [Effect::ChatField { field, value }] = output.effects.as_slice() else { panic!("one retained memory effect") };
    assert!(field.equals_ascii(b"hypaV3Data"));
    let summaries = value.get(b"summaries", budget).unwrap().and_then(risunest_ir::Value::array).unwrap();
    assert!(summaries.len() >= minimum);
    assert!(summaries.iter().any(|entry| matches!(entry.get(b"text", budget).unwrap(), Some(risunest_ir::Value::Text(text)) if text.as_unicode().is_some_and(|text| text.starts_with("new summary ")))));
    assert!(output.error.as_ref().and_then(Text::as_unicode).is_some_and(|text| !text.is_empty()));
}

#[tokio::test]
async fn real_hypa_v3_keeps_new_summaries_for_product_dependency_failures_and_request_stop() {
    if isolate_loopback_test(concat!(module_path!(), "::real_hypa_v3_keeps_new_summaries_for_product_dependency_failures_and_request_stop")) { return; }
    for kind in ["status", "json", "schema", "transport", "header", "url", "empty", "credentials", "absent", "request-stop", "summary-stop", "hard", "limit"] {
        let fixture = StoreFixture::new();
        let reply = match kind {
            "status" => Reply::plain(429, br#"{"error":"synthetic"}"#),
            "json" => Reply::plain(200, b"not json"),
            "schema" => Reply::plain(200, br#"{"data":17}"#),
            "absent" => Reply::plain(200, br#"{"data":[{}]}"#),
            "limit" => Reply::plain(200, &vec![b'x'; limits().response_bytes + 1]),
            "transport" => { let mut reply = Reply::plain(200, b"{"); reply.held = true; reply },
            _ => Reply::plain(200, JSON),
        };
        let mut server = Loopback::new(vec![reply]).await;
        let (operation, budget, slots) = operation();
        let stop = Cancellation::new();
        let endpoint = match kind { "url" => "http://[invalid".into(), "empty" => String::new(),
            "credentials" => format!("http://name:password@{}/v1", server.url.trim_start_matches("http://")), _ => format!("{}/v1", server.url) };
        let settings = defaults();
        if kind == "header" { settings.lock().unwrap().key = Some("\u{100}".into()); }
        let retrieval = PolicyRetrieval::new(target(&endpoint, &budget), fixture.cache(None, None), capture(settings), 2, &operation).unwrap();
        let summary = Summary::hypa_v3(&Text::from_str("subModel", &budget).unwrap(), &Text::from_str("", &budget).unwrap(),
            Processor::new(Snapshot::default(), Tape::new(Vec::new())), &budget).unwrap();
        let config = HypaV3Config { max_response: 0.0, memory_tokens_ratio: 0.5, extra_summarization_ratio: 0.0,
            max_chats_per_summary: 2.0, recent_memory_ratio: 0.3, similar_memory_ratio: 0.3,
            enable_similarity_correction: true, preserve_orphaned_memory: false, do_not_summarize_user_message: false,
            summary_chunk_separator: Text::from_str("\\n\\n", &budget).unwrap(), query_chat_count: 2.0,
            use_experimental_impl: false, contextual_embedding: false, prepared: None };
        let stage = HypaV3::new(config, None, HostTokens, retrieval, summary, Tape::new(vec![0.25; 128]));
        let mut executor = PolicyHttp::new(operation.clone(), limits()).unwrap();
        let (mut stage, mut step) = memory_call(stage, MemoryCall::Start(policy_history(&budget, 6, 20.0, 20.0)), &budget).await;
        let mut summaries = 0;
        let mut http = 0;
        let mut terminal = None;
        let mut output = None;
        for _ in 0..128 {
            let request = match step { Ok(Step::Done(value)) => { output = Some(value); break; }, Ok(Step::NeedsHost(request)) => request,
                Err(error) => { terminal = Some(error); break; } };
            let reply = match request {
                HostRequest::PluginTokenizer { .. } => HostReply::Tokens(vec![0.0; 3]),
                HostRequest::Llm(_) => {
                    if kind == "summary-stop" && summaries == 1 { stop.cancel(); HostReply::Cancelled }
                    else { summaries += 1; summary_reply(summaries, &budget) }
                }
                HostRequest::Http(request) => {
                    assert!(summaries >= 1, "new summaries precede retrieval");
                    http += 1;
                    if kind == "request-stop" { stop.cancel(); }
                    if kind == "hard" { operation.cancel().cancel(); }
                    let result = if kind == "transport" {
                        let mut pending = Box::pin(executor.execute(request, None));
                        tokio::select! { _ = server.captured() => {}, _ = &mut pending => panic!("transport did not wait") }
                        server.worker.abort(); pending.await
                    } else { executor.execute(request, Some(&stop)).await };
                    match result {
                        Ok(reply) => { (stage, step) = memory_http_call(stage, &mut executor, reply, &budget).await; continue; },
                        Err(error) => { terminal = Some(product_retrieval::terminal_failure(error)); break; }
                    }
                }
                _ => panic!("unexpected policy dependency"),
            };
            (stage, step) = memory_call(stage, MemoryCall::Resume(reply), &budget).await;
        }
        if matches!(kind, "hard" | "limit") {
            assert!(output.is_none());
            assert!(matches!(terminal, Some(StageError::Cancelled | StageError::Limit(_))), "{kind}: {terminal:?}");
        } else {
            assert!(terminal.is_none(), "{kind}: {terminal:?}");
            assert_new_v3_memory(output.as_ref().expect(kind), &budget, 1);
            assert!(budget.check().is_ok(), "request failure did not hard-cancel");
        }
        assert!(http <= 1, "no automatic retry or later request after retrieval failure");
        if matches!(kind, "header" | "url" | "empty" | "credentials" | "request-stop" | "summary-stop" | "hard") {
            assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        }
        drop(output); drop(stage); drop(executor); drop(operation);
        assert_eq!(slots.available_permits(), 1, "{kind}");
        assert_eq!(budget.owned_bytes(), Budget::context_bytes(), "{kind}");
    }
}


#[tokio::test]
async fn real_hypa_v2_sends_a_later_summary_batch_after_an_ordinary_http_failure() {
    if isolate_loopback_test(concat!(module_path!(), "::real_hypa_v2_sends_a_later_summary_batch_after_an_ordinary_http_failure")) { return; }
    for kind in ["transport", "json"] {
        let fixture = StoreFixture::new();
        let first = if kind == "transport" { let mut reply = Reply::plain(200, b"{"); reply.held = true; reply }
            else { Reply::plain(200, b"not json") };
        let mut server = Loopback::new(vec![first, Reply::plain(200, br#"{"choices":[{"text":"successful later batch"}]}"#)]).await;
        let (operation, budget, slots) = operation();
        let endpoint = format!("{}/v1", server.url);
        let retrieval = PolicyRetrieval::new(target(&endpoint, &budget), fixture.cache(None, None), capture(defaults()), 2, &operation).unwrap();
        let summary = Summary::hypa_v2(&Text::from_str("curie", &budget).unwrap(), &Text::from_str("", &budget).unwrap(), None,
            Processor::new(Snapshot::default(), Tape::new(Vec::new())), &budget).unwrap();
        let stage = HypaV2::new(HypaV2Config { max_response: 0.0, allocated_tokens: 0.0, chunk_size: 3.0 }, None, HostTokens, retrieval, summary);
        let mut executor = PolicyHttp::new(operation.clone(), limits()).unwrap();
        let (mut stage, mut step) = memory_call(stage, MemoryCall::Start(policy_history(&budget, 7, 20.0, 10.0)), &budget).await;
        let mut prompts = Vec::new();
        let mut output = None;
        for _ in 0..128 {
            let request = match step.unwrap() { Step::Done(value) => { output = Some(value); break; }, Step::NeedsHost(request) => request };
            let reply = match request {
                HostRequest::PluginTokenizer { .. } => HostReply::Tokens(vec![0.0; 3]),
                HostRequest::Http(mut request) => {
                    let body: Value = serde_json::from_slice(&request.body).unwrap();
                    assert_eq!(body["model"], "text-curie-001");
                    prompts.push(body["prompt"].as_str().unwrap().to_owned());
                    assert!(prompts.len() <= 2, "failed input is skipped rather than retried");
                    // Replace only the network boundary destination; Summary produces the actual headers and body.
                    request.url = format!("{}/completions", server.url);
                    let result = if kind == "transport" && prompts.len() == 1 {
                        let mut pending = Box::pin(executor.execute(request, None));
                        tokio::select! { _ = server.captured() => {}, _ = &mut pending => panic!("transport did not wait") }
                        server.worker.abort();
                        let result = pending.await;
                        // The next request has a fresh loopback listener, while retaining this operation's executor/generation.
                        server = Loopback::new(vec![Reply::plain(200, br#"{"choices":[{"text":"successful later batch"}]}"#)]).await;
                        result
                    } else { executor.execute(request, None).await };
                    (stage, step) = memory_http_call(stage, &mut executor, result.unwrap(), &budget).await;
                    continue;
                }
                _ => panic!("unexpected summary dependency"),
            };
            (stage, step) = memory_call(stage, MemoryCall::Resume(reply), &budget).await;
        }
        let output = output.expect("real HypaV2 completed");
        assert_eq!(prompts.len(), 2, "{kind}");
        assert!(prompts[0].contains("user: chat 1"));
        assert!(!prompts[1].contains("user: chat 1"));
        assert!(prompts[1].contains("user: chat 2"));
        assert!(output.error.as_ref().and_then(Text::as_unicode).is_some_and(|text| text.contains("can't summarize last 4 messages")));
        assert_eq!(output.current_tokens, 17.0, "only the successful second batch deducts its three input tokens");
        assert!(budget.check().is_ok());
        assert_eq!(executor.phase, Phase::Ready);
        drop(output); drop(stage); drop(executor); drop(operation);
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}

enum RetrievalCall { Prepare(Vec<Text>), Query(Text), Resume(HostReply) }
async fn policy_retrieval_call<R: Retrieval + Send + 'static>(mut retrieval: R, call: RetrievalCall, budget: &Budget)
    -> (R, Result<Step<RetrievalOutput>, StageError>)
{
    let budget = budget.clone();
    tokio::task::spawn_blocking(move || {
        let result = match call { RetrievalCall::Prepare(documents) => retrieval.prepare(documents, &budget),
            RetrievalCall::Query(query) => retrieval.query(query, &budget), RetrievalCall::Resume(reply) => retrieval.resume(reply, &budget) };
        (retrieval, result)
    }).await.unwrap()
}
async fn settle_policy_retrieval<R: Retrieval + Send + 'static>(mut retrieval: R, mut step: Result<Step<RetrievalOutput>, StageError>,
    executor: &mut PolicyHttp, budget: &Budget) -> (R, Result<RetrievalOutput, StageError>)
{
    loop {
        match step {
            Ok(Step::Done(output)) => return (retrieval, Ok(output)),
            Err(error) => return (retrieval, Err(error)),
            Ok(Step::NeedsHost(HostRequest::Http(request))) => {
                let reply = executor.execute(request, None).await.unwrap();
                (retrieval, step) = retrieval_http_call(retrieval, executor, reply, budget).await;
            }
            _ => panic!("retrieval HTTP boundary"),
        }
    }
}

#[tokio::test]
async fn product_policy_reserves_cached_duplicates_and_surplus_before_query_and_reuses_the_allowance() {
    if isolate_loopback_test(concat!(module_path!(), "::product_policy_reserves_cached_duplicates_and_surplus_before_query_and_reuses_the_allowance")) { return; }
    for ample in [false, true] {
        let fixture = StoreFixture::new();
        let mut server = Loopback::new(vec![Reply::plain(200, &vectors(1)), Reply::plain(200, &vectors(3)),
            Reply::plain(200, &vectors(1)), Reply::plain(200, &vectors(1)), Reply::plain(200, &vectors(1))]).await;
        let (operation, budget, slots) = operation();
        let endpoint = format!("{}/v1", server.url);
        let mut executor = PolicyHttp::new(operation.clone(), limits()).unwrap();
        let cold = PolicyRetrieval::new(target(&endpoint, &budget), fixture.cache(None, None), capture(defaults()), 64, &operation).unwrap();
        let (cold, step) = policy_retrieval_call(cold, RetrievalCall::Prepare(vec![Text::from_str("cached", &budget).unwrap()]), &budget).await;
        let (cold, output) = settle_policy_retrieval(cold, step, &mut executor, &budget).await;
        assert!(matches!(output, Ok(RetrievalOutput::Prepared))); drop(cold); server.captured().await;
        let documents = (0..1024).map(|_| Text::from_str("cached", &budget).unwrap()).chain(std::iter::once(Text::from_str("fresh", &budget).unwrap())).collect();
        let retrieval = PolicyRetrieval::new(target(&endpoint, &budget), fixture.cache(None, None), capture(defaults()), 64, &operation).unwrap();
        let (retrieval, step) = policy_retrieval_call(retrieval, RetrievalCall::Prepare(documents), &budget).await;
        let (mut retrieval, output) = settle_policy_retrieval(retrieval, step, &mut executor, &budget).await;
        assert!(matches!(output, Ok(RetrievalOutput::Prepared))); server.captured().await;
        let bound = (1025 + 3) * size_of::<risunest_memory::hanurai::ScoredDocument>();
        let before_query = budget.owned_bytes();
        let query = Text::from_str("query", &budget).unwrap();
        let query_bytes = budget.owned_bytes() - before_query;
        let leave = if ample { bound + 256 * 1024 } else { bound - 1 };
        let held = budget.reserve_owned(budget.limits().max_owned_bytes - budget.owned_bytes() - leave).unwrap();
        let before = budget.owned_bytes();
        let (next, step) = policy_retrieval_call(retrieval, RetrievalCall::Query(query), &budget).await; retrieval = next;
        if !ample {
            assert!(matches!(step, Err(StageError::Limit(_))), "score allowance must fail before query HTTP");
            assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        } else {
            assert!(matches!(step, Ok(Step::NeedsHost(HostRequest::Http(_)))));
            assert!(budget.owned_bytes() >= before + bound, "allowance stays live during query HTTP");
            let (next, result) = settle_policy_retrieval(retrieval, step, &mut executor, &budget).await; retrieval = next;
            let RetrievalOutput::Scored(rows) = result.unwrap() else { panic!("scores") };
            assert_eq!(rows.len(), 1027, "1024 cached duplicates plus every fresh/surplus row");
            drop(rows); server.captured().await;
            let retained = budget.owned_bytes();
            assert!(retained + query_bytes >= before + bound);
            for _ in 0..2 {
                let query = Text::from_str("query", &budget).unwrap();
                let (next, step) = policy_retrieval_call(retrieval, RetrievalCall::Query(query), &budget).await;
                let (next, result) = settle_policy_retrieval(next, step, &mut executor, &budget).await; retrieval = next;
                let RetrievalOutput::Scored(rows) = result.unwrap() else { panic!("scores") }; assert_eq!(rows.len(), 1027); drop(rows);
                server.captured().await;
                assert_eq!(budget.owned_bytes(), retained, "sequential policy queries reuse one allowance");
            }
        }
        drop(held); drop(retrieval); drop(executor); drop(operation);
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}


fn observation_number(value: &Value) -> f64 {
    match value.as_str() { Some("-0") => -0.0, Some("NaN") => f64::NAN, Some("Infinity") => f64::INFINITY,
        Some("-Infinity") => f64::NEG_INFINITY, _ => value.as_f64().unwrap() }
}
fn observation_target(input: &Value, budget: &Budget) -> EmbeddingTarget {
    let source = &input["target"];
    EmbeddingTarget::capture(&Text::from_str(source["model"].as_str().unwrap(), budget).unwrap(), None,
        &Text::from_str(source["endpoint"].as_str().unwrap_or(""), budget).unwrap(),
        &Text::from_str(source["customModel"].as_str().unwrap_or(""), budget).unwrap(), budget).unwrap()
}
fn observation_replies(input: &Value) -> Vec<Reply> {
    input["embeddings"].as_array().unwrap().iter().map(|call| {
        let data: Vec<_> = call["result"].as_array().unwrap().iter().map(|row| {
            let values: Vec<_> = row["values"].as_array().unwrap().iter().map(observation_number).collect();
            json!({"embedding": values})
        }).collect();
        Reply::plain(200, &serde_json::to_vec(&json!({"data": data})).unwrap())
    }).collect()
}
async fn seed_observation(fixture: &StoreFixture, input: &Value, budget: &Budget) {
    let Some(seeds) = input["cache"]["seeds"].as_array() else { return; };
    let target = observation_target(input, budget);
    let mut entries = Vec::new();
    for seed in seeds {
        let key = target.cache_key(&Text::from_str(seed["text"].as_str().unwrap(), budget).unwrap(), budget).unwrap();
        let mut bits = OwnedVec::new(budget).unwrap();
        for bit in seed["vector"]["bits"].as_array().unwrap() { bits.push(bit.as_u64().unwrap() as u32).unwrap(); }
        entries.push(CacheWrite { key: Some(key), bits });
    }
    let mut cache = fixture.cache(None, None);
    let budget = budget.clone();
    tokio::task::spawn_blocking(move || assert!(cache.write_batch(&target, &entries, &budget).is_ok(), "native cache seed write failed")).await.unwrap();
}
struct ObservationHttp { wires: std::collections::VecDeque<Value>, endpoint: String }
impl ObservationHttp {
    fn new(expected: &Value, endpoint: String) -> Self {
        Self { wires: expected["events"].as_array().unwrap().iter().filter(|event| event["kind"] == "wire").cloned().collect(), endpoint }
    }
    async fn reply(&mut self, mut request: HttpRequestSpec, executor: &mut PolicyHttp) -> ReplyEnvelope {
        let observed = self.wires.pop_front().expect("unrecorded HTTP request");
        assert_eq!(request.url, observed["destination"].as_str().unwrap());
        assert_eq!(serde_json::from_slice::<Value>(&request.body).unwrap(), observed["body"]);
        request.url = format!("{}/v1/embeddings", self.endpoint);
        executor.execute(request, None).await.unwrap()
    }
}
async fn settle_observation<R: Retrieval + Send + 'static>(mut retrieval: R, mut step: Result<Step<RetrievalOutput>, StageError>,
    http: &mut ObservationHttp, executor: &mut PolicyHttp, budget: &Budget) -> (R, RetrievalOutput)
{
    loop {
        match step.unwrap() {
            Step::Done(output) => return (retrieval, output),
            Step::NeedsHost(HostRequest::Http(request)) => {
                let reply = http.reply(request, executor).await;
                (retrieval, step) = retrieval_http_call(retrieval, executor, reply, budget).await;
            }
            _ => panic!("HTTP retrieval boundary"),
        }
    }
}

#[tokio::test]
async fn product_policy_cache_and_embedding_adapter_match_actual_typescript_retrieval_observations() {
    if isolate_loopback_test(concat!(module_path!(), "::product_policy_cache_and_embedding_adapter_match_actual_typescript_retrieval_observations")) { return; }
    let cases: Value = serde_json::from_str(include_str!("../../../tests/fixtures/rust-core/memory/hypa-v1/observations.cases.json")).unwrap();
    let expected: Value = serde_json::from_str(include_str!("../../../tests/fixtures/rust-core/memory/hypa-v1/observations.expected.json")).unwrap();
    for id in ["ranking-63-alternating", "fresh-f64-remains-distinct-from-cached-f32-near-tie",
        "warm-nfc-collision-keeps-original-text-duplicates", "stale-cache-recompute-retains-duplicate-order"] {
        let input = &cases["cases"].as_array().unwrap().iter().find(|case| case["id"] == id).unwrap()["input"];
        let expected = &expected["cases"].as_array().unwrap().iter().find(|case| case["id"] == id).unwrap()["result"];
        let fixture = StoreFixture::new();
        let mut server = Loopback::new(observation_replies(input)).await;
        let (operation, budget, slots) = operation();
        seed_observation(&fixture, input, &budget).await;
        let settings = Arc::new(Mutex::new(Settings { model: input["target"]["customModel"].as_str().map(str::to_owned), key: None }));
        let retrieval = PolicyRetrieval::new(observation_target(input, &budget), fixture.cache(None, None), capture(settings),
            input["target"]["cacheBatchLimit"].as_u64().unwrap() as usize, &operation).unwrap();
        let mut executor = PolicyHttp::new(operation.clone(), limits()).unwrap();
        let mut http = ObservationHttp::new(expected, server.url.clone());
        let docs = input["documents"].as_array().unwrap().iter().map(|value| Text::from_str(value.as_str().unwrap(), &budget).unwrap()).collect();
        let (retrieval, step) = policy_retrieval_call(retrieval, RetrievalCall::Prepare(docs), &budget).await;
        let (mut retrieval, output) = settle_observation(retrieval, step, &mut http, &mut executor, &budget).await;
        assert!(matches!(output, RetrievalOutput::Prepared));
        for (query, expected) in input["queries"].as_array().unwrap().iter().zip(expected["outcome"]["queries"].as_array().unwrap()) {
            let (next, step) = policy_retrieval_call(retrieval, RetrievalCall::Query(Text::from_str(query.as_str().unwrap(), &budget).unwrap()), &budget).await;
            let (next, output) = settle_observation(next, step, &mut http, &mut executor, &budget).await; retrieval = next;
            let RetrievalOutput::Scored(rows) = output else { panic!("scores") };
            let actual: Vec<_> = rows.iter().map(|row| (row.document.as_ref().and_then(Text::as_unicode), row.score.to_bits())).collect();
            let expected: Vec<_> = expected.as_array().unwrap().iter().map(|row| (row[0].as_str(), observation_number(&row[1]).to_bits())).collect();
            assert_eq!(actual, expected, "{id}");
            drop(rows);
        }
        assert!(http.wires.is_empty());
        for _ in input["embeddings"].as_array().unwrap() { server.captured().await; }
        drop(retrieval); drop(executor); drop(operation);
        assert_eq!(slots.available_permits(), 1); assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}

#[tokio::test]
async fn actual_typescript_hanurai_three_queries_compose_with_the_product_policy_adapter() {
    if isolate_loopback_test(concat!(module_path!(), "::actual_typescript_hanurai_three_queries_compose_with_the_product_policy_adapter")) { return; }
    use risunest_memory::hanurai::{Hanurai, HanuraiConfig};
    let cases: Value = serde_json::from_str(include_str!("../../../tests/fixtures/rust-core/memory/hypa-v1/observations.cases.json")).unwrap();
    let expected: Value = serde_json::from_str(include_str!("../../../tests/fixtures/rust-core/memory/hypa-v1/observations.expected.json")).unwrap();
    let id = "hanurai-excess-missing-document-returns-normally";
    let input = &cases["cases"].as_array().unwrap().iter().find(|case| case["id"] == id).unwrap()["input"];
    let expected = &expected["cases"].as_array().unwrap().iter().find(|case| case["id"] == id).unwrap()["result"];
    let fixture = StoreFixture::new(); let mut server = Loopback::new(observation_replies(input)).await;
    let (operation, budget, slots) = operation();
    let settings = Arc::new(Mutex::new(Settings { model: input["target"]["customModel"].as_str().map(str::to_owned), key: None }));
    let retrieval = PolicyRetrieval::new(observation_target(input, &budget), fixture.cache(None, None), capture(settings), 64, &operation).unwrap();
    let args = &input["hanurai"];
    let config = HanuraiConfig { split: args["split"].as_bool().unwrap(), reserve_tokens: args["reserve"].as_f64().unwrap(),
        context_limit_error: Text::from_str(expected["contextLimitError"].as_str().unwrap(), &budget).unwrap() };
    let stage = Hanurai::new(config, HostTokens, retrieval);
    let chats = args["chats"].as_array().unwrap().iter().map(|value| PromptMessage::from_value(
        risunest_ir::json::parse_json(&value.to_string(), &budget).unwrap(), &budget).unwrap()).collect();
    let (mut stage, mut step) = memory_call(stage, MemoryCall::Start(MemoryInput { chats, current_tokens: args["currentTokens"].as_f64().unwrap(),
        max_context_tokens: args["maxContextTokens"].as_f64().unwrap() }), &budget).await;
    let mut http = ObservationHttp::new(expected, server.url.clone());
    let mut counts: std::collections::VecDeque<_> = expected["events"].as_array().unwrap().iter().filter(|event| event["kind"] == "token").collect();
    let mut executor = PolicyHttp::new(operation.clone(), limits()).unwrap();
    let output = loop {
        let reply = match step.unwrap() {
            Step::Done(output) => break output,
            Step::NeedsHost(HostRequest::Http(request)) => {
                let reply = http.reply(request, &mut executor).await;
                (stage, step) = memory_http_call(stage, &mut executor, reply, &budget).await;
                continue;
            },
            Step::NeedsHost(HostRequest::PluginTokenizer { text }) => {
                let event = counts.pop_front().expect("recorded tokenizer boundary");
                assert_eq!(text.as_unicode(), event["message"]["content"].as_str());
                HostReply::Tokens(vec![0.0; event["count"].as_u64().unwrap() as usize])
            }
            _ => panic!("recorded Hanurai boundary"),
        };
        (stage, step) = memory_call(stage, MemoryCall::Resume(reply), &budget).await;
    };
    assert!(output.error.is_none()); assert!(output.effects.is_empty());
    let chats: Vec<Value> = output.chats.iter().map(|message| serde_json::from_str(risunest_ir::json::stringify(
        &message.to_value(&budget).unwrap(), &budget).unwrap().unwrap().as_unicode().unwrap()).unwrap()).collect();
    assert_eq!(json!(chats), expected["outcome"]["chats"]);
    assert_eq!(output.current_tokens, expected["outcome"]["tokens"].as_f64().unwrap());
    assert!(http.wires.is_empty() && counts.is_empty());
    for _ in input["embeddings"].as_array().unwrap() { server.captured().await; }
    drop(output); drop(stage); drop(executor); drop(operation);
    assert_eq!(slots.available_permits(), 1); assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}


#[tokio::test]
async fn actual_summary_and_embedding_headers_match_plugin_wire_and_reject_unsendable_keys_without_http() {
    if isolate_loopback_test(concat!(module_path!(), "::actual_summary_and_embedding_headers_match_plugin_wire_and_reject_unsendable_keys_without_http")) { return; }
    use risunest_memory::hypa_v2::SummaryService;
    for producer in ["summary", "embedding"] {
        let mut server = Loopback::new(vec![Reply::plain(429, JSON), Reply::plain(429, JSON)]).await;
        let (operation, budget, slots) = operation();
        let key = Text::from_str("caf\u{e9}\t  ", &budget).unwrap();
        let mut summary = Summary::supa(&Text::from_str("curie", &budget).unwrap(), &Text::from_str("", &budget).unwrap(), Some(&key),
            Processor::new(Snapshot::default(), Tape::new(Vec::new())), &budget).unwrap();
        let mut embedding = Service::new();
        let call = if producer == "summary" {
                let Step::NeedsHost(request) = summary.summarize(Text::from_str("synthetic history", &budget).unwrap(), &budget).unwrap() else { panic!("real summary request") };
                request
            } else {
                let target = RemoteTarget::capture(&Text::from_str("custom", &budget).unwrap(), None,
                    Some(&Text::from_str(&format!("{}/v1", server.url), &budget).unwrap()), None, &budget).unwrap();
                let config = CapturedConfig::capture(None, Some(&key), None, None, &budget).unwrap();
                let step = embedding.run(embeddings::Request { target, config, purpose: Purpose::Query,
                    input: Input::Scalar(Text::from_str("synthetic query", &budget).unwrap()) }, &budget).unwrap();
                let Step::NeedsHost(request) = step else { panic!("real embedding request") };
                request
            };
        let HostRequest::Http(mut input) = call else { panic!("real HTTP request") };
        input.url = format!("{}/v1/embeddings", server.url);
        assert_eq!(input.headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("authorization")).map(|(_, value)| value.as_str()), Some("Bearer caf\u{e9}"));
        let (plugin_operation, _, _) = self::operation();
        let baseline = native_plugin(input.url.clone(), input.body.clone(), input.headers.clone(), plugin_operation).await.unwrap();
        let before = server.captured().await;
        let plugin_headers = server.captured_raw_headers().await;
        for (name, value) in &mut input.headers { if name.eq_ignore_ascii_case("content-type") { *value = format!("\r\n\t{value} \t\r\n"); } }
        let mut executor = PolicyHttp::new(operation.clone(), limits()).unwrap();
        let reply = executor.execute(input, None).await.unwrap();
        let actual = executor.resume(reply, |reply| { let HostReply::Http { status, body } = reply else { panic!("raw status/body") }; WireReply { status, body } }).unwrap();
        let after = server.captured().await; let product_headers = server.captured_raw_headers().await;
        assert_eq!(actual, baseline); assert_eq!(after, before, "{producer}");
        assert_eq!(product_headers, plugin_headers, "{producer}");
        assert!(!after.headers.contains_key("origin"));
        let authorization = product_headers.iter().find(|line| line.to_ascii_lowercase().starts_with("authorization:")).unwrap();
        assert!(authorization.as_bytes().windows(2).any(|part| part == [0xc3, 0xa9]), "plugin from_str emits UTF-8 Latin-1 scalars");
        drop(executor); drop(summary); drop(embedding); drop(key); drop(operation);
        assert_eq!(slots.available_permits(), 1); assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
    let mut server = Loopback::new(vec![Reply::plain(200, JSON)]).await;
    let (operation, budget, slots) = operation();
    let mut executor = PolicyHttp::new(operation.clone(), limits()).unwrap();
    for invalid in ["unicode", "control", "surrogate"] {
        let key = match invalid { "unicode" => Text::from_str("\u{100}", &budget).unwrap(), "control" => Text::from_str("synthetic\u{1}", &budget).unwrap(),
            _ => Text::from_units(&[0xd800], &budget).unwrap() };
        let target = RemoteTarget::capture(&Text::from_str("custom", &budget).unwrap(), None,
            Some(&Text::from_str(&format!("{}/v1", server.url), &budget).unwrap()), None, &budget).unwrap();
        let config = CapturedConfig::capture(None, Some(&key), None, None, &budget).unwrap();
        let mut service = Service::new();
        let Step::NeedsHost(HostRequest::Http(input)) = service.run(embeddings::Request { target, config, purpose: Purpose::Query,
            input: Input::Scalar(Text::from_str("synthetic query", &budget).unwrap()) }, &budget).unwrap() else { panic!("real raw embedding headers") };
        let reply = executor.execute(input, None).await.unwrap();
        executor.resume(reply, |reply| assert!(matches!(reply, HostReply::Failed { .. }), "{invalid}")).unwrap();
        assert_eq!(executor.phase, Phase::Ready);
        assert!(budget.check().is_ok());
        assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)), "{invalid}");
    }
    drop(executor); drop(operation);
    assert_eq!(slots.available_permits(), 1); assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}


fn log_listing(recorder: &risunest_network_log::Recorder) -> risunest_network_log::Listing {
    recorder.list(risunest_network_log::Query { retained: false, search: String::new(), status: String::new(), offset: 0 }).unwrap()
}
fn exported_log(recorder: &risunest_network_log::Recorder, id: String) -> Value {
    let mut snapshot = recorder.snapshot(id).unwrap();
    let mut bytes = Vec::new(); snapshot.export_json(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn real_memory_and_shared_ollama_http_obey_network_log_capture_masking_and_disabled_settings() {
    if isolate_loopback_test(concat!(module_path!(), "::real_memory_and_shared_ollama_http_obey_network_log_capture_masking_and_disabled_settings")) { return; }
    use crate::native_generation::ollama_transport::{self, BufferedOllamaRequest, CapturedOllamaConfig, TransportLimits};
    let directory = tempfile::tempdir().unwrap();
    let recorder = risunest_network_log::Recorder::open(directory.path().join("recent"), directory.path().join("retained")).unwrap();
    recorder.configure(risunest_network_log::Settings { enabled: true, hide_auth: true, capture_binary: false, ..Default::default() }).unwrap();
    risunest_network_log::install(recorder.clone());
    let compressed = decode::tests::encode(decode::Coding::Gzip, JSON);
    let ollama_reply = br#"{"message":{"role":"assistant","content":"logged reply"},"done":true}"#;
    let mut server = Loopback::new(vec![Reply::plain(200, JSON), Reply::encoded(compressed.clone(), "gzip"), Reply::encoded(compressed.clone(), "gzip"),
        Reply::plain(200, ollama_reply), Reply::plain(200, JSON), Reply::plain(200, ollama_reply)]).await;
    let (operation, budget, slots) = operation();
    let mut executor = PolicyHttp::new(operation.clone(), limits()).unwrap();
    let mut ids = std::collections::BTreeSet::new();
    for (gzip, binary) in [(false, false), (true, false), (true, true)] {
        recorder.configure(risunest_network_log::Settings { enabled: true, hide_auth: true, capture_binary: binary, ..Default::default() }).unwrap();
        let mut input = request(format!("{}/v1/embeddings", server.url));
        input.headers.push(("Authorization".into(), "Bearer synthetic-memory-secret".into()));
        let request_body = input.body.clone();
        let reply = executor.execute(input, None).await.unwrap();
        executor.resume(reply, |reply| { let HostReply::Http { status: 200, body } = reply else { panic!("memory reply") }; assert_eq!(body, JSON); }).unwrap();
        server.captured().await;
        let listing = log_listing(&recorder);
        let added: Vec<_> = listing.items.iter().filter(|item| !ids.contains(&item.id)).collect();
        assert_eq!(added.len(), 1);
        let id = added[0].id.clone(); ids.insert(id.clone());
        let log = exported_log(&recorder, id);
        assert_eq!(log["source"], "native-memory-http");
        assert_eq!(log["request"]["body"]["data"], std::str::from_utf8(&request_body).unwrap());
        assert_eq!(log["response"]["status"], 200);
        assert!(!log.to_string().contains("synthetic-memory-secret"));
        let authorization = log["request"]["headers"].as_array().unwrap().iter().find(|header| header["name"] == "authorization").unwrap();
        assert_ne!(authorization["value"], "Bearer synthetic-memory-secret");
        if gzip {
            assert_eq!(log["response"]["body"]["representation"], "encoded-gzip");
            assert_eq!(log["response"]["body"]["observedBytes"], compressed.len());
            assert_eq!(log["response"]["body"]["capturedBytes"], if binary { compressed.len() } else { 0 });
            assert_eq!(log["response"]["body"]["data"], if binary { compressed.iter().map(|byte| format!("{byte:02x}")).collect::<String>() } else { String::new() });
        } else {
            assert_eq!(log["response"]["body"]["representation"], "observed");
            assert_eq!(log["response"]["body"]["data"], std::str::from_utf8(JSON).unwrap());
        }
    }
    let ollama_body = risunest_ir::json::parse_json(r#"{"model":"synthetic","messages":[{"role":"user","content":"synthetic history"}],"stream":false}"#, &budget).unwrap();
    let user = Text::from_str("Synthetic", &budget).unwrap();
    let messages = [PromptMessage::new(Role::User, Text::from_str("synthetic history", &budget).unwrap())];
    let host = server.url.clone();
    let config = CapturedOllamaConfig { host: &host, model: "synthetic", streaming: false, think: None, browser_user_agent: "Synthetic RisuNest" };
    let transport_limits = TransportLimits { decode: limits().decode, max_request_bytes: 65536, max_url_bytes: 4096, max_user_agent_bytes: 4096,
        max_buffered_bytes: 65536, max_line_bytes: 65536, max_output_bytes: 65536, max_output_value_units: 65536 };
    let value = ollama_transport::buffered(BufferedOllamaRequest { body: &ollama_body, messages: &messages, user_name: &user },
        &config, transport_limits, &budget, operation.cancel()).await.unwrap();
    assert!(matches!(value, risunest_ir::Value::Text(ref text) if text.equals_ascii(b"logged reply"))); drop(value);
    let wire = server.captured().await;
    let listing = log_listing(&recorder);
    let added: Vec<_> = listing.items.iter().filter(|item| !ids.contains(&item.id)).collect(); assert_eq!(added.len(), 1);
    let id = added[0].id.clone(); ids.insert(id.clone());
    let log = exported_log(&recorder, id);
    assert_eq!(log["source"], "native-generation-ollama");
    assert_eq!(log["request"]["body"]["data"], std::str::from_utf8(&wire.body).unwrap());
    assert_eq!(log["response"]["body"]["data"], std::str::from_utf8(ollama_reply).unwrap());
    recorder.configure(risunest_network_log::Settings { enabled: false, ..Default::default() }).unwrap();
    let reply = executor.execute(request(format!("{}/v1/embeddings", server.url)), None).await.unwrap(); executor.resume(reply, drop).unwrap(); server.captured().await;
    let value = ollama_transport::buffered(BufferedOllamaRequest { body: &ollama_body, messages: &messages, user_name: &user },
        &config, transport_limits, &budget, operation.cancel()).await.unwrap(); drop(value); server.captured().await;
    assert_eq!(log_listing(&recorder).items.len(), ids.len(), "disabled capture creates no entry for either transport");
    recorder.shutdown();
    drop(messages); drop(user); drop(ollama_body); drop(executor); drop(operation);
    assert_eq!(slots.available_permits(), 1); assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}
