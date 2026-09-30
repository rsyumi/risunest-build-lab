use super::*;
use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};

#[test]
fn successful_pending_replies_are_bounded_and_upload_resumes_the_same_id() {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let complete = Arc::new(AtomicBool::new(false));
    let polls = Arc::new(AtomicUsize::new(0));
    let deleted = Arc::new(AtomicUsize::new(0));
    let target = "a".repeat(64);
    let handler_target = target.clone();
    let handler_complete = complete.clone();
    let handler_polls = polls.clone();
    let handler_deleted = deleted.clone();
    let entered = runtime.enter();
    let router = axum::Router::new().fallback(move |request: axum::extract::Request| {
        let target = handler_target.clone();
        let complete = handler_complete.clone();
        let polls = handler_polls.clone();
        let deleted = handler_deleted.clone();
        async move {
            let method = request.method().clone();
            let path = request.uri().path().to_owned();
            axum::body::to_bytes(request.into_body(), MAX_METADATA_BYTES).await.unwrap();
            let (status, body) = match (method, path.as_str()) {
                (Method::POST, "/objects/delta") => (202, br#"{"jobId":"synthetic-delta"}"#.to_vec()),
                (Method::GET, "/object-deltas/synthetic-delta") => {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    (202, Vec::new())
                }
                (Method::DELETE, "/object-deltas/synthetic-delta") => {
                    deleted.fetch_add(1, Ordering::SeqCst);
                    (204, Vec::new())
                }
                (Method::GET, "/uploads/synthetic-upload") => {
                    polls.fetch_add(1, Ordering::SeqCst);
                    (200, serde_json::to_vec(&serde_json::json!({
                        "uploadId":"synthetic-upload", "manifest":{"hash":target,"size":"10"},
                        "chunkBytes":CHUNK.to_string(), "verified":[], "nextAfter":null,
                        "complete":complete.load(Ordering::SeqCst), "finishing":true,
                        "failure":null,"retryableFailure":null,
                    })).unwrap())
                }
                _ => panic!("unexpected synthetic transfer request: {path}"),
            };
            axum::response::Response::builder().status(status).body(axum::body::Body::from(body)).unwrap()
        }
    });
    drop(entered);
    let task = runtime.spawn(async move { axum::serve(listener, router).await.unwrap(); });
    let client = ServerClient::new(super::super::client::ServerConfig {
        directory: None, endpoint, library_id:"library".into(), device_id:"device".into(), token:"a".repeat(64),
    }).unwrap();
    let root = tempfile::tempdir().unwrap();
    let cache = Cache::open(root.path()).unwrap();
    let mut transfer = Transfer::new(&client, &cache).unwrap();
    transfer.pending_timeout = std::time::Duration::from_millis(20);
    let base = "b".repeat(64);
    *transfer.base_sizes.borrow_mut() = Some((vec![base.clone()], vec![(base.clone(), 17 * 1024 * 1024)]));
    assert!(!transfer.download_delta(&target, 17 * 1024 * 1024, &[base]).unwrap());
    assert_eq!(deleted.load(Ordering::SeqCst), 1);
    transfer.db.execute("INSERT INTO uploads VALUES(?1,'synthetic-upload','10')", [&target]).unwrap();
    let error = transfer.wait_upload("synthetic-upload", &target, 10).unwrap_err();
    assert_eq!(error.code, "upload-finalization-pending");
    assert!(error.retryable);
    let retained: String = transfer.db.query_row("SELECT id FROM uploads WHERE hash=?1", [&target], |row| row.get(0)).unwrap();
    assert_eq!(retained, "synthetic-upload");
    complete.store(true, Ordering::SeqCst);
    transfer.wait_upload(&retained, &target, 10).unwrap();
    assert_eq!(polls.load(Ordering::SeqCst), 2);
    assert_eq!(transfer.db.query_row("SELECT count(*) FROM uploads", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    task.abort();
}
