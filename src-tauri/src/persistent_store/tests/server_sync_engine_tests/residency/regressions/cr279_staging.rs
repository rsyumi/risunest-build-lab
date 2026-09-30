use super::*;

#[test]
fn staged_hydration_allows_nine_media_requests_alias_deletion_and_gc() {
    use crate::persistent_store::asset_object_catalog::AssetObjectRegistration;
    use crate::server_sync::residency::HydrationSession;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    let fixture = Fixture::new();
    let (directory, mut store) = prepared();
    fixture.bind(&mut store);
    let bytes = vec![31_u8; 256 * 1024 + 1];
    let alias = put(&mut store, "assets/staged-hydration.png", &bytes);
    let hash = alias.object_hash.as_ref().unwrap().clone();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    let cas = PayloadCas::new(store.repository_root()).unwrap();
    assert_eq!(cas.stat_object(&hash).unwrap(), None);
    let media_bytes = b"synthetic concurrent local media";
    let media = cas.prepare_bytes(media_bytes).unwrap();
    let garbage = cas.prepare_bytes(b"synthetic GC candidate").unwrap();
    store.asset_object_catalog().register(&[AssetObjectRegistration {
        object_hash: garbage.content_hash.clone(), byte_size: garbage.byte_size,
    }], 1).unwrap();
    let server = crate::native_media::streaming::MediaServer::start(directory.path().to_path_buf()).unwrap();
    let media_url = format!("{}{}?mime=application%2Foctet-stream&size={}",
        server.test_base_url(), hex::encode(&media.physical_key), media.byte_size);
    let (blocked_tx, blocked_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let hydration = {
        let root = directory.path().to_path_buf();
        let hash = hash.clone();
        std::thread::spawn(move || {
            let blocked = AtomicBool::new(false);
            let mut session = HydrationSession::new(&root, None).unwrap();
            session.open(&hash, &|| {
                let writing = std::fs::read_dir(root.join("assets/staging")).ok()
                    .into_iter().flatten().filter_map(|entry| entry.ok())
                    .any(|entry| entry.metadata().map(|metadata| metadata.len() > 0).unwrap_or(false));
                if writing && !blocked.swap(true, Ordering::SeqCst) {
                    blocked_tx.send(()).map_err(|_| crate::server_sync::SyncError::new("test-gate-closed", 500))?;
                    resume_rx.recv_timeout(Duration::from_secs(30))
                        .map_err(|_| crate::server_sync::SyncError::new("test-gate-timeout", 500))?;
                }
                Ok(())
            }).unwrap().unwrap()
        })
    };
    blocked_rx.recv_timeout(Duration::from_secs(15)).expect("hydration blocked after a staged chunk");
    assert_eq!(cas.stat_object(&hash).unwrap(), None);
    let barrier = Arc::new(std::sync::Barrier::new(10));
    let (finished_tx, finished_rx) = mpsc::channel();
    let requests: Vec<_> = (0..9).map(|_| {
        let url = media_url.clone();
        let barrier = barrier.clone();
        let finished = finished_tx.clone();
        std::thread::spawn(move || {
            let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(15)).build().unwrap();
            barrier.wait();
            let response = client.get(url).send().unwrap();
            assert_eq!(response.status().as_u16(), 200);
            assert_eq!(response.bytes().unwrap().as_ref(), media_bytes);
            finished.send(()).unwrap();
        })
    }).collect();
    barrier.wait();
    let media_finished = (0..9).all(|_| finished_rx.recv_timeout(Duration::from_secs(15)).is_ok());
    let (gc_tx, gc_rx) = mpsc::channel();
    let gc_worker = {
        let alias = alias.clone();
        std::thread::spawn(move || {
            remove_roots(&mut store, &alias);
            let gc = store.asset_gc_delete_page(4096, None, i64::MAX / 2, 0).unwrap();
            assert!(gc.report.deletion_enabled);
            gc_tx.send(()).unwrap();
            store
        })
    };
    let gc_finished = gc_rx.recv_timeout(Duration::from_secs(15)).is_ok();
    assert_eq!(cas.stat_object(&hash).unwrap(), None);
    resume_tx.send(()).unwrap();
    let mut held = hydration.join().unwrap();
    for request in requests { request.join().unwrap(); }
    let mut store = gc_worker.join().unwrap();
    assert!(media_finished, "nine HTTP media requests must finish while hydration copy is blocked");
    assert!(gc_finished, "alias deletion and real GC must finish while hydration copy is blocked");
    assert_eq!(cas.stat_object(&garbage.content_hash).unwrap(), None);
    assert_eq!(cas.read_object(&hash).unwrap().unwrap(), bytes);
    assert!(store.read_asset_alias("asset", &alias.key, None).unwrap().is_none());
    store.asset_object_catalog().register(&[AssetObjectRegistration {
        object_hash: hash.clone(), byte_size: bytes.len() as u64,
    }], 1).unwrap();
    store.asset_gc_delete_page(4096, None, i64::MAX / 2, 0).unwrap();
    assert_eq!(cas.stat_object(&hash).unwrap(), None);
    let mut actual = Vec::new();
    std::io::Read::read_to_end(&mut held, &mut actual).unwrap();
    assert_eq!(actual, bytes);
    assert!(store.read_asset_alias("asset", &alias.key, None).unwrap().is_none());
    assert_eq!(std::fs::read_dir(directory.path().join("assets/staging")).unwrap().count(), 0);
}
