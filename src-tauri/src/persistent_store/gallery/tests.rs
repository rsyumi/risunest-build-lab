use super::*;
use crate::asset_repository::{body_io, PayloadCas};
use crate::persistent_store::asset_object_catalog::AssetObjectRegistration;
use serde_json::{json, Value};

fn fixture() -> (tempfile::TempDir, PersistentStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = PersistentStore::open(directory.path()).unwrap();
    (directory, store)
}
fn query() -> GalleryQuery { serde_json::from_value(json!({})).unwrap() }
fn alias(key: &str, kind: &str, metadata: Value) -> AssetAlias {
    AssetAlias { key: key.into(), object_hash: None, kind: "inlay".into(), size: 0, mime: format!("{kind}/png"), name: key.into(), ext: "png".into(), inlay_type: Some(kind.into()), width: None, height: None, metadata }
}
fn put(store: &mut PersistentStore, alias: &AssetAlias) { store.commit_asset_alias(alias, store.revision().unwrap()).unwrap(); }
fn body(store: &mut PersistentStore, key: &str, value: &[u8]) -> AssetAlias {
    let object = PayloadCas::new(store.repository_root()).unwrap().prepare_bytes(value).unwrap();
    store.asset_object_catalog().register(&[AssetObjectRegistration { object_hash: object.content_hash.clone(), byte_size: object.byte_size }], 100).unwrap();
    let mut alias = alias(key, "image", json!({}));
    alias.object_hash = Some(object.content_hash); alias.size = object.byte_size as i64;
    put(store, &alias);
    alias
}
fn character(store: &mut PersistentStore) {
    store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), add_character: Some(json!({
        "chaId":"character", "name":"Synthetic character", "type":"character", "firstMessage":"{{inlay::detail}}", "chats":[{
            "id":"conversation", "name":"Synthetic chat", "note":"{{inlayed::conversation}}", "message":[
                {"chatId":"message", "role":"user", "data":"{{inlay::message}}", "variants":["{{inlayeddata::variant}}"]}
            ]
        }]
    })), ..Default::default() }).unwrap();
}
fn device_value(store: &mut PersistentStore, key: &str, value: &str) {
    use super::super::device_store::plugin_values::PluginDeviceMutation;
    store.device_store_mut().unwrap().write_plugin_device_values("synthetic-owner", &[PluginDeviceMutation::Set { space: "string".into(), key: key.into(), value: value.into() }]).unwrap();
}
fn scan(store: &mut PersistentStore) -> ScanStatus {
    let (_, prepared) = store.prepare_gallery_scan(store.revision().unwrap()).unwrap(); prepared.run();
    let status = store.gallery_scan_status().unwrap().unwrap(); assert_eq!(status.state, "complete"); status
}

#[test]
fn gallery_pages_sort_metadata_and_never_open_bodies() {
    let (_directory, mut store) = fixture();
    body(&mut store, "catalog", b"catalog fallback");
    put(&mut store, &alias("native", "image", json!({"createdAt":400,"pocketRisu":{"createdAt":600}})));
    put(&mut store, &alias("pocket", "audio", json!({"createdAt":0,"pocketRisu":{"createdAt":300}})));
    put(&mut store, &alias("tie", "video", json!({"createdAt":400})));
    put(&mut store, &alias("signature", "signature", json!({"createdAt":900})));
    body_io::reset_body_io();
    let page = store.gallery_page(&query()).unwrap();
    assert_eq!(page.items.iter().map(|item| (&*item.alias.key, item.sort_time)).collect::<Vec<_>>(), vec![("native",400.0),("tie",400.0),("pocket",300.0),("catalog",100.0)]);
    assert_eq!((page.total,page.counts.all,page.counts.image,page.counts.video,page.counts.audio),(4,4,2,1,1));
    assert_eq!(page.total_bytes, 16);
    let filtered = store.gallery_page(&GalleryQuery { r#type:"audio".into(), ..query() }).unwrap();
    assert_eq!(filtered.total,1); assert_eq!(filtered.counts.all,4);
    assert_eq!(store.gallery_keys(&GalleryQuery { sort:"oldest".into(), ..query() }).unwrap().keys, ["catalog","pocket","native","tie"]);
    assert_eq!(store.gallery_keys(&GalleryQuery { sort:"largest".into(), ..query() }).unwrap().keys, ["catalog","native","pocket","tie"]);
    assert_eq!(store.gallery_index(&query(), "pocket").unwrap(), Some(2));
    let work = body_io::take_body_io(); assert!(work.complete()); assert!(work.domains.is_empty()); assert_eq!(work.stat_requests,0);
}

#[test]
fn gallery_pages_are_bounded_and_delete_is_one_revision_gated_commit() {
    let (_directory, mut store) = fixture();
    let aliases = (0..600).map(|i| alias(&format!("item-{i:04}"), "image", json!({}))).collect::<Vec<_>>();
    store.commit_with_asset_aliases(&WorkingSetCommit { expected_revision:store.revision().unwrap(), ..Default::default() }, &aliases).unwrap();
    let page = store.gallery_page(&GalleryQuery { limit:usize::MAX, ..query() }).unwrap();
    assert_eq!((page.total,page.items.len()), (600,512));
    let next = store.gallery_page(&GalleryQuery { offset:512,limit:128,..query() }).unwrap(); assert_eq!(next.items.len(),88);
    assert_eq!(next.items[0].alias.key,"item-0512");
    let keys = store.gallery_keys(&query()).unwrap();
    let before = keys.revision;
    let result = store.gallery_delete(&keys.keys, before, None).unwrap();
    assert_eq!(result.revision,before+1); assert_eq!(store.gallery_page(&query()).unwrap().total,0);
    put(&mut store,&alias("keep","image",json!({})));
    assert!(matches!(store.gallery_delete(&["keep".into()], before, None),Err(StoreError::RevisionConflict {..})));
    assert_eq!(store.gallery_keys(&query()).unwrap().keys,["keep"]);
}

#[test]
fn gallery_scan_covers_roots_and_message_variants_from_one_real_snapshot() {
    let (_directory, mut store) = fixture();
    for key in ["detail","conversation","message","variant","settings","preset","plugin","unused"] { put(&mut store,&alias(key,"image",json!({}))); }
    character(&mut store);
    store.commit(&WorkingSetCommit { expected_revision:store.revision().unwrap(), root:Some(json!({"note":"{{inlay::settings}}"})), replace_presets:Some(vec![json!({"id":"preset","name":"Synthetic preset","prompt":"{{inlay::preset}}"})]), plugin_storage:Some(vec![super::super::PluginStorageMutation::Set { owner:"synthetic-owner".into(), key:"synthetic".into(),value:json!("{{inlayed::plugin}}") }]), ..Default::default() }).unwrap();
    let revision = store.revision().unwrap();
    let (_, prepared) = store.prepare_gallery_scan(revision).unwrap();
    store.commit(&WorkingSetCommit { expected_revision:revision, root:Some(json!({"note":"{{inlay::settings}} {{inlay::unused}}"})), ..Default::default() }).unwrap();
    prepared.run();
    let status=store.gallery_scan_status().unwrap().unwrap();
    assert_eq!((status.state.as_str(),status.revision,status.counts.used,status.counts.unused),("complete",revision,7,1));
    assert_eq!((status.completed_conversations,status.total_conversations),(1,1));
    assert_eq!(store.gallery_locations("unused").unwrap().state.as_deref(),Some("unused"));
    let locations=store.gallery_locations("variant").unwrap().locations;
    assert_eq!(locations.len(),1);assert_eq!(locations[0].kind,"message");assert_eq!(locations[0].message_id.as_deref(),Some("message"));assert_eq!(locations[0].message_index,Some(0));
    assert_eq!(store.gallery_keys(&GalleryQuery {character_id:Some("character".into()),..query()}).unwrap().keys.len(),4);
    assert_eq!(store.gallery_usage_count(&["message".into(),"variant".into(),"message".into()]).unwrap(),2);
    put(&mut store,&alias("new","image",json!({})));
    assert_eq!(store.gallery_locations("new").unwrap().state,None);
    assert_eq!(store.gallery_keys(&GalleryQuery {usage:"unused".into(),..query()}).unwrap().keys,["unused"]);
    assert!(matches!(store.gallery_delete(&["unused".into()],revision,None),Err(StoreError::RevisionConflict {..})));
    scan(&mut store);
    assert_eq!(store.gallery_locations("unused").unwrap().state.as_deref(),Some("used"));
}

#[test]
fn gallery_archive_payload_finds_rewritten_alias_and_unreadable_archive_never_proves_unused() {
    let (_directory, mut store) = fixture();
    body(&mut store,"message",b"old attachment");
    body(&mut store,"detail",b"unchanged attachment");
    put(&mut store,&alias("unused","image",json!({})));
    character(&mut store);
    store.archive_character("character",store.revision().unwrap(),1).unwrap();
    body(&mut store,"message",b"rewritten WebP attachment");
    let status=scan(&mut store);assert_eq!(status.unchecked_archives,0);
    assert_eq!((status.completed_conversations,status.total_conversations,status.completed_items,status.total_items),(1,1,2,2));
    let used=store.gallery_locations("message").unwrap();assert_eq!(used.state.as_deref(),Some("used"));
    assert!(used.locations.iter().any(|location|location.archived && location.message_id.as_deref()==Some("message")));
    assert_eq!(store.gallery_keys(&GalleryQuery {usage:"unused".into(),..query()}).unwrap().keys,["unused"]);
    let generation=super::super::active_generation(&store.connection).unwrap();
    let archived=super::super::archive::read_archived_object(&store.connection,&generation,"character").unwrap().unwrap();
    let cas=PayloadCas::new(store.repository_root()).unwrap();
    let bytes=cas.stat_object(&archived.object_hash).unwrap().unwrap();
    cas.unlink_exact_object(&archived.object_hash,bytes,&crate::asset_repository::object_physical_key(&archived.object_hash)).unwrap();
    let status=scan(&mut store);assert_eq!(status.unchecked_archives,1);
    assert_eq!(store.gallery_locations("message").unwrap().state.as_deref(),Some("unknown"));
    assert_eq!(store.gallery_locations("unused").unwrap().state.as_deref(),Some("unknown"));
    assert_eq!(store.gallery_locations("detail").unwrap().state.as_deref(),Some("used"));
    assert!(store.gallery_keys(&GalleryQuery {usage:"unused".into(),..query()}).unwrap().keys.is_empty());
}

#[test]
fn gallery_cancel_and_replacement_do_not_publish_results_into_another_library() {
    let (_directory,mut store)=fixture();
    put(&mut store,&alias("same","image",json!({})));
    let (_,prepared)=store.prepare_gallery_scan(store.revision().unwrap()).unwrap();
    assert!(store.gallery_scan_cancel().unwrap());prepared.run();
    assert_eq!(store.gallery_scan_status().unwrap().unwrap().state,"cancelled");assert_eq!(store.gallery_locations("same").unwrap().state,None);
    let completed = scan(&mut store);
    let (_,prepared)=store.prepare_gallery_scan(store.revision().unwrap()).unwrap();
    store.gallery_scan_cancel().unwrap(); prepared.run();
    let page = store.gallery_page(&GalleryQuery {usage:"unused".into(),..query()}).unwrap();
    assert_eq!(store.gallery_scan_status().unwrap().unwrap().state,"cancelled");
    assert_eq!(store.gallery_scan_result().unwrap().unwrap().id,completed.id);
    assert_eq!(page.items[0].alias.key,"same");
    let (_,prepared)=store.prepare_gallery_scan(store.revision().unwrap()).unwrap();
    let stage=store.replace_begin().unwrap().staging_id;
    store.replace_put_root(&stage,&json!({})).unwrap();
    store.replace_commit(&stage,Some(store.revision().unwrap())).unwrap();
    prepared.run();
    assert!(store.gallery_scan_status().unwrap().is_none());
    put(&mut store,&alias("same","image",json!({})));
    assert_eq!(store.gallery_locations("same").unwrap().state,None);
}

#[test]
fn gallery_scan_reads_device_plugin_values_and_a_later_device_write_refuses_the_unused_delete() {
    let (_directory, mut store) = fixture();
    for key in ["device", "unused"] { put(&mut store, &alias(key, "image", json!({}))); }
    device_value(&mut store, "note", "{{inlay::device}}");
    let status = scan(&mut store);
    assert_eq!((status.counts.used, status.counts.unused, status.unchecked_records), (1, 1, 0));
    let device = store.gallery_locations("device").unwrap();
    assert_eq!((device.state.as_deref(), device.locations[0].kind.as_str()), (Some("used"), "plugin"));
    device_value(&mut store, "later", "{{inlay::unused}}");
    assert_eq!(store.revision().unwrap(), status.revision);
    assert!(matches!(store.gallery_delete(&["unused".into()], status.revision, Some(&status.id)), Err(StoreError::RevisionConflict {..})));
    let rescanned = scan(&mut store);
    assert!(matches!(store.gallery_delete(&["unused".into()], rescanned.revision, Some(&status.id)), Err(StoreError::RevisionConflict {..})));
    assert_eq!(store.gallery_locations("unused").unwrap().state.as_deref(), Some("used"));
    store.gallery_delete(&["unused".into()], rescanned.revision, Some(&rescanned.id)).unwrap();
    assert_eq!(store.gallery_missing_keys(&["device".into(), "unused".into(), "never".into()]).unwrap(), ["unused", "never"]);
}

#[test]
fn an_undecodable_record_completes_the_scan_and_holds_back_unused() {
    let (_directory, mut store) = fixture();
    for key in ["message", "unused"] { put(&mut store, &alias(key, "image", json!({}))); }
    character(&mut store);
    store.connection.execute("UPDATE messages SET value='{\"data\":\"{{inlay::message}}\"' WHERE message_id='message'", []).unwrap();
    let status = scan(&mut store);
    assert_eq!((status.unchecked_records, status.counts.used, status.counts.unused, status.counts.unknown), (1, 1, 0, 1));
    assert_eq!(store.gallery_locations("message").unwrap().state.as_deref(), Some("used"));
    let unused = store.gallery_locations("unused").unwrap();
    assert_eq!((unused.state.as_deref(), unused.unchecked_records), (Some("unknown"), 1));
    assert!(store.gallery_keys(&GalleryQuery { usage: "unused".into(), ..query() }).unwrap().keys.is_empty());
}

#[test]
fn gallery_scan_hydrates_a_remote_archive_before_resolving_locations() {
    use crate::server_sync::lww_tests::{drain_publications, receive_available, LocalServerFixture};
    use crate::server_sync::residency::AssetPolicy;
    let server = LocalServerFixture::new();
    let (_directory, mut store) = fixture();
    let core = server.client(&store);
    let mut sent = body(&mut store, "message", b"synthetic remotely archived attachment");
    sent.metadata = json!({"createdAt":1234567890000_i64,"charId":"character","chatId":"conversation"});
    put(&mut store, &sent);
    character(&mut store);
    store.archive_character("character", store.revision().unwrap(), 1).unwrap();
    drain_publications(&core, &mut store, &[]).unwrap();
    let (_peer_directory, mut peer) = fixture();
    let peer_core = server.client(&peer);
    receive_available(&peer_core, &mut peer, &[]).unwrap();
    let received = peer.read_asset_alias("inlay", "message", None).unwrap().unwrap().value;
    assert_eq!(received.metadata, sent.metadata);
    let source_page = store.gallery_page(&query()).unwrap();
    let peer_page = peer.gallery_page(&query()).unwrap();
    assert_eq!(peer_page.items[0].sort_time, 1234567890000.0);
    assert_eq!(peer_page.items[0].sort_time, source_page.items[0].sort_time);
    store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    let generation = super::super::active_generation(&store.connection).unwrap();
    let archived = super::super::archive::read_archived_object(&store.connection, &generation, "character").unwrap().unwrap();
    let cas = PayloadCas::new(store.repository_root()).unwrap();
    assert!(cas.stat_object(&archived.object_hash).unwrap().is_none());
    let status = scan(&mut store);
    assert_eq!(status.unchecked_archives, 0);
    assert!(cas.stat_object(&archived.object_hash).unwrap().is_some());
    let used = store.gallery_locations("message").unwrap();
    assert_eq!(used.state.as_deref(), Some("used"));
    assert!(used.locations.iter().any(|location| location.archived && location.message_id.as_deref() == Some("message")));
}

#[test]
fn dropping_a_prepared_gallery_scan_releases_running_state_and_keeps_the_last_result() {
    let (_directory, mut store) = fixture();
    put(&mut store, &alias("unused", "image", json!({})));
    let complete = scan(&mut store);
    let (_, prepared) = store.prepare_gallery_scan(store.revision().unwrap()).unwrap();
    drop(prepared);
    assert_eq!(store.gallery_scan_status().unwrap().unwrap().state, "cancelled");
    assert_eq!(store.gallery_scan_result().unwrap().unwrap().id, complete.id);
    assert_eq!(store.gallery_keys(&GalleryQuery { usage: "unused".into(), ..query() }).unwrap().keys, ["unused"]);
    assert_ne!(scan(&mut store).id, complete.id);
}

#[test]
fn cold_storage_references_never_prove_unlocated_attachments_unused() {
    for root in [json!({"data":"\u{ef01}COLDSTORAGE\u{ef01}synthetic"}), json!({"coldstorage":"synthetic"}), json!({"coldStoragedChats":["synthetic"]})] {
        let (_directory, mut store) = fixture();
        put(&mut store, &alias("unused", "image", json!({})));
        store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), root: Some(root), ..Default::default() }).unwrap();
        let status = scan(&mut store);
        assert!(status.unchecked_records > 0);
        assert_eq!((status.counts.unused, status.counts.unknown), (0, 1));
        assert_eq!(store.gallery_locations("unused").unwrap().state.as_deref(), Some("unknown"));
    }
}

#[test]
fn pending_sync_references_and_same_revision_receives_guard_unused_deletion() {
    use super::super::lww::{self, ApplyReceive, Change, Header, Progress, StageReceive};
    use risunest_sync_wire::stamp::Stamp;
    let (_directory, mut store) = fixture();
    for key in ["held", "deferred", "unused"] { put(&mut store, &alias(key, "image", json!({}))); }
    character(&mut store);
    let original = scan(&mut store);
    let header = Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: "gallery-pending".into() };
    let stamp = Stamp { physical_ms: (store.lww_clock_state().unwrap().issued.unwrap().physical_ms.0 + 1).into(), logical: 0, writer_id: "00000000-0000-4000-8000-000000000001".into() };
    let (_source_directory, mut source) = fixture();
    character(&mut source);
    source.commit(&WorkingSetCommit { expected_revision: source.revision().unwrap(), conversations: Some(vec![super::super::ConversationMutation::ReplaceRange {
        character_id: "character".into(), conversation_id: "conversation".into(), start: 0, delete_count: 1,
        messages: vec![json!({"chatId":"pending-message","role":"user","data":"{{inlay::deferred}}"})], conversation: None, configured_index: None,
    }]), ..Default::default() }).unwrap();
    let mut objects = source.connection.prepare("SELECT hash,body FROM message_page_objects").unwrap();
    for object in objects.query_map([], |row| Ok((row.get::<_,String>(0)?,row.get::<_,Vec<u8>>(1)?))).unwrap() {
        let (hash, body) = object.unwrap(); store.lww_put_object(&hash, &body).unwrap();
    }
    let message_key = lww::unit_key(&["messages", "character", "conversation"]).unwrap();
    let value = lww::read_unit(&source.connection, &message_key).unwrap().unwrap().1;
    store.lww_stage_receive(&StageReceive { header: header.clone(), changes: vec![
        Change { key: lww::unit_key(&["character", "missing-parent", "desc"]).unwrap(), stamp: stamp.clone(), value: lww::inline(&json!("{{inlay::held}}")).unwrap() },
        Change { key: message_key, stamp, value },
    ], progress: Progress { kind: "server".into(), cursor: 1.into(), writer_id: None }, admitted_time_upper_ms: u64::MAX.into() }).unwrap();
    let applied = store.lww_apply_receive(&ApplyReceive { header: header.clone(), generating: vec![super::super::lww::MessageLocator { character_id: "character".into(), conversation_id: "conversation".into(), start: None }] }).unwrap();
    assert_eq!((applied.held_keys.len(), applied.deferred_keys.len()), (1, 1));
    store.lww_finish_receive(&header).unwrap();
    assert_eq!(store.revision().unwrap(), original.revision);
    assert!(matches!(store.gallery_delete(&["held".into(), "deferred".into()], original.revision, Some(&original.id)), Err(StoreError::RevisionConflict {..})));
    assert!(store.gallery_missing_keys(&["held".into(), "deferred".into()]).unwrap().is_empty());
    let rescanned = scan(&mut store);
    assert_eq!((rescanned.counts.used, rescanned.counts.unused, rescanned.unchecked_records), (2, 1, 0));
    for key in ["held", "deferred"] {
        let locations = store.gallery_locations(key).unwrap();
        assert_eq!(locations.state.as_deref(), Some("used"));
        assert_eq!(locations.locations[0].kind, "pending");
    }
    let page = store.gallery_page(&GalleryQuery { usage: "unused".into(), ..query() }).unwrap();
    assert_eq!(page.scan_id.as_deref(), Some(rescanned.id.as_str()));
    assert_eq!(page.items.iter().map(|item| item.alias.key.as_str()).collect::<Vec<_>>(), ["unused"]);
}
