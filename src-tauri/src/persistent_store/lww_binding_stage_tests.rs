use super::*;
use crate::persistent_store::commit;
use super::super::{inline, read_unit, unit_key, ApplyReceive, Progress, StageReceive, UnitMutation};
use crate::persistent_store::{
    active_generation, query, sync_selection::{ReplaceBindingRequest, SwitchBindingRequest, SyncTarget},
    ReadTarget, WorkingSetCommit,
};
use risunest_sync_wire::{descriptor::RecordDescriptor, stamp::Stamp};
use serde_json::{json, Value};

fn change(key: &[&str], value: Value) -> Change {
    Change { key: unit_key(key).unwrap(), stamp: Stamp { physical_ms: 7.into(), logical: 0, writer_id: "00000000-0000-4000-8000-000000000001".into() }, value: inline(&value).unwrap() }
}
fn context(store: &PersistentStore) -> (Header, String) {
    let header = Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: uuid::Uuid::new_v4().to_string() };
    let inspection = store.register_lww_binding_inspection(header.binding_authority, &SyncTarget::Server("remote".into()), "target", "library").unwrap();
    (header, inspection)
}
fn catalog(store: &PersistentStore, staging: &str) -> Value {
    query::materialize_target(&store.connection, &ReadTarget { generation: staging.into(), revision: store.revision().unwrap() }).unwrap()
}
fn activate(store: &mut PersistentStore, header: &Header, inspection: &str, stage: &BindingUnitStage) {
    let state = store.lww_binding_state().unwrap();
    let state = store.switch_lww_binding(&SwitchBindingRequest {
        initial_publication: false,
        header: Header { binding_authority: state.target_authority, request_id: uuid::Uuid::new_v4().to_string() },
        expected_selection_epoch: state.selection_epoch, target: SyncTarget::Server("remote".into()), inspection_id: Some(inspection.into()),
    }).unwrap();
    store.replace_lww_binding(&ReplaceBindingRequest {
        header: Header { binding_authority: state.target_authority, request_id: header.request_id.clone() },
        expected_selection_epoch: state.selection_epoch, staging_id: stage.staging_id.clone(), receive_id: header.request_id.clone(), target_id: "target".into(), library_id: "library".into(),
    }).unwrap();
}
fn save(store: &mut PersistentStore, mutations: Vec<UnitMutation>) {
    store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), unit_mutations: Some(mutations), ..Default::default() }).unwrap();
}
fn message_value(store: &PersistentStore, values: &[Value]) -> UnitValue {
    use risunest_external_storage_format::message_pages::{repage, MessageHash};
    let bodies = values.iter().map(|v| risunest_sync_wire::payload_value::encode(v).unwrap()).collect::<Vec<_>>();
    let repaged = repage::<crate::persistent_store::StoreError>(&Default::default(), bodies.iter().map(|b| MessageHash::from_bytes(b)).collect(), 0..0, bodies.len(), |i| Ok(bodies[i].clone())).unwrap();
    for object in repaged.objects { store.lww_put_object(&object.hash, &object.bytes).unwrap(); }
    let manifest = repaged.index.manifest().encode().unwrap();
    store.lww_put_object(&manifest.hash, &manifest.bytes).unwrap();
    let mut descriptor = RecordDescriptor::content(manifest.hash);
    descriptor.dependencies = repaged.index.pages.iter().map(|page| page.page.hash.clone()).collect();
    descriptor.dependencies.sort(); descriptor.dependencies.dedup();
    UnitValue::object(descriptor).unwrap()
}

#[test]
fn staging_is_incoming_only_and_does_not_remap_active_retired_ids_or_issue_stamps() {
    let dir = tempfile::tempdir().unwrap(); let mut store = PersistentStore::open(dir.path()).unwrap();
    save(&mut store, vec![
        UnitMutation::Set { key: unit_key(&["root","language"]).unwrap(), value: json!("local") },
        UnitMutation::Set { key: unit_key(&["exists","character","same-id"]).unwrap(), value: json!(true) },
    ]);
    save(&mut store, vec![UnitMutation::Delete { key: unit_key(&["exists","character","same-id"]).unwrap() }]);
    let clock = serde_json::to_value(store.lww_clock_state().unwrap()).unwrap();
    let revision = store.revision().unwrap(); let generation = active_generation(&store.connection).unwrap();
    let outbox = serde_json::to_value(store.lww_read_outbox(0.into(), 100).unwrap()).unwrap();
    let incoming = vec![change(&["root","language"], json!("remote")), change(&["exists","character","same-id"], json!({"type":"character"})), change(&["character","same-id","name"], json!("remote-name")), change(&["character","same-id","futureField"], json!({"opaque":true}))];
    let (header, inspection) = context(&store);
    let stage = store.lww_stage_binding_units(&header, &inspection, &incoming, 7.into()).unwrap();
    let value = catalog(&store, &stage.staging_id);
    assert_eq!(value["language"], "remote"); assert_eq!(value["characters"][0]["chaId"], "same-id");
    assert_eq!(value["characters"][0]["name"], "remote-name"); assert!(value["characters"][0].get("futureField").is_none());
    assert_eq!(store.read_root(None).unwrap().value["language"], "local");
    assert_eq!(store.revision().unwrap(), revision); assert_eq!(active_generation(&store.connection).unwrap(), generation);
    assert_eq!(serde_json::to_value(store.lww_clock_state().unwrap()).unwrap(), clock);
    assert_eq!(serde_json::to_value(store.lww_read_outbox(0.into(), 100).unwrap()).unwrap(), outbox);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_binding_source_units WHERE staging_id=?1", [&stage.staging_id], |r| r.get::<_,i64>(0)).unwrap(), incoming.len() as i64);
}

#[test]
fn held_children_survive_activation_restart_and_project_on_parent_arrival_without_restamping() {
    let dir = tempfile::tempdir().unwrap(); let mut store = PersistentStore::open(dir.path()).unwrap();
    let mut messages = change(&["messages","missing","chat"], Value::Null);
    messages.value = message_value(&store, &[json!({"role":"user","data":"synthetic-message","chatId":"m"})]);
    let incoming = vec![change(&["exists","conversation","missing","chat"], json!(true)), change(&["conversation","missing","chat","name"], json!("held-name")), messages.clone(), change(&["future","opaque"], json!({"untouched":true}))];
    let (header, inspection) = context(&store);
    let stage = store.lww_stage_binding_units(&header, &inspection, &incoming, 7.into()).unwrap();
    assert!(catalog(&store, &stage.staging_id)["characters"].as_array().unwrap().is_empty());
    activate(&mut store, &header, &inspection, &stage);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_receive_rows WHERE status='held'", [], |r| r.get::<_,i64>(0)).unwrap(), 3);
    drop(store); let mut store = PersistentStore::open(dir.path()).unwrap();
    let receive = Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: uuid::Uuid::new_v4().to_string() };
    store.lww_stage_receive(&StageReceive { header: receive.clone(), changes: vec![change(&["exists","character","missing"], json!({"type":"character"}))], progress: Progress {kind:"server".into(),cursor:1.into(),writer_id:None}, admitted_time_upper_ms:7.into() }).unwrap();
    let result = store.lww_apply_receive(&ApplyReceive { header: receive.clone(), generating:vec![] }).unwrap();
    store.lww_finish_receive(&receive).unwrap();
    assert!(result.held_keys.is_empty()); assert!(result.affected_keys.contains(&messages.key));
    let value = catalog(&store, &active_generation(&store.connection).unwrap());
    assert_eq!(value["characters"][0]["chats"][0]["name"], "held-name");
    assert_eq!(value["characters"][0]["chats"][0]["message"][0]["data"], "synthetic-message");
    assert_eq!(read_unit(&store.connection, &messages.key).unwrap(), Some((messages.stamp.clone(),messages.value.clone())));
    assert_eq!(store.connection.query_row("SELECT version FROM lww_units WHERE key=?1", [messages.key.as_str()], |r| r.get::<_,String>(0)).unwrap(), header.request_id);
    assert_eq!(read_unit(&store.connection, &incoming[3].key).unwrap(), Some((incoming[3].stamp.clone(),incoming[3].value.clone())));
    assert!(store.lww_read_outbox(store.lww_binding_authority().unwrap(),100).unwrap().entries.is_empty());
}

#[test]
fn held_message_deferred_during_generation_projects_after_restart_and_drain_without_restamping() {
    let dir=tempfile::tempdir().unwrap(); let mut store=PersistentStore::open(dir.path()).unwrap();
    let mut messages=change(&["messages","missing","chat"],Value::Null);
    messages.value=message_value(&store,&[json!({"data":"deferred-held-message","chatId":"m"})]);
    let incoming=vec![change(&["exists","conversation","missing","chat"],json!(true)),messages.clone()];
    let (header,inspection)=context(&store); let stage=store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).unwrap();
    activate(&mut store,&header,&inspection,&stage);
    let receive=Header{binding_authority:store.lww_binding_authority().unwrap(),request_id:uuid::Uuid::new_v4().to_string()};
    store.lww_stage_receive(&StageReceive{header:receive.clone(),changes:vec![change(&["exists","character","missing"],json!(true))],progress:Progress{kind:"server".into(),cursor:1.into(),writer_id:None},admitted_time_upper_ms:7.into()}).unwrap();
    let applied=store.lww_apply_receive(&ApplyReceive{header:receive.clone(),generating:vec![super::super::MessageLocator{character_id:"missing".into(),conversation_id:"chat".into(),start:None}]}).unwrap();
    assert_eq!(applied.deferred_keys,vec![messages.key.clone()]);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM messages",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    store.lww_finish_receive(&receive).unwrap();
    drop(store); let mut store=PersistentStore::open(dir.path()).unwrap();
    let drained=store.lww_drain_deferred(&ApplyReceive{header:Header{binding_authority:store.lww_binding_authority().unwrap(),request_id:uuid::Uuid::new_v4().to_string()},generating:vec![]}).unwrap();
    assert!(drained.deferred_keys.is_empty()); assert!(drained.affected_keys.contains(&messages.key));
    let value=catalog(&store,&active_generation(&store.connection).unwrap());
    assert_eq!(value["characters"][0]["chats"][0]["message"][0]["data"],"deferred-held-message");
    assert_eq!(read_unit(&store.connection,&messages.key).unwrap(),Some((messages.stamp.clone(),messages.value.clone())));
    assert_eq!(store.connection.query_row("SELECT version FROM lww_units WHERE key=?1",[messages.key.as_str()],|r|r.get::<_,String>(0)).unwrap(),header.request_id);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_receive_rows WHERE request_id=?1",[&header.request_id],|r|r.get::<_,i64>(0)).unwrap(),0);
    assert!(store.lww_read_outbox(store.lww_binding_authority().unwrap(),100).unwrap().entries.is_empty());
}

#[test]
fn source_retirement_suppresses_descendants_and_order_uses_creation_stamps() {
    let dir = tempfile::tempdir().unwrap(); let mut store = PersistentStore::open(dir.path()).unwrap();
    let mut retired = change(&["exists","character","retired"], json!(true)); retired.value = UnitValue::Deleted;
    let mut first = change(&["exists","character","a"], json!(true)); first.stamp.physical_ms = 3.into();
    let incoming = vec![retired, change(&["character","retired","name"], json!("hidden")), change(&["exists","character","b"], json!(true)), first, change(&["order","characters"], json!(["retired","b"]))];
    let (header, inspection) = context(&store); let stage = store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).unwrap();
    let value = catalog(&store,&stage.staging_id);
    assert_eq!(value["characters"].as_array().unwrap().iter().map(|c| c["chaId"].as_str().unwrap()).collect::<Vec<_>>(), ["b","a"]);
    activate(&mut store,&header,&inspection,&stage);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_retired", [], |r| r.get::<_,i64>(0)).unwrap(), 1);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_receive_rows WHERE status='held'", [], |r| r.get::<_,i64>(0)).unwrap(), 0);
    assert_eq!(read_unit(&store.connection,&incoming[1].key).unwrap(), Some((incoming[1].stamp.clone(),incoming[1].value.clone())));
}

#[test]
fn archive_alias_and_reference_controls_stage_without_reading_any_large_dependency_body() {
    let dir = tempfile::tempdir().unwrap(); let mut store = PersistentStore::open(dir.path()).unwrap();
    let body_hash = "ab".repeat(32); let asset_hash = "cd".repeat(32);
    for hash in [&body_hash,&asset_hash] { store.connection.execute("INSERT INTO message_page_objects VALUES(?1,?2)", params![hash,b"poison-body".as_slice()]).unwrap(); }
    let archived = crate::persistent_store::archive::ArchivedObject { object_hash:body_hash.clone(),shared_object_hash:body_hash, archived_at:1,conversation_count:1,message_count:1,asset_hashes:vec![asset_hash.clone()],shared_asset_hashes:vec![asset_hash.clone()],identity_remap:Vec::new() };
    let mut archive = change(&["archive","archived"],Value::Null); archive.value = projection::archive_value(&store.connection,&archived).unwrap();
    let hashes = (0..100).map(|i| format!("{i:064x}")).collect::<Vec<_>>();
    let (root,objects) = risunest_sync_wire::descriptor::build_reference_tree(&hashes,false).unwrap();
    for (hash,bytes) in objects { store.lww_put_object(&hash,&bytes).unwrap(); }
    let mut descriptor = RecordDescriptor::content("ef".repeat(32)); descriptor.dependency_root = root;
    let mut opaque = change(&["future","object"],Value::Null); opaque.value = UnitValue::object(descriptor).unwrap();
    let incoming = vec![change(&["exists","character","archived"],json!(true)),change(&["character","archived","name"],json!("archive-name")),change(&["exists","conversation","archived","hidden"],json!(true)),archive,change(&["asset","assets/synthetic.bin"],json!({"key":"assets/synthetic.bin","objectHash":asset_hash,"kind":"asset","size":1024,"mime":"application/octet-stream","name":"synthetic","ext":"bin","metadata":{}})),opaque];
    let (header,inspection)=context(&store); let stage=store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).unwrap();
    let value=catalog(&store,&stage.staging_id); assert!(value["characters"].as_array().unwrap().is_empty());
    let marker:String=store.connection.query_row("SELECT detail FROM characters WHERE generation=?1 AND character_id='archived'",[&stage.staging_id],|r|r.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&marker).unwrap()["chaId"],"archived");
    assert_eq!(store.connection.query_row("SELECT count(*) FROM characters WHERE generation=?1 AND archived_object IS NOT NULL",[&stage.staging_id],|r|r.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM asset_aliases WHERE generation=?1",[&stage.staging_id],|r|r.get::<_,i64>(0)).unwrap(),1);
    let original:String=store.connection.query_row("SELECT archived_object FROM characters WHERE generation=?1 AND character_id='archived'",[&stage.staging_id],|r|r.get(0)).unwrap();
    let mut changed:Value=serde_json::from_str(&original).unwrap(); changed["archivedAt"]=json!(2);
    store.connection.execute("UPDATE characters SET archived_object=?2 WHERE generation=?1 AND character_id='archived'",params![stage.staging_id,changed.to_string()]).unwrap();
    assert!(crate::persistent_store::sync_selection::validate_binding_stage_content(&store.connection,&stage.staging_id).is_err());
    let state=store.lww_binding_state().unwrap();
    assert!(store.switch_lww_binding(&SwitchBindingRequest {
        header: Header { binding_authority: state.target_authority, request_id: uuid::Uuid::new_v4().to_string() },
        expected_selection_epoch: state.selection_epoch.clone(), target: SyncTarget::Server("remote".into()), inspection_id: Some(inspection.clone()), initial_publication: false,
    }).is_err());
    assert_eq!(store.lww_binding_state().unwrap().selection_epoch,state.selection_epoch);
    store.connection.execute("UPDATE characters SET archived_object=?2 WHERE generation=?1 AND character_id='archived'",params![stage.staging_id,original]).unwrap();
    activate(&mut store,&header,&inspection,&stage);
    assert!(store.read_character("archived",None).is_err());
}

#[test]
fn admission_failure_leaves_active_state_and_staging_tables_untouched() {
    let dir=tempfile::tempdir().unwrap(); let mut store=PersistentStore::open(dir.path()).unwrap();
    let (header,inspection)=context(&store); let incoming=vec![change(&["root","language"],json!("remote"))];
    assert!(store.lww_stage_binding_units(&header,&inspection,&incoming,6.into()).is_err());
    let mut invalid=change(&["messages","c","chat"],Value::Null); invalid.value=UnitValue::object(RecordDescriptor::content("12".repeat(32))).unwrap();
    assert!(store.lww_stage_binding_units(&header,&inspection,&[invalid],7.into()).is_err());
    assert!(store.lww_stage_binding_units(&header,&inspection,&[incoming[0].clone(),incoming[0].clone()],7.into()).is_err());
    assert!(store.lww_stage_binding_units(&Header{binding_authority:1.into(),..header.clone()},&inspection,&incoming,7.into()).is_err());
    assert_eq!(store.revision().unwrap(),0); assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_binding_sources",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM root WHERE generation LIKE 'staging-%'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}

#[test]
fn frozen_stage_reopens_retries_exactly_and_rejects_changed_source_or_projection() {
    let dir=tempfile::tempdir().unwrap(); let mut store=PersistentStore::open(dir.path()).unwrap();
    let (header,inspection)=context(&store); let incoming=vec![change(&["future","opaque"],json!({"x":1})),change(&["root","language"],json!("remote"))];
    let stage=store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).unwrap();
    let before=store.connection.query_row("SELECT changes FROM lww_binding_stages WHERE staging_id=?1",[&stage.staging_id],|r|r.get::<_,String>(0)).unwrap();
    drop(store); let mut store=PersistentStore::open(dir.path()).unwrap();
    let mut reordered=incoming.clone(); reordered.reverse();
    assert_eq!(store.lww_stage_binding_units(&header,&inspection,&reordered,7.into()).unwrap(),stage);
    assert!(store.lww_stage_binding_units(&header,&inspection,&incoming,8.into()).is_err());
    let mut altered=incoming.clone(); altered[0].value=inline(&json!({"x":2})).unwrap();
    assert!(store.lww_stage_binding_units(&header,&inspection,&altered,7.into()).is_err());
    assert_eq!(store.connection.query_row("SELECT changes FROM lww_binding_stages WHERE staging_id=?1",[&stage.staging_id],|r|r.get::<_,String>(0)).unwrap(),before);
    store.replace_put_root(&stage.staging_id,&json!({"language":"mutated"})).unwrap();
    assert!(store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).is_err());
    assert_eq!(store.lww_binding_authority().unwrap(),0.into()); assert_eq!(store.revision().unwrap(),0);
}

#[test]
fn staging_database_lives_in_app_scratch_beside_a_crash_leftover_and_is_removed() {
    let dir=tempfile::tempdir().unwrap(); let mut store=PersistentStore::open(dir.path()).unwrap();
    let scratch_root=dir.path().join("external-storage").join("scratch");
    let leftover=scratch_root.join("binding-stage-leftover");
    std::fs::create_dir_all(&leftover).unwrap(); std::fs::write(leftover.join("incoming.sqlite"),b"synthetic crash leftover").unwrap();
    let (header,inspection)=context(&store);
    STAGE_SCRATCH.with(|scratch| scratch.replace(None));
    store.lww_stage_binding_units(&header,&inspection,&[change(&["root","language"],json!("remote"))],7.into()).unwrap();
    let staged=STAGE_SCRATCH.with(|scratch| scratch.take()).expect("binding stage scratch");
    assert!(staged.starts_with(&scratch_root),"{staged:?}");
    assert!(!staged.exists());
    assert_eq!(std::fs::read_dir(&scratch_root).unwrap().count(),1);
}

#[test]
fn crash_after_atomic_copy_retains_exact_receipt_before_caller_registration() {
    let dir=tempfile::tempdir().unwrap(); let mut store=PersistentStore::open(dir.path()).unwrap();
    let (header,inspection)=context(&store); let incoming=vec![change(&["character","missing","name"],json!("held"))];
    FAIL_AFTER_STAGE_COPY.with(|fail| fail.set(true));
    assert!(store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).is_err());
    let frozen:(String,String)=store.connection.query_row("SELECT staging_id,source_digest FROM lww_binding_sources WHERE request_id=?1",[&header.request_id],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_binding_stages WHERE staging_id=?1 AND receive_id=?2",params![frozen.0,header.request_id],|r|r.get::<_,i64>(0)).unwrap(),1);
    drop(store); let mut store=PersistentStore::open(dir.path()).unwrap();
    let stage: super::super::BindingUnitStage =store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).unwrap();
    assert_eq!((stage.staging_id,stage.source_digest),frozen); assert_eq!(store.revision().unwrap(),0);
}

#[test]
fn startup_sweeps_unattested_and_abandoned_stages_but_retains_frozen_binding_stage() {
    let dir=tempfile::tempdir().unwrap(); let mut store=PersistentStore::open(dir.path()).unwrap();
    let abandoned=store.replace_begin().unwrap().staging_id;
    let (header,inspection)=context(&store); let incoming=vec![change(&["root","language"],json!("remote"))];
    let stage=store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).unwrap();
    let (other_header,other_inspection)=context(&store);
    let unattested=store.lww_stage_binding_units(&other_header,&other_inspection,&[],0.into()).unwrap();
    store.connection.execute("DELETE FROM lww_binding_stages WHERE staging_id=?1",[&unattested.staging_id]).unwrap();
    drop(store); let mut store=PersistentStore::open(dir.path()).unwrap();
    for id in [&abandoned,&unattested.staging_id] {
        assert_eq!(commit::generation_state(&store.connection,id).unwrap().as_deref(),Some("retired"));
    }
    while store.purge_retired_batch(256).unwrap() {}
    for id in [&abandoned,&unattested.staging_id] {
        assert_eq!(store.connection.query_row("SELECT count(*) FROM root WHERE generation=?1",[id],|r|r.get::<_,i64>(0)).unwrap(),0);
    }
    assert!(store.lww_stage_binding_units(&other_header,&other_inspection,&[],0.into()).is_err());
    assert_eq!(store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).unwrap(),stage);
}

#[test]
fn uncertain_target_activation_replays_device_intent_without_replacing_later_edits_or_losing_holds() {
    let dir=tempfile::tempdir().unwrap(); let mut store=PersistentStore::open(dir.path()).unwrap();
    let (header,inspection)=context(&store); let incoming=vec![change(&["character","missing","name"],json!("held")),change(&["root","language"],json!("remote"))];
    let stage=store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).unwrap();
    let state=store.lww_binding_state().unwrap();
    let state=store.switch_lww_binding(&SwitchBindingRequest {
        initial_publication: false,
        header:Header{binding_authority:state.target_authority,request_id:uuid::Uuid::new_v4().to_string()},expected_selection_epoch:state.selection_epoch,
        target:SyncTarget::Server("remote".into()),inspection_id:Some(inspection.clone()),
    }).unwrap();
    let activation=Header{binding_authority:state.target_authority,request_id:header.request_id.clone()};
    let source=sorted_source(&incoming).unwrap().into_iter().cloned().collect::<Vec<_>>();
    let rows=super::super::intent_rows::write(&mut store.device_store_mut().unwrap().connection,&activation.request_id,"target",source.iter().map(super::super::intent_rows::target_row)).unwrap();
    let intent=super::super::Intent::Target{device_revision: super::super::device_revision(store.device_store().unwrap().connection()).unwrap(),staging_id:stage.staging_id.clone(),changes:rows};
    let (stamp,digest)=store.reserve_intent(&activation,&intent).unwrap();
    let result=commit::replace_commit_lww(&mut store.connection,&stage.staging_id,&activation,&stamp,&digest,&[],true,&source,None,None,None).unwrap();
    drop(store); let mut store=PersistentStore::open(dir.path()).unwrap();
    assert_eq!(store.revision().unwrap(),result.revision);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_receive_rows WHERE status='held'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    save(&mut store,vec![UnitMutation::Set{key:unit_key(&["root","language"]).unwrap(),value:json!("later")}]);
    assert_eq!(store.lww_replace_target(&activation,&stage.staging_id,&source).unwrap().revision,result.revision);
    assert_eq!(store.read_root(None).unwrap().value["language"],"later");
    assert_eq!(store.connection.query_row("SELECT source_digest FROM lww_binding_sources WHERE staging_id=?1",[&stage.staging_id],|r|r.get::<_,String>(0)).unwrap(),stage.source_digest);
    assert!(store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).is_err());
}

#[test]
fn target_proof_and_status_corruption_cannot_change_frozen_source_digest() {
    let dir=tempfile::tempdir().unwrap(); let mut store=PersistentStore::open(dir.path()).unwrap();
    let (header,inspection)=context(&store); let incoming=vec![change(&["character","missing","name"],json!("hidden"))];
    let stage=store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).unwrap();
    let mut different=incoming.clone(); different[0].stamp.logical=1;
    assert!(validate_binding_source(&store.connection,&stage.staging_id,&header,&different).is_err());
    store.connection.execute("UPDATE lww_binding_source_units SET status='ready' WHERE staging_id=?1",[&stage.staging_id]).unwrap();
    assert!(validate_binding_source(&store.connection,&stage.staging_id,&header,&incoming).is_err());
    assert_eq!(store.connection.query_row("SELECT source_digest FROM lww_binding_sources WHERE staging_id=?1",[&stage.staging_id],|r|r.get::<_,String>(0)).unwrap(),stage.source_digest);
}

#[test]
fn missing_source_or_status_proof_fails_before_switch_activation_or_writer_reservation() {
    for delete_source in [true,false] {
        let dir=tempfile::tempdir().unwrap(); let mut store=PersistentStore::open(dir.path()).unwrap();
        let (header,inspection)=context(&store); let incoming=vec![change(&["character","missing","name"],json!("held"))];
        let stage=store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).unwrap();
        let table=if delete_source { "lww_binding_sources" } else { "lww_binding_source_units" };
        store.connection.execute(&format!("DELETE FROM {table} WHERE staging_id=?1"),[&stage.staging_id]).unwrap();
        let clock=serde_json::to_value(store.lww_clock_state().unwrap()).unwrap(); let state=store.lww_binding_state().unwrap();
        assert!(store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).is_err());
        assert!(store.switch_lww_binding(&SwitchBindingRequest {
            initial_publication: false,
            header:Header {binding_authority:state.target_authority,request_id:uuid::Uuid::new_v4().to_string()},expected_selection_epoch:state.selection_epoch.clone(),target:SyncTarget::Server("remote".into()),inspection_id:Some(inspection),
        }).is_err());
        assert!(store.prepare_lww_new_device(&header,&stage.staging_id).is_err());
        assert!(store.lww_replace_target(&header,&stage.staging_id,&incoming).is_err());
        assert_eq!(serde_json::to_value(store.lww_clock_state().unwrap()).unwrap(),clock);
        assert_eq!(store.lww_binding_state().unwrap().selection_epoch,state.selection_epoch);
        assert_eq!(store.device_store().unwrap().connection().query_row("SELECT count(*) FROM lww_new_device_authorizations",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        assert_eq!(store.device_store().unwrap().connection().query_row("SELECT count(*) FROM lww_intents",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        assert_eq!(store.revision().unwrap(),0);
    }
}

#[test]
fn missing_source_after_switch_rejects_replacement_without_creating_intent_or_changing_clock() {
    let dir=tempfile::tempdir().unwrap(); let mut store=PersistentStore::open(dir.path()).unwrap();
    let (header,inspection)=context(&store); let incoming=vec![change(&["root","language"],json!("remote"))];
    let stage=store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).unwrap();
    let state=store.lww_binding_state().unwrap();
    let state=store.switch_lww_binding(&SwitchBindingRequest {
        initial_publication: false,
        header:Header{binding_authority:state.target_authority,request_id:uuid::Uuid::new_v4().to_string()},expected_selection_epoch:state.selection_epoch,target:SyncTarget::Server("remote".into()),inspection_id:Some(inspection),
    }).unwrap();
    store.connection.execute("DELETE FROM lww_binding_sources WHERE staging_id=?1",[&stage.staging_id]).unwrap();
    let clock=serde_json::to_value(store.lww_clock_state().unwrap()).unwrap();
    assert!(store.replace_lww_binding(&ReplaceBindingRequest {
        header:Header{binding_authority:state.target_authority,request_id:header.request_id.clone()},expected_selection_epoch:state.selection_epoch,
        staging_id:stage.staging_id,receive_id:header.request_id,target_id:"target".into(),library_id:"library".into(),
    }).is_err());
    assert_eq!(serde_json::to_value(store.lww_clock_state().unwrap()).unwrap(),clock);
    assert_eq!(store.device_store().unwrap().connection().query_row("SELECT count(*) FROM lww_intents WHERE complete=0",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(store.revision().unwrap(),0);
}

#[test]
fn pending_switch_journal_rechecks_missing_source_before_authority_reservation() {
    use crate::persistent_store::sync_selection::BindingSelectionChange;
    let dir=tempfile::tempdir().unwrap(); let mut store=PersistentStore::open(dir.path()).unwrap();
    let (header,inspection)=context(&store); let incoming=vec![change(&["root","language"],json!("remote"))];
    let stage=store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).unwrap();
    let state=store.lww_binding_state().unwrap();
    let request=SwitchBindingRequest {
        initial_publication: false,
        header:Header{binding_authority:state.target_authority,request_id:uuid::Uuid::new_v4().to_string()},expected_selection_epoch:state.selection_epoch.clone(),target:SyncTarget::Server("remote".into()),inspection_id:Some(inspection.clone()),
    };
    let selection=BindingSelectionChange { initial_publication: false,expected_epoch:state.selection_epoch.clone(),new_epoch:uuid::Uuid::new_v4().to_string(),target:request.target.clone(),library_id:Some("library".into()),target_id:Some("target".into()),inspection_id:Some(inspection)};
    store.connection.execute("INSERT INTO lww_binding_switch_requests VALUES(?1,?2,?3)",params![request.header.request_id,serde_json::to_string(&request).unwrap(),serde_json::to_string(&selection).unwrap()]).unwrap();
    store.connection.execute("DELETE FROM lww_binding_sources WHERE staging_id=?1",[&stage.staging_id]).unwrap();
    assert!(store.switch_lww_binding(&request).is_err());
    assert_eq!(store.lww_binding_authority().unwrap(),state.target_authority);
    assert_eq!(store.lww_binding_state().unwrap().selection_epoch,state.selection_epoch);
    assert_eq!(store.device_store().unwrap().connection().query_row("SELECT count(*) FROM lww_intents",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}

#[test]
fn empty_incoming_stage_copies_no_active_records_and_preserves_exact_empty_receipt() {
    let dir=tempfile::tempdir().unwrap(); let mut store=PersistentStore::open(dir.path()).unwrap();
    save(&mut store,vec![UnitMutation::Set{key:unit_key(&["exists","character","local"]).unwrap(),value:json!(true)}]);
    let (header,inspection)=context(&store); let stage=store.lww_stage_binding_units(&header,&inspection,&[],0.into()).unwrap();
    assert!(catalog(&store,&stage.staging_id)["characters"].as_array().unwrap().is_empty());
    assert_eq!(store.connection.query_row("SELECT changes FROM lww_binding_stages WHERE staging_id=?1",[&stage.staging_id],|r|r.get::<_,String>(0)).unwrap(),stage.source_digest);
    activate(&mut store,&header,&inspection,&stage);
    assert!(catalog(&store,&active_generation(&store.connection).unwrap())["characters"].as_array().unwrap().is_empty());
}

#[test]
fn binding_stage_digest_streams_large_message_catalog_and_detects_tail_changes() {
    use crate::persistent_store::{hash_work::{reset_hash_work, take_hash_work}, sync_selection::binding_stage_digest};
    let dir = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(dir.path()).unwrap();
    let staging = store.replace_begin().unwrap().staging_id;
    let message = json!({"role":"char","data":"x".repeat(4096),"chatId":"synthetic"}).to_string();
    {
        let tx = store.connection.transaction().unwrap();
        let mut insert = tx.prepare("INSERT INTO messages(generation,character_id,conversation_id,message_index,value) VALUES(?1,'character','conversation',?2,?3)").unwrap();
        for index in 0..8192 { insert.execute(params![staging,index,message]).unwrap(); }
        drop(insert);
        tx.commit().unwrap();
    }
    let expected = catalog_digest(&store.connection, &staging).unwrap();
    reset_hash_work();
    assert_eq!(binding_stage_digest(&store.connection, &staging).unwrap(), expected);
    let work = take_hash_work();
    assert!(work.incomplete.is_empty());
    assert_eq!(work.domains["binding_catalog_proof"].calls, 1);
    assert!(work.domains["binding_catalog_proof"].bytes > 32 * 1024 * 1024);
    assert!(!work.domains.contains_key("binding_stage_proof"));
    store.connection.execute("UPDATE messages SET value='{}' WHERE generation=?1 AND message_index=8191",[&staging]).unwrap();
    assert_ne!(binding_stage_digest(&store.connection, &staging).unwrap(), expected);
    store.connection.execute("UPDATE messages SET value=?2 WHERE generation=?1 AND message_index=8191",params![staging,message]).unwrap();
    assert_eq!(binding_stage_digest(&store.connection, &staging).unwrap(), expected);
}

#[test]
fn binding_stage_digest_covers_raw_root_plugin_owners_and_asset_metadata() {
    use crate::persistent_store::sync_selection::binding_stage_digest;
    let dir = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(dir.path()).unwrap();
    let staging = store.replace_begin().unwrap().staging_id;
    let root = json!({"futureRoot":{"nested":true},"hypaV3":{"memos":[{"chatId":"synthetic"}]},"plugins":[{"name":"synthetic"}]}).to_string();
    store.connection.execute("UPDATE root SET value=?2 WHERE generation=?1",params![staging,root]).unwrap();
    store.connection.execute("INSERT INTO plugin_storage(generation,owner,storage_key,byte_size,ordinal,value) VALUES(?1,'orphan','key',2,0,'{}')",[&staging]).unwrap();
    store.connection.execute("INSERT INTO asset_aliases(generation,logical_key,kind,size,mime,name,ext,metadata) VALUES(?1,'assets/synthetic','asset',0,'application/octet-stream','synthetic','bin','{}')",[&staging]).unwrap();
    store.connection.execute("INSERT INTO asset_owner_heads(generation,owner_kind,owner_locator,present,manifest_hash,entry_count) VALUES(?1,'root-module-assets','synthetic',1,?2,0)",params![staging,"ab".repeat(32)]).unwrap();
    let expected = binding_stage_digest(&store.connection, &staging).unwrap();
    for sql in [
        "UPDATE root SET value='{}' WHERE generation=?1",
        "UPDATE plugin_storage SET owner='different' WHERE generation=?1",
        "UPDATE plugin_storage SET value='null' WHERE generation=?1",
        "UPDATE plugin_storage SET ordinal=1 WHERE generation=?1",
        "UPDATE asset_aliases SET metadata='{\"unknown\":true}' WHERE generation=?1",
        "UPDATE asset_aliases SET size=1 WHERE generation=?1",
        "UPDATE asset_owner_heads SET entry_count=1 WHERE generation=?1",
        "DELETE FROM asset_owner_heads WHERE generation=?1",
    ] {
        let tx = store.connection.transaction().unwrap();
        tx.execute(sql,[&staging]).unwrap();
        assert_ne!(binding_stage_digest(&tx, &staging).unwrap(), expected, "{sql}");
        tx.rollback().unwrap();
        assert_eq!(binding_stage_digest(&store.connection, &staging).unwrap(), expected);
    }
}

#[test]
fn triggered_binding_copy_does_not_spool_source_rows_into_an_ephemeral_btree() {
    let dir = tempfile::tempdir().unwrap();
    let store = PersistentStore::open(dir.path()).unwrap();
    store.connection.execute("ATTACH DATABASE ?1 AS binding_incoming", [store.database_path.to_string_lossy().as_ref()]).unwrap();
    let columns = catalog_columns(&store.connection, "messages").unwrap();
    let opcodes = |sql: &str| {
        let expanded = store.connection.prepare(sql).unwrap().expanded_sql().unwrap();
        let mut stmt = store.connection.prepare(&format!("EXPLAIN {expanded}")).unwrap();
        stmt.query_map([], |row| row.get::<_, String>(1)).unwrap().collect::<Result<Vec<_>, _>>().unwrap()
    };
    let bulk = format!("INSERT INTO messages(generation,{0}) SELECT ?1,{0} FROM binding_incoming.messages WHERE generation='incoming'", columns.join(","));
    assert!(opcodes(&bulk).iter().any(|opcode| opcode == "OpenEphemeral"));
    assert!(!opcodes(&binding_copy_insert_sql("messages", &columns)).iter().any(|opcode| opcode == "OpenEphemeral"));
    store.connection.execute_batch("DETACH DATABASE binding_incoming").unwrap();
}

#[test]
fn binding_copy_streams_large_multicolumn_catalog_without_changing_source_types() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("incoming.sqlite");
    let mut source = Connection::open(&path).unwrap();
    schema::initialize(&mut source).unwrap();
    source.execute("INSERT INTO root VALUES('incoming','{}')", []).unwrap();
    let body = json!({"role":"char","data":"x".repeat(4096)}).to_string();
    {
        let tx = source.transaction().unwrap();
        let mut insert = tx.prepare("INSERT INTO messages(generation,character_id,conversation_id,message_index,message_id,value,canonical_hash,canonical_size) VALUES('incoming','character','conversation',?1,NULL,?2,'synthetic-hash',?3)").unwrap();
        for index in 0..8192 { insert.execute(params![index,body,body.len() as i64]).unwrap(); }
        drop(insert);
        tx.execute("INSERT INTO plugin_storage(generation,owner,storage_key,byte_size,ordinal,value,assigned_at) VALUES('incoming','orphan','raw-types',3,0,?1,?2)", params![vec![0u8,1,255],1.5f64]).unwrap();
        tx.commit().unwrap();
    }
    let expected = catalog_digest(&source, "incoming").unwrap();
    source.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    let mut store = PersistentStore::open(&dir.path().join("target")).unwrap();
    store.connection.execute("ATTACH DATABASE ?1 AS binding_incoming", [path.to_string_lossy().as_ref()]).unwrap();
    {
        let tx = store.connection.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
        for &(table, _) in GENERATION_TABLES { copy_binding_table(&tx, table, "copied").unwrap(); }
        assert_eq!(catalog_digest(&tx, "copied").unwrap(), expected);
        tx.commit().unwrap();
    }
    store.connection.execute_batch("DETACH DATABASE binding_incoming").unwrap();
    assert_eq!(catalog_digest(&source, "incoming").unwrap(), expected);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM messages WHERE generation='copied'", [], |row| row.get::<_,i64>(0)).unwrap(),8192);
    let types: (String,String,String) = store.connection.query_row("SELECT typeof(value),typeof(assigned_at),typeof(claimed_from) FROM plugin_storage WHERE generation='copied'", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    assert_eq!(types,("blob".into(),"real".into(),"null".into()));
}

#[test]
fn late_binding_copy_trigger_failure_rolls_back_every_family_and_retries_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(dir.path()).unwrap();
    let values = (0..513).map(|index| json!({"role":"char","data":"x".repeat(4096),"chatId":format!("synthetic-{index}")})).collect::<Vec<_>>();
    let mut messages = change(&["messages","character","conversation"], Value::Null);
    messages.value = message_value(&store, &values);
    let incoming = vec![change(&["root","language"],json!("remote")),change(&["exists","character","character"],json!(true)),change(&["exists","conversation","character","conversation"],json!(true)),messages];
    let (header,inspection) = context(&store);
    let generation = active_generation(&store.connection).unwrap();
    let clock = serde_json::to_value(store.lww_clock_state().unwrap()).unwrap();
    let source_objects: i64 = store.connection.query_row("SELECT count(*) FROM message_page_objects", [], |row| row.get(0)).unwrap();
    store.connection.execute_batch("CREATE TRIGGER reject_binding_tail BEFORE INSERT ON messages WHEN NEW.generation LIKE 'staging-%' AND NEW.message_index=512 BEGIN SELECT RAISE(ABORT,'synthetic tail failure'); END;").unwrap();
    assert!(store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).is_err());
    for &(table, _) in GENERATION_TABLES {
        assert_eq!(store.connection.query_row(&format!("SELECT count(*) FROM {table} WHERE generation LIKE 'staging-%'"), [], |row| row.get::<_,i64>(0)).unwrap(),0,"{table}");
    }
    for table in ["lww_binding_sources","lww_binding_source_units","lww_binding_stages"] {
        assert_eq!(store.connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get::<_,i64>(0)).unwrap(),0,"{table}");
    }
    assert_eq!(active_generation(&store.connection).unwrap(),generation);
    assert_eq!(store.revision().unwrap(),0);
    assert_eq!(store.lww_binding_authority().unwrap(),header.binding_authority);
    assert_eq!(serde_json::to_value(store.lww_clock_state().unwrap()).unwrap(),clock);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM message_page_objects", [], |row| row.get::<_,i64>(0)).unwrap(),source_objects);
    store.connection.execute_batch("DROP TRIGGER reject_binding_tail").unwrap();
    let stage = store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).unwrap();
    assert_eq!(store.connection.query_row("SELECT count(*) FROM messages WHERE generation=?1", [&stage.staging_id], |row| row.get::<_,i64>(0)).unwrap(),513);
    assert_eq!(store.lww_stage_binding_units(&header,&inspection,&incoming,7.into()).unwrap(),stage);
    activate(&mut store,&header,&inspection,&stage);
    let last: String = store.connection.query_row("SELECT value FROM messages WHERE generation=?1 AND message_index=512", [active_generation(&store.connection).unwrap()], |row| row.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&last).unwrap(),values[512]);
}

#[test]
fn binding_replacement_applies_large_shared_and_device_units_from_their_bodies() {
    let dir = tempfile::tempdir().unwrap(); let mut store = PersistentStore::open(dir.path()).unwrap();
    store.device_store_mut().unwrap().set_section_participating(crate::persistent_store::device_store::Section::LocalPlugins, true).unwrap();
    let limit = risunest_sync_wire::unit::MAX_INLINE_UNIT_BYTES;
    let object = |store: &PersistentStore, value: &Value| {
        let body = risunest_sync_wire::payload_value::encode(value).unwrap();
        let hash = risunest_sync_wire::hash(&body);
        store.lww_put_object(&hash, &body).unwrap();
        UnitValue::object(RecordDescriptor::content(hash)).unwrap()
    };
    let language = json!("l".repeat(limit + 1)); let plugin = "p".repeat(limit + 7);
    let mut root = change(&["root","language"], Value::Null); root.value = object(&store, &language);
    let mut local = change(&["plugin-local","synthetic-owner","string","large"], Value::Null); local.value = object(&store, &json!(plugin));
    let incoming = vec![root.clone(), local.clone()];
    let (header, inspection) = context(&store);
    let stage = store.lww_stage_binding_units(&header, &inspection, &incoming, 7.into()).unwrap();
    assert_eq!(catalog(&store, &stage.staging_id)["language"], language);
    activate(&mut store, &header, &inspection, &stage);
    assert_eq!(store.read_root(None).unwrap().value["language"], language);
    assert_eq!(read_unit(&store.connection, &root.key).unwrap(), Some((root.stamp.clone(), root.value.clone())));
    let device = store.device_store().unwrap().connection();
    assert_eq!(read_unit(device, &local.key).unwrap(), Some((local.stamp.clone(), local.value.clone())));
    assert_eq!(device.query_row("SELECT value FROM plugin_device_storage WHERE owner='synthetic-owner' AND space='string' AND key='large' AND tombstone=0", [], |r| r.get::<_,String>(0)).unwrap(), plugin);
}

#[test]
fn a_bootstrap_proves_the_stage_catalog_once_per_boundary() {
    use crate::persistent_store::hash_work::{reset_hash_work, take_hash_work};
    let proofs = || take_hash_work().domains.get("binding_catalog_proof").map_or(0, |work| work.calls);
    let dir = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(dir.path()).unwrap();
    let (header, inspection) = context(&store);
    let incoming = vec![change(&["root","language"],json!("remote")),change(&["exists","character","character"],json!(true))];
    reset_hash_work();
    let stage = store.lww_stage_binding_units(&header, &inspection, &incoming, 7.into()).unwrap();
    let staged = proofs();
    reset_hash_work();
    let state = store.lww_binding_state().unwrap();
    let state = store.switch_lww_binding(&SwitchBindingRequest {
        header: Header { binding_authority: state.target_authority, request_id: uuid::Uuid::new_v4().to_string() },
        expected_selection_epoch: state.selection_epoch, target: SyncTarget::Server("remote".into()), inspection_id: Some(inspection.clone()), initial_publication: false,
    }).unwrap();
    let switched = proofs();
    reset_hash_work();
    store.replace_lww_binding(&ReplaceBindingRequest {
        header: Header { binding_authority: state.target_authority, request_id: header.request_id.clone() },
        expected_selection_epoch: state.selection_epoch, staging_id: stage.staging_id.clone(), receive_id: header.request_id.clone(), target_id: "target".into(), library_id: "library".into(),
    }).unwrap();
    let replaced = proofs();
    assert_eq!((staged, switched, replaced), (1, 1, 1));
    assert_eq!(active_generation(&store.connection).unwrap(), stage.staging_id);
}

#[test]
fn a_binding_stage_cannot_be_activated_as_an_ordinary_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(dir.path()).unwrap();
    let (header, inspection) = context(&store);
    let stage = store.lww_stage_binding_units(&header, &inspection, &[change(&["root","language"],json!("remote"))], 7.into()).unwrap();
    let revision = store.revision().unwrap();
    assert!(store.replace_commit(&stage.staging_id, Some(revision)).is_err());
    assert_eq!(store.revision().unwrap(), revision);
    assert_eq!(commit::generation_state(&store.connection, &stage.staging_id).unwrap().as_deref(), Some("staging"));
    activate(&mut store, &header, &inspection, &stage);
    assert_eq!(active_generation(&store.connection).unwrap(), stage.staging_id);
}

#[test]
fn streaming_binding_rejects_late_producer_failure_and_retries_without_partial_stage() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(dir.path()).unwrap();
    let (header, inspection) = context(&store);
    let generation = active_generation(&store.connection).unwrap();
    let failed = store.lww_stage_binding_units_stream(&header, &inspection, 7.into(), |emit| {
        for index in 0..1025 { emit(change(&["root", &format!("synthetic-{index:04}")], json!(index)))?; }
        Err(error("synthetic-producer-tail"))
    });
    assert!(failed.is_err());
    for table in ["lww_binding_stages", "lww_binding_sources", "lww_binding_source_units"] {
        assert_eq!(store.connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get::<_,i64>(0)).unwrap(), 0);
    }
    assert_eq!(active_generation(&store.connection).unwrap(), generation);
    let stage = store.lww_stage_binding_units_stream(&header, &inspection, 7.into(), |emit| {
        for index in (0..1025).rev() { emit(change(&["root", &format!("synthetic-{index:04}")], json!(index)))?; }
        Ok(())
    }).unwrap();
    let retry = store.lww_stage_binding_units_stream(&header, &inspection, 7.into(), |emit| {
        for index in 0..1025 { emit(change(&["root", &format!("synthetic-{index:04}")], json!(index)))?; }
        Ok(())
    }).unwrap();
    assert_eq!(retry, stage);
    assert_eq!(store.connection.query_row("SELECT length(changes) FROM lww_binding_stages WHERE staging_id=?1", [&stage.staging_id], |row| row.get::<_,i64>(0)).unwrap(), 64);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_binding_source_units WHERE staging_id=?1", [&stage.staging_id], |row| row.get::<_,i64>(0)).unwrap(), 1025);
    activate(&mut store, &header, &inspection, &stage);
    assert_eq!(store.revision().unwrap(), 1);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_units", [], |row| row.get::<_,i64>(0)).unwrap(), 1025);
}

#[test]
fn streamed_binding_source_digest_matches_the_complete_sorted_input() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(dir.path()).unwrap();
    let (header, inspection) = context(&store);
    let changes = vec![change(&["root","username"], json!("synthetic")), change(&["root","language"], json!("ko"))];
    let expected = source_digest(&sorted_source(&changes).unwrap()).unwrap();
    let stage = store.lww_stage_binding_units_stream(&header, &inspection, 7.into(), |emit| {
        for change in &changes { emit(change.clone())?; }
        Ok(())
    }).unwrap();
    assert_eq!(stage.source_digest, expected);
    let rows = binding_source_rows(&store.connection, &stage.staging_id).unwrap();
    assert_eq!(source_digest_rows(&rows).unwrap(), expected);
}
