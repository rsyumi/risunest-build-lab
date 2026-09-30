use super::*;
use crate::persistent_store::commands::{with_store, PersistentStoreState};
use super::super::{events::ServerSyncEventsState, residency::AssetPolicy};
use risunest_sync_server::{http, store::Store};
use tauri::test::MockRuntime;

struct Fixture {
    app: tauri::App<MockRuntime>,
    server: Arc<Store>,
    task: tokio::task::JoinHandle<()>,
    runtime: tokio::runtime::Runtime,
    config: ServerConfig,
    _root: tempfile::TempDir,
    _server_root: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let server_root = tempfile::tempdir().unwrap();
        let server = Arc::new(Store::init(server_root.path()).unwrap());
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let serving = server.clone();
        let task = runtime.spawn(async move { axum::serve(listener, http::router(serving)).await.unwrap(); });
        let registration = server.add_device().unwrap();
        let config = ServerConfig { directory: None, endpoint, library_id: registration.library_id,
            device_id: registration.device_id, token: registration.token };
        let app = tauri::test::mock_builder().build(tauri::test::mock_context(tauri::test::noop_assets())).unwrap();
        app.manage(PersistentStoreState::with_test_store(PersistentStore::open(root.path()).unwrap()));
        app.manage(crate::native_file_jobs::NativeFileJobState::initialize(root.path().join("native-file-jobs")));
        app.manage(ServerSyncCommandState::default());
        app.manage(ServerSyncEventsState::default());
        Self { _root: root, _server_root: server_root, server, runtime, task, app, config }
    }
    fn policy(&self) -> AssetPolicy {
        with_store(self.app.state(), |store| store.device_store()?.asset_residency_policy()).unwrap()
    }
    fn revision(&self) -> i64 { with_store(self.app.state(), |store| store.revision()).unwrap() }
    fn assert_released(&self) {
        let state = self.app.state::<ServerSyncCommandState>();
        assert!(!state.running.load(Ordering::Acquire));
        drop(self.app.state::<crate::native_file_jobs::NativeFileJobState>().admission.server().unwrap());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { self.task.abort(); }
}

#[test]
fn binding_policy_rolls_back_on_refusal_and_listener_is_released_only_on_commit() {
    let fixture = Fixture::new();
    let app = fixture.app.handle();
    let events = app.state::<ServerSyncEventsState>();
    let listener = events.hold_test_listener();
    server_sync_bind_operation(app, fixture.config.clone(), Some(AssetPolicy::Remote)).unwrap();
    assert_eq!(fixture.policy(), AssetPolicy::Remote);
    assert!(*listener.borrow());
    assert!(!events.has_test_listener());
    let listener = events.hold_test_listener();
    let error = server_sync_bind_operation(app, fixture.config.clone(), Some(AssetPolicy::Full)).err().expect("binding an already-bound server must fail");
    assert_eq!(error.code, "server-already-bound");
    assert_eq!(fixture.policy(), AssetPolicy::Remote);
    assert!(!*listener.borrow());
    assert!(events.has_test_listener());
    fixture.assert_released();

    fixture.server.revoke_device(&fixture.config.device_id).unwrap();
    let next = fixture.server.add_device().unwrap();
    let next = ServerConfig { device_id: next.device_id, token: next.token, ..fixture.config.clone() };
    let error = server_sync_reregister_operation(app, next.clone(), fixture.revision() + 1, Some(AssetPolicy::Full)).err().expect("reregistering after a revision change must fail");
    assert_eq!(error.code, "local-revision-changed");
    assert_eq!(fixture.policy(), AssetPolicy::Remote);
    assert!(!*listener.borrow());
    assert!(events.has_test_listener());
    fixture.assert_released();
    server_sync_reregister_operation(app, next, fixture.revision(), Some(AssetPolicy::Full)).unwrap();
    assert_eq!(fixture.policy(), AssetPolicy::Full);
    assert!(*listener.borrow());
    assert!(!events.has_test_listener());
    let listener = events.hold_test_listener();
    server_sync_unbind_operation(app).unwrap();
    assert!(*listener.borrow());
    assert!(!events.has_test_listener());
    assert!(!job_store(app).unwrap().server_status().unwrap().configured);
    fixture.assert_released();
}

#[test]
fn unavailable_job_store_returns_retryable_fault_and_drops_command_admission() {
    let fixture = Fixture::new();
    fixture.app.unmanage::<PersistentStoreState>().unwrap();
    fixture.app.manage(PersistentStoreState::default());
    let error = server_sync_asset_evict_operation(fixture.app.handle()).err().expect("eviction without a local store must fail");
    assert_eq!((error.code.as_str(), error.status, error.retryable), ("local-store-unavailable", 503, true));
    fixture.assert_released();
    fixture.app.unmanage::<PersistentStoreState>().unwrap();
    fixture.app.manage(PersistentStoreState::with_test_store(PersistentStore::open(fixture._root.path()).unwrap()));
    assert!(!job_store(fixture.app.handle()).unwrap().server_status().unwrap().configured);
}

#[test]
fn cancel_reaches_running_asset_command_and_retry_gets_a_fresh_flag() {
    let fixture = Fixture::new();
    server_sync_bind_operation(fixture.app.handle(), fixture.config.clone(), Some(AssetPolicy::Remote)).unwrap();
    let guard = crate::asset_repository::coordinator::lock_repository_mutation().unwrap();
    let (sent, received) = std::sync::mpsc::channel();
    let handle = fixture.app.handle().clone();
    let worker = std::thread::spawn(move || { sent.send(server_sync_asset_policy_operation(&handle, AssetPolicy::Full)).unwrap(); });
    let state = fixture.app.state::<ServerSyncCommandState>();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !state.running.load(Ordering::Acquire) {
        assert!(std::time::Instant::now() < deadline, "command entered native running ownership");
        std::thread::yield_now();
    }
    let cancelled = state.cancelled.lock().unwrap().clone();
    state.cancel().unwrap();
    assert!(cancelled.load(Ordering::Acquire));
    drop(guard);
    let error = received.recv_timeout(std::time::Duration::from_secs(3)).unwrap().err().expect("a cancelled asset policy command must fail");
    worker.join().unwrap();
    assert_eq!(error.code, "cancelled");
    fixture.assert_released();
    server_sync_asset_policy_operation(fixture.app.handle(), AssetPolicy::Remote).unwrap();
    let current = state.cancelled.lock().unwrap().clone();
    assert!(!Arc::ptr_eq(&current, &cancelled));
    assert!(!current.load(Ordering::Acquire));
    assert_eq!(fixture.policy(), AssetPolicy::Remote);
    fixture.assert_released();
}
