use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
struct Requests {
    checkpoints: AtomicUsize,
    pins: AtomicUsize,
    deletes: AtomicUsize,
    expire_pin: AtomicBool,
    refuse_delete: AtomicBool,
}

fn fixture() -> (Fixture, Arc<Requests>) {
    let root = tempfile::tempdir().unwrap();
    let server = Arc::new(Store::init(root.path()).unwrap());
    let requests = Arc::new(Requests::default());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let entered = runtime.enter();
    let observed = requests.clone();
    let router = http::router(server.clone()).layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let observed = observed.clone();
            async move {
                let path = request.uri().path();
                let post = request.method() == axum::http::Method::POST;
                if post && path == "/checkpoints" {
                    observed.checkpoints.fetch_add(1, Ordering::SeqCst);
                }
                let expired = post && path == "/read-pins";
                if expired {
                    observed.pins.fetch_add(1, Ordering::SeqCst);
                }
                let deleting = request.method() == axum::http::Method::DELETE
                    && (path.starts_with("/read-pins/") || path.starts_with("/checkpoints/"));
                if deleting {
                    observed.deletes.fetch_add(1, Ordering::SeqCst);
                }
                let response = if expired && observed.expire_pin.swap(false, Ordering::SeqCst) {
                    Some((410, r#"{"error":"journal-pruned"}"#))
                } else if deleting && observed.refuse_delete.load(Ordering::SeqCst) {
                    Some((403, r#"{"error":"forbidden"}"#))
                } else {
                    None
                };
                if let Some((status, body)) = response {
                    axum::body::to_bytes(request.into_body(), risunest_sync_wire::MAX_METADATA_BYTES)
                        .await.unwrap();
                    return axum::response::Response::builder()
                        .status(status)
                        .body(axum::body::Body::from(body))
                        .unwrap();
                }
                next.run(request).await
            }
        },
    ));
    drop(entered);
    let task = runtime.spawn(async move { axum::serve(listener, router).await.unwrap() });
    (Fixture { _server_root: root, server, runtime, task, endpoint }, requests)
}

fn edit(store: &mut PersistentStore, key: &str, value: &str) {
    store.commit(&WorkingSetCommit {
        plugin_storage: Some(vec![PluginStorageMutation::Set {
            owner: "synthetic-recovery".into(),
            key: key.into(),
            value: json!(value),
        }]),
        ..empty_working_set_commit(store.revision().unwrap())
    }).unwrap();
}

fn rows(store: &PersistentStore, table: &str) -> Vec<Vec<rusqlite::types::Value>> {
    let mut statement = store.connection.prepare(&format!("SELECT * FROM {table} ORDER BY rowid")).unwrap();
    let columns = statement.column_count();
    let result = statement.query_map([], |row| {
        (0..columns).map(|column| row.get(column)).collect()
    }).unwrap().map(|row| row.unwrap()).collect();
    result
}

#[derive(Clone, Copy)]
enum Failure {
    JournalBase,
    ExpiredPin,
    DeleteRefused,
}

fn recover_without_changing_local_state(failure: Failure) {
    let (fixture, requests) = fixture();
    let (_first_root, mut first) = prepared();
    let (_second_root, mut second) = prepared();
    fixture.bind(&mut first);
    fixture.bind(&mut second);
    assert_eq!(settle(&mut first).phase, "idle");
    assert_eq!(settle(&mut second).phase, "idle");
    edit(&mut first, "remote", "before");
    assert_eq!(settle(&mut first).phase, "idle");
    assert_eq!(settle(&mut second).phase, "idle");
    edit(&mut first, "remote", "after");
    assert_eq!(settle(&mut first).phase, "idle");
    edit(&mut second, "local", "preserved");
    let revision = second.revision().unwrap();
    let root = second.read_root(None).unwrap().value;
    let tables = ["plugin_storage", "server_sync_state", "server_sync_base", "server_sync_scope_base",
        "server_sync_dirty", "server_sync_operation", "server_sync_operation_records", "server_sync_operation_pages"];
    let before: Vec<_> = tables.iter().map(|table| rows(&second, table)).collect();
    assert!(!rows(&second, "server_sync_dirty").is_empty());
    requests.checkpoints.store(0, Ordering::SeqCst);
    requests.pins.store(0, Ordering::SeqCst);
    requests.deletes.store(0, Ordering::SeqCst);
    match failure {
        Failure::JournalBase => {
            let absent = serde_json::to_string(&risunest_sync_wire::RecordVersion::Absent).unwrap();
            let key = crate::logical_records::encode_logical_record_key(&crate::logical_records::LogicalRecordLocator::Plugin {
                owner: "synthetic-recovery".into(),
                storage_key: "remote".into(),
            }).unwrap();
            assert_eq!(second.connection.execute("UPDATE server_sync_remote SET version=?1 WHERE domain='library' AND key=?2", params![absent, key]).unwrap(), 1);
        }
        Failure::ExpiredPin => requests.expire_pin.store(true, Ordering::SeqCst),
        Failure::DeleteRefused => requests.refuse_delete.store(true, Ordering::SeqCst),
    }
    let client = crate::server_sync::client::ServerClient::new(second.server_config().unwrap().unwrap()).unwrap();
    let observed = fixture.server.head().unwrap();
    let refreshed = crate::server_sync::remote::refresh(&mut second.connection, &client, &observed, &[Domain::Library]).unwrap();
    assert_eq!(refreshed, observed);
    assert_eq!(second.revision().unwrap(), revision);
    assert_eq!(second.read_root(None).unwrap().value, root);
    for (table, expected) in tables.iter().zip(before) {
        assert_eq!(rows(&second, table), expected, "metadata recovery changed {table}");
    }
    assert_eq!(requests.pins.load(Ordering::SeqCst), 1);
    assert_eq!(requests.checkpoints.load(Ordering::SeqCst), if matches!(failure, Failure::DeleteRefused) { 0 } else { 1 });
    assert_eq!(requests.deletes.load(Ordering::SeqCst), 1);
    let complete: bool = second.connection.query_row("SELECT complete FROM server_sync_remote_cursor WHERE singleton=1", [], |row| row.get(0)).unwrap();
    assert!(complete);
    assert_eq!(settle(&mut second).phase, "idle");
    let generation = active_generation(&second.connection).unwrap();
    for (key, expected) in [("remote", "after"), ("local", "preserved")] {
        let value: String = second.connection.query_row("SELECT value FROM plugin_storage WHERE generation=?1 AND storage_key=?2", params![generation, key], |row| row.get(0)).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&value).unwrap(), json!(expected));
    }
}

#[test]
fn a_journal_base_mismatch_rebuilds_one_checkpoint_without_touching_local_state() {
    recover_without_changing_local_state(Failure::JournalBase);
}

#[test]
fn a_pruned_read_pin_rebuilds_one_checkpoint_without_touching_local_state() {
    recover_without_changing_local_state(Failure::ExpiredPin);
}

#[test]
fn refused_metadata_lease_cleanup_preserves_the_completed_fetch() {
    recover_without_changing_local_state(Failure::DeleteRefused);
}
