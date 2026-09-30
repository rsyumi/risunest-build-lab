use super::*;
use std::fs;

#[test]
fn dependency_promotion_bounds_wal_writes_and_preserves_durable_objects() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let cache = Cache::open(source.path()).unwrap();
    let mut store = PersistentStore::open(target.path()).unwrap();
    let hashes: Vec<_> = (0..256)
        .map(|i| {
            cache
                .put(format!("synthetic promotion {i:04}").as_bytes())
                .unwrap()
        })
        .collect();
    store
        .connection
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    store
        .connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    store
        .promote_server_dependencies(&cache, &hashes, || Ok(()))
        .unwrap();
    let page_size: i64 = store
        .connection
        .pragma_query_value(None, "page_size", |r| r.get(0))
        .unwrap();
    let bytes = fs::metadata(target.path().join("persistent/persistent.sqlite-wal"))
        .unwrap()
        .len();
    let frames = bytes.saturating_sub(32) / (page_size as u64 + 24);
    eprintln!("256 small objects produced {frames} WAL frames ({bytes} bytes)");
    assert!(
        frames < 128,
        "256 small objects produced {frames} WAL frames ({bytes} bytes)"
    );
    drop(store);
    let store = PersistentStore::open(target.path()).unwrap();
    let pins: i64 = store
        .connection
        .query_row("SELECT count(*) FROM server_sync_objects", [], |r| r.get(0))
        .unwrap();
    let catalog: i64 = store
        .connection
        .query_row("SELECT count(*) FROM asset_objects", [], |r| r.get(0))
        .unwrap();
    assert_eq!((pins, catalog), (256, 256));
    let cas = PayloadCas::new(target.path()).unwrap();
    for (i, hash) in hashes.iter().enumerate() {
        assert_eq!(
            cas.read_object(hash).unwrap().unwrap(),
            format!("synthetic promotion {i:04}").as_bytes()
        );
    }
}

#[test]
fn cancelled_promotion_keeps_durable_roots_and_resumes() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let cache = Cache::open(source.path()).unwrap();
    let mut store = PersistentStore::open(target.path()).unwrap();
    let hashes: Vec<_> = (0..300)
        .map(|i| {
            cache
                .put(format!("synthetic resumable {i}").as_bytes())
                .unwrap()
        })
        .collect();
    let checks = std::cell::Cell::new(0);
    let error = store
        .promote_server_dependencies(&cache, &hashes, || {
            checks.set(checks.get() + 1);
            if checks.get() == 258 {
                Err(SyncError::new("cancelled", 409))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
    assert_eq!(error.code, "cancelled");
    let count: i64 = store
        .connection
        .query_row("SELECT count(*) FROM server_sync_objects", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 256,
        "cancellation between batches leaves committed GC roots"
    );
    drop(store);
    let mut store = PersistentStore::open(target.path()).unwrap();
    store
        .promote_server_dependencies(&cache, &hashes, || Ok(()))
        .unwrap();
    let count: i64 = store
        .connection
        .query_row("SELECT count(*) FROM asset_objects", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 300);
    let cas = PayloadCas::new(target.path()).unwrap();
    for (i, hash) in hashes.iter().enumerate() {
        assert_eq!(
            cas.read_object(hash).unwrap().unwrap(),
            format!("synthetic resumable {i}").as_bytes()
        );
    }
}

#[test]
fn dependency_copy_yields_repository_lock_after_committing_roots() {
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let cache = Cache::open(source.path()).unwrap();
    let mut store = PersistentStore::open(target.path()).unwrap();
    let bytes = b"synthetic guarded promotion";
    let hash = cache.put(bytes).unwrap();
    let cas = PayloadCas::new(target.path()).unwrap();
    let media_bytes = b"synthetic concurrent promotion media";
    let media = cas.prepare_bytes(media_bytes).unwrap();
    let server = crate::native_media::streaming::MediaServer::start(target.path().to_path_buf()).unwrap();
    let media_url = format!("{}{}?mime=application%2Foctet-stream&size={}",
        server.test_base_url(), hex::encode(&media.physical_key), media.byte_size);
    let checks = std::cell::Cell::new(0);
    let readers = std::cell::RefCell::new(Vec::new());
    let collectors = std::cell::RefCell::new(Vec::new());
    let retained_garbage = std::cell::RefCell::new(None);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store.promote_server_dependencies(&cache, &[hash.clone()], || {
        checks.set(checks.get() + 1);
        let reader = rusqlite::Connection::open(target.path().join("persistent/persistent.sqlite"))?;
        let rooted: bool = reader.query_row("SELECT EXISTS(SELECT 1 FROM server_sync_objects WHERE hash=?1)",
            [&hash], |row| row.get(0))?;
        let catalogued: bool = reader.query_row("SELECT EXISTS(SELECT 1 FROM asset_objects WHERE object_hash=?1)",
            [&hash], |row| row.get(0))?;
        assert_eq!(rooted, checks.get() == 2);
        assert!(!catalogued);
        assert_eq!(cas.stat_object(&hash).unwrap(), None);
        let (gc_sender, gc_receiver) = std::sync::mpsc::channel();
        let root = target.path().to_path_buf();
        let promoted_hash = hash.clone();
        let barrier_index = checks.get();
        collectors.borrow_mut().push(std::thread::spawn(move || {
            let cas = PayloadCas::new(&root).unwrap();
            let garbage = cas.prepare_bytes(format!("synthetic promotion GC {barrier_index}").as_bytes()).unwrap();
            let mut collector = PersistentStore::open(&root).unwrap();
            collector.asset_object_catalog().register(&[AssetObjectRegistration {
                object_hash: garbage.content_hash.clone(), byte_size: garbage.byte_size,
            }], 1).unwrap();
            let gc = collector.asset_gc_delete_page(4096, None, i64::MAX / 2, 0)
                .map(|page| page.report.deletion_enabled);
            let rooted_after_gc: bool = collector.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM server_sync_objects WHERE hash=?1)",
                [&promoted_hash], |row| row.get(0),
            ).unwrap();
            let _ = gc_sender.send((gc, garbage.content_hash.clone(), garbage.byte_size,
                cas.stat_object(&garbage.content_hash).unwrap(),
                cas.stat_object(&promoted_hash).unwrap(), rooted_after_gc));
        }));
        let (gc, garbage_hash, garbage_byte_size, garbage_size, promoted_size, rooted_after_gc) = gc_receiver
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| SyncError::new("repository-locked-during-promotion-gc", 409))?;
        if checks.get() == 1 {
            assert!(gc.expect("GC before promotion roots must succeed"));
            assert_eq!(garbage_size, None);
        } else {
            assert_eq!(gc, Err(crate::persistent_store::StoreError::Store {
                message: "marked CAS object is missing".to_owned(),
            }));
            assert_eq!(garbage_size, Some(garbage_byte_size));
            assert_eq!(cas.read_object(&garbage_hash).unwrap().unwrap(),
                format!("synthetic promotion GC {}", checks.get()).as_bytes());
            *retained_garbage.borrow_mut() = Some(garbage_hash);
        }
        assert_eq!(promoted_size, None);
        assert_eq!(rooted_after_gc, rooted);
        if checks.get() != 2 { return Ok(()); }
        assert!(rooted);
        let (sender, receiver) = std::sync::mpsc::channel();
        for _ in 0..9 {
            let sender = sender.clone();
            readers.borrow_mut().push(std::thread::spawn(move || {
                let _guard = crate::asset_repository::coordinator::lock_repository_mutation().unwrap();
                let _ = sender.send(());
            }));
        }
        drop(sender);
        for _ in 0..9 {
            receiver.recv_timeout(Duration::from_secs(2))
                .map_err(|_| SyncError::new("repository-locked-during-copy", 409))?;
        }
        let outcomes = std::thread::scope(|scope| {
            let barrier = Arc::new(Barrier::new(10));
            let requests: Vec<_> = (0..9).map(|_| {
                let barrier = barrier.clone();
                let url = media_url.clone();
                scope.spawn(move || {
                    barrier.wait();
                    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(2)).build()
                        .map_err(|error| error.to_string())?;
                    let response = client.get(url).send().map_err(|error| error.to_string())?;
                    let status = response.status().as_u16();
                    let body = response.bytes().map_err(|error| error.to_string())?;
                    Ok::<_, String>((status, body))
                })
            }).collect();
            barrier.wait();
            requests.into_iter().map(|request| request.join()).collect::<Vec<_>>()
        });
        for outcome in outcomes {
            let (status, body) = outcome.expect("media worker must finish")
                .expect("nine HTTP media requests must finish while promotion copy is paused");
            assert_eq!(status, 200);
            assert_eq!(body.as_ref(), media_bytes);
        }
        Ok(())
    })));
    let reader_outcomes: Vec<_> = readers.into_inner().into_iter().map(|reader| reader.join()).collect();
    let collector_outcomes: Vec<_> = collectors.into_inner().into_iter().map(|collector| collector.join()).collect();
    for outcome in reader_outcomes { outcome.expect("repository reader must finish"); }
    for outcome in collector_outcomes { outcome.expect("GC worker must finish"); }
    let result = result.expect("promotion fixture must not panic");
    assert!(result.is_ok(), "promotion copy must allow repository readers: {result:?}");
    assert!(PayloadCas::new(target.path()).unwrap().stat_object(&hash).unwrap().is_some());
    assert_eq!(checks.get(), 2);
    let gc = store.asset_gc_delete_page(4096, None, i64::MAX / 2, 0).unwrap();
    assert!(gc.report.deletion_enabled);
    let garbage_hash = retained_garbage.into_inner().expect("pre-copy GC must retain garbage");
    assert_eq!(cas.stat_object(&garbage_hash).unwrap(), None);
    assert_eq!(cas.read_object(&hash).unwrap().unwrap(), bytes);
    let inventory: (i64, i64) = store.connection.query_row(
        "SELECT (SELECT count(*) FROM server_sync_objects WHERE hash=?1), (SELECT count(*) FROM asset_objects WHERE object_hash=?1)",
        [&hash], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(inventory, (1, 1));
}
