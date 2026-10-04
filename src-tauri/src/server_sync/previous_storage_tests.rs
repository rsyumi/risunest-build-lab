use super::{
    lww_client::LwwClient,
    lww_tests::{drain_publications, header, local, publish_cycle, put_asset, receive_available, LocalServerFixture},
    residency::{AssetPolicy, Residency},
};
use crate::{
    asset_repository::PayloadCas,
    persistent_store::{AssetAlias, PersistentStore, WorkingSetCommit},
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// A server that answers every request with 404 while `offline` is set.
pub(crate) fn switchable_server(offline: Arc<AtomicBool>) -> LocalServerFixture {
    LocalServerFixture::with_router(move |router| {
        router.layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let offline = offline.clone();
                async move {
                    if offline.load(Ordering::SeqCst) {
                        return axum::response::Response::builder()
                            .status(404)
                            .body(axum::body::Body::from("{\"error\":\"not-found\"}"))
                            .unwrap();
                    }
                    next.run(request).await
                }
            },
        ))
    })
}

/// Binds the store to `server` as a first binding and queues its whole state,
/// as the first publication after a switch does.
pub(crate) fn bind_server(store: &mut PersistentStore, server: &LocalServerFixture) -> LwwClient {
    use crate::persistent_store::sync_selection::{SwitchBindingRequest, SyncTarget};
    let client = server.client(store);
    let original = store.lww_binding_state().unwrap();
    let inspected = super::binding::inspect(store, &header(store)).unwrap();
    store.switch_lww_binding(&SwitchBindingRequest {
        header: header(store),
        expected_selection_epoch: original.selection_epoch,
        target: SyncTarget::Server("server".into()),
        inspection_id: Some(inspected.inspection_id),
        initial_publication: false,
    }).unwrap();
    super::binding::activate(store, &header(store), None).unwrap();
    let mut after = None;
    loop {
        let page = store.lww_queue_unit_state_page(&header(store), after.as_ref(), 256).unwrap();
        if !page.has_more {
            break;
        }
        after = page.after_key;
    }
    client
}

/// Aliases that share one local body, committed together.
fn local_aliases(store: &mut PersistentStore, prefix: &str, count: usize) -> String {
    let template = put_asset(store, &format!("assets/{prefix}-shared.png"), format!("synthetic shared {prefix}").as_bytes());
    let aliases = (0..count).map(|index| AssetAlias { key: format!("assets/{prefix}-{index:04}.png"), ..template.clone() }).collect::<Vec<_>>();
    store.commit_with_asset_aliases(&WorkingSetCommit { expected_revision: store.revision().unwrap(), ..Default::default() }, &aliases).unwrap();
    template.object_hash.unwrap()
}

fn evicted_asset(store: &mut PersistentStore, client: &LwwClient, key: &str, bytes: &[u8]) -> String {
    let hash = put_asset(store, key, bytes).object_hash.unwrap();
    drain_publications(client, store, &[]).unwrap();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    assert!(PayloadCas::new(store.repository_root()).unwrap().stat_object(&hash).unwrap().is_none());
    hash
}

#[test]
fn archive_push_under_remote_policy_uploads_nothing_the_server_already_holds() {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let core = server.client(&store);
    let alias = put_asset(&mut store, "assets/archive-remote.png", b"synthetic archive under remote policy");
    let hash = alias.object_hash.clone().unwrap();
    store.commit(&WorkingSetCommit {
        expected_revision: store.revision().unwrap(),
        add_character: Some(serde_json::json!({"chaId":"archive-remote","type":"character","name":"Synthetic","image":alias.key,"chats":[]})),
        ..Default::default()
    }).unwrap();
    drain_publications(&core, &mut store, &[]).unwrap();
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    let cas = PayloadCas::new(store.repository_root()).unwrap();
    assert!(cas.stat_object(&hash).unwrap().is_none());
    store.archive_character("archive-remote", store.revision().unwrap(), 1).unwrap();
    drain_publications(&core, &mut store, &[]).unwrap();
    assert!(store.lww_read_outbox(store.lww_binding_authority().unwrap(), 16).unwrap().entries.is_empty());
    assert!(cas.stat_object(&hash).unwrap().is_none());
}

#[test]
fn first_publication_to_a_new_server_copies_bodies_only_the_previous_server_holds() {
    let offline = Arc::new(AtomicBool::new(false));
    let previous = switchable_server(offline.clone());
    let next = LocalServerFixture::new();
    let (_root, mut store) = local();
    let old = previous.client(&store);
    let bytes = b"synthetic body held by the previous server".to_vec();
    let hash = evicted_asset(&mut store, &old, "assets/previous-only.png", &bytes);
    let previous_proof = Residency::open(store.repository_root()).unwrap().object(&hash, None).unwrap().unwrap();
    let new = bind_server(&mut store, &next);
    drain_publications(&new, &mut store, &[]).unwrap();
    let cas = PayloadCas::new(store.repository_root()).unwrap();
    assert!(cas.stat_object(&hash).unwrap().is_none(), "the remote policy keeps the copied body off this device");
    assert_eq!(serde_json::to_value(store.asset_residency_status().unwrap()).unwrap()["policy"], "remote");
    let scratch = tempfile::tempdir().unwrap();
    let mut kept = super::residency::open_transient_server_proof_with_check(store.repository_root(), scratch.path(), &previous_proof, &|| Ok(())).unwrap().unwrap();
    let mut read = Vec::new();
    std::io::Read::read_to_end(&mut kept, &mut read).unwrap();
    assert_eq!(read, bytes, "the previous server keeps its copy");
    let routed = Residency::open(store.repository_root()).unwrap().object(&hash, None).unwrap().unwrap();
    assert_eq!(routed.config.library_id, new.client.config().library_id);
    offline.store(true, Ordering::SeqCst);
    store.asset_residency_set_policy(AssetPolicy::Full, || Ok(())).unwrap();
    assert_eq!(cas.read_object(&hash).unwrap().unwrap(), bytes, "later downloads come from the new server");
}

#[test]
fn a_previous_server_body_that_cannot_be_fetched_stops_only_the_page_that_references_it() {
    let offline = Arc::new(AtomicBool::new(false));
    let previous = switchable_server(offline.clone());
    let next = LocalServerFixture::new();
    let (_root, mut store) = local();
    let old = previous.client(&store);
    let bytes = b"synthetic body on a later page".to_vec();
    let missing = evicted_asset(&mut store, &old, "assets/z-previous-only.png", &bytes);
    local_aliases(&mut store, "a", 300);
    let new = bind_server(&mut store, &next);
    let authority = store.lww_binding_authority().unwrap();
    let missing_key = risunest_sync_wire::unit::UnitKey::new(&["asset", "assets/z-previous-only.png"]).unwrap();
    let first_page = store.lww_read_outbox(authority, 256).unwrap().entries;
    assert_eq!(first_page.len(), 256);
    assert!(first_page.iter().all(|entry| entry.key != missing_key));
    assert!(store.lww_read_outbox(authority, 4096).unwrap().entries.iter().any(|entry| entry.key == missing_key));
    offline.store(true, Ordering::SeqCst);
    assert!(publish_cycle(&new, &mut store, &[]).unwrap().is_some());
    let error = publish_cycle(&new, &mut store, &[]).unwrap_err();
    assert_eq!(error.code, "previous-storage-unavailable");
    let pending = store.lww_read_outbox(authority, 4096).unwrap().entries;
    assert!(pending.iter().any(|entry| entry.key == missing_key));
    assert!(pending.iter().all(|entry| !first_page.iter().any(|published| published.key == entry.key)));
    let (_peer_root, mut peer) = local();
    let reader = next.client(&peer);
    receive_available(&reader, &mut peer, &[]).unwrap();
    assert!(peer.read_asset_alias("asset", "assets/a-0000.png", None).unwrap().is_some());
    assert!(peer.read_asset_alias("asset", "assets/z-previous-only.png", None).unwrap().is_none());
    offline.store(false, Ordering::SeqCst);
    drain_publications(&new, &mut store, &[]).unwrap();
    assert!(store.lww_read_outbox(authority, 16).unwrap().entries.is_empty());
    receive_available(&reader, &mut peer, &[]).unwrap();
    assert!(peer.read_asset_alias("asset", "assets/z-previous-only.png", None).unwrap().is_some());
    assert!(PayloadCas::new(store.repository_root()).unwrap().stat_object(&missing).unwrap().is_none());
}
