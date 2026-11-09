use super::*;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[test]
fn state_restore_registers_remote_sizes_without_downloading_local_or_duplicate_assets() {
    let server = LocalServerFixture::new();
    let writer = ServerWriter::new(&server.server);
    let (_root, mut store) = local();
    let cas = crate::asset_repository::PayloadCas::new(store.repository_root()).unwrap();
    let local = cas.prepare_bytes(b"already local").unwrap();
    let mut expected = BTreeMap::new();
    for (index, body) in [b"remote one".as_slice(), b"second remote body", b"already local", b"remote one"].into_iter().enumerate() {
        let hash = risunest_sync_wire::hash(body);
        server.server.put_object(&writer.device, &hash, body).unwrap();
        if hash != local.content_hash {
            expected.insert(hash.clone(), body.len() as i64);
        }
        writer.push(&format!("alias-{index}"), &UnitKey::new(&["asset", &format!("alias-{index}")]).unwrap(), 100,
            UnitValue::inline(&serde_json::to_vec(&serde_json::json!({"objectHash":hash})).unwrap()).unwrap());
    }
    let client = server.client(&store);
    let upper = client.admission().unwrap();
    crate::asset_repository::body_io::reset_body_io();
    let (_, changes) = client.state(&mut store, upper).unwrap();
    let work = crate::asset_repository::body_io::take_body_io();
    assert_eq!(changes.len(), 4);
    assert_eq!((work.stat_requests, work.batch_stat_requests), (0, 1));
    let db = crate::sqlite_open::open(store.repository_root().join("persistent").join(crate::persistent_store::DATABASE_FILE)).unwrap();
    let mut query = db.prepare("SELECT object_hash,byte_size FROM asset_objects").unwrap();
    let registered = query.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))).unwrap()
        .collect::<rusqlite::Result<BTreeMap<_, _>>>().unwrap();
    assert_eq!(registered, expected);
    assert!(cas.stat_objects(expected.keys().map(String::as_str)).unwrap().iter().all(Option::is_none));
    assert_eq!(cas.read_object(&local.content_hash).unwrap().unwrap(), b"already local");
}

/// Exercises state listing, retention and catalog registration with fresh synthetic peers.
#[test]
#[ignore = "synthetic metadata restore measurement"]
fn metadata_restore_measurement() {
    let count = std::env::var("RISUNEST_METADATA_ASSETS").ok()
        .map(|value| value.parse::<usize>().unwrap()).unwrap_or(30_000);
    let delay_ms = std::env::var("RISUNEST_METADATA_DELAY_MS").ok()
        .map(|value| value.parse::<u64>().unwrap()).unwrap_or(0);
    let requests = Arc::new(Mutex::new(BTreeMap::<String, u64>::new()));
    let observed = requests.clone();
    let server = LocalServerFixture::with_router(move |router| {
        router.layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                *observed.lock().unwrap().entry(request.uri().path().to_owned()).or_default() += 1;
                async move {
                    if delay_ms > 0 { tokio::time::sleep(Duration::from_millis(delay_ms)).await; }
                    next.run(request).await
                }
            },
        ))
    });
    let writer = ServerWriter::new(&server.server);
    let mut aliases = Vec::with_capacity(count);
    for index in 0..count {
        let body = format!("synthetic-asset-{index:08}").into_bytes();
        let hash = risunest_sync_wire::hash(&body);
        server.server.put_object(&writer.device, &hash, &body).unwrap();
        aliases.push(UnitChange {
            key: UnitKey::new(&["asset", &format!("synthetic-{index:08}")]).unwrap(),
            stamp: Stamp { physical_ms: 100.into(), logical: 0, writer_id: ServerWriter::WRITER.into() },
            value: UnitValue::inline(&serde_json::to_vec(&serde_json::json!({"objectHash": hash})).unwrap()).unwrap(),
        });
    }
    for (index, page) in aliases.chunks(512).enumerate() {
        server.server.push(&writer.device, &PushRequest {
            library_id: writer.library_id.clone(), writer_id: ServerWriter::WRITER.into(),
            operation_id: format!("seed-{index}"), changes: page.to_vec(),
        }).unwrap();
    }
    for run in 0..3 {
        let (_root, mut store) = local();
        let client = server.client(&store);
        let upper = client.admission().unwrap();
        requests.lock().unwrap().clear();
        let io = client.client.test_io.as_ref().unwrap();
        io.reset();
        let start = Instant::now();
        let (_, changes) = client.state(&mut store, upper).unwrap();
        let elapsed = start.elapsed();
        assert_eq!(changes.len(), count);
        let db = crate::sqlite_open::open(store.repository_root().join("persistent").join(crate::persistent_store::DATABASE_FILE)).unwrap();
        let registered: i64 = db.query_row("SELECT count(*) FROM asset_objects", [], |row| row.get(0)).unwrap();
        assert_eq!(registered, count as i64);
        let paths = requests.lock().unwrap();
        println!("metadata_restore {}", serde_json::json!({
            "assets":count, "run":run, "delayMs":delay_ms,
            "elapsedMs":elapsed.as_secs_f64()*1000.0,
            "stateRequests":paths.get("/state").copied().unwrap_or(0),
            "http":io.snapshot(), "registered":registered,
        }));
    }
}
