use super::*;
use super::super::{archive, AssetOwnerLocator, ConversationMutation};
use std::cell::{Cell, RefCell};

fn fixture() -> (tempfile::TempDir, PersistentStore) {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let staging = store.replace_begin().unwrap().staging_id;
    store.replace_put_root(&staging, &json!({"username":"synthetic","modules":[],"plugins":[]})).unwrap();
    store.replace_put_presets(&staging, &[]).unwrap();
    store.replace_add_characters(&staging, &[
        json!({"chaId":"source","type":"character","name":"source","chatPage":1,
            "futureOwner":{"nested":[1,"retained"]},"privateOwn":"source local",
            "chatFolders":[{"id":"folder","name":"kept","folded":false,"unknown":7},{"id":"other","folded":true}],
            "chats":[{"id":"selected","folderId":"folder","name":"selected","futureChat":{"keep":true},
                "message":[{"chatId":"duplicate","data":"first"},{"chatId":"duplicate","data":"second"},{"data":"no id"}]},
                {"id":"untouched","name":"untouched","message":[{"data":"other history"}]}]}),
        json!({"chaId":"unrelated","type":"character","name":"unrelated","chats":[]}),
    ]).unwrap();
    store.replace_commit(&staging, Some(0)).unwrap();
    (directory, store)
}

fn copy_request(store: &mut PersistentStore, id: &str) -> ConflictCopyRequest {
    let lease = store.acquire_revision(store.revision().unwrap()).unwrap().lease;
    ConflictCopyRequest { header: lww::Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: id.into() },
        expected_revision: store.revision().unwrap(), source: store.read_conflict_copy_source(&lease, "source", "selected").unwrap(),
        target_character_id: "copy".into(), target_conversation_id: "copied-chat".into(), character_fields: vec![],
        conversation_fields: vec![], ranges: vec![], introduced_asset_bindings: vec![], introduced_owner_head: None }
}

fn set(field: &str, value: Value) -> CopyField { CopyField::Set { field: field.into(), value } }
fn mutation(parts: &[&str], value: Value) -> lww::UnitMutation { lww::UnitMutation::Set { key: lww::unit_key(parts).unwrap(), value } }
fn assert_conflict(store: &mut PersistentStore, request: &ConflictCopyRequest, kind: CopyConflict) {
    let before = store.revision().unwrap();
    assert_eq!(store.copy_conflicted_conversation(request, &|| false).unwrap(), ConflictCopyResult::Conflict { kind });
    assert_eq!(store.revision().unwrap(), before);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_requests WHERE request_id=?1", [&request.header.request_id], |row| row.get::<_,i64>(0)).unwrap(), 0);
}

#[test]
fn copy_uses_captured_history_and_only_selected_scope_without_restoring_original() {
    let (_directory, mut store) = fixture();
    let mut request = copy_request(&mut store, "copy-selected");
    let unrelated = store.read_character("unrelated", None).unwrap();
    store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), conversations: Some(vec![ConversationMutation::ReplaceRange {
        character_id:"source".into(), conversation_id:"selected".into(), start:0, delete_count:0,
        messages:vec![json!({"data":"received insertion"})], conversation:None, configured_index:None,
    }]), ..Default::default() }).unwrap();
    store.archive_character("source", store.revision().unwrap(), 123).unwrap();
    let archived = archive::read_archived_object(&store.connection, &super::super::active_generation(&store.connection).unwrap(), "source").unwrap();
    request.expected_revision = store.revision().unwrap();
    request.character_fields = vec![set("name", json!("local name")), set("privateOwn", json!("local private"))];
    request.conversation_fields = vec![set("futureChat", json!({"local":true}))];
    request.ranges = vec![CopyRange { start:1, delete_count:1, messages:vec![json!({"chatId":"duplicate","data":"local replacement"})] },
        CopyRange { start:2, delete_count:0, messages:vec![json!({"data":"local insert"})] }];
    let result = store.copy_conflicted_conversation(&request, &|| false).unwrap();
    assert!(matches!(result, ConflictCopyResult::Committed { source_character_id, source_conversation_id, revision, .. }
        if source_character_id == "source" && source_conversation_id == "selected" && revision == request.expected_revision + 1));
    assert_eq!(archive::read_archived_object(&store.connection, &super::super::active_generation(&store.connection).unwrap(), "source").unwrap(), archived);
    assert_eq!(store.read_character("unrelated", None).unwrap().unwrap().value, unrelated.unwrap().value);
    assert!(store.read_character("source", None).is_err());
    let copy = store.read_character("copy", None).unwrap().unwrap().value;
    assert_eq!(copy["name"], "local name");
    assert_eq!(copy["privateOwn"], "local private");
    assert_eq!(copy["futureOwner"], json!({"nested":[1,"retained"]}));
    assert_eq!(copy["chatPage"], 0);
    assert_eq!(copy["chatFolders"], json!([{"id":"folder","name":"kept","folded":false,"unknown":7}]));
    assert!(store.read_conversation("copy", "untouched", None).unwrap().is_none());
    let chat = store.read_conversation("copy", "copied-chat", None).unwrap().unwrap().value;
    assert_eq!(chat["futureChat"], json!({"local":true}));
    assert_eq!(chat["message"], json!([{"chatId":"duplicate","data":"first"},{"chatId":"duplicate","data":"local replacement"},
        {"data":"local insert"},{"data":"no id"}]));
    assert_eq!(store.device_store().unwrap().connection().query_row("SELECT count(*) FROM lww_intents WHERE request_id=?1",
        [&request.header.request_id], |row| row.get::<_,i64>(0)).unwrap(), 0);
}

#[test]
fn copy_receipt_precedes_expired_lease_revision_and_changed_selection() {
    let (directory, mut store) = fixture();
    let request = copy_request(&mut store, "copy-replay");
    store.copy_conflicted_conversation(&request, &|| false).unwrap();
    store.release_revision(&request.source.lease).unwrap();
    drop(store);
    let mut store = PersistentStore::open(directory.path()).unwrap();
    assert!(matches!(store.copy_conflicted_conversation(&request, &|| true).unwrap(), ConflictCopyResult::Replayed {
        committed_revision, current_identity_matches:true, requires_current_recovery:true, .. } if committed_revision == request.expected_revision + 1));
    store.connection.execute("UPDATE library_sync_selection SET selection_epoch='different-selection' WHERE singleton=1", []).unwrap();
    assert!(matches!(store.copy_conflicted_conversation(&request, &|| false).unwrap(), ConflictCopyResult::Replayed { current_identity_matches:false, .. }));
    let mut altered = request.clone(); altered.character_fields.push(set("name", json!("different request")));
    assert!(store.copy_conflicted_conversation(&altered, &|| false).unwrap_err().to_string().contains("request-id-integrity"));
    let (_other_directory, mut other) = fixture();
    assert_eq!(other.copy_conflicted_conversation(&request, &|| false).unwrap(), ConflictCopyResult::HistoricalStatusUnavailable);
    assert!(other.read_character("copy", None).unwrap().is_none());
}

#[test]
fn fresh_copy_rejects_changed_identity_ranges_structural_fields_and_target_collision() {
    let (_directory, mut store) = fixture();
    let original = copy_request(&mut store, "copy-reject");
    let mut bad = original.clone(); bad.source.identity.selection_epoch = "wrong".into();
    assert_conflict(&mut store, &bad, CopyConflict::Identity);
    let mut bad = original.clone(); bad.ranges.push(CopyRange { start:1, delete_count:9, messages:vec![] });
    assert_conflict(&mut store, &bad, CopyConflict::Range);
    let mut bad = original.clone(); bad.character_fields.push(set("chaId", json!("hijacked")));
    assert_conflict(&mut store, &bad, CopyConflict::UnsupportedField);
    let mut bad = original.clone(); bad.target_character_id = "unrelated".into();
    assert_conflict(&mut store, &bad, CopyConflict::TargetCollision);
    store.release_revision(&original.source.lease).unwrap();
    assert_conflict(&mut store, &original, CopyConflict::SourceHistory);
}

fn alias(cas: &PayloadCas, key: &str, kind: &str, bytes: &[u8]) -> AssetAlias {
    let object = cas.prepare_bytes(bytes).unwrap();
    AssetAlias { key:key.into(), kind:kind.into(), object_hash:Some(object.content_hash), size:object.byte_size as i64,
        mime:"image/png".into(), name:"synthetic".into(), ext:"png".into(), inlay_type:(kind == "inlay").then(|| "image".into()),
        width:None, height:None, metadata:json!({"future":{"keep":true}}) }
}

fn asset_fixture() -> (tempfile::TempDir, PersistentStore, ConflictCopyRequest, AssetAlias, AssetAlias, String) {
    let (directory, mut store) = fixture();
    let cas = PayloadCas::new(directory.path()).unwrap();
    let old = alias(&cas, "same-key", "inlay", b"old synthetic asset");
    let image = AssetAlias { kind:"asset".into(), inlay_type:None, ..old.clone() };
    store.commit_with_asset_aliases(&WorkingSetCommit { expected_revision:store.revision().unwrap(), unit_mutations:Some(vec![
        mutation(&["character","source","image"], json!("same-key")),
        mutation(&["character","source","additionalAssets"], json!([["name","same-key","png"]])),
        mutation(&["conversation","source","selected","note"], json!("{{inlay::same-key}}")),
    ]), ..Default::default() }, &[old.clone(), image]).unwrap();
    let mut request = copy_request(&mut store, "copy-assets");
    let new = alias(&cas, "same-key", "inlay", b"new synthetic asset");
    store.commit_with_asset_aliases(&WorkingSetCommit { expected_revision:store.revision().unwrap(), ..Default::default() }, &[new.clone()]).unwrap();
    let witness = store.acquire_revision(store.revision().unwrap()).unwrap().lease;
    request.expected_revision = store.revision().unwrap();
    (directory, store, request, old, new, witness)
}

#[test]
fn copy_preserves_old_and_introduced_bindings_per_token_and_manifest_without_overwriting_aliases() {
    let (directory, mut store, mut request, old, new, witness) = asset_fixture();
    request.conversation_fields = vec![set("note", json!("😀{{inlay::same-key}} {{inlayed::same-key}}")),
        set("future./field", json!({"[]\"":{"a/b~c":"{{inlayeddata::same-key}}"}}))];
    request.introduced_asset_bindings = vec![
        IntroducedAssetBinding { scope:AssetScope::ConversationField { mutation:0 }, location:AssetLocation { pointer:"".into(), token_index:Some(0) }, alias:old.clone(), witness_lease:request.source.lease.clone() },
        IntroducedAssetBinding { scope:AssetScope::ConversationField { mutation:0 }, location:AssetLocation { pointer:"".into(), token_index:Some(1) }, alias:new.clone(), witness_lease:witness.clone() },
        IntroducedAssetBinding { scope:AssetScope::ConversationField { mutation:1 }, location:AssetLocation { pointer:"/[]\"/a~1b~0c".into(), token_index:Some(0) }, alias:new.clone(), witness_lease:witness },
    ];
    assert!(matches!(store.copy_conflicted_conversation(&request, &|| false).unwrap(), ConflictCopyResult::Committed { .. }));
    let chat = store.read_conversation("copy", "copied-chat", None).unwrap().unwrap().value;
    let references = crate::lossless_f0::scan_portable_fragment(crate::lossless_f0::PortableFragment::Conversation { value:&chat, character_id:"copy" }).unwrap();
    let note = references.iter().filter(|reference| reference.source_path == "$.note").collect::<Vec<_>>();
    assert_eq!(note.len(), 2);
    assert_ne!(note[0].target_key, note[1].target_key);
    assert_eq!(store.read_asset_alias("inlay", &note[0].target_key, None).unwrap().unwrap().value.object_hash, old.object_hash);
    assert_eq!(store.read_asset_alias("inlay", &note[1].target_key, None).unwrap().unwrap().value.object_hash, new.object_hash);
    assert_eq!(store.read_asset_alias("inlay", "same-key", None).unwrap().unwrap().value, new);
    let owner = AssetOwnerLocator::CharacterAdditionalAssets { character_id:"copy".into() };
    let head = store.read_asset_owner_head(&owner, None).unwrap().unwrap().value;
    let bytes = PayloadCas::new(directory.path()).unwrap().read_object(head.manifest_hash.as_deref().unwrap()).unwrap().unwrap();
    let entries = crate::asset_repository::owner_manifest_codec::decode_owner_manifest(&bytes).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].payload_hash.map(hex::encode), old.object_hash);
    assert_ne!(entries[0].tuple[1], "same-key");
    assert_eq!(store.read_asset_alias("asset", &entries[0].tuple[1], None).unwrap().unwrap().value.metadata, old.metadata);
}

#[test]
fn copy_missing_wrong_expired_and_duplicate_occurrence_evidence_is_precommit_conflict() {
    let (_directory, mut store, mut request, old, new, witness) = asset_fixture();
    request.conversation_fields.push(set("note", json!("{{inlay::same-key}}")));
    assert_conflict(&mut store, &request, CopyConflict::AssetBinding);
    let proof = IntroducedAssetBinding { scope:AssetScope::ConversationField { mutation:0 },
        location:AssetLocation { pointer:"".into(), token_index:Some(0) }, alias:new, witness_lease:witness.clone() };
    request.introduced_asset_bindings.push(IntroducedAssetBinding { alias:old, ..proof.clone() });
    assert_conflict(&mut store, &request, CopyConflict::AssetBinding);
    request.introduced_asset_bindings = vec![proof.clone(), proof.clone()];
    assert_conflict(&mut store, &request, CopyConflict::AssetBinding);
    request.introduced_asset_bindings = vec![proof];
    store.release_revision(&witness).unwrap();
    assert_conflict(&mut store, &request, CopyConflict::AssetBinding);
}

#[test]
fn copy_receipt_failure_rolls_back_owner_aliases_units_and_single_revision() {
    let (_directory, mut store, request, _, _, _) = asset_fixture();
    let before = store.lww_read_outbox(request.header.binding_authority, 256).unwrap();
    store.connection.execute_batch("CREATE TEMP TRIGGER reject_copy_receipt BEFORE INSERT ON main.lww_requests WHEN NEW.request_id='copy-assets' BEGIN SELECT RAISE(ABORT,'synthetic-copy-receipt'); END").unwrap();
    assert!(store.copy_conflicted_conversation(&request, &|| false).is_err());
    assert_eq!(store.revision().unwrap(), request.expected_revision);
    assert!(store.read_character("copy", None).unwrap().is_none());
    assert_eq!(store.lww_read_outbox(request.header.binding_authority, 256).unwrap().entries, before.entries);
    assert!(store.list_asset_aliases(None).unwrap().value.iter().all(|alias| !alias.key.starts_with("assets/copy-")));
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_requests WHERE request_id='copy-assets'", [], |row| row.get::<_,i64>(0)).unwrap(), 0);
    store.connection.execute_batch("DROP TRIGGER reject_copy_receipt").unwrap();
    assert!(matches!(store.copy_conflicted_conversation(&request, &|| false).unwrap(), ConflictCopyResult::Committed { .. }));
}

#[test]
fn copy_cancellation_after_manifest_publish_catalogs_durable_abort_candidate() {
    use crate::asset_repository::job_pins::{collect_durable_cas_job_roots_read_only, durable_cas_job_ids};
    let (directory, mut store, request, _, _, _) = asset_fixture();
    let cas = PayloadCas::new(directory.path()).unwrap();
    let published = RefCell::new(None);
    let result = store.copy_conflicted_conversation(&request, &|| {
        let roots = collect_durable_cas_job_roots_read_only(directory.path());
        let found = roots.object_hashes.into_iter().find(|hash| cas.stat_object(hash).unwrap().is_some());
        let cancel = found.is_some(); if cancel { *published.borrow_mut() = found; } cancel
    }).unwrap();
    assert_eq!(result, ConflictCopyResult::Conflict { kind:CopyConflict::Cancelled });
    let hash = published.into_inner().expect("cancellation reached first published manifest");
    assert_eq!(store.revision().unwrap(), request.expected_revision);
    assert!(store.read_character("copy", None).unwrap().is_none());
    assert_eq!(store.connection.query_row("SELECT count(*) FROM asset_objects WHERE object_hash=?1", [&hash], |row| row.get::<_,i64>(0)).unwrap(), 1);
    assert!(durable_cas_job_ids(directory.path()).unwrap().is_empty());
    assert!(cas.stat_object(&hash).unwrap().is_some());
    assert!(matches!(store.copy_conflicted_conversation(&request, &|| false).unwrap(), ConflictCopyResult::Committed { .. }));
}

#[test]
fn copy_holds_binding_and_selection_locks_through_publication_and_library_commit() {
    use crate::asset_repository::job_pins::durable_cas_job_ids;
    let (directory, mut store, request, _, _, _) = asset_fixture();
    let device = Connection::open(directory.path().join("persistent").join(super::super::device_store::DEVICE_DATABASE_FILE)).unwrap();
    let library = Connection::open(&store.database_path).unwrap();
    device.busy_timeout(std::time::Duration::ZERO).unwrap(); library.busy_timeout(std::time::Duration::ZERO).unwrap();
    let attempted = Cell::new(false);
    store.copy_conflicted_conversation(&request, &|| {
        if !attempted.get() && !durable_cas_job_ids(directory.path()).unwrap().is_empty() {
            attempted.set(true);
            assert_eq!(device.execute("UPDATE lww_clock SET binding_authority='999' WHERE singleton=1", []).unwrap_err().sqlite_error_code(), Some(rusqlite::ErrorCode::DatabaseBusy));
            assert_eq!(library.execute("UPDATE library_sync_selection SET selection_epoch='racing' WHERE singleton=1", []).unwrap_err().sqlite_error_code(), Some(rusqlite::ErrorCode::DatabaseBusy));
        }
        false
    }).unwrap();
    assert!(attempted.get());
    assert_eq!(store.lww_binding_authority().unwrap(), request.header.binding_authority);
}

#[test]
fn copied_standard_units_survive_peer_receive_and_archive_restore_with_local_fields_filtered() {
    let (_directory, mut source) = fixture();
    let request = copy_request(&mut source, "copy-peer");
    source.copy_conflicted_conversation(&request, &|| false).unwrap();
    let entries = source.lww_read_outbox(request.header.binding_authority, 512).unwrap().entries.into_iter()
        .filter(|entry| entry.version == request.header.request_id).collect::<Vec<_>>();
    assert!(entries.iter().any(|entry| entry.key.components() == ["exists","character","copy"]));
    assert!(entries.iter().any(|entry| entry.key.components() == ["messages","copy","copied-chat"]));
    assert!(!entries.iter().any(|entry| entry.key.components().last().is_some_and(|field| field == "privateOwn")));
    let (_target_directory, mut target) = fixture();
    for row in source.connection.prepare("SELECT hash,body FROM message_page_objects").unwrap().query_map([], |row|
        Ok((row.get::<_,String>(0)?, row.get::<_,Vec<u8>>(1)?))).unwrap() {
        let (hash, body) = row.unwrap(); target.lww_put_object(&hash, &body).unwrap();
    }
    let header = lww::Header { binding_authority:target.lww_binding_authority().unwrap(), request_id:"copy-peer-receive".into() };
    target.lww_stage_receive(&lww::StageReceive { header:header.clone(), changes:entries.into_iter()
        .map(|entry| lww::Change { key:entry.key, stamp:entry.stamp, value:entry.value }).collect(),
        progress:lww::Progress { kind:"server".into(), cursor:1.into(), writer_id:None }, admitted_time_upper_ms:u64::MAX.into() }).unwrap();
    target.lww_apply_receive(&lww::ApplyReceive { header:header.clone(), generating:vec![] }).unwrap();
    target.lww_finish_receive(&header).unwrap();
    let copied = target.read_conversation("copy", "copied-chat", None).unwrap().unwrap().value;
    assert_eq!(copied["message"], source.read_conversation("copy", "copied-chat", None).unwrap().unwrap().value["message"]);
    assert!(target.read_character("copy", None).unwrap().unwrap().value.get("privateOwn").is_none());
    target.archive_character("copy", target.revision().unwrap(), 10).unwrap();
    target.restore_character("copy", target.revision().unwrap()).unwrap();
    assert_eq!(target.read_conversation("copy", "copied-chat", None).unwrap().unwrap().value, copied);
}

#[test]
fn library_identity_or_revision_change_after_preflight_is_refused_without_writes() {
    let (_directory, mut store) = fixture();
    let request = copy_request(&mut store, "copy-library-race");
    let library = Connection::open(&store.database_path).unwrap();
    let changed = Cell::new(false);
    let result = store.copy_conflicted_conversation(&request, &|| {
        if !changed.replace(true) {
            library.execute("UPDATE local_library_identity SET library_epoch='replacement-library' WHERE singleton=1", []).unwrap();
        }
        false
    }).unwrap();
    assert_eq!(result, ConflictCopyResult::HistoricalStatusUnavailable);
    assert_eq!(store.revision().unwrap(), request.expected_revision);
    assert!(store.read_character("copy", None).unwrap().is_none());
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_requests WHERE request_id=?1", [&request.header.request_id], |row| row.get::<_,i64>(0)).unwrap(), 0);

    let (directory, mut store) = fixture();
    let writer = RefCell::new(PersistentStore::open(directory.path()).unwrap());
    let request = copy_request(&mut store, "copy-revision-race");
    let changed = Cell::new(false);
    let error = store.copy_conflicted_conversation(&request, &|| {
        if !changed.replace(true) {
            writer.borrow_mut().commit(&WorkingSetCommit { expected_revision: request.expected_revision,
                root: Some(json!({"username":"concurrent","modules":[],"plugins":[]})), ..Default::default() }).unwrap();
        }
        false
    }).unwrap_err();
    assert_eq!(error, StoreError::RevisionConflict { expected: request.expected_revision, actual: request.expected_revision + 1 });
    assert_eq!(store.revision().unwrap(), request.expected_revision + 1);
    assert!(store.read_character("copy", None).unwrap().is_none());
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_requests WHERE request_id=?1", [&request.header.request_id], |row| row.get::<_,i64>(0)).unwrap(), 0);
}

#[test]
fn removed_submitted_rows_validate_witnesses_but_publish_no_unused_aliases() {
    let (_directory, mut store, mut request, _, new, witness) = asset_fixture();
    request.ranges = vec![CopyRange { start:0, delete_count:0, messages:vec![json!({"data":"{{inlay::same-key}}"})] },
        CopyRange { start:0, delete_count:1, messages:vec![] }];
    assert_conflict(&mut store, &request, CopyConflict::AssetBinding);
    request.introduced_asset_bindings = vec![IntroducedAssetBinding { scope:AssetScope::Message { range:0, message:0 },
        location:AssetLocation { pointer:"/data".into(), token_index:Some(0) }, alias:new.clone(), witness_lease:witness }];
    assert!(matches!(store.copy_conflicted_conversation(&request, &|| false).unwrap(), ConflictCopyResult::Committed { .. }));
    assert!(store.list_asset_aliases(None).unwrap().value.iter().all(|alias| alias.key == "same-key" || alias.object_hash != new.object_hash));
    assert_eq!(store.read_conversation("copy", "copied-chat", None).unwrap().unwrap().value["message"],
        store.read_conversation("source", "selected", Some(&request.source.lease)).unwrap().unwrap().value["message"]);
}

#[test]
fn missing_manifest_control_and_unbound_legacy_asset_refuse_copy_without_fallback() {
    let (_directory, mut store, _, _, _, _) = asset_fixture();
    let character = store.read_character("source", None).unwrap().unwrap().value;
    let owner = AssetOwnerLocator::CharacterAdditionalAssets { character_id:"source".into() };
    store.commit(&WorkingSetCommit { expected_revision:store.revision().unwrap(), character:Some(character),
        asset_owner_heads:Some(vec![AssetOwnerHead { owner, present:true, manifest_hash:Some("f".repeat(64)), entry_count:1 }]), ..Default::default() }).unwrap();
    let request = copy_request(&mut store, "copy-missing-control");
    assert_conflict(&mut store, &request, CopyConflict::AssetBinding);
    let (_directory, mut legacy) = fixture();
    let unbound = AssetAlias { key:"legacy-key".into(), object_hash:None, kind:"asset".into(), size:0, mime:"".into(), name:"".into(), ext:"".into(),
        inlay_type:None, width:None, height:None, metadata:json!({}) };
    legacy.commit_with_asset_aliases(&WorkingSetCommit { expected_revision:legacy.revision().unwrap(),
        unit_mutations:Some(vec![mutation(&["character","source","image"], json!("legacy-key"))]), ..Default::default() }, &[unbound]).unwrap();
    let request = copy_request(&mut legacy, "copy-legacy-binding");
    assert_conflict(&mut legacy, &request, CopyConflict::AssetBinding);
}

#[test]
fn extra_unused_scope_and_changed_binding_are_precommit_conflicts() {
    let (_directory, mut store, mut request, _, new, witness) = asset_fixture();
    request.introduced_asset_bindings.push(IntroducedAssetBinding { scope:AssetScope::Message { range:99, message:0 },
        location:AssetLocation { pointer:"/data".into(), token_index:Some(0) }, alias:new, witness_lease:witness });
    assert_conflict(&mut store, &request, CopyConflict::AssetBinding);
    request.introduced_asset_bindings.clear();
    store.device_store().unwrap().connection().execute("UPDATE lww_clock SET binding_authority='987' WHERE singleton=1", []).unwrap();
    assert_conflict(&mut store, &request, CopyConflict::Identity);
}

#[test]
fn introduced_owner_manifest_requires_matching_scoped_head_and_alias_witnesses() {
    use crate::asset_repository::owner_manifest_codec::{encode_owner_manifest, OwnerManifestEntry};
    let (directory, mut store, mut request, _, new, _) = asset_fixture();
    let cas = PayloadCas::new(directory.path()).unwrap();
    let image = AssetAlias { kind:"asset".into(), inlay_type:None, ..new.clone() };
    let entries = vec![OwnerManifestEntry { tuple:["new tuple".into(), "same-key".into(), "png".into()],
        payload_hash:Some(hex::decode(image.object_hash.as_ref().unwrap()).unwrap().try_into().unwrap()) }];
    let bytes = encode_owner_manifest(&entries).unwrap();
    let object = cas.prepare_bytes(&bytes).unwrap();
    let head = AssetOwnerHead { owner:AssetOwnerLocator::CharacterAdditionalAssets { character_id:"source".into() },
        present:true, manifest_hash:Some(object.content_hash), entry_count:1 };
    let mut character = store.read_character("source", None).unwrap().unwrap().value;
    character["additionalAssets"] = json!([["new tuple","same-key","png"]]);
    store.commit_with_asset_aliases(&WorkingSetCommit { expected_revision:store.revision().unwrap(), character:Some(character),
        asset_owner_heads:Some(vec![head.clone()]), ..Default::default() }, &[image.clone()]).unwrap();
    let witness = store.acquire_revision(store.revision().unwrap()).unwrap().lease;
    request.expected_revision = store.revision().unwrap();
    request.character_fields = vec![set("additionalAssets", json!([["new tuple","same-key","png"]]))];
    request.introduced_owner_head = Some(IntroducedOwnerHead { mutation:0, head:head.clone(), witness_lease:witness.clone() });
    request.introduced_asset_bindings = vec![IntroducedAssetBinding { scope:AssetScope::CharacterField { mutation:0 },
        location:AssetLocation { pointer:"/0/1".into(), token_index:None }, alias:image, witness_lease:witness }];
    let mut wrong = request.clone(); wrong.introduced_owner_head.as_mut().unwrap().head.entry_count = 2;
    assert_conflict(&mut store, &wrong, CopyConflict::AssetBinding);
    assert!(matches!(store.copy_conflicted_conversation(&request, &|| false).unwrap(), ConflictCopyResult::Committed { .. }));
    let copied = store.read_asset_owner_head(&AssetOwnerLocator::CharacterAdditionalAssets { character_id:"copy".into() }, None).unwrap().unwrap().value;
    let decoded = crate::asset_repository::owner_manifest_codec::decode_owner_manifest(&cas.read_object(copied.manifest_hash.as_deref().unwrap()).unwrap().unwrap()).unwrap();
    assert_eq!(decoded[0].payload_hash.map(hex::encode), new.object_hash);
    assert_eq!(decoded[0].tuple[0], "new tuple");
    assert_ne!(decoded[0].tuple[1], "same-key");
}

#[test]
fn receipt_digest_keeps_native_integer_precision_and_canonical_object_order() {
    let (_directory, mut store) = fixture();
    let mut request = copy_request(&mut store, "copy-exact-digest");
    request.character_fields.push(set("futureNumber", json!(9_007_199_254_740_992i64)));
    request.conversation_fields.push(set("futureObject", serde_json::from_str("{\"b\":2,\"a\":1}").unwrap()));
    store.copy_conflicted_conversation(&request, &|| false).unwrap();
    let mut reordered = request.clone();
    reordered.conversation_fields[0] = set("futureObject", serde_json::from_str("{\"a\":1,\"b\":2}").unwrap());
    assert!(matches!(store.copy_conflicted_conversation(&reordered, &|| false).unwrap(), ConflictCopyResult::Replayed { .. }));
    let mut changed = request.clone();
    changed.character_fields[0] = set("futureNumber", json!(9_007_199_254_740_993i64));
    assert!(store.copy_conflicted_conversation(&changed, &|| false).unwrap_err().to_string().contains("request-id-integrity"));
    assert_eq!(store.read_character("copy", None).unwrap().unwrap().value["futureNumber"].as_i64(), Some(9_007_199_254_740_992i64));
}

#[test]
fn cancelled_copy_keeps_unreferenced_witness_payload_until_caller_discards_lease() {
    let (directory, mut store) = fixture();
    let mut request = copy_request(&mut store, "copy-witness-lifetime");
    let cas = PayloadCas::new(directory.path()).unwrap();
    let introduced = alias(&cas, "ephemeral", "inlay", b"witness-only synthetic payload");
    let hash = introduced.object_hash.clone().unwrap();
    store.asset_object_catalog().register(&[asset_object_catalog::AssetObjectRegistration {
        object_hash:hash.clone(), byte_size:introduced.size as u64 }], 0).unwrap();
    store.commit_asset_alias(&introduced, store.revision().unwrap()).unwrap();
    let witness = store.acquire_revision(store.revision().unwrap()).unwrap().lease;
    store.delete_asset_alias("inlay", "ephemeral", store.revision().unwrap()).unwrap();
    request.expected_revision = store.revision().unwrap();
    request.ranges = vec![CopyRange { start:0, delete_count:0, messages:vec![json!({"data":"{{inlay::ephemeral}}"})] }];
    request.introduced_asset_bindings = vec![IntroducedAssetBinding { scope:AssetScope::Message { range:0, message:0 },
        location:AssetLocation { pointer:"/data".into(), token_index:Some(0) }, alias:introduced.clone(), witness_lease:witness.clone() }];
    assert_eq!(store.copy_conflicted_conversation(&request, &|| true).unwrap(), ConflictCopyResult::Conflict { kind:CopyConflict::Cancelled });
    assert_eq!(store.read_asset_alias("inlay", "ephemeral", Some(&witness)).unwrap().unwrap().value, introduced);
    let held = store.asset_gc_dry_run(64, None, 100, 10).unwrap().report;
    assert!(held.marked_hashes.contains(&hash));
    assert!(!held.potential_delete_hashes.contains(&hash));
    store.release_revision(&witness).unwrap();
    let discarded = store.asset_gc_dry_run(64, None, 100, 10).unwrap().report;
    assert!(discarded.potential_delete_hashes.contains(&hash));
    assert!(!discarded.marked_hashes.contains(&hash));
}
