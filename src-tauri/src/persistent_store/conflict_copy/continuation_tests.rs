use super::*;
use std::cell::Cell;

fn fixture_start() -> (tempfile::TempDir, PersistentStore) {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let staging = store.replace_begin().unwrap().staging_id;
    store.replace_put_root(&staging, &json!({"username":"synthetic","modules":[],"plugins":[]})).unwrap();
    store.replace_put_presets(&staging, &[]).unwrap();
    store.replace_add_characters(&staging, &[json!({"chaId":"source","type":"character","name":"source",
        "chatPage":0,"chatFolders":[{"id":"folder","name":"folder","folded":false}],"privateOwn":"local",
        "futureOwner":{"retained":true},"chats":[{"id":"selected","name":"selected","folderId":"folder",
        "futureChat":{"retained":true},"message":[{"chatId":"same","data":"first"},{"chatId":"same","data":"second"},{"data":"third"}]}]})]).unwrap();
    store.replace_commit(&staging, Some(0)).unwrap();
    (directory, store)
}

fn fixture_finish(directory: tempfile::TempDir, mut store: PersistentStore) -> (tempfile::TempDir, PersistentStore, ConflictCopyRequest, String, CopyOrigin) {
    let source_lease = store.acquire_revision(store.revision().unwrap()).unwrap().lease;
    let original = ConflictCopyRequest { header: lww::Header { request_id:"original-copy".into(), binding_authority:store.lww_binding_authority().unwrap() },
        expected_revision:store.revision().unwrap(), source:store.read_conflict_copy_source(&source_lease, "source", "selected").unwrap(),
        target_character_id:"copy".into(), target_conversation_id:"copied-chat".into(), character_fields:vec![], conversation_fields:vec![],
        ranges:vec![], introduced_asset_bindings:vec![], introduced_owner_head:None };
    assert!(matches!(store.copy_conflicted_conversation(&original, &|| false).unwrap(), ConflictCopyResult::Committed { .. }));
    let OpenContinuationResult::Ready { context_id, origin, .. } = store.open_conflict_copy_continuation(&original).unwrap() else { panic!("open failed") };
    (directory, store, original, context_id, origin)
}

fn fixture() -> (tempfile::TempDir, PersistentStore, ConflictCopyRequest, String, CopyOrigin) {
    let (directory, store) = fixture_start(); fixture_finish(directory, store)
}

fn request(store: &mut PersistentStore, id: &str, context_id: &str, origin: &CopyOrigin, version: u64) -> CopyContinuationRequest {
    CopyContinuationRequest { header:lww::Header { request_id:id.into(), binding_authority:origin.binding_authority.clone() }, origin:origin.clone(),
        context_id:context_id.into(), basis_version:version, current_lease:store.acquire_revision(store.revision().unwrap()).unwrap().lease,
        expected_revision:store.revision().unwrap(), capture_id:format!("capture-{id}"), character_fields:vec![], conversation_fields:vec![],
        ranges:vec![], introduced_asset_bindings:vec![], introduced_owner_head:None }
}
fn set(field: &str, value: Value) -> CopyField { CopyField::Set { field:field.into(), value } }
fn range(start: i64, delete_count: i64, rows: Value) -> CopyRange { CopyRange { start, delete_count, messages:rows.as_array().unwrap().clone() } }
fn history(store: &PersistentStore) -> Value { store.read_conversation("copy", "copied-chat", None).unwrap().unwrap().value["message"].clone() }
fn receipt_count(store: &PersistentStore, id: &str) -> i64 {
    store.connection.query_row("SELECT count(*) FROM lww_requests WHERE request_id=?1", [id], |row| row.get(0)).unwrap()
}
fn outbox_count(store: &PersistentStore) -> i64 { store.connection.query_row("SELECT count(*) FROM lww_outbox", [], |row| row.get(0)).unwrap() }
fn rejected(store: &mut PersistentStore, request: &CopyContinuationRequest, kind: ContinuationConflict) {
    let revision = store.revision().unwrap();
    let before = history(store);
    let outbox = outbox_count(store);
    assert_eq!(store.continue_conflict_copy(request, &|| false).unwrap(), CopyContinuationResult::Conflict { kind });
    assert_eq!(store.revision().unwrap(), revision);
    assert_eq!(history(store), before);
    assert_eq!(outbox_count(store), outbox);
    assert_eq!(receipt_count(store, &request.header.request_id), 0);
}
fn committed(store: &mut PersistentStore, request: &CopyContinuationRequest) {
    assert!(matches!(store.continue_conflict_copy(request, &|| false).unwrap(), CopyContinuationResult::Committed {
        revision, capture_id, next_basis_version, source_character_id, target_character_id, .. }
        if revision == request.expected_revision + 1 && capture_id == request.capture_id
            && next_basis_version == request.basis_version + 1 && source_character_id == "source" && target_character_id == "copy"));
    assert_eq!(receipt_count(store, &request.header.request_id), 1);
}

#[test]
fn ordered_suffix_preserves_archived_source_unknown_fields_and_single_receipt() {
    let (_directory, mut store, _original, id, origin) = fixture();
    store.archive_character("source", store.revision().unwrap(), 33).unwrap();
    let generation = super::super::super::active_generation(&store.connection).unwrap();
    let archived = archive::read_archived_object(&store.connection, &generation, "source").unwrap();
    let mut suffix = request(&mut store, "suffix", &id, &origin, 0);
    suffix.character_fields = vec![set("name", json!("later name")),set("privateOwn", json!("later local"))];
    suffix.conversation_fields = vec![set("futureChat", json!({"later":true}))];
    suffix.ranges = vec![range(1,0,json!([{"data":"local insert"}])),range(1,1,json!([{"data":"edited insert"}])),range(3,1,json!([]))];
    committed(&mut store, &suffix);
    assert_eq!(history(&store), json!([{"chatId":"same","data":"first"},{"data":"edited insert"},{"chatId":"same","data":"second"}]));
    let owner = store.read_character("copy", None).unwrap().unwrap().value;
    assert_eq!(owner["name"], "later name"); assert_eq!(owner["privateOwn"], "later local");
    assert_eq!(owner["futureOwner"], json!({"retained":true}));
    assert_eq!(store.read_conversation("copy", "copied-chat", None).unwrap().unwrap().value["futureChat"], json!({"later":true}));
    assert_eq!(archive::read_archived_object(&store.connection, &generation, "source").unwrap(), archived);
    assert_eq!(store.device_store().unwrap().connection().query_row("SELECT count(*) FROM lww_intents WHERE request_id='suffix'", [], |row| row.get::<_,i64>(0)).unwrap(), 0);
}

#[test]
fn continuation_preserves_new_target_conversations_folders_and_untouched_remote_metadata() {
    let (_directory, mut store, _original, id, origin) = fixture();
    let mut owner = store.read_character("copy", None).unwrap().unwrap().value;
    owner["chatPage"] = json!(1); owner["chatFolders"] = json!([{"id":"new","name":"new","folded":true}]);
    owner["coldstorage"] = json!("synthetic-cold-owner"); owner["coldStoragedChats"] = json!(["synthetic-cold-chat"]);
    owner["futureOwner"] = json!({"remote":true});
    store.commit(&WorkingSetCommit { expected_revision:store.revision().unwrap(), character:Some(owner), conversations:Some(vec![ConversationMutation::ReplaceRange {
        character_id:"copy".into(), conversation_id:"another".into(), start:0,delete_count:0,messages:vec![json!({"data":"remote history"})],
        conversation:Some(json!({"id":"another","name":"another","folderId":"new"})),configured_index:Some(1) }]), ..Default::default() }).unwrap();
    let mut suffix = request(&mut store, "preserve-remote", &id, &origin, 0);
    suffix.character_fields.push(set("name", json!("local name"))); suffix.ranges.push(range(2,1,json!([{"data":"local third"}])));
    committed(&mut store, &suffix);
    let owner = store.read_character("copy", None).unwrap().unwrap().value;
    assert_eq!(owner["chatPage"],1); assert_eq!(owner["chatFolders"],json!([{"id":"new","name":"new","folded":true}]));
    assert_eq!(owner["coldstorage"],"synthetic-cold-owner"); assert_eq!(owner["coldStoragedChats"],json!(["synthetic-cold-chat"]));
    assert_eq!(owner["futureOwner"],json!({"remote":true}));
    assert_eq!(store.read_conversation("copy","another",None).unwrap().unwrap().value["message"],json!([{"data":"remote history"}]));
    let mut later = request(&mut store,"later-field",&id,&origin,1);
    later.character_fields.push(set("futureOwner", json!({"local":true})));
    rejected(&mut store,&later,ContinuationConflict::Field);
}

#[test]
fn one_hop_basis_survives_later_revisions_and_rotates_only_obsolete_pins() {
    let (_directory, mut store, original, id, origin) = fixture();
    let mut first = request(&mut store,"first-hop",&id,&origin,0);
    first.ranges.push(range(0,0,json!([{"data":"first insertion"}])));
    assert!(matches!(store.release_revision(&original.source.lease),Err(StoreError::SnapshotPinned { .. })));
    committed(&mut store,&first);
    store.release_revision(&original.source.lease).unwrap();
    assert!(matches!(store.release_revision(&first.current_lease),Err(StoreError::SnapshotPinned { .. })));
    store.commit(&WorkingSetCommit { expected_revision:store.revision().unwrap(), root_mutations:Some(vec![super::super::super::RootMutation::Set { key:"username".into(), value:json!("new unrelated revision") }]), ..Default::default() }).unwrap();
    let mut second = request(&mut store,"second-hop",&id,&origin,1);
    second.ranges.push(range(0,1,json!([{"data":"second insertion"}])));
    committed(&mut store,&second);
    store.release_revision(&first.current_lease).unwrap();
    assert!(matches!(store.release_revision(&second.current_lease),Err(StoreError::SnapshotPinned { .. })));
    assert_eq!(store.close_copy_continuation(&id,0),CloseContinuationResult::BasisVersionConflict);
    assert_eq!(store.close_copy_continuation(&id,2),CloseContinuationResult::Closed);
    store.release_revision(&second.current_lease).unwrap();
    assert!(matches!(store.read_conversation("copy","copied-chat",Some(&second.current_lease)),Err(StoreError::SnapshotReleased)));
    assert_eq!(history(&store)[0]["data"],"second insertion");
}

#[test]
fn exact_history_and_evolving_range_guards_reject_without_effects() {
    let (_directory, mut store, _original, id, origin) = fixture();
    for (index, ranges) in [vec![range(-1,0,json!([]))],vec![range(0,-1,json!([]))],vec![range(4,0,json!([]))],
        vec![range(2,i64::MAX,json!([]))],vec![range(0,3,json!([])),range(1,0,json!([]))]].into_iter().enumerate() {
        let mut suffix = request(&mut store,&format!("bad-range-{index}"),&id,&origin,0); suffix.ranges=ranges;
        rejected(&mut store,&suffix,ContinuationConflict::Range);
    }
    store.commit(&WorkingSetCommit { expected_revision:store.revision().unwrap(),conversations:Some(vec![ConversationMutation::ReplaceRange {
        character_id:"copy".into(),conversation_id:"copied-chat".into(),start:0,delete_count:0,messages:vec![json!({"chatId":"same","data":"first"})],conversation:None,configured_index:None }]),..Default::default() }).unwrap();
    let mut suffix=request(&mut store,"ambiguous-history",&id,&origin,0); suffix.ranges.push(range(1,1,json!([{"data":"must not retarget"}])));
    rejected(&mut store,&suffix,ContinuationConflict::History);
}

#[test]
fn structural_fields_and_missing_null_metadata_are_distinct() {
    let (_directory, mut store, _original, id, origin) = fixture();
    for field in ["chaId","chats","chatPage","chatFolders","coldstorage","coldStoragedChats","archived","type"] {
        let mut suffix=request(&mut store,&format!("structural-{field}"),&id,&origin,0); suffix.character_fields.push(set(field,json!(null)));
        rejected(&mut store,&suffix,ContinuationConflict::UnsupportedField);
    }
    let mut owner=store.read_character("copy",None).unwrap().unwrap().value; owner["previouslyMissing"]=Value::Null;
    store.commit(&WorkingSetCommit { expected_revision:store.revision().unwrap(),character:Some(owner),..Default::default() }).unwrap();
    let mut suffix=request(&mut store,"null-is-present",&id,&origin,0); suffix.character_fields.push(set("previouslyMissing",json!("local")));
    rejected(&mut store,&suffix,ContinuationConflict::Field);
}

#[test]
fn receipt_replays_after_close_release_cancel_selection_and_library_changes() {
    let (_directory,mut store,original,id,origin)=fixture();
    let mut suffix=request(&mut store,"replay",&id,&origin,0); suffix.ranges.push(range(0,1,json!([{"data":"durable"}])));
    committed(&mut store,&suffix);
    assert_eq!(store.close_copy_continuation(&id,1),CloseContinuationResult::Closed);
    store.release_all_revision_leases().unwrap();
    assert!(matches!(store.continue_conflict_copy(&suffix,&|| true).unwrap(),CopyContinuationResult::Replayed {
        committed_revision,current_identity_matches:true,requires_current_recovery:true,.. } if committed_revision==suffix.expected_revision+1));
    store.connection.execute("UPDATE library_sync_selection SET selection_epoch='changed-selection' WHERE singleton=1",[]).unwrap();
    assert!(matches!(store.continue_conflict_copy(&suffix,&|| false).unwrap(),CopyContinuationResult::Replayed {current_identity_matches:false,..}));
    assert_eq!(store.open_conflict_copy_continuation(&original).unwrap(),OpenContinuationResult::CommittedRefreshRequired);
    let (_other_directory,mut other,_,_,_)=fixture();
    assert_eq!(other.continue_conflict_copy(&suffix,&|| false).unwrap(),CopyContinuationResult::HistoricalStatusUnavailable);
}

#[test]
fn receipt_digest_distinguishes_order_scope_and_exact_native_integers() {
    let (_directory,mut store,_original,id,origin)=fixture();
    let mut suffix=request(&mut store,"digest",&id,&origin,0);
    suffix.character_fields.push(set("futureOwner",json!({"b":1,"a":9007199254740993u64})));
    suffix.ranges=vec![range(0,0,json!([{"data":"a"}])),range(1,0,json!([{"data":"b"}]))]; committed(&mut store,&suffix);
    let mut reordered=suffix.clone(); reordered.character_fields[0]=set("futureOwner",json!({"a":9007199254740993u64,"b":1}));
    assert!(matches!(store.continue_conflict_copy(&reordered,&|| false).unwrap(),CopyContinuationResult::Replayed { .. }));
    for changed in [
        { let mut r=suffix.clone();r.ranges.reverse();r },
        { let mut r=suffix.clone();r.character_fields[0]=set("futureOwner",json!({"a":9007199254740992u64,"b":1}));r },
        { let mut r=suffix.clone();r.capture_id.push('x');r },
        { let mut r=suffix.clone();r.origin.target_conversation_id.push('x');r },
    ] { assert!(store.continue_conflict_copy(&changed,&|| false).unwrap_err().to_string().contains("request-id-integrity")); }
}

fn make_unknown(store: &mut PersistentStore, suffix: &CopyContinuationRequest) {
    let competing=Connection::open(&store.database_path).unwrap();
    let renamed=Cell::new(false);
    let result=store.continue_conflict_copy(suffix,&|| {
        if renamed.get() { return true; }
        let found=competing.query_row("SELECT revision FROM lww_requests WHERE request_id=?1",[&suffix.header.request_id],|row|row.get::<_,i64>(0)).optional().unwrap();
        if found.is_some() { competing.execute("ALTER TABLE lww_requests RENAME TO unresolved_receipts",[]).unwrap();renamed.set(true);true } else { false }
    }).unwrap();
    assert!(renamed.get());
    assert_eq!(result,CopyContinuationResult::OutcomeUnresolved {candidate_request_id:suffix.header.request_id.clone()});
}

#[test]
fn unresolved_receipt_blocks_close_open_new_request_and_keeps_candidate_pins() {
    let (_directory,mut store,original,id,origin)=fixture();
    let mut suffix=request(&mut store,"unknown",&id,&origin,0);suffix.ranges.push(range(0,1,json!([{"data":"committed before fault"}])));
    make_unknown(&mut store,&suffix);
    assert_eq!(store.copy_continuations.contexts[&id].head.version,0);
    assert_eq!(store.close_copy_continuation(&id,0),CloseContinuationResult::CloseBlocked {reason:CloseBlockedReason::OutcomeUnresolved,candidate_request_id:"unknown".into()});
    assert_eq!(store.open_conflict_copy_continuation(&original).unwrap(),OpenContinuationResult::OutcomeUnresolved {candidate_request_id:"unknown".into()});
    assert!(matches!(store.release_revision(&original.source.lease),Err(StoreError::SnapshotPinned { .. })));
    assert!(matches!(store.release_revision(&suffix.current_lease),Err(StoreError::SnapshotPinned { .. })));
    assert!(store.read_conversation("source","selected",Some(&original.source.lease)).unwrap().is_some());
    let mut other=suffix.clone();other.header.request_id="other-request".into();other.ranges.clear();
    assert_eq!(store.continue_conflict_copy(&other,&|| false).unwrap(),CopyContinuationResult::OutcomeUnresolved {candidate_request_id:"unknown".into()});
    let mut changed=suffix.clone();changed.ranges.clear();
    assert!(store.continue_conflict_copy(&changed,&||false).unwrap_err().to_string().contains("request-id-integrity"));
    assert_eq!(store.revision().unwrap(),suffix.expected_revision+1);
    store.connection.execute("ALTER TABLE unresolved_receipts RENAME TO lww_requests",[]).unwrap();
    assert!(matches!(store.continue_conflict_copy(&suffix,&|| true).unwrap(),CopyContinuationResult::Replayed { .. }));
    assert_eq!(store.copy_continuations.contexts[&id].head.version,1);
    assert_eq!(receipt_count(&store,"unknown"),1);
    assert_eq!(store.close_copy_continuation(&id,1),CloseContinuationResult::Closed);
    store.release_revision(&original.source.lease).unwrap();store.release_revision(&suffix.current_lease).unwrap();
}

#[test]
fn terminal_renderer_cleanup_drops_unknown_contexts_without_losing_historical_receipt() {
    let (_directory,mut store,_original,id,origin)=fixture();
    let suffix=request(&mut store,"terminal-unknown",&id,&origin,0);make_unknown(&mut store,&suffix);
    store.release_all_revision_leases().unwrap();
    assert!(store.copy_continuations.contexts.is_empty());assert!(store.revision_leases.is_empty());
    store.connection.execute("ALTER TABLE unresolved_receipts RENAME TO lww_requests",[]).unwrap();
    assert!(matches!(store.continue_conflict_copy(&suffix,&|| false).unwrap(),CopyContinuationResult::Replayed { .. }));
    assert_eq!(receipt_count(&store,"terminal-unknown"),1);
}

#[test]
fn final_revision_guard_rejects_commit_after_preflight_without_target_change_or_receipt() {
    let (directory,store)=fixture_start();
    // Open before acquiring another reader; startup checkpoint admission is not the race under test.
    let competing=std::cell::RefCell::new(PersistentStore::open(directory.path()).unwrap());
    let (_directory,mut store,_original,id,origin)=fixture_finish(directory,store);
    let mut suffix=request(&mut store,"revision-race",&id,&origin,0);suffix.ranges.push(range(0,1,json!([{"data":"must not commit"}])));
    let changed=Cell::new(false);
    let before=history(&store);
    let result=store.continue_conflict_copy(&suffix,&|| {
        if !changed.replace(true) {
            let mut competing=competing.borrow_mut();
            let mut owner=competing.read_character("source",None).unwrap().unwrap().value;owner["name"]=json!("competing revision");
            let expected_revision=competing.revision().unwrap();
            competing.commit(&WorkingSetCommit { expected_revision,character:Some(owner),..Default::default() }).unwrap();
        }
        false
    });
    assert_eq!(result.unwrap_err(),StoreError::RevisionConflict {expected:suffix.expected_revision,actual:suffix.expected_revision+1});
    assert_eq!(store.revision().unwrap(),suffix.expected_revision+1);assert_eq!(history(&store),before);assert_eq!(receipt_count(&store,"revision-race"),0);
    assert!(store.copy_continuations.contexts[&id].candidate.is_none());
}

#[test]
fn receipt_failure_rolls_back_data_outbox_revision_and_releases_candidate_only_pins() {
    let (_directory,mut store,original,id,origin)=fixture();
    let mut suffix=request(&mut store,"receipt-failure",&id,&origin,0);suffix.ranges.push(range(0,1,json!([{"data":"rolled back"}])));
    let before=history(&store);let outbox=outbox_count(&store);
    store.connection.execute_batch("CREATE TRIGGER reject_continuation_receipt BEFORE INSERT ON lww_requests WHEN NEW.request_id='receipt-failure' BEGIN SELECT RAISE(ABORT,'synthetic receipt failure'); END;").unwrap();
    assert!(store.continue_conflict_copy(&suffix,&|| false).is_err());
    assert_eq!(store.revision().unwrap(),suffix.expected_revision);assert_eq!(history(&store),before);assert_eq!(outbox_count(&store),outbox);
    assert_eq!(receipt_count(&store,"receipt-failure"),0);assert!(store.copy_continuations.contexts[&id].candidate.is_none());
    store.release_revision(&suffix.current_lease).unwrap();
    assert!(matches!(store.release_revision(&original.source.lease),Err(StoreError::SnapshotPinned { .. })));
}

#[test]
fn cancelled_and_expired_evidence_leave_settled_basis_usable() {
    let (_directory,mut store,_original,id,origin)=fixture();
    let suffix=request(&mut store,"cancelled",&id,&origin,0);
    assert_eq!(store.continue_conflict_copy(&suffix,&|| true).unwrap(),CopyContinuationResult::Conflict {kind:ContinuationConflict::Cancelled});
    assert_eq!(receipt_count(&store,"cancelled"),0);
    store.release_revision(&suffix.current_lease).unwrap();rejected(&mut store,&suffix,ContinuationConflict::EvidenceExpired);
    let valid=request(&mut store,"valid-after-cancel",&id,&origin,0);committed(&mut store,&valid);
}

fn asset(cas: &PayloadCas, kind: &str, key: &str, bytes: &[u8]) -> AssetAlias {
    let object=cas.prepare_bytes(bytes).unwrap();
    AssetAlias {kind:kind.into(),key:key.into(),object_hash:Some(object.content_hash),size:object.byte_size as i64,
        mime:"image/png".into(),name:"synthetic".into(),ext:"png".into(),inlay_type:(kind=="inlay").then(||"image".into()),
        width:None,height:None,metadata:json!({"unknown":"retained"})}
}
fn proof(scope: AssetScope, pointer: &str, token: Option<usize>, alias: &AssetAlias, lease: &str) -> IntroducedAssetBinding {
    IntroducedAssetBinding {scope,location:AssetLocation {pointer:pointer.into(),token_index:token},alias:alias.clone(),witness_lease:lease.into()}
}
fn asset_fixture() -> (tempfile::TempDir,PersistentStore,ConflictCopyRequest,String,CopyOrigin,AssetAlias) {
    let (directory,mut store)=fixture_start();let cas=PayloadCas::new(directory.path()).unwrap();
    let old=asset(&cas,"asset","same-key",b"original immutable asset");
    let mut owner=store.read_character("source",None).unwrap().unwrap().value;owner["image"]=json!("same-key");
    store.commit_with_asset_aliases(&WorkingSetCommit {expected_revision:store.revision().unwrap(),character:Some(owner),..Default::default()},&[old.clone()]).unwrap();
    let (directory,store,original,id,origin)=fixture_finish(directory,store);(directory,store,original,id,origin,old)
}

#[test]
fn repeated_asset_metadata_uses_canonical_baseline_without_adopting_unrelated_remote_fields() {
    let (directory,mut store,original,id,origin,old)=asset_fixture();
    let original_key=store.read_character("copy",None).unwrap().unwrap().value["image"].as_str().unwrap().to_owned();
    assert_ne!(original_key,"same-key");
    let new=asset(&PayloadCas::new(directory.path()).unwrap(),"asset","same-key",b"new immutable asset");
    store.commit_asset_alias(&new,store.revision().unwrap()).unwrap();
    let witness=store.acquire_revision(store.revision().unwrap()).unwrap().lease;
    let mut owner=store.read_character("copy",None).unwrap().unwrap().value;owner["futureOwner"]=json!({"remote":true});
    store.commit(&WorkingSetCommit {expected_revision:store.revision().unwrap(),character:Some(owner),..Default::default()}).unwrap();
    let mut first=request(&mut store,"image-first",&id,&origin,0);first.character_fields.push(set("image",json!("same-key")));
    first.introduced_asset_bindings.push(proof(AssetScope::CharacterField {mutation:0},"",None,&new,&witness));committed(&mut store,&first);
    let first_key=store.read_character("copy",None).unwrap().unwrap().value["image"].as_str().unwrap().to_owned();assert_ne!(first_key,original_key);
    assert_eq!(store.read_asset_alias("asset",&first_key,None).unwrap().unwrap().value.object_hash,new.object_hash);
    let mut second=request(&mut store,"image-second",&id,&origin,1);second.character_fields.push(set("image",json!("same-key")));
    second.introduced_asset_bindings.push(proof(AssetScope::CharacterField {mutation:0},"",None,&old,&original.source.lease));committed(&mut store,&second);
    let owner=store.read_character("copy",None).unwrap().unwrap().value;assert_eq!(owner["image"],original_key);assert_eq!(owner["futureOwner"],json!({"remote":true}));
    assert_eq!(store.read_asset_alias("asset","same-key",None).unwrap().unwrap().value,new);
    let mut remote=request(&mut store,"remote-not-acknowledged",&id,&origin,2);remote.character_fields.push(set("futureOwner",json!({"local":true})));
    rejected(&mut store,&remote,ContinuationConflict::Field);
}

#[test]
fn scoped_tokens_keep_distinct_old_and_new_bindings_and_validate_deleted_rows() {
    let (directory,mut store,_original,id,origin)=fixture();let cas=PayloadCas::new(directory.path()).unwrap();
    let old=asset(&cas,"inlay","shared",b"old occurrence");store.commit_asset_alias(&old,store.revision().unwrap()).unwrap();
    let old_lease=store.acquire_revision(store.revision().unwrap()).unwrap().lease;
    let new=asset(&cas,"inlay","shared",b"new occurrence");store.commit_asset_alias(&new,store.revision().unwrap()).unwrap();
    let new_lease=store.acquire_revision(store.revision().unwrap()).unwrap().lease;
    let mut suffix=request(&mut store,"scoped-tokens",&id,&origin,0);
    suffix.conversation_fields=vec![set("note",json!("😀{{inlay::shared}} {{inlayed::shared}}")),set("future./field",json!({"a/b~c":"{{inlayeddata::shared}}"}))];
    suffix.ranges=vec![range(0,0,json!([{"data":"{{inlay::shared}}"}])),range(0,1,json!([]))];
    suffix.introduced_asset_bindings=vec![
        proof(AssetScope::ConversationField {mutation:0},"",Some(0),&old,&old_lease),
        proof(AssetScope::ConversationField {mutation:0},"",Some(1),&new,&new_lease),
        proof(AssetScope::ConversationField {mutation:1},"/a~1b~0c",Some(0),&new,&new_lease)];
    rejected(&mut store,&suffix,ContinuationConflict::AssetBinding);
    suffix.introduced_asset_bindings.push(proof(AssetScope::Message {range:0,message:0},"/data",Some(0),&new,&new_lease));
    committed(&mut store,&suffix);
    let chat=store.read_conversation("copy","copied-chat",None).unwrap().unwrap().value;
    let refs=crate::lossless_f0::scan_portable_fragment(crate::lossless_f0::PortableFragment::Conversation {value:&chat,character_id:"copy"}).unwrap();
    let notes=refs.iter().filter(|reference|reference.source_path=="$.note").collect::<Vec<_>>();assert_eq!(notes.len(),2);assert_ne!(notes[0].target_key,notes[1].target_key);
    assert_eq!(store.read_asset_alias("inlay",&notes[0].target_key,None).unwrap().unwrap().value.object_hash,old.object_hash);
    assert_eq!(store.read_asset_alias("inlay",&notes[1].target_key,None).unwrap().unwrap().value.object_hash,new.object_hash);
    assert_eq!(history(&store).as_array().unwrap().len(),3);
    let mut altered=suffix.clone();altered.introduced_asset_bindings[0].location.token_index=Some(1);
    assert!(store.continue_conflict_copy(&altered,&||false).unwrap_err().to_string().contains("request-id-integrity"));
}

#[test]
fn missing_wrong_duplicate_unused_and_expired_asset_proofs_never_guess_current_binding() {
    let (directory,mut store,_original,id,origin)=fixture();let cas=PayloadCas::new(directory.path()).unwrap();
    let a=asset(&cas,"inlay","same",b"a");store.commit_asset_alias(&a,store.revision().unwrap()).unwrap();
    let witness=store.acquire_revision(store.revision().unwrap()).unwrap().lease;
    let b=asset(&cas,"inlay","same",b"b");store.commit_asset_alias(&b,store.revision().unwrap()).unwrap();
    let mut suffix=request(&mut store,"proof-failures",&id,&origin,0);suffix.ranges.push(range(0,1,json!([{"data":"{{inlay::same}}"}])));
    rejected(&mut store,&suffix,ContinuationConflict::AssetBinding);
    let binding=proof(AssetScope::Message {range:0,message:0},"/data",Some(0),&a,&witness);
    suffix.introduced_asset_bindings=vec![IntroducedAssetBinding {alias:b,..binding.clone()}];rejected(&mut store,&suffix,ContinuationConflict::AssetBinding);
    suffix.introduced_asset_bindings=vec![binding.clone(),binding.clone()];rejected(&mut store,&suffix,ContinuationConflict::AssetBinding);
    suffix.introduced_asset_bindings=vec![IntroducedAssetBinding {scope:AssetScope::Message {range:9,message:0},..binding.clone()}];rejected(&mut store,&suffix,ContinuationConflict::AssetBinding);
    suffix.introduced_asset_bindings=vec![binding];store.release_revision(&witness).unwrap();rejected(&mut store,&suffix,ContinuationConflict::AssetBinding);
}

#[test]
fn alias_rebinding_without_message_change_is_a_conflict() {
    let (directory,mut store,_original,id,origin,_old)=asset_fixture();
    let key=store.read_character("copy",None).unwrap().unwrap().value["image"].as_str().unwrap().to_owned();
    let replacement=asset(&PayloadCas::new(directory.path()).unwrap(),"asset",&key,b"wrong current referent");
    store.commit_asset_alias(&replacement,store.revision().unwrap()).unwrap();
    let suffix=request(&mut store,"alias-drift",&id,&origin,0);rejected(&mut store,&suffix,ContinuationConflict::AssetBinding);
}

#[test]
fn context_budget_refusals_do_not_evict_and_receipts_replay_at_capacity() {
    let (_directory,mut store,original,id,origin)=fixture();
    store.copy_continuations.limits.contexts=1;
    assert!(matches!(store.open_conflict_copy_continuation(&original).unwrap(),OpenContinuationResult::Ready {context_id,..} if context_id==id));
    let mut another=original.clone();another.header.request_id="second-copy".into();another.target_character_id="copy-two".into();another.expected_revision=store.revision().unwrap();
    store.copy_conflicted_conversation(&another,&||false).unwrap();
    assert_eq!(store.open_conflict_copy_continuation(&another).unwrap(),OpenContinuationResult::Conflict {kind:ContinuationConflict::Budget});
    assert_eq!(store.copy_continuations.contexts.len(),1);
    let mut suffix=request(&mut store,"budgeted",&id,&origin,0);suffix.character_fields.push(set("name",json!("saved")));
    let defaults=store.copy_continuations.limits.clone();
    for mode in 0..9 {
        store.copy_continuations.limits=defaults.clone();
        match mode {0=>store.copy_continuations.limits.request_bytes=1,1=>store.copy_continuations.limits.basis_bytes=1,
            2=>store.copy_continuations.limits.store_basis_bytes=1,3=>store.copy_continuations.limits.inflight_bytes=1,
            4=>store.copy_continuations.limits.metadata_bytes=1,5=>store.copy_continuations.limits.inflight_metadata_bytes=1,
            6=>store.copy_continuations.limits.leases=1,7=>store.copy_continuations.limits.store_leases=1,_=>store.copy_continuations.limits.store_metadata_bytes=1}
        rejected(&mut store,&suffix,ContinuationConflict::Budget);
        assert!(matches!(store.release_revision(&original.source.lease),Err(StoreError::SnapshotPinned {..})));
    }
    store.copy_continuations.limits=defaults;committed(&mut store,&suffix);
    store.copy_continuations.limits.request_bytes=1;
    assert!(matches!(store.continue_conflict_copy(&suffix,&||true).unwrap(),CopyContinuationResult::Replayed {..}));
    assert_eq!(store.close_copy_continuation(&id,1),CloseContinuationResult::Closed);
    store.copy_continuations.limits=ContinuationLimits::default();
    assert!(matches!(store.open_conflict_copy_continuation(&another).unwrap(),OpenContinuationResult::Ready {..}));
}

// These boundaries use the actual serialized retained values, not the admission accounting
// helper. The 128-byte context ID/framing allowance is part of the conservative byte contract.
fn serialized_metadata_charge(origin:&CopyOrigin,head:&Head)->usize {
    serde_json::to_vec(&(origin,head)).unwrap().len()-serde_json::to_vec(&head.basis).unwrap().len()+128
}

#[test]
fn candidate_request_proofs_and_both_metadata_views_obey_exact_byte_boundaries() {
    let (directory,mut store,_original,id,origin)=fixture();
    let mut a=asset(&PayloadCas::new(directory.path()).unwrap(),"inlay","proof-heavy",b"synthetic proof payload");
    a.metadata=json!({"proofDetail":"p".repeat(2048)});
    store.commit_asset_alias(&a,store.revision().unwrap()).unwrap();let witness=store.acquire_revision(store.revision().unwrap()).unwrap().lease;
    let mut suffix=request(&mut store,"full-candidate-budget",&id,&origin,0);
    suffix.ranges.push(range(0,1,json!([{"data":"{{inlay::proof-heavy}}"}])));
    suffix.introduced_asset_bindings.push(proof(AssetScope::Message {range:0,message:0},"/data",Some(0),&a,&witness));
    suffix.character_fields.push(set("largeFuture",json!("m".repeat(2048))));
    let prepared=store.prepare_copy_continuation(&suffix,&continuation_digest(&suffix).unwrap(),&||false).unwrap();
    let candidate_bytes=serde_json::to_vec(&prepared.candidate).unwrap().len();
    let next_metadata=serialized_metadata_charge(&origin,&prepared.candidate.next);
    let inflight_metadata=next_metadata+serde_json::to_vec(&prepared.candidate.leases).unwrap().len();
    assert!(next_metadata>serialized_metadata_charge(&origin,&store.copy_continuations.contexts[&id].head));
    assert!(candidate_bytes>serde_json::to_vec(&prepared.candidate.next).unwrap().len()+2048);
    drop(prepared);
    store.copy_continuations.limits.inflight_bytes=candidate_bytes-1;
    rejected(&mut store,&suffix,ContinuationConflict::Budget);
    store.copy_continuations.limits.inflight_bytes=candidate_bytes;
    store.copy_continuations.limits.inflight_metadata_bytes=inflight_metadata-1;
    rejected(&mut store,&suffix,ContinuationConflict::Budget);
    store.copy_continuations.limits.inflight_metadata_bytes=inflight_metadata;
    store.copy_continuations.limits.metadata_bytes=next_metadata-1;
    rejected(&mut store,&suffix,ContinuationConflict::Budget);
    store.copy_continuations.limits.metadata_bytes=next_metadata;
    committed(&mut store,&suffix);
    assert_eq!(store.read_character("copy",None).unwrap().unwrap().value["largeFuture"],json!("m".repeat(2048)));
    assert!(matches!(store.release_revision(&witness),Err(StoreError::SnapshotPinned {..})));
}

#[test]
fn aggregate_settled_and_promoted_basis_metadata_fit_only_at_full_store_boundary() {
    let (_directory,mut store,original,id,origin)=fixture();
    let mut second=original.clone();second.header.request_id="other-budget-copy".into();second.target_character_id="copy-two".into();
    second.expected_revision=store.revision().unwrap();second.character_fields.push(set("otherFuture",json!("o".repeat(1024))));
    store.copy_conflicted_conversation(&second,&||false).unwrap();
    let OpenContinuationResult::Ready {context_id:second_id,..}=store.open_conflict_copy_continuation(&second).unwrap() else {panic!("second context")};
    let mut suffix=request(&mut store,"aggregate-budget",&id,&origin,0);
    suffix.ranges.push(range(0,1,json!([{"data":"history".repeat(1024)}])));
    suffix.character_fields.push(set("largeFuture",json!("m".repeat(2048))));
    let prepared=store.prepare_copy_continuation(&suffix,&continuation_digest(&suffix).unwrap(),&||false).unwrap();
    let old_total_basis=store.copy_continuations.contexts.values().map(|context|serde_json::to_vec(&context.head.basis).unwrap().len()).sum::<usize>();
    let old_total_metadata=store.copy_continuations.contexts.values().map(|context|serialized_metadata_charge(&context.origin,&context.head)).sum::<usize>();
    let before=&store.copy_continuations.contexts[&id].head;
    let promoted_basis=old_total_basis-serde_json::to_vec(&before.basis).unwrap().len()+serde_json::to_vec(&prepared.candidate.next.basis).unwrap().len();
    let promoted_metadata=old_total_metadata-serialized_metadata_charge(&origin,before)+serialized_metadata_charge(&origin,&prepared.candidate.next);
    assert!(promoted_basis>old_total_basis);assert!(promoted_metadata>old_total_metadata);drop(prepared);
    store.copy_continuations.limits.store_basis_bytes=promoted_basis-1;
    rejected(&mut store,&suffix,ContinuationConflict::Budget);
    store.copy_continuations.limits.store_basis_bytes=promoted_basis;
    store.copy_continuations.limits.store_metadata_bytes=promoted_metadata-1;
    rejected(&mut store,&suffix,ContinuationConflict::Budget);
    store.copy_continuations.limits.store_metadata_bytes=promoted_metadata;
    committed(&mut store,&suffix);
    assert!(matches!(store.open_conflict_copy_continuation(&second).unwrap(),OpenContinuationResult::Ready {context_id,..} if context_id==second_id));
    assert_eq!(store.read_character("copy-two",None).unwrap().unwrap().value["otherFuture"],json!("o".repeat(1024)));
}

#[test]
fn shared_and_distinct_proof_handles_obey_context_and_store_union_boundaries() {
    for distinct in [false,true] {
        let (directory,mut store,_original,id,origin)=fixture();
        let a=asset(&PayloadCas::new(directory.path()).unwrap(),"inlay","witness-count",b"witness union payload");
        store.commit_asset_alias(&a,store.revision().unwrap()).unwrap();
        let first=store.acquire_revision(store.revision().unwrap()).unwrap().lease;
        let second=store.acquire_revision(store.revision().unwrap()).unwrap().lease;
        let mut suffix=request(&mut store,"reader-boundary",&id,&origin,0);
        suffix.ranges.push(range(0,1,json!([{"data":"{{inlay::witness-count}}"},{"data":"{{inlay::witness-count}}"}])));
        suffix.introduced_asset_bindings=vec![proof(AssetScope::Message {range:0,message:0},"/data",Some(0),&a,&first),
            proof(AssetScope::Message {range:0,message:1},"/data",Some(0),&a,if distinct {&second}else{&first})];
        let required=if distinct {4}else{3}; // Original source, current target and unique introduced witnesses.
        store.copy_continuations.limits.leases=required-1;rejected(&mut store,&suffix,ContinuationConflict::Budget);
        store.copy_continuations.limits.leases=required;
        store.copy_continuations.limits.store_leases=required-1;rejected(&mut store,&suffix,ContinuationConflict::Budget);
        store.copy_continuations.limits.store_leases=required;committed(&mut store,&suffix);
        assert!(matches!(store.release_revision(&first),Err(StoreError::SnapshotPinned {..})));
        if distinct {assert!(matches!(store.release_revision(&second),Err(StoreError::SnapshotPinned {..})));}
        else {store.release_revision(&second).unwrap();}
        assert_eq!(history(&store).as_array().unwrap().len(),4);
    }
}

#[test]
fn archived_or_deleted_target_is_never_implicitly_restored() {
    let (_directory,mut store,_original,id,origin)=fixture();
    store.archive_character("copy",store.revision().unwrap(),77).unwrap();
    let suffix=request(&mut store,"archived-target",&id,&origin,0);
    assert_eq!(store.continue_conflict_copy(&suffix,&||false).unwrap(),CopyContinuationResult::Conflict {kind:ContinuationConflict::TargetArchived});
    assert_eq!(store.revision().unwrap(),suffix.expected_revision);assert_eq!(receipt_count(&store,"archived-target"),0);
    let (_directory,mut store,_original,id,origin)=fixture();
    store.commit(&WorkingSetCommit {expected_revision:store.revision().unwrap(),delete_character_ids:Some(vec!["copy".into()]),..Default::default()}).unwrap();
    let suffix=request(&mut store,"deleted-target",&id,&origin,0);
    assert_eq!(store.continue_conflict_copy(&suffix,&||false).unwrap(),CopyContinuationResult::Conflict {kind:ContinuationConflict::TargetMissing});
    assert_eq!(store.revision().unwrap(),suffix.expected_revision);assert_eq!(receipt_count(&store,"deleted-target"),0);
}

#[test]
fn initial_context_pins_introduced_witness_and_actual_gc_roots_until_close_and_release() {
    let (directory,mut store,original,_id,_origin)=fixture();
    let cas=PayloadCas::new(directory.path()).unwrap();let a=asset(&cas,"inlay","ephemeral",b"retained only by original proof");let hash=a.object_hash.clone().unwrap();
    store.asset_object_catalog().register(&[asset_object_catalog::AssetObjectRegistration {object_hash:hash.clone(),byte_size:a.size as u64}],0).unwrap();
    store.commit_asset_alias(&a,store.revision().unwrap()).unwrap();let witness=store.acquire_revision(store.revision().unwrap()).unwrap().lease;
    store.delete_asset_alias("inlay","ephemeral",store.revision().unwrap()).unwrap();
    let mut copy=original.clone();copy.header.request_id="copy-with-discarded-proof".into();copy.target_character_id="copy-two".into();copy.expected_revision=store.revision().unwrap();
    copy.ranges=vec![range(0,0,json!([{"data":"{{inlay::ephemeral}}"}])),range(0,1,json!([]))];
    copy.introduced_asset_bindings=vec![proof(AssetScope::Message {range:0,message:0},"/data",Some(0),&a,&witness)];
    store.copy_conflicted_conversation(&copy,&||false).unwrap();
    let OpenContinuationResult::Ready {context_id,..}=store.open_conflict_copy_continuation(&copy).unwrap() else {panic!("open")};
    assert!(matches!(store.release_revision(&witness),Err(StoreError::SnapshotPinned {..})));
    assert_eq!(store.read_asset_alias("inlay","ephemeral",Some(&witness)).unwrap().unwrap().value,a);
    let held=store.asset_gc_dry_run(64,None,100,10).unwrap().report;assert!(held.marked_hashes.contains(&hash));assert!(!held.potential_delete_hashes.contains(&hash));
    assert_eq!(store.close_copy_continuation(&context_id,0),CloseContinuationResult::Closed);store.release_revision(&witness).unwrap();
    let dropped=store.asset_gc_dry_run(64,None,100,10).unwrap().report;assert!(!dropped.marked_hashes.contains(&hash));assert!(dropped.potential_delete_hashes.contains(&hash));
    assert_eq!(store.open_conflict_copy_continuation(&copy).unwrap(),OpenContinuationResult::Conflict {kind:ContinuationConflict::EvidenceExpired});
}

#[test]
fn promoted_historical_witness_roots_survive_live_alias_removal_and_basis_rotation() {
    for rotate in [false,true] {
        let (directory,mut store,_original,id,origin)=fixture();
        let cas=PayloadCas::new(directory.path()).unwrap();let a=asset(&cas,"inlay","historical",b"promoted historical witness payload");
        let hash=a.object_hash.clone().unwrap();
        store.asset_object_catalog().register(&[asset_object_catalog::AssetObjectRegistration {object_hash:hash.clone(),byte_size:a.size as u64}],0).unwrap();
        store.commit_asset_alias(&a,store.revision().unwrap()).unwrap();let witness=store.acquire_revision(store.revision().unwrap()).unwrap().lease;
        store.delete_asset_alias("inlay","historical",store.revision().unwrap()).unwrap();
        let mut suffix=request(&mut store,"promoted-witness",&id,&origin,0);
        assert!(store.read_asset_alias("inlay","historical",Some(&suffix.current_lease)).unwrap().is_none());
        suffix.ranges.push(range(0,1,json!([{"data":"{{inlay::historical}}"}])));
        suffix.introduced_asset_bindings.push(proof(AssetScope::Message {range:0,message:0},"/data",Some(0),&a,&witness));
        committed(&mut store,&suffix);
        assert!(matches!(store.release_revision(&witness),Err(StoreError::SnapshotPinned {..})));
        let chat=store.read_conversation("copy","copied-chat",None).unwrap().unwrap().value;
        let refs=crate::lossless_f0::scan_portable_fragment(crate::lossless_f0::PortableFragment::Conversation {value:&chat,character_id:"copy"}).unwrap();
        let copied_key=refs.iter().find(|reference|reference.target_kind=="inlay").unwrap().target_key.clone();
        let (version,last_reader)=if rotate {
            let next=request(&mut store,"capture-promoted-alias",&id,&origin,1);committed(&mut store,&next);
            store.release_revision(&witness).unwrap();store.release_revision(&suffix.current_lease).unwrap();
            (2,next.current_lease)
        } else {(1,suffix.current_lease.clone())};
        store.commit(&WorkingSetCommit {expected_revision:store.revision().unwrap(),conversations:Some(vec![ConversationMutation::ReplaceRange {
            character_id:"copy".into(),conversation_id:"copied-chat".into(),start:0,delete_count:1,messages:vec![json!({"data":"new live value"})],conversation:None,configured_index:None }]),..Default::default()}).unwrap();
        store.delete_asset_alias("inlay",&copied_key,store.revision().unwrap()).unwrap();
        assert!(store.read_asset_alias("inlay",&copied_key,None).unwrap().is_none());
        if !rotate {assert!(matches!(store.release_revision(&witness),Err(StoreError::SnapshotPinned {..})));}
        let held=store.asset_gc_dry_run(64,None,100,10).unwrap().report;
        assert!(held.marked_hashes.contains(&hash));assert!(!held.potential_delete_hashes.contains(&hash));
        assert_eq!(store.close_copy_continuation(&id,version),CloseContinuationResult::Closed);
        store.release_revision(&last_reader).unwrap();if !rotate {store.release_revision(&witness).unwrap();}
        let released=store.asset_gc_dry_run(64,None,100,10).unwrap().report;
        assert!(!released.marked_hashes.contains(&hash));assert!(released.potential_delete_hashes.contains(&hash));
    }
}

fn source_manifest(store: &mut PersistentStore, alias: &AssetAlias, label: &str) -> (AssetOwnerHead,String) {
    use crate::asset_repository::owner_manifest_codec::{encode_owner_manifest,OwnerManifestEntry};
    let bytes=encode_owner_manifest(&[OwnerManifestEntry {tuple:[label.into(),alias.key.clone(),"png".into()],
        payload_hash:Some(hex::decode(alias.object_hash.as_ref().unwrap()).unwrap().try_into().unwrap())}]).unwrap();
    let object=PayloadCas::new(&store.repository_root).unwrap().prepare_bytes(&bytes).unwrap();
    let head=AssetOwnerHead {owner:AssetOwnerLocator::CharacterAdditionalAssets {character_id:"source".into()},present:true,manifest_hash:Some(object.content_hash),entry_count:1};
    let mut owner=store.read_character("source",None).unwrap().unwrap().value;owner["additionalAssets"]=json!([[label,alias.key,"png"]]);
    store.commit_with_asset_aliases(&WorkingSetCommit {expected_revision:store.revision().unwrap(),character:Some(owner),asset_owner_heads:Some(vec![head.clone()]),..Default::default()},std::slice::from_ref(alias)).unwrap();
    let witness=store.acquire_revision(store.revision().unwrap()).unwrap().lease;(head,witness)
}
fn add_manifest_request(store:&mut PersistentStore,id:&str,context:&str,origin:&CopyOrigin,version:u64,head:&AssetOwnerHead,witness:&str,alias:&AssetAlias,label:&str)->CopyContinuationRequest {
    let mut r=request(store,id,context,origin,version);r.character_fields.push(set("additionalAssets",json!([[label,alias.key,"png"]])));
    r.introduced_asset_bindings.push(proof(AssetScope::CharacterField {mutation:0},"/0/1",None,alias,witness));
    r.introduced_owner_head=Some(IntroducedOwnerHead {mutation:0,head:head.clone(),witness_lease:witness.into()});r
}

#[test]
fn repeated_owner_manifest_updates_validate_scoped_heads_and_keep_unknown_fields() {
    let (directory,mut store,_original,id,origin,old)=asset_fixture();
    let (head,witness)=source_manifest(&mut store,&old,"first tuple");
    let first=add_manifest_request(&mut store,"manifest-first",&id,&origin,0,&head,&witness,&old,"first tuple");
    let mut wrong=first.clone();wrong.introduced_owner_head.as_mut().unwrap().head.entry_count=2;rejected(&mut store,&wrong,ContinuationConflict::AssetBinding);
    store.hydrate_copy_continuation(&first,&||false,None).unwrap();committed(&mut store,&first);
    let new=asset(&PayloadCas::new(directory.path()).unwrap(),"asset","same-key",b"second manifest payload");
    let (head,witness)=source_manifest(&mut store,&new,"second tuple");
    let second=add_manifest_request(&mut store,"manifest-second",&id,&origin,1,&head,&witness,&new,"second tuple");committed(&mut store,&second);
    let target=store.read_asset_owner_head(&AssetOwnerLocator::CharacterAdditionalAssets {character_id:"copy".into()},None).unwrap().unwrap().value;
    let bytes=PayloadCas::new(directory.path()).unwrap().read_object(target.manifest_hash.as_deref().unwrap()).unwrap().unwrap();
    let entries=crate::asset_repository::owner_manifest_codec::decode_owner_manifest(&bytes).unwrap();
    assert_eq!(entries[0].tuple[0],"second tuple");assert_eq!(entries[0].payload_hash.map(hex::encode),new.object_hash);assert_ne!(entries[0].tuple[1],"same-key");
    assert_eq!(store.read_character("copy",None).unwrap().unwrap().value["futureOwner"],json!({"retained":true}));
}

#[test]
fn initial_context_pins_distinct_owner_head_witness_until_close() {
    let (_directory,mut store,original,_id,_origin,old)=asset_fixture();
    let (head,witness)=source_manifest(&mut store,&old,"introduced prefix");
    let mut copy=original.clone();copy.header.request_id="copy-with-head-witness".into();copy.target_character_id="copy-two".into();copy.expected_revision=store.revision().unwrap();
    copy.character_fields=vec![set("additionalAssets",json!([["introduced prefix","same-key","png"]]))];
    copy.introduced_asset_bindings=vec![proof(AssetScope::CharacterField {mutation:0},"/0/1",None,&old,&original.source.lease)];
    copy.introduced_owner_head=Some(IntroducedOwnerHead {mutation:0,head:head.clone(),witness_lease:witness.clone()});
    store.copy_conflicted_conversation(&copy,&||false).unwrap();
    let OpenContinuationResult::Ready {context_id,required_lease_ids,..}=store.open_conflict_copy_continuation(&copy).unwrap() else {panic!("open")};
    assert!(required_lease_ids.contains(&original.source.lease));assert!(required_lease_ids.contains(&witness));
    assert!(matches!(store.release_revision(&witness),Err(StoreError::SnapshotPinned {..})));
    assert_eq!(store.read_asset_owner_head(&head.owner,Some(&witness)).unwrap().unwrap().value,head);
    assert_eq!(store.close_copy_continuation(&context_id,0),CloseContinuationResult::Closed);store.release_revision(&witness).unwrap();
    assert!(matches!(store.read_asset_owner_head(&head.owner,Some(&witness)),Err(StoreError::SnapshotReleased)));
}

#[test]
fn cancellation_after_control_publication_preserves_catalog_candidate_and_retry() {
    use crate::asset_repository::job_pins::{collect_durable_cas_job_roots_read_only,durable_cas_job_ids};
    let (directory,mut store,_original,id,origin,old)=asset_fixture();let cas=PayloadCas::new(directory.path()).unwrap();
    let (head,witness)=source_manifest(&mut store,&old,"cancel tuple");
    let suffix=add_manifest_request(&mut store,"manifest-cancel",&id,&origin,0,&head,&witness,&old,"cancel tuple");
    let published=std::cell::RefCell::new(None);
    let result=store.continue_conflict_copy(&suffix,&|| {
        let found=collect_durable_cas_job_roots_read_only(directory.path()).object_hashes.into_iter().find(|hash|cas.stat_object(hash).unwrap().is_some());
        let cancel=found.is_some();if cancel {*published.borrow_mut()=found;}cancel
    }).unwrap();
    assert_eq!(result,CopyContinuationResult::Conflict {kind:ContinuationConflict::Cancelled});
    let hash=published.into_inner().expect("reached published control");assert_eq!(store.revision().unwrap(),suffix.expected_revision);assert_eq!(receipt_count(&store,"manifest-cancel"),0);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM asset_objects WHERE object_hash=?1",[&hash],|row|row.get::<_,i64>(0)).unwrap(),1);
    assert!(cas.stat_object(&hash).unwrap().is_some());assert!(durable_cas_job_ids(directory.path()).unwrap().is_empty());
    committed(&mut store,&suffix);
}

#[test]
fn exact_authority_and_selection_guards_are_rechecked_before_effects() {
    let (_directory,mut store,_original,id,origin)=fixture();let suffix=request(&mut store,"binding-change",&id,&origin,0);
    store.device_store().unwrap().connection().execute("UPDATE lww_clock SET binding_authority='999' WHERE singleton=1",[]).unwrap();
    rejected(&mut store,&suffix,ContinuationConflict::Identity);
    let (_directory,mut store,_original,id,origin)=fixture();let suffix=request(&mut store,"selection-change",&id,&origin,0);
    let changed=Cell::new(false);
    let competing=Connection::open(&store.database_path).unwrap();
    let result=store.continue_conflict_copy(&suffix,&|| {
        if !changed.replace(true) {competing.execute("UPDATE library_sync_selection SET selection_epoch='new-selection' WHERE singleton=1",[]).unwrap();}false
    }).unwrap();
    assert_eq!(result,CopyContinuationResult::Conflict {kind:ContinuationConflict::Identity});assert_eq!(store.revision().unwrap(),suffix.expected_revision);assert_eq!(receipt_count(&store,"selection-change"),0);
}

#[test]
fn continuation_units_survive_existing_peer_receive_and_archive_restore() {
    let (_directory,mut source,original,id,origin)=fixture();
    let mut suffix=request(&mut source,"peer-suffix",&id,&origin,0);
    suffix.character_fields=vec![set("name",json!("shared continuation")),set("privateOwn",json!("local continuation"))];
    suffix.ranges.push(range(0,1,json!([{"data":"shared message"}])));committed(&mut source,&suffix);
    let entries=source.lww_read_outbox(origin.binding_authority,512).unwrap().entries.into_iter()
        .filter(|entry|entry.version==original.header.request_id || entry.version==suffix.header.request_id).collect::<Vec<_>>();
    assert!(entries.iter().any(|entry|entry.key.components()==["character","copy","name"] && entry.version==suffix.header.request_id));
    assert!(entries.iter().any(|entry|entry.key.components()==["messages","copy","copied-chat"] && entry.version==suffix.header.request_id));
    assert!(!entries.iter().any(|entry|entry.key.components().last().is_some_and(|key|key=="privateOwn")));
    let (_target_directory,mut target)=fixture_start();
    for row in source.connection.prepare("SELECT hash,body FROM message_page_objects").unwrap().query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,Vec<u8>>(1)?))).unwrap() {
        let (hash,body)=row.unwrap();target.lww_put_object(&hash,&body).unwrap();
    }
    let header=lww::Header {binding_authority:target.lww_binding_authority().unwrap(),request_id:"peer-continued-copy".into()};
    target.lww_stage_receive(&lww::StageReceive {header:header.clone(),changes:entries.into_iter().map(|entry|lww::Change {key:entry.key,stamp:entry.stamp,value:entry.value}).collect(),
        progress:lww::Progress {kind:"server".into(),cursor:1.into(),writer_id:None},admitted_time_upper_ms:u64::MAX.into()}).unwrap();
    target.lww_apply_receive(&lww::ApplyReceive {header:header.clone(),generating:vec![]}).unwrap();target.lww_finish_receive(&header).unwrap();
    assert_eq!(history(&target),history(&source));assert_eq!(target.read_character("copy",None).unwrap().unwrap().value["name"],"shared continuation");
    assert!(target.read_character("copy",None).unwrap().unwrap().value.get("privateOwn").is_none());
    target.archive_character("copy",target.revision().unwrap(),100).unwrap();target.restore_character("copy",target.revision().unwrap()).unwrap();
    assert_eq!(history(&target),history(&source));
}

#[test]
fn final_transaction_holds_device_authority_and_library_selection_until_commit() {
    use crate::asset_repository::job_pins::durable_cas_job_ids;
    let (directory,mut store,_original,id,origin)=fixture();let suffix=request(&mut store,"held-guards",&id,&origin,0);
    let device=Connection::open(directory.path().join("persistent").join(super::super::super::device_store::DEVICE_DATABASE_FILE)).unwrap();device.busy_timeout(std::time::Duration::ZERO).unwrap();
    let library=Connection::open(&store.database_path).unwrap();library.busy_timeout(std::time::Duration::ZERO).unwrap();
    let attempted=Cell::new(false);
    let result=store.continue_conflict_copy(&suffix,&|| {
        if !durable_cas_job_ids(directory.path()).unwrap().is_empty() && !attempted.replace(true) {
            assert_eq!(device.execute("UPDATE lww_clock SET binding_authority='987' WHERE singleton=1",[]).unwrap_err().sqlite_error_code(),Some(rusqlite::ErrorCode::DatabaseBusy));
            assert_eq!(library.execute("UPDATE library_sync_selection SET selection_epoch='racing' WHERE singleton=1",[]).unwrap_err().sqlite_error_code(),Some(rusqlite::ErrorCode::DatabaseBusy));
        }false
    }).unwrap();
    assert!(attempted.get());assert!(matches!(result,CopyContinuationResult::Committed {..}));assert_eq!(store.lww_binding_authority().unwrap(),origin.binding_authority);
}

#[derive(Clone,Copy,Debug)]
enum ReaderTransfer {
    Export,ContentCapture,
    #[cfg(feature="native-official-publication")]
    Publication,
    #[cfg(feature="native-official-publication")]
    PublicationJob,
    #[cfg(feature="native-kei-upload-pilot")]
    Kei,
    #[cfg(feature="native-kei-upload-pilot")]
    KeiJob,
}

fn transfer_and_close_reader(store:&mut PersistentStore,lease:&str,revision:i64,kind:ReaderTransfer)->StoreResult<()> {
    match kind {
        ReaderTransfer::Export=>{
            let prepared=store.detach_risu_save_export(lease)?;
            assert!(matches!(store.read_root(Some(lease)),Err(StoreError::SnapshotReleased)));
            assert_eq!(prepared.reader()?.target.revision,revision);
            store.reattach_risu_save_export(prepared)?;
            assert_eq!(store.read_root(Some(lease))?.revision,revision);
            store.release_revision(lease)?;
        },
        ReaderTransfer::ContentCapture=>{
            let prepared=store.prepare_content_capture_from_lease("synthetic-transfer","synthetic-consumer",lease)?;
            assert_eq!(prepared.identity.revision,revision);
            assert!(matches!(store.read_root(Some(lease)),Err(StoreError::SnapshotReleased)));drop(prepared);
        },
        #[cfg(feature="native-official-publication")]
        ReaderTransfer::Publication=>{
            let prepared=store.prepare_official_publication(lease,revision)?;
            assert!(matches!(store.read_root(Some(lease)),Err(StoreError::SnapshotReleased)));drop(prepared);
        },
        #[cfg(feature="native-official-publication")]
        ReaderTransfer::PublicationJob=>{
            let prepared=store.prepare_official_publication_for_job(lease,revision,"synthetic-transfer-job",1)?;
            assert!(matches!(store.read_root(Some(lease)),Err(StoreError::SnapshotReleased)));drop(prepared);
            let mut job=DurableCasJob::open(&store.repository_root,"synthetic-transfer-job")?;
            assert!(job.is_sealed());job.release(CasReleaseOutcome::Aborted)?;
        },
        #[cfg(feature="native-kei-upload-pilot")]
        ReaderTransfer::Kei|ReaderTransfer::KeiJob=>{
            let account=json!({"id":"synthetic-account","token":"synthetic-token","kei":true,"data":{}});
            let prepared=if matches!(kind,ReaderTransfer::Kei) {
                store.prepare_kei_upload(lease,"http://127.0.0.1:9/prepare-only","synthetic-account","synthetic-token",&account)?
            }else{
                store.prepare_kei_job_upload(lease,revision,"http://127.0.0.1:9/prepare-only","synthetic-account","synthetic-token",&account)?
            };
            assert_eq!(prepared.revision(),revision);
            assert!(matches!(store.read_root(Some(lease)),Err(StoreError::SnapshotReleased)));drop(prepared);
        },
    }
    Ok(())
}

#[test]
fn every_existing_reader_transfer_refuses_context_pins_and_still_transfers_after_close() {
    let transfers=[ReaderTransfer::Export,ReaderTransfer::ContentCapture,
        #[cfg(feature="native-official-publication")] ReaderTransfer::Publication,
        #[cfg(feature="native-official-publication")] ReaderTransfer::PublicationJob,
        #[cfg(feature="native-kei-upload-pilot")] ReaderTransfer::Kei,
        #[cfg(feature="native-kei-upload-pilot")] ReaderTransfer::KeiJob];
    for kind in transfers {for role in 0..3 {
        let (directory,mut store,original,id,origin)=fixture();
        let (lease,followup,replayed)=match role {
            0=>(original.source.lease.clone(),request(&mut store,"after-transfer-refusal",&id,&origin,0),false),
            1=>{
                let suffix=request(&mut store,"candidate-transfer",&id,&origin,0);make_unknown(&mut store,&suffix);
                store.connection.execute("ALTER TABLE unresolved_receipts RENAME TO lww_requests",[]).unwrap();
                (suffix.current_lease.clone(),suffix,true)
            },
            _=>{
                let a=asset(&PayloadCas::new(directory.path()).unwrap(),"inlay","transfer-witness",b"promoted transfer witness");
                store.commit_asset_alias(&a,store.revision().unwrap()).unwrap();let witness=store.acquire_revision(store.revision().unwrap()).unwrap().lease;
                store.delete_asset_alias("inlay","transfer-witness",store.revision().unwrap()).unwrap();
                let mut suffix=request(&mut store,"promoted-transfer",&id,&origin,0);
                suffix.ranges.push(range(0,1,json!([{"data":"{{inlay::transfer-witness}}"}])));
                suffix.introduced_asset_bindings.push(proof(AssetScope::Message {range:0,message:0},"/data",Some(0),&a,&witness));
                committed(&mut store,&suffix);
                (witness,request(&mut store,"after-promoted-refusal",&id,&origin,1),false)
            },
        };
        let root=store.read_root(Some(&lease)).unwrap();let readers=store.active_readers.active_count();
        let revision=store.revision().unwrap();let jobs=crate::asset_repository::job_pins::durable_cas_job_ids(directory.path()).unwrap();
        let error=transfer_and_close_reader(&mut store,&lease,root.revision,kind).unwrap_err();
        assert_eq!(error,StoreError::SnapshotPinned {lease:lease.clone(),reason:"copy-continuation".into()},"{kind:?}, role {role}");
        assert_eq!(store.read_root(Some(&lease)).unwrap().value,root.value);
        assert_eq!(store.active_readers.active_count(),readers);assert_eq!(store.revision().unwrap(),revision);
        assert_eq!(crate::asset_repository::job_pins::durable_cas_job_ids(directory.path()).unwrap(),jobs);
        if replayed {assert!(matches!(store.continue_conflict_copy(&followup,&||false).unwrap(),CopyContinuationResult::Replayed {..}));}
        else {committed(&mut store,&followup);}
        assert_eq!(store.close_copy_continuation(&id,followup.basis_version+1),CloseContinuationResult::Closed);
        let readers=store.active_readers.active_count();transfer_and_close_reader(&mut store,&lease,root.revision,kind).unwrap();
        assert_eq!(store.active_readers.active_count(),readers-1);
        assert!(matches!(store.read_root(Some(&lease)),Err(StoreError::SnapshotReleased)));
    }}
}

#[cfg(feature="native-official-publication")]
#[test]
fn pinned_publication_job_refuses_before_durable_preparation_can_mask_its_error() {
    let (directory,mut store,original,id,_origin)=fixture();
    let mut occupied=DurableCasJob::begin(directory.path(),"occupied-publication",CasJobKind::OfficialPublicationOrExportPreparation,
        CasJobOwner::native_file_job("occupied-publication"),1).unwrap();
    let journal=directory.path().join("assets/job-pins/job-occupied-publication.journal");
    let before=std::fs::read(&journal).unwrap();
    let error=store.prepare_official_publication_for_job(&original.source.lease,original.source.identity.revision,"occupied-publication",1)
        .err().expect("pinned lease refusal precedes the existing journal collision");
    assert_eq!(error,StoreError::SnapshotPinned {lease:original.source.lease.clone(),reason:"copy-continuation".into()});
    assert!(!occupied.is_sealed());assert_eq!(std::fs::read(&journal).unwrap(),before);
    let reopened=DurableCasJob::open(directory.path(),"occupied-publication").unwrap();
    assert!(!reopened.is_sealed());assert!(!reopened.is_released());drop(reopened);
    assert!(store.read_root(Some(&original.source.lease)).is_ok());
    occupied.release(CasReleaseOutcome::Aborted).unwrap();
    assert_eq!(store.close_copy_continuation(&id,0),CloseContinuationResult::Closed);
    let prepared=store.prepare_official_publication_for_job(&original.source.lease,original.source.identity.revision,"available-publication",1).unwrap();
    drop(prepared);let mut job=DurableCasJob::open(directory.path(),"available-publication").unwrap();assert!(job.is_sealed());
    job.release(CasReleaseOutcome::Aborted).unwrap();
    assert!(matches!(store.read_root(Some(&original.source.lease)),Err(StoreError::SnapshotReleased)));
}
