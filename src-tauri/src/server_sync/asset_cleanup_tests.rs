use super::{lww_tests::{local, put_asset, drain_publications, LocalServerFixture}, residency::AssetPolicy};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use super::asset_cleanup_progress::{Progress, Stage};
use crate::asset_repository::PayloadCas;

#[test]
fn retained_cleanup_uses_one_page_and_no_payload_hashing() {
    let requests = Arc::new((AtomicUsize::new(0), AtomicUsize::new(0)));
    let observed = requests.clone();
    let server = LocalServerFixture::with_router(move |router| router.layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let observed = observed.clone();
            async move {
                if request.uri().path().ends_with("/objects/retention") { observed.0.fetch_add(1, Ordering::Relaxed); }
                if request.uri().path().ends_with("/objects/missing") { observed.1.fetch_add(1, Ordering::Relaxed); }
                next.run(request).await
            }
        },
    )));
    let (_root, mut store) = local();
    let core = server.client(&store);
    let count = 257;
    let size = 4096;
    for index in 0..count {
        let mut body = vec![42; size];
        body[..8].copy_from_slice(&(index as u64).to_le_bytes());
        put_asset(&mut store, &format!("assets/cleanup-{index}.png"), &body);
    }
    drain_publications(&core, &mut store, &[]).unwrap();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    requests.0.store(0, Ordering::Relaxed);
    requests.1.store(0, Ordering::Relaxed);
    crate::persistent_store::hash_work::reset_hash_work();
    let started = std::time::Instant::now();
    let result = store.asset_residency_evict(|| Ok(())).unwrap();
    let elapsed = started.elapsed().as_millis();
    let work = crate::persistent_store::hash_work::take_hash_work();
    assert!(work.incomplete.is_empty());
    let hashed = work.domains.get("cas_existing_verify").map_or(0, |work| work.bytes);
    let retained = requests.0.load(Ordering::Relaxed);
    let missing = requests.1.load(Ordering::Relaxed);
    eprintln!("cleanup sample: objects={count} bytes={} retention_requests={retained} missing_requests={missing} payload_hash_bytes={hashed} elapsed_ms={elapsed}", count * size);
    assert_eq!(result.evicted_bytes, (count * size) as u64);
    assert_eq!((retained, missing, hashed), (1, 0, 0));
}

#[test]
fn cleanup_uploads_unpublished_objects_before_offloading_and_reports_each_stage() {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let _core = server.client(&store);
    let body = b"synthetic unpublished retained content";
    let object = put_asset(&mut store, "assets/unpublished.png", body);
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    let readings = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = readings.clone();
    let mut progress = Progress::new(move |snapshot| observed.lock().unwrap().push(snapshot));
    assert_eq!(store.asset_residency_evict_observed(None, || Ok(()), &mut progress).unwrap().evicted_bytes, body.len() as u64);
    drop(progress);
    let readings = readings.lock().unwrap();
    for stage in [Stage::Preparing, Stage::Checking, Stage::Uploading, Stage::Deleting, Stage::Finalizing] {
        assert!(readings.iter().any(|snapshot| snapshot.stage == stage), "missing {stage:?}");
    }
    let last = readings.last().unwrap();
    assert_eq!((last.evicted_items, last.evicted_bytes), (1, body.len() as u64));
    assert!(readings.windows(2).all(|pair| pair[0].sequence < pair[1].sequence));
    let cas = PayloadCas::new(store.repository_root()).unwrap();
    assert!(cas.stat_object(object.object_hash.as_ref().unwrap()).unwrap().is_none());
    store.asset_residency_set_policy(AssetPolicy::Full, || Ok(())).unwrap();
    assert_eq!(cas.read_object(object.object_hash.as_ref().unwrap()).unwrap().unwrap(), body);
}

#[test]
fn cleanup_repairs_damaged_server_bodies_with_or_without_an_unpublished_neighbor() {
    for missing_file in [false, true] {
        for neighbor in [false, true] {
            damaged_server_cleanup(missing_file, neighbor, false);
        }
    }
}

#[test]
fn cleanup_keeps_every_local_body_when_the_repair_source_is_corrupt() {
    damaged_server_cleanup(false, true, true);
}

fn damaged_server_cleanup(missing_file: bool, neighbor: bool, corrupt_local: bool) {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let core = server.client(&store);
    let body = vec![42; 128 * 1024];
    let object = put_asset(&mut store, "assets/damaged-server.png", &body);
    let hash = object.object_hash.unwrap();
    drain_publications(&core, &mut store, &[]).unwrap();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    let remote = server.root().join("objects").join(&hash[..2]).join(&hash);
    assert!(remote.is_file());
    if missing_file { std::fs::remove_file(&remote).unwrap(); }
    else { std::fs::write(&remote, b"synthetic truncated server object").unwrap(); }
    let neighbor_body = (0..100).map(|index| format!("synthetic unpublished neighbor {index}").into_bytes())
        .find(|body| risunest_sync_wire::hash(body) < hash).unwrap();
    let neighbor_object = neighbor.then(|| put_asset(&mut store, "assets/unpublished-neighbor.png", &neighbor_body));
    let local_path = store.repository_root().join(crate::asset_repository::object_physical_key(&hash));
    let local_body = if corrupt_local { vec![43; body.len()] } else { body.clone() };
    if corrupt_local { std::fs::write(&local_path, &local_body).unwrap(); }
    let readings = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = readings.clone();
    let mut progress = Progress::new(move |snapshot| observed.lock().unwrap().push(snapshot));
    let result = store.asset_residency_evict_observed(None, || Ok(()), &mut progress);
    drop(progress);
    let readings = readings.lock().unwrap();
    let cas = PayloadCas::new(store.repository_root()).unwrap();
    if corrupt_local {
        assert!(result.is_err());
        assert!(readings.iter().all(|snapshot| snapshot.stage != Stage::Deleting && snapshot.evicted_bytes == 0));
        assert_eq!(std::fs::read(&local_path).unwrap(), local_body);
        assert_eq!(cas.read_object(neighbor_object.as_ref().unwrap().object_hash.as_ref().unwrap()).unwrap().unwrap(), neighbor_body);
        assert_eq!(std::fs::read(&remote).unwrap(), b"synthetic truncated server object");
    } else {
        let expected_bytes = body.len() + if neighbor { neighbor_body.len() } else { 0 };
        assert_eq!(result.unwrap().evicted_bytes, expected_bytes as u64);
        assert_eq!(std::fs::read(&remote).unwrap(), body);
        assert!(!local_path.exists());
        assert!(readings.iter().any(|snapshot| snapshot.stage == Stage::Uploading));
        assert_eq!(readings.last().unwrap().evicted_bytes, expected_bytes as u64);
        store.asset_residency_set_policy(AssetPolicy::Full, || Ok(())).unwrap();
        assert_eq!(cas.read_object(&hash).unwrap().unwrap(), body);
        if let Some(object) = neighbor_object {
            assert_eq!(cas.read_object(object.object_hash.as_ref().unwrap()).unwrap().unwrap(), neighbor_body);
        }
    }
}

#[test]
fn cancellation_at_the_delete_boundary_preserves_local_bytes_and_retry_is_safe() {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let core = server.client(&store);
    let body = b"synthetic cancelled retained content";
    let object = put_asset(&mut store, "assets/cancel-cleanup.png", body);
    drain_publications(&core, &mut store, &[]).unwrap();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = cancelled.clone();
    let mut progress = Progress::new(move |snapshot| { if snapshot.stage == Stage::Deleting { observed.store(true, Ordering::Release); } });
    let error = store.asset_residency_evict_observed(Some(cancelled.clone()), || {
        if cancelled.load(Ordering::Acquire) { Err(super::SyncError::new("cancelled", 409)) } else { Ok(()) }
    }, &mut progress).err().unwrap();
    assert_eq!(error.code, "cancelled");
    let cas = PayloadCas::new(store.repository_root()).unwrap();
    assert_eq!(cas.read_object(object.object_hash.as_ref().unwrap()).unwrap().unwrap(), body);
    assert_eq!(store.asset_residency_evict(|| Ok(())).unwrap().evicted_bytes, body.len() as u64);
}
