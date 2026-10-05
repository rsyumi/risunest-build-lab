use super::*;
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn only_upload_delta_cache_quota_falls_back_to_full_chunks() {
    let base = vec![23; risunest_sync_wire::delta::MAX_TARGET_BYTES + 1024 * 1024];
    let mut target = base.clone();
    target[12345] = 24;
    for (status, code, fallback) in [
        (429, "delta-cache-quota", true),
        (429, "unknown-quota", false),
        (403, "forbidden", false),
    ] {
        let deltas = Arc::new(AtomicUsize::new(0));
        let chunks = Arc::new(AtomicUsize::new(0));
        let delta_count = deltas.clone();
        let chunk_count = chunks.clone();
        let server = crate::server_sync::lww_tests::LocalServerFixture::with_router(|router| {
            router.layer(axum::middleware::from_fn(move |request: axum::extract::Request, next: axum::middleware::Next| {
                let deltas = delta_count.clone();
                let chunks = chunk_count.clone();
                async move {
                    let path = request.uri().path();
                    if path.starts_with("/uploads/") && path.ends_with("/delta") {
                        deltas.fetch_add(1, Ordering::SeqCst);
                        axum::body::to_bytes(request.into_body(), risunest_sync_wire::transfer::MAX_BATCH_BYTES).await.unwrap();
                        return axum::response::Response::builder().status(status)
                            .body(axum::body::Body::from(format!(r#"{{"error":"{code}"}}"#))).unwrap();
                    }
                    if path.contains("/chunks/") { chunks.fetch_add(1, Ordering::SeqCst); }
                    next.run(request).await
                }
            }))
        });
        let credential = server.server.add_device().unwrap();
        let client = ServerClient::new(ServerConfig {
            directory: None, endpoint: server.endpoint.clone(), library_id: credential.library_id,
            device_id: credential.device_id, token: credential.token,
        }).unwrap();
        let local = tempfile::tempdir().unwrap();
        let cache = Cache::open(local.path()).unwrap();
        let base_hash = cache.put(&base).unwrap();
        let target_hash = cache.put(&target).unwrap();
        let transfer = Transfer::new(&client, &cache).unwrap();
        let result = transfer.upload(std::slice::from_ref(&target_hash), &[base_hash]);
        assert_eq!(deltas.load(Ordering::SeqCst), 1);
        let progress = client.lane().snapshot("send");
        assert_eq!(progress.files_total, 1);
        assert_eq!(progress.bytes_total, target.len() as u64);
        if fallback {
            result.unwrap();
            assert_eq!((progress.files_done, progress.bytes_done), (1, target.len() as u64));
            assert!(progress.sent_bytes > target.len() as u64);
            assert!(chunks.load(Ordering::SeqCst) > 0);
            let (mut body, size) = server.server.open_object(&target_hash).unwrap();
            assert_eq!(size, target.len() as u64);
            let mut actual = Vec::new();
            body.read_to_end(&mut actual).unwrap();
            assert_eq!(actual, target);
        } else {
            let error = result.unwrap_err();
            assert_eq!((progress.files_done, progress.bytes_done), (0, 0));
            assert_eq!((error.status, error.code.as_str()), (status, code));
            assert_eq!(chunks.load(Ordering::SeqCst), 0);
            assert_eq!(server.server.object_size(&target_hash).unwrap(), None);
        }
    }
}
