use super::{
    commands::hydrate_binding_assets,
    lww_tests::{drain_publications, header, local, put_asset, LocalServerFixture},
    residency::AssetPolicy,
};
use crate::{asset_repository::PayloadCas, persistent_store::{PersistentStore, WorkingSetCommit}};
use serde_json::{json, Value};
use std::sync::{Arc, atomic::AtomicBool};

fn character(id: &str, image: &str) -> Value {
    json!({"chaId":id,"type":"character","name":"Synthetic","image":image,"chats":[]})
}

fn add(store: &mut PersistentStore, value: Value) {
    store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), add_character: Some(value), ..Default::default() }).unwrap();
}

fn replace(store: &mut PersistentStore, value: Value) {
    store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), replace_character: Some(value), ..Default::default() }).unwrap();
}

fn hydrate(store: &PersistentStore) -> super::Result<()> {
    hydrate_binding_assets(store, &header(store), Arc::new(AtomicBool::new(false)), None, || {})
}

fn status(store: &PersistentStore) -> Value {
    serde_json::to_value(store.asset_residency_status().unwrap()).unwrap()
}

#[test]
fn remote_policy_fetches_only_current_representatives_and_reuses_them_after_restart() {
    let server = LocalServerFixture::new();
    let (root, mut store) = local();
    let core = server.client(&store);
    let main = put_asset(&mut store, "assets/main.png", b"synthetic representative");
    let emotion = put_asset(&mut store, "assets/emotion.png", b"synthetic emotion");
    let extra = put_asset(&mut store, "assets/extra.png", b"synthetic extra");
    drain_publications(&core, &mut store, &[]).unwrap();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    let mut first = character("first", &main.key);
    first["emotionImages"] = json!([["neutral", emotion.key]]);
    first["additionalAssets"] = json!([["extra", extra.key, "png"]]);
    add(&mut store, first);
    add(&mut store, character("same-image", &main.key));
    let mut trashed = character("trashed", &extra.key);
    trashed["trashTime"] = json!(1);
    add(&mut store, trashed);
    assert_eq!(status(&store)["localBytes"], 0);
    assert_eq!(status(&store)["serverObjects"], 3);
    assert_eq!(status(&store)["unavailableObjects"], 0);
    let visits = std::cell::Cell::new(0);
    hydrate_binding_assets(&store, &header(&store), Arc::new(AtomicBool::new(false)), Some("first"), || visits.set(visits.get() + 1)).unwrap();
    assert_eq!(visits.get(), 1, "two representatives sharing a hash use one body");
    let cas = PayloadCas::new(store.repository_root()).unwrap();
    assert_eq!(cas.read_object(main.object_hash.as_ref().unwrap()).unwrap().unwrap(), b"synthetic representative");
    assert!(cas.stat_object(emotion.object_hash.as_ref().unwrap()).unwrap().is_none());
    assert!(cas.stat_object(extra.object_hash.as_ref().unwrap()).unwrap().is_none());
    assert_eq!(status(&store)["localBytes"], main.size);
    assert_eq!(status(&store)["serverObjects"], 2);
    assert_eq!(status(&store)["unavailableObjects"], 0);
    assert_eq!(store.asset_residency_evict(|| Ok(())).unwrap().evicted_bytes, 0);
    crate::asset_repository::body_io::reset_body_io();
    hydrate(&store).unwrap();
    let work = crate::asset_repository::body_io::take_body_io();
    assert_eq!((work.stat_requests, work.batch_stat_requests), (0, 0));
    assert!(work.domains.is_empty(), "checking retained representatives does not open their bodies");
    let temporary = tempfile::tempdir().unwrap();
    let other = PayloadCas::new(temporary.path()).unwrap();
    let body = other.prepare_bytes(b"unrelated temporary object").unwrap();
    other.unlink_exact_object(&body.content_hash, body.byte_size, &crate::asset_repository::object_physical_key(&body.content_hash)).unwrap();
    crate::asset_repository::body_io::reset_body_io();
    hydrate(&store.open_native_job_store().unwrap()).unwrap();
    assert_eq!(crate::asset_repository::body_io::take_body_io().batch_stat_requests, 0, "job stores share only this live repository's checks");
    let equivalent = PayloadCas::new(root.path().join(".")).unwrap();
    equivalent.unlink_exact_object(main.object_hash.as_ref().unwrap(), main.size as u64, &crate::asset_repository::object_physical_key(main.object_hash.as_ref().unwrap())).unwrap();
    hydrate(&store).unwrap();
    assert_eq!(cas.read_object(main.object_hash.as_ref().unwrap()).unwrap().unwrap(), b"synthetic representative");
    drop(core);
    drop(server);
    drop(store);
    let reopened = PersistentStore::open(root.path()).unwrap();
    crate::asset_repository::body_io::reset_body_io();
    hydrate(&reopened).expect("an already-local representative needs no server");
    assert_eq!(crate::asset_repository::body_io::take_body_io().batch_stat_requests, 1, "a new store rechecks physical presence");
    assert_eq!(PayloadCas::new(root.path()).unwrap().read_object(main.object_hash.as_ref().unwrap()).unwrap().unwrap(), b"synthetic representative");
}

#[test]
fn offload_tracks_replacement_alias_overwrite_trash_and_deletion_without_changing_logical_assets() {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let core = server.client(&store);
    let old = put_asset(&mut store, "assets/old.png", b"old synthetic image");
    let new = put_asset(&mut store, "assets/new.png", b"new synthetic image");
    add(&mut store, character("first", &old.key));
    drain_publications(&core, &mut store, &[]).unwrap();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    let cas = PayloadCas::new(store.repository_root()).unwrap();
    assert_eq!(store.asset_residency_evict(|| Ok(())).unwrap().evicted_bytes, new.size as u64);
    assert!(cas.stat_object(old.object_hash.as_ref().unwrap()).unwrap().is_some());
    replace(&mut store, character("first", &new.key));
    hydrate(&store).unwrap();
    assert_eq!(store.asset_residency_evict(|| Ok(())).unwrap().evicted_bytes, old.size as u64);
    assert!(cas.stat_object(old.object_hash.as_ref().unwrap()).unwrap().is_none());
    assert!(store.read_asset_alias("asset", &old.key, None).unwrap().is_some());
    replace(&mut store, character("first", &old.key));
    hydrate(&store).unwrap();
    assert_eq!(cas.read_object(old.object_hash.as_ref().unwrap()).unwrap().unwrap(), b"old synthetic image");
    replace(&mut store, character("first", &new.key));
    hydrate(&store).unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();

    // Keep another logical reference so the previous body remains an offload candidate.
    // Unreferenced bodies belong to ordinary asset GC instead.
    put_asset(&mut store, "assets/previous-new.png", b"new synthetic image");
    let replacement = put_asset(&mut store, &new.key, b"replacement under the same logical key");
    drain_publications(&core, &mut store, &[]).unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    assert!(cas.stat_object(replacement.object_hash.as_ref().unwrap()).unwrap().is_some());
    assert!(cas.stat_object(new.object_hash.as_ref().unwrap()).unwrap().is_none());
    let mut trashed = character("first", &new.key);
    trashed["trashTime"] = json!(1);
    replace(&mut store, trashed);
    store.asset_residency_evict(|| Ok(())).unwrap();
    assert!(cas.stat_object(replacement.object_hash.as_ref().unwrap()).unwrap().is_none());
    replace(&mut store, character("first", &new.key));
    hydrate(&store).unwrap();
    assert!(cas.stat_object(replacement.object_hash.as_ref().unwrap()).unwrap().is_some());
    store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), delete_character_ids: Some(vec!["first".into()]), ..Default::default() }).unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    assert!(cas.stat_object(replacement.object_hash.as_ref().unwrap()).unwrap().is_none());
}

#[test]
fn an_archived_catalog_image_stays_local_while_its_archive_can_be_offloaded() {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let core = server.client(&store);
    let main = put_asset(&mut store, "assets/archive-main.png", b"synthetic archived representative");
    add(&mut store, character("archived", &main.key));
    store.archive_character("archived", store.revision().unwrap(), 1).unwrap();
    drain_publications(&core, &mut store, &[]).unwrap();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    assert!(store.asset_residency_evict(|| Ok(())).unwrap().evicted_bytes > 0);
    assert!(PayloadCas::new(store.repository_root()).unwrap().stat_object(main.object_hash.as_ref().unwrap()).unwrap().is_some());
    store.restore_character("archived", store.revision().unwrap()).unwrap();
    assert_eq!(store.read_character("archived", None).unwrap().unwrap().value["image"], main.key);
}

#[test]
fn a_missing_representative_is_never_reported_local_and_does_not_hide_available_images() {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let core = server.client(&store);
    let available = put_asset(&mut store, "assets/available.png", b"synthetic available image");
    drain_publications(&core, &mut store, &[]).unwrap();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    let missing = put_asset(&mut store, "assets/missing.png", b"synthetic missing image");
    let cas = PayloadCas::new(store.repository_root()).unwrap();
    cas.unlink_exact_object(missing.object_hash.as_ref().unwrap(), missing.size as u64, &crate::asset_repository::object_physical_key(missing.object_hash.as_ref().unwrap())).unwrap();
    add(&mut store, character("missing", &missing.key));
    add(&mut store, character("available", &available.key));
    assert_eq!(status(&store)["unavailableObjects"], 1);
    assert_eq!(status(&store)["localBytes"], 0);
    hydrate(&store).expect("an icon no storage holds is skipped");
    assert!(cas.stat_object(available.object_hash.as_ref().unwrap()).unwrap().is_some());
    assert!(cas.stat_object(missing.object_hash.as_ref().unwrap()).unwrap().is_none());
    assert_eq!(status(&store)["unavailableObjects"], 1);
    assert_eq!(status(&store)["localBytes"], available.size);
}

/// The aliases the quick data check finds without a body.
fn absent_aliases(store: &mut PersistentStore) -> Vec<String> {
    let revision = store.revision().unwrap();
    let lease = store.acquire_revision(revision).unwrap().lease;
    let findings = store.data_health_reader(&lease).unwrap().scan(2000, &crate::local_backup::NeverCancelled).unwrap();
    store.release_revision(&lease).unwrap();
    findings.items.iter()
        .filter(|finding| finding.code == crate::data_health::codes::ALIAS_OBJECT_ABSENT)
        .map(|finding| finding.owner.id.clone())
        .collect()
}

#[test]
fn an_icon_no_storage_holds_is_skipped_beside_an_available_one_and_stays_in_the_data_check() {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let core = server.client(&store);
    let available = put_asset(&mut store, "assets/quiet-available.png", b"synthetic quiet available image");
    drain_publications(&core, &mut store, &[]).unwrap();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    let missing = put_asset(&mut store, "assets/quiet-missing.png", b"synthetic quiet missing image");
    let cas = PayloadCas::new(store.repository_root()).unwrap();
    let missing_hash = missing.object_hash.as_ref().unwrap();
    cas.unlink_exact_object(missing_hash, missing.size as u64, &crate::asset_repository::object_physical_key(missing_hash)).unwrap();
    add(&mut store, character("missing", &missing.key));
    add(&mut store, character("available", &available.key));
    assert!(cas.stat_object(available.object_hash.as_ref().unwrap()).unwrap().is_none());

    let lane = Arc::new(super::progress::ProgressLane::default());
    let visits = std::cell::Cell::new(0);
    super::progress::within(&lane, || hydrate_binding_assets(&store, &header(&store), Arc::new(AtomicBool::new(false)), Some("missing"), || visits.set(visits.get() + 1)))
        .expect("an icon no storage holds is not a sync error");
    assert_eq!(visits.get(), 1, "only the available icon is fetched");
    assert_eq!(cas.read_object(available.object_hash.as_ref().unwrap()).unwrap().unwrap(), b"synthetic quiet available image");
    assert!(cas.stat_object(missing_hash).unwrap().is_none());
    let scope = lane.snapshot("hydrate").asset_scope.expect("the available icon is planned");
    assert_eq!((scope.done, scope.total, scope.settled), (1, Some(1), true), "the skipped icon is not planned work");

    let again = Arc::new(super::progress::ProgressLane::default());
    super::progress::within(&again, || hydrate(&store)).expect("a later pass stays quiet");
    assert!(again.snapshot("hydrate").asset_scope.is_none(), "nothing is left to download");
    assert_eq!(status(&store)["unavailableObjects"], 1);
    assert_eq!(absent_aliases(&mut store), vec![missing.key.clone()]);
}

#[test]
fn an_unreachable_server_still_fails_the_icon_pass() {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let core = server.client(&store);
    let main = put_asset(&mut store, "assets/unreachable-main.png", b"synthetic unreachable representative");
    drain_publications(&core, &mut store, &[]).unwrap();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    add(&mut store, character("unreachable", &main.key));
    drop(core);
    drop(server);
    // A parallel test's fixture can bind the freed port and answer, so point the binding at a port nothing serves.
    let mut stored = store.server_stored_config().unwrap().unwrap();
    stored.endpoint = "http://127.0.0.1:1".into();
    store.server_save_config(&stored).unwrap();
    let error = hydrate(&store).expect_err("a held icon the server cannot send is still an error");
    assert_eq!(error.code, "server-unreachable");
    assert!(PayloadCas::new(store.repository_root()).unwrap().stat_object(main.object_hash.as_ref().unwrap()).unwrap().is_none());
}

#[test]
fn remote_representative_hydration_obeys_cancellation_and_binding_authority() {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let core = server.client(&store);
    let main = put_asset(&mut store, "assets/cancelled-main.png", b"synthetic cancelled representative");
    drain_publications(&core, &mut store, &[]).unwrap();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    add(&mut store, character("cancelled", &main.key));
    let cancelled = hydrate_binding_assets(&store, &header(&store), Arc::new(AtomicBool::new(true)), None, || panic!("no body may finish"));
    assert_eq!(cancelled.unwrap_err().code, "cancelled");
    let mut stale = header(&store);
    stale.binding_authority = (stale.binding_authority.0 + 1).into();
    assert_eq!(hydrate_binding_assets(&store, &stale, Arc::new(AtomicBool::new(false)), None, || panic!("no body may finish")).unwrap_err().code, "binding-authority-changed");
    assert!(PayloadCas::new(store.repository_root()).unwrap().stat_object(main.object_hash.as_ref().unwrap()).unwrap().is_none());
    hydrate(&store).unwrap();
}

#[test]
fn selected_representative_finishes_first_and_replacement_does_not_keep_the_old_image() {
    let server = LocalServerFixture::new();
    let (root, mut store) = local();
    let core = server.client(&store);
    let first = put_asset(&mut store, "assets/first.png", b"synthetic first representative");
    let second = put_asset(&mut store, "assets/second.png", b"synthetic second representative");
    drain_publications(&core, &mut store, &[]).unwrap();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    add(&mut store, character("first", &first.key));
    add(&mut store, character("selected", &second.key));
    let cas = PayloadCas::new(root.path()).unwrap();
    let visits = std::cell::Cell::new(0);
    hydrate_binding_assets(&store, &header(&store), Arc::new(AtomicBool::new(false)), Some("selected"), || {
        visits.set(visits.get() + 1);
        if visits.get() == 1 {
            assert!(cas.stat_object(second.object_hash.as_ref().unwrap()).unwrap().is_some());
            assert!(cas.stat_object(first.object_hash.as_ref().unwrap()).unwrap().is_none());
        }
    }).unwrap();
    assert_eq!(visits.get(), 2);
    replace(&mut store, character("selected", &first.key));
    assert_eq!(store.read_character("selected", None).unwrap().unwrap().value["image"], first.key);
    store.asset_residency_evict(|| Ok(())).unwrap();
    assert!(cas.stat_object(first.object_hash.as_ref().unwrap()).unwrap().is_some());
    assert!(cas.stat_object(second.object_hash.as_ref().unwrap()).unwrap().is_none());
}

#[test]
fn offload_rechecks_a_representative_changed_while_server_retention_was_in_flight() {
    let replacement = Arc::new(std::sync::Mutex::new(None::<(std::path::PathBuf, String)>));
    let observed = replacement.clone();
    let server = LocalServerFixture::with_router(move |router| router.layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let observed = observed.clone();
            async move {
                let retention = request.uri().path().ends_with("/objects/retention");
                let response = next.run(request).await;
                if retention && response.status().is_success() {
                    let change = observed.lock().unwrap().take();
                    if let Some((root, image)) = change {
                        tokio::task::spawn_blocking(move || {
                            replace(&mut PersistentStore::open(&root).unwrap(), character("changing", &image));
                        }).await.unwrap();
                    }
                }
                response
            }
        },
    )));
    let (root, mut store) = local();
    let core = server.client(&store);
    let old = put_asset(&mut store, "assets/before.png", b"synthetic before retention");
    let new = put_asset(&mut store, "assets/after.png", b"synthetic after retention");
    add(&mut store, character("changing", &old.key));
    drain_publications(&core, &mut store, &[]).unwrap();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    *replacement.lock().unwrap() = Some((root.path().to_path_buf(), new.key.clone()));
    assert_eq!(store.asset_residency_evict(|| Ok(())).unwrap().evicted_bytes, 0);
    assert!(replacement.lock().unwrap().is_none(), "the image changed during retention");
    assert_eq!(store.read_character("changing", None).unwrap().unwrap().value["image"], new.key);
    let cas = PayloadCas::new(root.path()).unwrap();
    assert!(cas.stat_object(new.object_hash.as_ref().unwrap()).unwrap().is_some());
    assert_eq!(store.asset_residency_evict(|| Ok(())).unwrap().evicted_bytes, old.size as u64);
    assert!(cas.stat_object(old.object_hash.as_ref().unwrap()).unwrap().is_none());
    assert!(cas.stat_object(new.object_hash.as_ref().unwrap()).unwrap().is_some());
}

#[test]
fn a_device_joining_with_remote_assets_fetches_received_representatives_and_follows_their_changes() {
    let server = LocalServerFixture::new();
    let (_source_root, mut source) = local();
    let (target_root, mut target) = local();
    let sender = server.client(&source);
    let main = put_asset(&mut source, "assets/joined-main.png", b"synthetic joined representative");
    let emotion = put_asset(&mut source, "assets/joined-emotion.png", b"synthetic joined emotion");
    let next = put_asset(&mut source, "assets/joined-next.png", b"synthetic joined replacement");
    let mut joined = character("joined", &main.key);
    joined["emotionImages"] = json!([["neutral", emotion.key]]);
    add(&mut source, joined.clone());
    drain_publications(&sender, &mut source, &[]).unwrap();
    server.prepare_binding_candidate(&target);
    let counters = Arc::new(super::client::TestIoCounters::default());
    super::binding::first_binding_cycle(&mut target, counters.clone(), |_| {}).unwrap();
    target.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    let cas = PayloadCas::new(target_root.path()).unwrap();
    let held = |alias: &crate::persistent_store::AssetAlias| cas.stat_object(alias.object_hash.as_ref().unwrap()).unwrap().is_some();
    assert!(!held(&main) && !held(&emotion) && !held(&next), "joining leaves every body on the server");
    hydrate(&target).unwrap();
    assert!(held(&main));
    assert!(!held(&emotion) && !held(&next));

    joined["image"] = json!(next.key);
    replace(&mut source, joined);
    drain_publications(&sender, &mut source, &[]).unwrap();
    let receiver = LocalServerFixture::reopen_client(&target, counters).unwrap();
    super::lww_tests::receive_available(&receiver, &mut target, &[]).unwrap();
    hydrate(&target).unwrap();
    assert!(held(&next), "a received image change is fetched");
    target.asset_residency_evict(|| Ok(())).unwrap();
    assert!(held(&next));
    assert!(!held(&main), "cleanup no longer keeps the replaced image");
    assert!(!held(&emotion));
}

#[test]
fn closed_folder_images_are_hydrated_and_retained_with_character_icons() {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let core = server.client(&store);
    let image = put_asset(&mut store, "assets/closed-folder.png", b"synthetic closed folder");
    drain_publications(&core, &mut store, &[]).unwrap();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), root_mutations: Some(vec![
        crate::persistent_store::RootMutation::Set { key: "characterOrder".into(), value: json!([{"id":"folder", "name":"Folder", "imgFile":image.key,"data":[]}]) }
    ]), ..Default::default() }).unwrap();
    hydrate(&store).unwrap();
    let cas = PayloadCas::new(store.repository_root()).unwrap();
    assert_eq!(cas.read_object(image.object_hash.as_ref().unwrap()).unwrap().unwrap(), b"synthetic closed folder");
    assert_eq!(store.asset_residency_evict(|| Ok(())).unwrap().evicted_bytes, 0);
}

#[test]
fn non_array_character_order_keeps_representatives_available() {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let core = server.client(&store);
    let image = put_asset(&mut store, "assets/order-fallback.png", b"synthetic order fallback");
    drain_publications(&core, &mut store, &[]).unwrap();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    add(&mut store, character("order-fallback", &image.key));
    let cas = PayloadCas::new(store.repository_root()).unwrap();
    for order in [Value::Null, json!(false), json!(7), json!("unexpected"), json!({"unexpected": true})] {
        store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), root_mutations: Some(vec![
            crate::persistent_store::RootMutation::Set { key: "characterOrder".into(), value: order }
        ]), ..Default::default() }).unwrap();
        hydrate(&store).unwrap();
        assert_eq!(status(&store)["localBytes"], image.size);
        assert_eq!(store.asset_residency_evict(|| Ok(())).unwrap().evicted_bytes, 0);
        assert_eq!(cas.read_object(image.object_hash.as_ref().unwrap()).unwrap().unwrap(), b"synthetic order fallback");
    }
}

#[test]
fn visible_images_precede_selected_images_and_follow_display_order_instead_of_hash_order() {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let core = server.client(&store);
    let mut assets = (0..3).map(|index| put_asset(&mut store, &format!("assets/rank-{index}.png"), format!("synthetic rank {index}").as_bytes())).collect::<Vec<_>>();
    assets.sort_by(|a,b|b.object_hash.cmp(&a.object_hash));
    drain_publications(&core, &mut store, &[]).unwrap();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    for (index,asset) in assets.iter().enumerate() { add(&mut store, character(&format!("rank-{index}"), &asset.key)); }
    let priority = crate::persistent_store::asset_residency::RepresentativeImagePriority {
        ordered: assets.iter().map(|asset|asset.key.clone()).collect(),
        visible: vec![assets[0].key.clone(), assets[1].key.clone()],
    };
    let cas = PayloadCas::new(store.repository_root()).unwrap();
    let done = std::cell::Cell::new(0);
    super::commands::hydrate_binding_assets_ordered(&store, &header(&store), Arc::new(AtomicBool::new(false)), Some("rank-2"), Some(&priority), || {
        let index = done.get();
        assert!(cas.stat_object(assets[index].object_hash.as_ref().unwrap()).unwrap().is_some());
        for later in &assets[index+1..] { assert!(cas.stat_object(later.object_hash.as_ref().unwrap()).unwrap().is_none()); }
        done.set(index+1);
    }).unwrap();
    assert_eq!(done.get(), 3);
}

#[test]
#[ignore = "synthetic icon-cache timing, run separately in a quiet slot"]
fn representative_image_cache_small_and_1000_trend() {
    use crate::persistent_store::{AssetAlias, asset_object_catalog::AssetObjectRegistration};
    for count in [10, 1000] {
        let (root, mut store) = local();
        let cas = PayloadCas::new(root.path()).unwrap();
        let mut aliases = Vec::new();
        let mut registrations = Vec::new();
        let mut characters = Vec::new();
        for index in 0..count {
            let bytes = format!("synthetic icon trend {index}").into_bytes();
            let object = cas.prepare_bytes(&bytes).unwrap();
            let key = format!("assets/icon-{index}.png");
            registrations.push(AssetObjectRegistration { object_hash: object.content_hash.clone(), byte_size: object.byte_size });
            aliases.push(AssetAlias { key: key.clone(), object_hash: Some(object.content_hash), kind: "asset".into(), size: bytes.len() as i64,
                mime: "image/png".into(), name: "synthetic".into(), ext: "png".into(), inlay_type: None, width: None, height: None, metadata: json!({}) });
            characters.push(character(&format!("icon-{index}"), &key));
        }
        store.asset_object_catalog().register(&registrations, 1).unwrap();
        let staging = store.replace_begin().unwrap().staging_id;
        store.replace_put_root(&staging, &json!({"characterOrder":characters.iter().map(|character|character["chaId"].clone()).collect::<Vec<_>>()})).unwrap();
        store.replace_put_presets(&staging, &[]).unwrap();
        store.replace_add_characters(&staging, &characters).unwrap();
        store.replace_put_asset_aliases(&staging, &aliases).unwrap();
        store.replace_commit(&staging, Some(store.revision().unwrap())).unwrap();
        let server = LocalServerFixture::new();
        let _core = server.client(&store);
        store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
        drop(store);
        let mut cold = Vec::new();
        let mut warm = Vec::new();
        let mut warm_stats = Vec::new();
        for _ in 0..5 {
            let store = PersistentStore::open(root.path()).unwrap();
            let start = std::time::Instant::now();
            hydrate(&store).unwrap();
            cold.push(start.elapsed().as_micros());
            crate::asset_repository::body_io::reset_body_io();
            let job = store.open_native_job_store().unwrap();
            let start = std::time::Instant::now();
            hydrate(&job).unwrap();
            warm.push(start.elapsed().as_micros());
            warm_stats.push(crate::asset_repository::body_io::take_body_io().batch_stat_requests);
        }
        println!("W2_ICON_TREND {}", json!({"icons":count,"samples":5,"coldCheckUs":cold,"warmJobCheckUs":warm,"warmBatchStats":warm_stats}));
        assert!(warm_stats.iter().all(|count| *count == 0));
    }
}
