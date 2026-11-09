use super::*;
use crate::persistent_store::query;

#[path = "lww_retention_tests.rs"]
mod retention;

#[path = "lww_quarantine_tests.rs"]
mod quarantine;

/// Replacement changes as they were computed before the merge: both libraries
/// captured into whole maps.
pub(super) fn whole_library_changes(
    db: &mut Connection,
    generation: &str,
    staging_id: &str,
    source: Option<&BTreeMap<UnitKey, UnitValue>>,
) -> StoreResult<(Vec<(UnitKey, UnitValue)>, BTreeSet<UnitKey>)> {
    let before = projection::capture_all(db, generation, true)?;
    let mut after = projection::capture_all(db, staging_id, true)?;
    let mut overrides = BTreeSet::new();
    for (key, value) in source.into_iter().flatten() {
        if after.get(key) != Some(value) {
            overrides.insert(key.clone());
        }
        after.insert(key.clone(), value.clone());
    }
    let mut keys = before.keys().chain(after.keys()).cloned().collect::<BTreeSet<_>>();
    let stored: Vec<String> = db.prepare("SELECT key FROM lww_units")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    for raw in stored {
        let key: UnitKey = wire(raw.try_into())?;
        if projection::known(&key) {
            keys.insert(key);
        }
    }
    let mut changes = Vec::new();
    for key in keys {
        if archived_character(&key).is_some_and(|id| {
            after.get(&unit_key(&["archive", &id]).unwrap()).is_some_and(|value| matches!(value, UnitValue::Object { .. }))
        }) && source.is_none_or(|units| !units.contains_key(&key))
        {
            continue;
        }
        let value = after.get(&key).cloned().unwrap_or(UnitValue::Deleted);
        if !matches!(value, UnitValue::Deleted) && parent_status(db, &key)? == "retired" {
            return Err(error("retired-record-id"));
        }
        let prior = read_unit(db, &key)?.map(|(_, v)| v).or_else(|| before.get(&key).cloned()).unwrap_or(UnitValue::Deleted);
        if prior != value {
            changes.push((key, value));
        }
    }
    overrides.retain(|key| changes.binary_search_by(|(changed, _)| changed.cmp(key)).is_ok());
    Ok((changes, overrides))
}

fn store() -> (tempfile::TempDir, PersistentStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = PersistentStore::open(dir.path()).unwrap();
    (dir, store)
}
fn mutation(key: &[&str], value: Value) -> UnitMutation {
    UnitMutation::Set {
        key: UnitKey::new(key).unwrap(),
        value,
    }
}
fn save(store: &mut PersistentStore, mutations: Vec<UnitMutation>) -> RevisionResult {
    store
        .commit(&WorkingSetCommit {
            expected_revision: store.revision().unwrap(),
            unit_mutations: Some(mutations),
            ..Default::default()
        })
        .unwrap()
}
fn stamp(time: u64) -> Stamp {
    Stamp {
        physical_ms: time.into(),
        logical: 0,
        writer_id: "00000000-0000-4000-8000-000000000001".into(),
    }
}
fn receive(
    store: &mut PersistentStore,
    id: &str,
    changes: Vec<Change>,
    generating: Vec<MessageLocator>,
) -> ApplyResult {
    let header = Header {
        binding_authority: store.lww_binding_authority().unwrap(),
        request_id: id.into(),
    };
    store
        .lww_stage_receive(&StageReceive {
            header: header.clone(),
            changes,
            progress: Progress {
                kind: "server".into(),
                cursor: 1.into(),
                writer_id: None,
            },
            admitted_time_upper_ms: u64::MAX.into(),
        })
        .unwrap();
    let result = store
        .lww_apply_receive(&ApplyReceive {
            header: header.clone(),
            generating,
        })
        .unwrap();
    store.lww_finish_receive(&header).unwrap();
    result
}
fn change(parts: &[&str], time: u64, value: Value) -> Change {
    Change {
        key: UnitKey::new(parts).unwrap(),
        stamp: stamp(time),
        value: inline(&value).unwrap(),
    }
}

#[test]
fn empty_identity_selections_replace_and_commit_without_initializing_empty_owners() {
    let (_dir, mut store) = store();
    let stage = store.replace_begin().unwrap().staging_id;
    store.replace_put_root(&stage, &serde_json::json!({
        "botPresetsId":"", "selectedPersona":"", "personas":[], "username":"retained"
    })).unwrap();
    let replaced = store.replace_commit(&stage, Some(0)).unwrap();
    assert_eq!(replaced.revision, 1);
    let root = store.read_root(None).unwrap().value;
    assert_eq!(root["botPresetsId"], "");
    assert_eq!(root["selectedPersona"], "");
    assert_eq!(root["username"], "retained");
    let outbox = store.lww_read_outbox(0.into(), 100).unwrap().entries;
    for field in ["botPresetsId", "selectedPersona"] {
        assert_eq!(outbox.iter().find(|entry| entry.key == unit_key(&["root", field]).unwrap()).unwrap().value,
            inline(&serde_json::json!("")).unwrap());
    }
    assert!(outbox.iter().all(|entry| entry.key.components()[0] != "exists"));
    save(&mut store, vec![
        mutation(&["root", "botPresetsId"], serde_json::json!("missing-preset")),
        mutation(&["root", "selectedPersona"], serde_json::json!("missing-persona")),
    ]);
    let cleared = save(&mut store, vec![
        mutation(&["root", "botPresetsId"], serde_json::json!("")),
        mutation(&["root", "selectedPersona"], serde_json::json!("")),
    ]);
    assert_eq!(cleared.revision, 3);
    let root = store.read_root(None).unwrap().value;
    assert_eq!(root["botPresetsId"], "");
    assert_eq!(root["selectedPersona"], "");
    assert_eq!(root["username"], "retained");
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_initialization_scopes", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn unchanged_units_emit_nothing_and_a_commit_shares_one_stamp() {
    let (_dir, mut store) = store();
    save(
        &mut store,
        vec![
            mutation(&["root", "openAIKey"], serde_json::json!("synthetic")),
            mutation(&["root", "language"], serde_json::json!("en")),
        ],
    );
    let first = store.lww_read_outbox(0.into(), 10).unwrap();
    assert_eq!(first.entries.len(), 2);
    assert_eq!(first.entries[0].stamp, first.entries[1].stamp);
    store
        .lww_ack_outbox(
            &Header {
                binding_authority: 0.into(),
                request_id: "ack".into(),
            },
            &first
                .entries
                .iter()
                .map(|e| AckEntry {
                    key: e.key.clone(),
                    version: e.version.clone(),
                    stamp: e.stamp.clone(),
                    value_identity: e.value.identity().unwrap(),
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
    save(
        &mut store,
        vec![mutation(
            &["root", "openAIKey"],
            serde_json::json!("synthetic"),
        )],
    );
    assert!(store
        .lww_read_outbox(0.into(), 10)
        .unwrap()
        .entries
        .is_empty());
}
#[test]
fn later_local_edit_survives_exact_version_ack() {
    let (_dir, mut store) = store();
    save(
        &mut store,
        vec![mutation(&["root", "openAIKey"], serde_json::json!("a"))],
    );
    let old = store
        .lww_read_outbox(0.into(), 1)
        .unwrap()
        .entries
        .remove(0);
    save(
        &mut store,
        vec![mutation(&["root", "openAIKey"], serde_json::json!("b"))],
    );
    store
        .lww_ack_outbox(
            &Header {
                binding_authority: 0.into(),
                request_id: "ack".into(),
            },
            &[AckEntry {
                key: old.key,
                version: old.version,
                stamp: old.stamp,
                value_identity: old.value.identity().unwrap(),
            }],
        )
        .unwrap();
    assert_eq!(
        store.lww_read_outbox(0.into(), 10).unwrap().entries.len(),
        1
    );
}
#[test]
fn remote_lww_compares_pending_state_and_equal_stamp_integrity_is_rejected() {
    let (_dir, mut store) = store();
    save(
        &mut store,
        vec![mutation(&["root", "openAIKey"], serde_json::json!("local"))],
    );
    let old = store
        .lww_read_outbox(0.into(), 1)
        .unwrap()
        .entries
        .remove(0);
    receive(
        &mut store,
        "older",
        vec![change(
            &["root", "openAIKey"],
            old.stamp.physical_ms.0 - 1,
            serde_json::json!("old"),
        )],
        vec![],
    );
    assert_eq!(
        store.lww_read_outbox(0.into(), 10).unwrap().entries.len(),
        1
    );
    receive(
        &mut store,
        "newer",
        vec![change(
            &["root", "openAIKey"],
            old.stamp.physical_ms.0 + 1,
            serde_json::json!("new"),
        )],
        vec![],
    );
    assert!(store
        .lww_read_outbox(0.into(), 10)
        .unwrap()
        .entries
        .is_empty());
    assert_eq!(store.read_root(None).unwrap().value["openAIKey"], "new");
    let mut conflicting = change(
        &["root", "openAIKey"],
        old.stamp.physical_ms.0 + 1,
        serde_json::json!("bad"),
    );
    conflicting.stamp = stamp(old.stamp.physical_ms.0 + 1);
    assert!(store
        .lww_stage_receive(&StageReceive {
            header: Header {
                binding_authority: 0.into(),
                request_id: "bad".into()
            },
            changes: vec![conflicting],
            progress: Progress {
                kind: "server".into(),
                cursor: 2.into(),
                writer_id: None
            },
            admitted_time_upper_ms: u64::MAX.into()
        })
        .unwrap_err()
        .to_string()
        .contains("equal-stamp-integrity"));
}
#[test]
fn missing_parent_is_durable_and_hard_retirement_suppresses_future_children() {
    let (dir, mut store) = store();
    let result = receive(
        &mut store,
        "child",
        vec![change(
            &["character", "char", "name"],
            10,
            serde_json::json!("child"),
        )],
        vec![],
    );
    assert_eq!(result.held_keys.len(), 1);
    drop(store);
    let mut store = PersistentStore::open(dir.path()).unwrap();
    receive(
        &mut store,
        "parent",
        vec![change(
            &["exists", "character", "char"],
            9,
            serde_json::json!({"type":"character"}),
        )],
        vec![],
    );
    assert_eq!(
        store.read_character("char", None).unwrap().unwrap().value["name"],
        "child"
    );
    receive(
        &mut store,
        "delete",
        vec![Change {
            key: UnitKey::new(&["exists", "character", "char"]).unwrap(),
            stamp: stamp(20),
            value: UnitValue::Deleted,
        }],
        vec![],
    );
    receive(
        &mut store,
        "stale",
        vec![
            change(
                &["exists", "character", "char"],
                30,
                serde_json::json!({"type":"character"}),
            ),
            change(
                &["character", "char", "name"],
                31,
                serde_json::json!("revival"),
            ),
        ],
        vec![],
    );
    assert!(store.read_character("char", None).unwrap().is_none());
    assert!(read_unit(&store.connection, &unit_key(&["character", "char", "name"]).unwrap()).unwrap().is_none());
    assert!(is_retired(
        &store.connection,
        &unit_key(&["exists", "character", "char"]).unwrap()
    )
    .unwrap());
}
#[test]
fn unknown_units_are_opaque_and_never_recaptured() {
    let (_dir, mut store) = store();
    let unknown = change(&["future-unit", "x"], 1, serde_json::json!({"opaque":2}));
    receive(&mut store, "opaque", vec![unknown.clone()], vec![]);
    save(
        &mut store,
        vec![mutation(&["root", "language"], serde_json::json!("ko"))],
    );
    assert_eq!(
        read_unit(&store.connection, &unknown.key)
            .unwrap()
            .unwrap()
            .1,
        unknown.value
    );
    assert!(store
        .lww_read_outbox(0.into(), 10)
        .unwrap()
        .entries
        .iter()
        .all(|e| e.key != unknown.key));
}

#[test]
fn commit_input_capture_conserves_actual_reserve_completed_and_rejected_retry_hashes() {
    use crate::persistent_store::hash_work::{reset_commit_intent_inputs, take_commit_intent_inputs,
        reset_hash_work, take_hash_work, DomainWork};
    let (_directory, mut store) = store();
    let header = Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: "synthetic-commit-input".into() };
    let input = WorkingSetCommit { expected_revision: store.revision().unwrap(), request_id: Some(header.request_id.clone()),
        unit_mutations: Some(vec![mutation(&["root", "language"], serde_json::json!("synthetic\nvalue"))]),
        ..Default::default() };
    let intent = Intent::Commit { commit: input.clone(), aliases: vec![] };
    let mut changed = input.clone(); changed.expected_revision += 1;
    let changed = Intent::Commit { commit: changed, aliases: vec![] };
    let expected = serde_json::to_vec(&intent).unwrap();
    let changed_expected = serde_json::to_vec(&changed).unwrap();
    reset_hash_work(); reset_commit_intent_inputs();
    let result = store.commit(&input).unwrap();
    assert_eq!(store.commit(&input).unwrap().revision, result.revision);
    let mut changed_input = input.clone(); changed_input.expected_revision += 1;
    assert!(store.commit(&changed_input).is_err());
    let work = take_hash_work(); let inputs = take_commit_intent_inputs();
    assert_eq!(inputs, vec![expected.clone(), expected, changed_expected]);
    assert_eq!(work.domains["native_intent"], DomainWork { calls: inputs.len() as u64,
        bytes: inputs.iter().map(|input| input.len() as u64).sum() });
    let receipt: String = store.connection.query_row(
        "SELECT digest FROM lww_requests WHERE request_id=?1", [&header.request_id], |row| row.get(0)).unwrap();
    assert_eq!(receipt, risunest_sync_wire::hash(inputs[0].as_slice()));
    for bytes in inputs {
        let typed: CommitIntentInput = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(serde_json::to_vec(&typed).unwrap(), bytes);
    }
    store.completed_intent(&header, &intent).unwrap();
    assert!(take_commit_intent_inputs().is_empty());
}

fn full_device_backup(label: Option<&str>) -> Vec<device_store::sections::PreparedSectionRows> {
    use risunest_external_storage_format::section::SectionKind;
    let (_dir, mut source) = store();
    if let Some(label) = label { write_restore_device_fixture(&mut source, label); }
    source.device_store_mut().unwrap().capture_backup_sections(
        &[SectionKind::Hypa, SectionKind::LocalPlugins, SectionKind::LocalSettings], &std::env::temp_dir(),
    ).unwrap()
}

fn write_restore_device_fixture(store: &mut PersistentStore, label: &str) {
    use device_store::{hypa::HypaEmbeddingWrite, plugin_values::PluginDeviceMutation};
    let device = store.device_store_mut().unwrap();
    device.write_hypa_embeddings(&[HypaEmbeddingWrite {
        cache_key: "a".repeat(64), producer: "synthetic".into(), model: label.into(), endpoint: None,
        preprocess_version: 1, dimensions: 1, vector: vec![0,0,128,63], metadata: None,
    }]).unwrap();
    device.write_plugin_device_values("orphan", &[
        PluginDeviceMutation::Set { space:"string".into(), key:"shared-key".into(), value:label.into() },
        PluginDeviceMutation::Set { space:"json".into(), key:"shared-key".into(), value:serde_json::to_string(&serde_json::json!({"label":label})).unwrap() },
    ]).unwrap();
    device.write_setting("accountst", &serde_json::json!(label)).unwrap();
    device.connection().execute("INSERT INTO plugin_permissions VALUES(?1,'synthetic',1)", [label]).unwrap();
}

fn restore_stage(store: &mut PersistentStore, language: &str) -> String {
    let stage = store.replace_begin().unwrap().staging_id;
    let mut root = store.read_root(None).unwrap().value;
    root["language"] = serde_json::json!(language);
    store.replace_put_root(&stage, &root).unwrap();
    store.replace_put_presets(&stage, &[]).unwrap();
    stage
}

fn assert_device_restore_label(store: &PersistentStore, label: &str) {
    let device = store.device_store().unwrap();
    assert_eq!(device.read_setting("accountst").unwrap(), Some(serde_json::json!(label)));
    assert_eq!(device.connection().query_row("SELECT model FROM hypa_embeddings WHERE cache_key=?1 AND tombstone=0", ["a".repeat(64)], |row| row.get::<_,String>(0)).unwrap(), label);
    let values: Vec<(String,String)> = device.connection().prepare("SELECT space,value FROM plugin_device_storage WHERE owner='orphan' AND key='shared-key' AND tombstone=0 ORDER BY space").unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().collect::<Result<_,_>>().unwrap();
    assert_eq!(values, vec![("json".into(), serde_json::to_string(&serde_json::json!({"label":label})).unwrap()), ("string".into(),label.into())]);
}

#[test]
fn replacement_scratch_failure_preserves_the_library_and_allows_retry() {
    let (directory, mut store) = store();
    let before = store.read_root(None).unwrap();
    let stage = restore_stage(&mut store, "restored");
    let header = Header { binding_authority: 0.into(), request_id: "scratch-failure".into() };
    let scratch = directory.path().join("external-storage").join("scratch");
    std::fs::create_dir_all(scratch.parent().unwrap()).unwrap();
    if scratch.is_dir() { std::fs::remove_dir(&scratch).unwrap(); }
    std::fs::write(&scratch, b"synthetic unavailable scratch").unwrap();
    assert!(store.lww_commit_replacement(&header, &stage).is_err());
    assert_eq!(store.revision().unwrap(), before.revision);
    assert_eq!(store.read_root(None).unwrap().value, before.value);
    assert_eq!(store.device_store().unwrap().connection().query_row(
        "SELECT count(*) FROM lww_intents WHERE request_id=?1", [&header.request_id],
        |row| row.get::<_,i64>(0)).unwrap(), 0);
    std::fs::remove_file(&scratch).unwrap();
    store.lww_commit_replacement(&header, &stage).unwrap();
    assert_eq!(store.read_root(None).unwrap().value["language"], "restored");
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
}

#[test]
fn full_restore_library_hypa_and_enabled_plugin_changes_share_stamp_and_receipt() {
    let (_dir, mut store) = store();
    store.device_store_mut().unwrap().set_section_participating(device_store::Section::LocalPlugins,true).unwrap();
    write_restore_device_fixture(&mut store, "old");
    store.device_store().unwrap().write_setting("nightlyWarned", &serde_json::json!(true)).unwrap();
    store.device_store().unwrap().write_setting("official-account.association.v1", &serde_json::json!({"synthetic":"current"})).unwrap();
    receive(&mut store,"accepted-before-restore",vec![change(&["future-unit","opaque"],1,serde_json::json!({"retained":true}))],vec![]);
    let progress = store.lww_receive_progress(0.into()).unwrap();
    clear_outbox(&mut store);
    let clock = store.lww_clock_state().unwrap();
    let sections = full_device_backup(Some("restored"));
    let stage = restore_stage(&mut store,"restored");
    let original = BTreeMap::from([(unit_key(&["future-unit","backup"]).unwrap(),inline(&serde_json::json!({"opaque":3})).unwrap())]);
    let header = Header { binding_authority:0.into(), request_id:"full-device-restore".into() };
    let revision = store.lww_commit_replacement_with_device_sections(&header,&stage,Some(&original),&sections.iter().collect::<Vec<_>>()).unwrap().revision;
    assert_device_restore_label(&store,"restored");
    assert!(store.device_store().unwrap().read_setting("nightlyWarned").unwrap().is_none());
    assert_eq!(store.device_store().unwrap().read_setting("official-account.association.v1").unwrap(),Some(serde_json::json!({"synthetic":"current"})));
    assert_eq!(store.device_store().unwrap().connection().query_row("SELECT count(*) FROM plugin_permissions WHERE code_hash='old'",[],|row| row.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(store.lww_clock_state().unwrap().writer_id,clock.writer_id);
    assert_eq!(store.lww_clock_state().unwrap().accepted,clock.accepted);
    assert_eq!(serde_json::to_value(store.lww_receive_progress(0.into()).unwrap()).unwrap(),serde_json::to_value(progress).unwrap());
    let outbox = store.lww_read_outbox(0.into(),100).unwrap().entries;
    assert!(outbox.iter().any(|entry| entry.key==unit_key(&["root","language"]).unwrap()));
    assert_eq!(outbox.iter().filter(|entry| is_device(&entry.key)).count(),3);
    assert!(outbox.iter().all(|entry| entry.stamp==outbox[0].stamp&&entry.version==header.request_id));
    assert!(outbox.iter().all(|entry| !entry.key.as_str().contains("accountst")&&!entry.key.as_str().contains("synthetic")));
    let persisted: String=store.device_store().unwrap().connection().query_row("SELECT body FROM lww_intents WHERE request_id=?1",[&header.request_id],|row| row.get(0)).unwrap();
    let Intent::Replacement{source_units,device_sections,..}=serde_json::from_str(&persisted).unwrap() else {panic!()};
    assert_eq!(source_units,Some(super::intent_rows::source_digest(&original).unwrap())); assert!(device_sections.is_some());
    let (_peer_dir,mut peer)=self::store();
    peer.device_store_mut().unwrap().set_section_participating(device_store::Section::LocalPlugins,true).unwrap();
    receive(&mut peer,"full-restored-peer",outbox.iter().map(|entry|Change{key:entry.key.clone(),stamp:entry.stamp.clone(),value:entry.value.clone()}).collect(),vec![]);
    assert_eq!(peer.read_root(None).unwrap().value["language"],"restored");
    assert_eq!(peer.device_store().unwrap().connection().query_row("SELECT model FROM hypa_embeddings WHERE cache_key=?1",["a".repeat(64)],|row| row.get::<_,String>(0)).unwrap(),"restored");
    assert!(peer.device_store().unwrap().read_setting("accountst").unwrap().is_none());
    write_restore_device_fixture(&mut store,"later");
    assert_eq!(store.lww_commit_replacement_with_device_sections(&header,&stage,Some(&original),&sections.iter().collect::<Vec<_>>()).unwrap().revision,revision);
    assert_device_restore_label(&store,"later");
    let altered=full_device_backup(Some("altered"));
    assert!(store.lww_commit_replacement_with_device_sections(&header,&stage,Some(&original),&altered.iter().collect::<Vec<_>>()).is_err());
    assert!(store.lww_commit_replacement_units(&header,&stage,Some(&original)).is_err());
    assert_device_restore_label(&store,"later");
}

#[test]
fn full_restore_required_empty_sections_publish_device_deletions_and_clear_local_user_data() {
    let (_dir,mut store)=store();
    store.device_store_mut().unwrap().set_section_participating(device_store::Section::LocalPlugins,true).unwrap();
    write_restore_device_fixture(&mut store,"old"); clear_outbox(&mut store);
    store.device_store().unwrap().write_setting("risu_lastsaved",&serde_json::json!("control")).unwrap();
    store.device_store().unwrap().write_setting("risuNestStartupExclusions",&serde_json::json!(["plugin-script"])).unwrap();
    let sections=full_device_backup(None); let stage=restore_stage(&mut store,"empty");
    let header=Header{binding_authority:0.into(),request_id:"empty-full-device-restore".into()};
    store.lww_commit_replacement_with_device_sections(&header,&stage,None,&sections.iter().collect::<Vec<_>>()).unwrap();
    let device=store.device_store().unwrap();
    assert_eq!(device.hypa_embedding_usage().unwrap().0,0);
    assert!(device.list_plugin_device_storage().unwrap().is_empty());
    assert!(device.read_setting("accountst").unwrap().is_none());
    assert_eq!(device.read_setting("risu_lastsaved").unwrap(),Some(serde_json::json!("control")));
    assert_eq!(device.read_setting("risuNestStartupExclusions").unwrap(),Some(serde_json::json!(["plugin-script"])));
    assert_eq!(device.connection().query_row("SELECT count(*) FROM plugin_permissions",[],|row|row.get::<_,i64>(0)).unwrap(),0);
    let changes=store.lww_read_outbox(0.into(),100).unwrap().entries;
    assert_eq!(changes.iter().filter(|entry|is_device(&entry.key)&&matches!(entry.value,UnitValue::Deleted)).count(),3);
    assert!(changes.iter().all(|entry|entry.stamp==changes[0].stamp));
}

#[test]
fn full_restore_installs_carried_device_rows_as_this_writer_and_removes_left_out_rows() {
    use device_store::plugin_values::PluginDeviceMutation;
    let (_dir,mut store)=store(); write_restore_device_fixture(&mut store,"old");
    store.device_store_mut().unwrap().write_plugin_device_values("orphan",&[PluginDeviceMutation::Set{
        space:"string".into(), key:"local-only".into(), value:"dropped".into(),
    }]).unwrap();
    let sections=full_device_backup(Some("restored")); let stage=restore_stage(&mut store,"restored");
    let header=Header{binding_authority:0.into(),request_id:"carried-device-restore".into()};
    store.lww_commit_replacement_with_device_sections(&header,&stage,None,&sections.iter().collect::<Vec<_>>()).unwrap();
    assert_device_restore_label(&store,"restored");
    let writer=store.lww_clock_state().unwrap().writer_id;
    let device=store.device_store().unwrap();
    assert_eq!(device.read_plugin_device_value("orphan","string","local-only").unwrap(),None);
    let restored:(String,Option<String>)=device.connection().query_row(
        "SELECT writer_id,published_clock FROM plugin_device_storage WHERE owner='orphan' AND space='string' AND key='shared-key'",
        [],|row|Ok((row.get(0)?,row.get(1)?)),
    ).unwrap();
    assert_eq!(restored,(writer,None));
    assert_eq!(device.connection().query_row("SELECT granted FROM plugin_permissions WHERE code_hash='restored' AND permission='synthetic'",[],|row|row.get::<_,i64>(0)).unwrap(),1);
}

#[test]
fn full_restore_rejects_missing_duplicate_and_versioned_device_sections_before_reserving() {
    use risunest_external_storage_format::section::SectionKind;
    let (_dir,mut store)=store(); let stage=restore_stage(&mut store,"invalid");
    let sections=full_device_backup(None); let before=store.lww_clock_state().unwrap().issued;
    let header=Header{binding_authority:0.into(),request_id:"invalid-device-scope".into()};
    assert!(store.lww_commit_replacement_with_device_sections(&header,&stage,None,&sections[..2].iter().collect::<Vec<_>>()).is_err());
    assert!(store.lww_commit_replacement_with_device_sections(&header,&stage,None,&[&sections[0],&sections[0],&sections[2]]).is_err());
    let empty_fingerprint=risunest_external_storage_format::format::FingerprintBuilder::new(&SectionKind::Hypa.fingerprint_domain()).finish();
    let versioned=device_store::sections::SectionSpoolBuilder::new(device_store::Section::Hypa,&std::env::temp_dir()).unwrap().finish(&empty_fingerprint).unwrap();
    assert_eq!(versioned.kind(),SectionKind::Hypa);
    assert!(store.lww_commit_replacement_with_device_sections(&header,&stage,None,&[&versioned,&sections[1],&sections[2]]).is_err());
    assert_eq!(store.lww_clock_state().unwrap().issued,before);
    assert_eq!(store.device_store().unwrap().connection().query_row("SELECT count(*) FROM lww_intents",[],|row|row.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(store.revision().unwrap(),0);
}

#[test]
fn full_restore_recovers_original_inputs_after_library_and_device_transaction_failures() {
    for (phase, scratch_failures) in [(0,0),(1,0),(2,0),(0,1),(1,3),(2,3)] {
        let (dir,mut store)=store();
        store.device_store_mut().unwrap().set_section_participating(device_store::Section::LocalPlugins,true).unwrap();
        write_restore_device_fixture(&mut store,"old"); clear_outbox(&mut store);
        let sections=full_device_backup(Some("frozen")); let stage=restore_stage(&mut store,"frozen");
        let header=Header{binding_authority:0.into(),request_id:format!("crash-device-restore-{phase}")};
        let original=BTreeMap::from([(unit_key(&["future-unit","frozen"]).unwrap(),inline(&serde_json::json!(phase)).unwrap())]);
        if phase==0 { store.connection.execute_batch("CREATE TEMP TRIGGER reject_library BEFORE UPDATE ON generations BEGIN SELECT RAISE(ABORT,'synthetic-library-failure'); END").unwrap(); }
        else { store.device_store().unwrap().connection().execute_batch(if phase==1 {
            "CREATE TEMP TRIGGER reject_device BEFORE INSERT ON device_settings BEGIN SELECT RAISE(ABORT,'synthetic-device-failure'); END"
        } else {
            "CREATE TEMP TRIGGER reject_device_outbox BEFORE INSERT ON lww_outbox BEGIN SELECT RAISE(ABORT,'synthetic-device-outbox-failure'); END"
        }).unwrap(); }
        assert!(store.lww_commit_replacement_with_device_sections(&header,&stage,Some(&original),&sections.iter().collect::<Vec<_>>()).is_err());
        assert_eq!(store.revision().unwrap(),if phase==0 {0}else{1});
        assert_device_restore_label(&store,"old");
        let reserved=store.lww_clock_state().unwrap().issued.unwrap();
        assert_eq!(store.device_store().unwrap().connection().query_row("SELECT complete FROM lww_intents WHERE request_id=?1",[&header.request_id],|row|row.get::<_,bool>(0)).unwrap(),false);
        let frozen_rows: i64=store.device_store().unwrap().connection().query_row("SELECT count(*) FROM lww_intent_rows WHERE request_id=?1",[&header.request_id],|row|row.get(0)).unwrap();
        assert!(frozen_rows>0);
        drop(sections); drop(store);
        let scratch=dir.path().join("external-storage").join("scratch");
        if scratch_failures>0 {
            std::fs::remove_dir(&scratch).unwrap();
            std::fs::write(&scratch,b"synthetic unavailable scratch").unwrap();
            for attempt in 1..=scratch_failures {
                let store=PersistentStore::open(dir.path()).unwrap();
                assert_eq!(store.revision().unwrap(),if phase==0 {0}else{1});
                assert_device_restore_label(&store,"old");
                let device=store.device_store().unwrap().connection();
                let evidence: (i64,bool,bool)=device.query_row(
                    "SELECT f.failures,f.quarantined,i.complete FROM lww_intent_failures f JOIN lww_intents i USING(request_id) WHERE request_id=?1",
                    [&header.request_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
                assert_eq!(evidence,(attempt,attempt==3,false));
                assert_eq!(device.query_row("SELECT count(*) FROM lww_intent_rows WHERE request_id=?1",[&header.request_id],|row|row.get::<_,i64>(0)).unwrap(),frozen_rows);
            }
            std::fs::remove_file(&scratch).unwrap();
        }
        let mut store=PersistentStore::open(dir.path()).unwrap();
        if scratch_failures==3 {
            assert_device_restore_label(&store,"old");
            let selected=store.lww_quarantined_intents().unwrap().into_iter().find(|intent|intent.request_id==header.request_id).unwrap();
            assert!(selected.recoverable && !selected.discardable);
            assert!(store.lww_complete_quarantined_intent(&selected.request_id,&selected.token,store.revision().unwrap()).unwrap());
        }
        assert_eq!(store.revision().unwrap(),1); assert_device_restore_label(&store,"frozen");
        assert_eq!(read_unit(&store.connection,&unit_key(&["future-unit","frozen"]).unwrap()).unwrap().unwrap().1,original.values().next().unwrap().clone());
        assert_eq!(store.lww_clock_state().unwrap().issued,Some(reserved.clone()));
        let out=store.lww_read_outbox(0.into(),100).unwrap().entries;
        assert!(out.iter().all(|entry|entry.stamp==reserved&&entry.version==header.request_id));
        let sections=full_device_backup(Some("frozen"));
        assert_eq!(store.lww_commit_replacement_with_device_sections(&header,&stage,Some(&original),&sections.iter().collect::<Vec<_>>()).unwrap().revision,1);
        assert_eq!(store.device_store().unwrap().connection().query_row("SELECT complete FROM lww_intents WHERE request_id=?1",[&header.request_id],|row|row.get::<_,bool>(0)).unwrap(),true);
        assert!(store.lww_quarantined_intents().unwrap().is_empty());
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(),0);
    }
}

#[test]
fn full_restore_stale_authority_cannot_replay_device_changes_after_library_activation() {
    let (_dir,mut store)=store(); write_restore_device_fixture(&mut store,"old");
    let sections=full_device_backup(Some("frozen")); let stage=restore_stage(&mut store,"frozen");
    let header=Header{binding_authority:0.into(),request_id:"stale-full-device-restore".into()};
    let device=store.device_store().unwrap().connection();
    device.execute_batch("CREATE TEMP TRIGGER reject_device BEFORE INSERT ON device_settings BEGIN SELECT RAISE(ABORT,'synthetic-device-failure'); END").unwrap();
    assert!(store.lww_commit_replacement_with_device_sections(&header,&stage,None,&sections.iter().collect::<Vec<_>>()).is_err());
    let device=store.device_store().unwrap().connection();
    device.execute_batch("DROP TRIGGER reject_device; UPDATE lww_clock SET binding_authority='1'").unwrap();
    assert!(store.lww_recover_intents().is_err()); assert_device_restore_label(&store,"old");
    assert_eq!(store.device_store().unwrap().connection().query_row("SELECT complete FROM lww_intents WHERE request_id=?1",[&header.request_id],|row|row.get::<_,bool>(0)).unwrap(),false);
}

#[test]
fn full_restore_corrupted_frozen_intent_fails_before_replaying_device_rows() {
    let (_dir,mut store)=store(); write_restore_device_fixture(&mut store,"old");
    let sections=full_device_backup(Some("frozen")); let stage=restore_stage(&mut store,"frozen");
    let header=Header{binding_authority:0.into(),request_id:"corrupted-device-restore".into()};
    store.device_store().unwrap().connection().execute_batch("CREATE TEMP TRIGGER reject_device BEFORE INSERT ON device_settings BEGIN SELECT RAISE(ABORT,'synthetic-device-failure'); END").unwrap();
    assert!(store.lww_commit_replacement_with_device_sections(&header,&stage,None,&sections.iter().collect::<Vec<_>>()).is_err());
    let device=store.device_store().unwrap().connection();
    device.execute_batch("DROP TRIGGER reject_device").unwrap();
    let body:String=device.query_row("SELECT body FROM lww_intents WHERE request_id=?1",[&header.request_id],|row|row.get(0)).unwrap();
    let mut intent:Intent=serde_json::from_str(&body).unwrap();
    let Intent::Replacement{device_sections:Some(sections),..}=&mut intent else {panic!()};
    let device_store::sections::SectionValueRow::Hypa{model,..}=&mut sections.to_mut().hypa[0].value else {panic!()};
    *model="corrupted".into();
    device.execute("UPDATE lww_intents SET body=?1 WHERE request_id=?2",params![serde_json::to_string(&intent).unwrap(),header.request_id]).unwrap();
    assert!(store.lww_recover_intents().is_err()); assert_device_restore_label(&store,"old");
    assert_eq!(store.revision().unwrap(),1);
    assert_eq!(store.device_store().unwrap().connection().query_row("SELECT complete FROM lww_intents WHERE request_id=?1",[&header.request_id],|row|row.get::<_,bool>(0)).unwrap(),false);
}

#[test]
fn full_restore_disabled_plugin_local_queues_one_stamp_and_enable_preserves_restored_values() {
    let (_dir,mut store)=store(); write_restore_device_fixture(&mut store,"old");
    let key=unit_key(&["plugin-local","orphan","string","shared-key"]).unwrap();
    let remote=Change{key:key.clone(),stamp:Stamp{physical_ms:(store.lww_clock_state().unwrap().issued.unwrap().physical_ms.0+1).into(),logical:0,writer_id:stamp(1).writer_id},value:inline(&serde_json::json!("received-disabled")).unwrap()};
    receive(&mut store,"disabled-before-restore",vec![remote.clone()],vec![]);
    let device=store.device_store().unwrap().connection();
    let unit_before=read_unit(device,&key).unwrap().unwrap();
    let sections=full_device_backup(Some("restored-off")); let stage=restore_stage(&mut store,"restored-off");
    let header=Header{binding_authority:0.into(),request_id:"off-full-device-restore".into()};
    store.lww_commit_replacement_with_device_sections(&header,&stage,None,&sections.iter().collect::<Vec<_>>()).unwrap();
    assert_device_restore_label(&store,"restored-off");
    let device=store.device_store().unwrap().connection();
    let restored=read_unit(device,&key).unwrap().unwrap();
    assert!(restored.0>unit_before.0);
    assert_eq!(restored.1,inline(&serde_json::json!("restored-off")).unwrap());
    let pending: Vec<(String,String,String)>=device.prepare("SELECT key,stamp,version FROM lww_outbox WHERE json_extract(key,'$[0]')='plugin-local' ORDER BY key").unwrap().query_map([],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap().collect::<Result<_,_>>().unwrap();
    assert_eq!(pending.len(),2);
    assert!(pending.iter().all(|(_,stamp,version)|*stamp==serde_json::to_string(&restored.0).unwrap()&&*version==header.request_id));
    assert!(store.lww_read_outbox(0.into(),100).unwrap().entries.iter().all(|entry|entry.key.components()[0]!="plugin-local"));
    store.device_store_mut().unwrap().set_section_participating(device_store::Section::LocalPlugins,true).unwrap();
    assert_device_restore_label(&store,"restored-off");
    assert_eq!(read_unit(store.device_store().unwrap().connection(),&key).unwrap().unwrap(),restored);
    assert_eq!(store.lww_read_outbox(0.into(),100).unwrap().entries.iter().filter(|entry|entry.key.components()[0]=="plugin-local"&&entry.stamp==restored.0&&entry.version==header.request_id).count(),2);
    receive(&mut store,"later-remote-plugin",vec![Change{key:key.clone(),stamp:Stamp{physical_ms:(restored.0.physical_ms.0+1).into(),logical:0,writer_id:remote.stamp.writer_id},value:inline(&serde_json::json!("later-remote")).unwrap()}],vec![]);
    assert_eq!(store.device_store().unwrap().connection().query_row("SELECT value FROM plugin_device_storage WHERE owner='orphan' AND space='string' AND key='shared-key'",[],|row|row.get::<_,String>(0)).unwrap(),"later-remote");
}

#[test]
fn full_restore_unchanged_device_values_keep_original_stamps_and_emit_no_device_entries() {
    let (_dir,mut store)=store(); write_restore_device_fixture(&mut store,"same"); clear_outbox(&mut store);
    let keys=[unit_key(&["hypa",&"a".repeat(64)]).unwrap(),unit_key(&["plugin-local","orphan","string","shared-key"]).unwrap()];
    let before:Vec<_>=keys.iter().map(|key|read_unit(store.device_store().unwrap().connection(),key).unwrap()).collect();
    let sections=full_device_backup(Some("same")); let stage=restore_stage(&mut store,"device-no-op");
    let header=Header{binding_authority:0.into(),request_id:"same-full-device-restore".into()};
    store.lww_commit_replacement_with_device_sections(&header,&stage,None,&sections.iter().collect::<Vec<_>>()).unwrap();
    let after:Vec<_>=keys.iter().map(|key|read_unit(store.device_store().unwrap().connection(),key).unwrap()).collect();
    assert_eq!(before,after);
    assert_eq!(store.device_store().unwrap().connection().query_row("SELECT count(*) FROM lww_outbox WHERE version=?1",[&header.request_id],|row|row.get::<_,i64>(0)).unwrap(),0);
    assert_device_restore_label(&store,"same");
}

#[test]
fn full_restore_disabled_receive_without_later_local_edit_reprojects_on_enable() {
    let (_dir,mut store)=store();
    let remote=change(&["plugin-local","orphan","string","received"],1,serde_json::json!("remote-only"));
    receive(&mut store,"disabled-no-local-edit",vec![remote.clone()],vec![]);
    assert!(store.device_store().unwrap().list_plugin_device_storage().unwrap().is_empty());
    let clock=store.lww_clock_state().unwrap().issued;
    store.device_store_mut().unwrap().set_section_participating(device_store::Section::LocalPlugins,true).unwrap();
    assert_eq!(store.lww_clock_state().unwrap().issued,clock);
    assert_eq!(read_unit(store.device_store().unwrap().connection(),&remote.key).unwrap().unwrap(),(remote.stamp,remote.value));
    assert_eq!(store.device_store().unwrap().connection().query_row("SELECT value FROM plugin_device_storage WHERE owner='orphan' AND key='received'",[],|row|row.get::<_,String>(0)).unwrap(),"remote-only");
    assert!(store.lww_read_outbox(0.into(),100).unwrap().entries.is_empty());
}

#[test]
fn backup_capture_pins_both_stores_before_releasing_concurrent_reservation_and_device_writes() {
    use std::{sync::mpsc, time::Duration};
    let (dir,mut store)=store();
    save(&mut store,vec![mutation(&["root","language"],serde_json::json!("before"))]);
    write_restore_device_fixture(&mut store,"before");
    let revision=store.revision().unwrap();
    let mut writer=PersistentStore::open(dir.path()).unwrap();
    writer.device_store().unwrap().connection().busy_timeout(Duration::from_millis(20)).unwrap();
    let (start_tx,start_rx)=mpsc::channel(); let (blocked_tx,blocked_rx)=mpsc::channel();
    let (release_tx,release_rx)=mpsc::channel(); let (done_tx,done_rx)=mpsc::channel();
    let worker=std::thread::spawn(move || {
        start_rx.recv().unwrap();
        let attempted=writer.commit(&WorkingSetCommit{expected_revision:revision,unit_mutations:Some(vec![mutation(&["root","language"],serde_json::json!("blocked"))]),..Default::default()});
        let device_attempt=writer.device_store().unwrap().write_setting("accountst",&serde_json::json!("blocked"));
        blocked_tx.send((attempted.is_err(),device_attempt.is_err())).unwrap();
        release_rx.recv().unwrap();
        writer.device_store().unwrap().connection().busy_timeout(Duration::from_secs(5)).unwrap();
        save(&mut writer,vec![mutation(&["root","language"],serde_json::json!("after"))]);
        write_restore_device_fixture(&mut writer,"after");
        done_tx.send(()).unwrap();
    });
    BACKUP_CAPTURE_PINNED.with(|hook| *hook.borrow_mut()=Some(Box::new(move || {
        start_tx.send(()).unwrap();
        assert_eq!(blocked_rx.recv_timeout(Duration::from_secs(5)).unwrap(),(true,true));
    })));
    BACKUP_CAPTURE_RELEASED.with(|hook| *hook.borrow_mut()=Some(Box::new(move || {
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    })));
    let (lease,sections)=store.lww_acquire_backup_capture(revision).unwrap(); worker.join().unwrap();
    assert_eq!(store.read_root(Some(&lease.lease)).unwrap().value["language"],"before");
    assert_eq!(read_unit(&store.revision_leases[&lease.lease].connection,&unit_key(&["root","language"]).unwrap()).unwrap().unwrap().1,inline(&serde_json::json!("before")).unwrap());
    assert_eq!(store.read_root(None).unwrap().value["language"],"after");
    assert_device_restore_label(&store,"after");
    let frozen=device_store::sections::freeze_backup_sections(&sections.iter().collect::<Vec<_>>()).unwrap();
    let device_store::sections::SectionValueRow::Hypa{model,..}=&frozen.hypa[0].value else {panic!()};
    assert_eq!(model,"before");
    assert!(frozen.local_plugins.iter().any(|row| matches!(&row.value,device_store::sections::SectionValueRow::Plugin{space,value} if space=="string"&&value=="before")));
    let (_target_dir,mut target)=self::store(); let stage=restore_stage(&mut target,"before");
    target.lww_commit_replacement_with_device_sections(&Header{binding_authority:0.into(),request_id:"capture-boundary-restore".into()},&stage,None,&sections.iter().collect::<Vec<_>>()).unwrap();
    assert_device_restore_label(&target,"before");
    store.release_revision(&lease.lease).unwrap(); assert!(store.revision_leases.is_empty());
}

#[test]
fn backup_capture_spools_device_sections_in_app_scratch_and_removes_them_with_the_capture() {
    let (dir,mut store)=store(); write_restore_device_fixture(&mut store,"scratch");
    let scratch=dir.path().join("external-storage").join("scratch");
    let spools=|| std::fs::read_dir(&scratch).unwrap().filter(|entry| entry.as_ref().unwrap().file_name().to_string_lossy().starts_with("section-spool-")).count();
    let (lease,sections)=store.lww_acquire_backup_capture(store.revision().unwrap()).unwrap();
    assert_eq!(sections.len(),3); assert_eq!(spools(),3);
    drop(sections); assert_eq!(spools(),0);
    store.release_revision(&lease.lease).unwrap();
}

#[test]
fn backup_capture_busy_and_revision_failure_release_barrier_without_issuing_stamp() {
    let (_dir,mut store)=store();
    let issued=store.lww_clock_state().unwrap().issued;
    assert!(matches!(store.lww_acquire_backup_capture(1),Err(StoreError::RevisionConflict{..})));
    assert!(store.revision_leases.is_empty());
    assert_eq!(store.lww_clock_state().unwrap().issued,issued);
    store.device_store().unwrap().write_setting("accountst",&serde_json::json!("after-failure")).unwrap();
    let header=Header{binding_authority:0.into(),request_id:"pending-capture-intent".into()};
    store.reserve_intent(&header,&Intent::Commit{commit:WorkingSetCommit{expected_revision:0,unit_mutations:Some(vec![mutation(&["root","language"],serde_json::json!("pending"))]),..Default::default()},aliases:vec![]}).unwrap();
    let issued=store.lww_clock_state().unwrap().issued;
    assert!(matches!(store.lww_acquire_backup_capture(0),Err(StoreError::CommitBusy)));
    assert_eq!(store.revision().unwrap(),0); assert!(store.revision_leases.is_empty());
    assert_eq!(store.lww_clock_state().unwrap().issued,issued);
    store.lww_recover_intents().unwrap();
    let (lease,sections)=store.lww_acquire_backup_capture(1).unwrap();
    assert_eq!(sections.len(),3); assert_eq!(store.read_root(Some(&lease.lease)).unwrap().value["language"],"pending");
    store.release_revision(&lease.lease).unwrap();
}

#[test]
fn backup_capture_spool_failure_releases_library_lease_and_device_barrier() {
    let (dir,mut store)=store();
    store.device_store().unwrap().connection().execute_batch("INSERT INTO hypa_embeddings VALUES('invalid-key','synthetic','model',NULL,1,1,x'00000000',NULL,0,'0','',NULL,NULL,NULL)").unwrap();
    let clock=store.lww_clock_state().unwrap().issued;
    assert!(store.lww_acquire_backup_capture(0).is_err());
    assert!(store.revision_leases.is_empty()); assert_eq!(store.active_readers.active_count(),0);
    assert_eq!(store.lww_clock_state().unwrap().issued,clock);
    let mut writer=PersistentStore::open(dir.path()).unwrap();
    writer.device_store().unwrap().connection().busy_timeout(std::time::Duration::from_millis(20)).unwrap();
    writer.device_store().unwrap().write_setting("accountst",&serde_json::json!("released")).unwrap();
    save(&mut writer,vec![mutation(&["root","language"],serde_json::json!("released"))]);
    assert_eq!(store.revision().unwrap(),1);
}

#[test]
fn backup_capture_rejects_active_staged_and_applied_unfinished_receive_but_ignores_detached_rows() {
    let (_dir,mut store)=store();
    let header=Header{binding_authority:0.into(),request_id:"capture-pending-receive".into()};
    store.lww_stage_receive(&StageReceive{header:header.clone(),changes:vec![change(&["root","language"],1,serde_json::json!("received"))],progress:Progress{kind:"server".into(),cursor:1.into(),writer_id:None},admitted_time_upper_ms:2.into()}).unwrap();
    assert!(matches!(store.lww_acquire_backup_capture(0),Err(StoreError::CommitBusy)));
    store.lww_apply_receive(&ApplyReceive{header:header.clone(),generating:vec![]}).unwrap();
    assert!(matches!(store.lww_acquire_backup_capture(store.revision().unwrap()),Err(StoreError::CommitBusy)));
    store.lww_finish_receive(&header).unwrap();
    let (lease,sections)=store.lww_acquire_backup_capture(store.revision().unwrap()).unwrap();
    assert_eq!(sections.len(),3); store.release_revision(&lease.lease).unwrap();
    store.device_store().unwrap().connection().execute("UPDATE lww_receive SET finished=0,authority='999' WHERE request_id=?1",[&header.request_id]).unwrap();
    let (lease,_)=store.lww_acquire_backup_capture(store.revision().unwrap()).unwrap();
    assert_eq!(store.lww_binding_authority().unwrap(),0.into()); store.release_revision(&lease.lease).unwrap();
}

#[test]
fn device_replacement_receipt_rejects_false_markers_incomplete_and_altered_identity_without_writes() {
    let (_dir,mut store)=store();
    let header=Header{binding_authority:0.into(),request_id:"exact-device-receipt".into()};
    let sections=full_device_backup(Some("receipt")); let stage=restore_stage(&mut store,"receipt");
    store.connection.execute("INSERT INTO app_kv VALUES('device-backup-commit',?1)",[&header.request_id]).unwrap();
    assert!(store.lww_device_replacement_receipt(&header,&stage).unwrap().is_none());
    store.device_store().unwrap().connection().execute_batch("CREATE TEMP TRIGGER reject_device BEFORE INSERT ON device_settings BEGIN SELECT RAISE(ABORT,'synthetic-device-failure'); END").unwrap();
    assert!(store.lww_commit_replacement_with_device_sections(&header,&stage,None,&sections.iter().collect::<Vec<_>>()).is_err());
    assert_eq!(store.revision().unwrap(),1);
    assert!(store.lww_device_replacement_receipt(&header,&stage).unwrap().is_none());
    assert!(store.lww_device_replacement_receipt(&header,"other-stage").is_err());
    assert!(store.lww_device_replacement_receipt(&Header{binding_authority:1.into(),..header.clone()},&stage).is_err());
    let clock=store.lww_clock_state().unwrap().issued;
    store.device_store().unwrap().connection().execute_batch("DROP TRIGGER reject_device").unwrap();
    assert!(store.lww_device_replacement_receipt(&header,&stage).unwrap().is_none());
    assert_eq!(store.lww_clock_state().unwrap().issued,clock);
    store.lww_recover_intents().unwrap();
    assert_eq!(store.lww_device_replacement_receipt(&header,&stage).unwrap().unwrap().revision,1);
    let library=Connection::open_with_flags(&store.database_path,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let device_path:String=store.device_store().unwrap().connection().query_row("SELECT file FROM pragma_database_list WHERE name='main'",[],|row|row.get(0)).unwrap();
    let device=Connection::open_with_flags(device_path,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(completed_device_replacement_receipt(&library,&device,&header,&stage).unwrap().unwrap().revision,1);
    store.connection.execute("UPDATE lww_requests SET digest='corrupted' WHERE request_id=?1",[&header.request_id]).unwrap();
    assert!(store.lww_device_replacement_receipt(&header,&stage).is_err());
    assert_eq!(store.lww_clock_state().unwrap().issued,clock);
}

#[test]
fn device_replacement_receipt_cannot_attest_a_library_only_replacement() {
    let (_dir,mut store)=store(); let stage=restore_stage(&mut store,"library-only");
    let header=Header{binding_authority:0.into(),request_id:"not-full-device-receipt".into()};
    store.lww_commit_replacement(&header,&stage).unwrap();
    assert!(store.lww_device_replacement_receipt(&header,&stage).is_err());
}

#[test]
fn backup_device_revision_is_fixed_after_own_and_second_connection_mutations() {
    use device_store::plugin_values::PluginDeviceMutation;
    fn write(store:&mut PersistentStore,value:&str) {
        store.device_store_mut().unwrap().write_plugin_device_values("synthetic",&[
            PluginDeviceMutation::Set{space:"string".into(),key:"revision-proof".into(),value:value.into()},
        ]).unwrap();
    }
    let (dir,mut primary)=store();
    write(&mut primary,"before");
    let mut writer=PersistentStore::open(dir.path()).unwrap();
    let revision=primary.device_store().unwrap().revision().unwrap();
    let (lease,_)=primary.lww_acquire_backup_capture(primary.revision().unwrap()).unwrap();
    assert_eq!(primary.lww_backup_device_revision(&lease.lease).unwrap(),revision);
    write(&mut primary,"own-after");
    assert!(primary.device_store().unwrap().revision().unwrap()>revision);
    assert_eq!(primary.lww_backup_device_revision(&lease.lease).unwrap(),revision);
    write(&mut writer,"second-after");
    assert!(writer.device_store().unwrap().revision().unwrap()>revision+1);
    assert_eq!(primary.lww_backup_device_revision(&lease.lease).unwrap(),revision);
    primary.release_revision(&lease.lease).unwrap();
}

#[test]
fn backup_device_revision_rejects_absent_released_unpinned_foreign_and_changed_identity_leases() {
    let (_dir,mut primary)=store();
    assert!(matches!(primary.lww_backup_device_revision("absent"),Err(StoreError::SnapshotReleased)));
    let ordinary=primary.acquire_revision(0).unwrap();
    assert!(matches!(primary.lww_backup_device_revision(&ordinary.lease),Err(StoreError::Validation{message}) if message=="backup-capture-lease-required"));
    primary.release_revision(&ordinary.lease).unwrap();
    let (_foreign,mut foreign)=store();
    let (foreign_lease,_)=foreign.lww_acquire_backup_capture(0).unwrap();
    assert!(matches!(primary.lww_backup_device_revision(&foreign_lease.lease),Err(StoreError::SnapshotReleased)));
    foreign.release_revision(&foreign_lease.lease).unwrap();
    let (lease,_)=primary.lww_acquire_backup_capture(0).unwrap();
    let generation=primary.revision_leases[&lease.lease].target.generation.clone();
    primary.revision_leases.get_mut(&lease.lease).unwrap().target.generation="foreign-generation".into();
    assert!(matches!(primary.lww_backup_device_revision(&lease.lease),Err(StoreError::Validation{message}) if message=="backup-lease-identity-changed"));
    primary.revision_leases.get_mut(&lease.lease).unwrap().target.generation=generation;
    primary.revision_leases.get_mut(&lease.lease).unwrap().target.revision=1;
    assert!(matches!(primary.lww_backup_device_revision(&lease.lease),Err(StoreError::Validation{message}) if message=="backup-lease-identity-changed"));
    primary.revision_leases.get_mut(&lease.lease).unwrap().target.revision=0;
    assert_eq!(primary.lww_backup_device_revision(&lease.lease).unwrap(),0);
    primary.release_revision(&lease.lease).unwrap();
    assert!(matches!(primary.lww_backup_device_revision(&lease.lease),Err(StoreError::SnapshotReleased)));
}

#[test]
fn backup_unit_values_are_lease_fixed_and_include_opaque_held_and_permanent_deletions() {
    let (_dir,mut store)=store();
    save(&mut store,vec![mutation(&["root","language"],serde_json::json!("before")),mutation(&["exists","character","retired"],serde_json::json!({"type":"character"})),mutation(&["character","retired","name"],serde_json::json!("Retired"))]);
    store.commit(&WorkingSetCommit{expected_revision:store.revision().unwrap(),unit_mutations:Some(vec![UnitMutation::Delete{key:unit_key(&["exists","character","retired"]).unwrap()}]),..Default::default()}).unwrap();
    let retired_child = unit_key(&["character","retired","name"]).unwrap();
    // Older stores can still carry a child of an already retired record.
    store.connection.execute("INSERT OR REPLACE INTO lww_units(key,stamp,value,version,identity) SELECT ?1,stamp,value,version,identity FROM lww_units WHERE key=?2",
        params![retired_child.as_str(), unit_key(&["root","language"]).unwrap().as_str()]).unwrap();
    let opaque=change(&["future-unit","opaque-backup"],1,serde_json::json!({"future":3}));
    let held=change(&["character","missing-parent","name"],1,serde_json::json!("held"));
    receive(&mut store,"backup-opaque-held",vec![opaque.clone(),held.clone()],vec![]);
    let revision=store.revision().unwrap(); let (lease,sections)=store.lww_acquire_backup_capture(revision).unwrap();
    assert_eq!(sections.len(),3);
    let before=store.lww_backup_unit_values(&lease.lease).unwrap();
    assert_eq!(before[&opaque.key],opaque.value);
    assert_eq!(before[&held.key],held.value);
    assert_eq!(before[&unit_key(&["exists","character","retired"]).unwrap()],UnitValue::Deleted);
    assert!(!before.contains_key(&retired_child));
    assert!(before.keys().all(|key|!is_device(key)));
    save(&mut store,vec![mutation(&["root","language"],serde_json::json!("after"))]);
    assert_eq!(store.lww_backup_unit_values(&lease.lease).unwrap(),before);
    assert_eq!(json_value(&before[&unit_key(&["root","language"]).unwrap()]).unwrap().unwrap(),"before");
    assert!(matches!(store.lww_backup_unit_values("absent-lease"),Err(StoreError::SnapshotReleased)));
    store.release_revision(&lease.lease).unwrap();
    assert!(matches!(store.lww_backup_unit_values(&lease.lease),Err(StoreError::SnapshotReleased)));
}

fn incoming_message_value(target: &PersistentStore, character: &str, conversation: &str, text: &str) -> UnitValue {
    let (_dir,mut source)=store(); create_conversation(&mut source,character,conversation);
    source.commit(&WorkingSetCommit{expected_revision:source.revision().unwrap(),conversations:Some(vec![super::super::ConversationMutation::ReplaceRange{character_id:character.into(),conversation_id:conversation.into(),start:0,delete_count:0,messages:vec![serde_json::json!({"data":text,"chatId":"synthetic-message"})],conversation:None,configured_index:None}]),..Default::default()}).unwrap();
    copy_objects(&source,target);
    read_unit(&source.connection,&unit_key(&["messages",character,conversation]).unwrap()).unwrap().unwrap().1
}

#[test]
fn library_backup_capture_pins_finished_held_and_deferred_proofs_without_device_spools() {
    let (_dir,mut store)=store(); create_conversation(&mut store,"char","chat");
    let time=store.lww_clock_state().unwrap().issued.unwrap().physical_ms.0+1;
    let message=Change{key:unit_key(&["messages","char","chat"]).unwrap(),stamp:stamp(time),value:incoming_message_value(&store,"char","chat","library-only-deferred")};
    let held=change(&["character","missing","name"],time,serde_json::json!("library-only-held"));
    let header=Header{binding_authority:store.lww_binding_authority().unwrap(),request_id:"library-only-capture".into()};
    store.lww_stage_receive(&StageReceive{header:header.clone(),changes:vec![message.clone(),held.clone()],progress:Progress{kind:"server".into(),cursor:1.into(),writer_id:None},admitted_time_upper_ms:u64::MAX.into()}).unwrap();
    assert!(matches!(store.lww_acquire_library_backup_capture(store.revision().unwrap()),Err(StoreError::CommitBusy)));
    let applied=store.lww_apply_receive(&ApplyReceive{header:header.clone(),generating:vec![MessageLocator{character_id:"char".into(),conversation_id:"chat".into(),start:None}]}).unwrap();
    assert_eq!(applied.held_keys,vec![held.key.clone()]);
    assert_eq!(applied.deferred_keys,vec![message.key.clone()]);
    assert!(matches!(store.lww_acquire_library_backup_capture(store.revision().unwrap()),Err(StoreError::CommitBusy)));
    assert!(store.revision_leases.is_empty());
    store.lww_finish_receive(&header).unwrap();
    store.device_store().unwrap().connection().execute_batch("INSERT INTO hypa_embeddings VALUES('invalid-key','synthetic','model',NULL,1,1,x'00000000',NULL,0,'0','',NULL,NULL,NULL)").unwrap();
    assert!(store.lww_acquire_backup_capture(store.revision().unwrap()).is_err());
    assert!(store.revision_leases.is_empty());
    let lease=store.lww_acquire_library_backup_capture(store.revision().unwrap()).unwrap();
    assert!(store.revision_leases[&lease.lease].connection.is_readonly("backup_device").unwrap());
    let units=store.lww_backup_unit_values(&lease.lease).unwrap();
    assert_eq!(units[&held.key],held.value);
    assert_eq!(units[&message.key],message.value);
    assert!(units.keys().all(|key|!is_device(key)));
    store.device_store().unwrap().connection().execute("UPDATE lww_receive SET finished=0,digest='changed-after-pin' WHERE request_id=?1",[&header.request_id]).unwrap();
    assert_eq!(store.lww_backup_unit_values(&lease.lease).unwrap(),units);
    assert!(matches!(store.lww_acquire_library_backup_capture(store.revision().unwrap()),Err(StoreError::CommitBusy)));
    store.release_revision(&lease.lease).unwrap();
    assert!(store.revision_leases.is_empty());
    assert_eq!(store.active_readers.active_count(),0);
    assert!(matches!(store.lww_backup_unit_values(&lease.lease),Err(StoreError::SnapshotReleased)));
}

#[test]
fn backup_held_messages_restore_with_one_stamp_and_project_after_reopen_and_later_parent() {
    let (dir,mut store)=store();
    let message=Change{key:unit_key(&["messages","waiting","chat"]).unwrap(),stamp:stamp(2),value:incoming_message_value(&store,"waiting","chat","held-backup-message")};
    let conversation=change(&["exists","conversation","waiting","chat"],2,serde_json::json!(true));
    let name=change(&["character","waiting","name"],2,serde_json::json!("held-name"));
    let opaque=change(&["future","restore-opaque"],2,serde_json::json!({"opaque":true}));
    let applied=receive(&mut store,"held-backup-source",vec![conversation.clone(),message.clone(),name.clone(),opaque.clone()],vec![]);
    assert_eq!(applied.held_keys.len(),3);
    let (lease,sections)=store.lww_acquire_backup_capture(store.revision().unwrap()).unwrap();
    let units=store.lww_backup_unit_values(&lease.lease).unwrap();
    for change in [&conversation,&message,&name,&opaque] { assert_eq!(units[&change.key],change.value); }
    store.release_revision(&lease.lease).unwrap();
    let stage=restore_stage(&mut store,"held-restore");
    let header=Header{binding_authority:0.into(),request_id:"held-local-restore".into()};
    store.lww_commit_replacement_with_device_sections(&header,&stage,Some(&units),&sections.iter().collect::<Vec<_>>()).unwrap();
    let restored=read_unit(&store.connection,&message.key).unwrap().unwrap();
    assert!(restored.0>message.stamp); assert_eq!(restored.1,message.value);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_receive_rows WHERE request_id=?1 AND status='held'",[&header.request_id],|row|row.get::<_,i64>(0)).unwrap(),3);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM characters WHERE generation=?1",[active_generation(&store.connection).unwrap()],|row|row.get::<_,i64>(0)).unwrap(),0);
    let progress=serde_json::to_value(store.lww_receive_progress(0.into()).unwrap()).unwrap();
    drop(store); let mut store=PersistentStore::open(dir.path()).unwrap();
    let applied=receive(&mut store,"later-parent",vec![change(&["exists","character","waiting"],3,serde_json::json!({"type":"character"}))],vec![]);
    assert!(applied.affected_keys.contains(&message.key));
    let database=query::materialize_target(&store.connection,&super::super::ReadTarget{generation:active_generation(&store.connection).unwrap(),revision:store.revision().unwrap()}).unwrap();
    assert_eq!(database["characters"][0]["name"],"held-name");
    assert_eq!(database["characters"][0]["chats"][0]["message"][0]["data"],"held-backup-message");
    assert_eq!(read_unit(&store.connection,&message.key).unwrap().unwrap(),restored);
    assert_eq!(store.connection.query_row("SELECT version FROM lww_units WHERE key=?1",[message.key.as_str()],|row|row.get::<_,String>(0)).unwrap(),header.request_id);
    let pending=store.lww_read_outbox(0.into(),100).unwrap().entries;
    assert!(pending.iter().any(|entry|entry.key==message.key&&entry.stamp==restored.0&&entry.version==header.request_id));
    assert_eq!(serde_json::to_value(store.lww_receive_progress(0.into()).unwrap()).unwrap(),progress);
    assert_eq!(read_unit(&store.connection,&opaque.key).unwrap().unwrap().1,opaque.value);
    receive(&mut store,"retire-restored-parent",vec![Change{key:unit_key(&["exists","character","waiting"]).unwrap(),stamp:stamp(4),value:UnitValue::Deleted}],vec![]);
    receive(&mut store,"retired-parent-arrives-again",vec![change(&["exists","character","waiting"],5,serde_json::json!(true)),Change{stamp:stamp(restored.0.physical_ms.0+1),..message.clone()}],vec![]);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM characters WHERE generation=?1",[active_generation(&store.connection).unwrap()],|row|row.get::<_,i64>(0)).unwrap(),0);
    assert!(read_unit(&store.connection,&message.key).unwrap().is_none());
    assert_eq!(parent_status(&store.connection,&message.key).unwrap(),"retired");
    assert!(store.lww_read_outbox(0.into(),100).unwrap().entries.iter().all(|entry|entry.key!=message.key));
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_receive_rows WHERE key=?1 AND status<>'done'",[message.key.as_str()],|row|row.get::<_,i64>(0)).unwrap(),0);
}

#[test]
fn backup_deferred_winner_projects_at_restore_and_requires_verified_message_controls() {
    let (_dir,mut store)=store(); create_conversation(&mut store,"char","chat");
    let message=Change{key:unit_key(&["messages","char","chat"]).unwrap(),stamp:stamp(store.lww_clock_state().unwrap().issued.unwrap().physical_ms.0+1),value:incoming_message_value(&store,"char","chat","deferred-backup-message")};
    receive(&mut store,"deferred-backup-source",vec![message.clone()],vec![MessageLocator{character_id:"char".into(),conversation_id:"chat".into(),start:None}]);
    let (lease,sections)=store.lww_acquire_backup_capture(store.revision().unwrap()).unwrap();
    let units=store.lww_backup_unit_values(&lease.lease).unwrap(); assert_eq!(units[&message.key],message.value);
    let reader=&store.revision_leases[&lease.lease];
    let mut database=query::materialize_target(&reader.connection,&reader.target).unwrap();
    assert!(database["characters"][0]["chats"][0]["message"].as_array().unwrap().is_empty());
    let characters=database.as_object_mut().unwrap().remove("characters").unwrap(); let presets=database.as_object_mut().unwrap().remove("botPresets").unwrap();
    store.release_revision(&lease.lease).unwrap();
    let stage=store.replace_begin().unwrap().staging_id;
    store.replace_put_root(&stage,&database).unwrap(); store.replace_put_presets(&stage,presets.as_array().unwrap()).unwrap(); store.replace_add_characters(&stage,characters.as_array().unwrap()).unwrap();
    let UnitValue::Object{descriptor,..}=&message.value else {panic!()};
    let hash=&descriptor.dependencies[0]; let body=store.lww_object_body(hash).unwrap().unwrap();
    store.connection.execute("DELETE FROM message_page_objects WHERE hash=?1",[hash]).unwrap();
    let clock=store.lww_clock_state().unwrap().issued; let revision=store.revision().unwrap();
    let header=Header{binding_authority:0.into(),request_id:"deferred-local-restore".into()};
    assert!(store.lww_commit_replacement_with_device_sections(&header,&stage,Some(&units),&sections.iter().collect::<Vec<_>>()).is_err());
    assert_eq!(store.revision().unwrap(),revision); assert_eq!(store.lww_clock_state().unwrap().issued,clock);
    store.lww_put_object(hash,&body).unwrap();
    store.lww_commit_replacement_with_device_sections(&header,&stage,Some(&units),&sections.iter().collect::<Vec<_>>()).unwrap();
    let restored=read_unit(&store.connection,&message.key).unwrap().unwrap();
    assert!(restored.0>message.stamp); assert_eq!(restored.1,message.value);
    let database=query::materialize_target(&store.connection,&super::super::ReadTarget{generation:active_generation(&store.connection).unwrap(),revision:store.revision().unwrap()}).unwrap();
    assert_eq!(database["characters"][0]["chats"][0]["message"][0]["data"],"deferred-backup-message");
    store.lww_drain_deferred(&ApplyReceive{header:Header{binding_authority:0.into(),request_id:"drain-restored-deferred".into()},generating:vec![]}).unwrap();
    assert_eq!(read_unit(&store.connection,&message.key).unwrap().unwrap(),restored);
    assert!(store.lww_read_outbox(0.into(),100).unwrap().entries.iter().any(|entry|entry.key==message.key&&entry.stamp==restored.0&&entry.version==header.request_id));
}

#[test]
fn backup_held_proofs_are_pinned_and_reject_equal_stamp_conflicts_or_changed_receipts() {
    let (_dir,mut store)=store();
    let held=change(&["character","missing","name"],2,serde_json::json!("before"));
    receive(&mut store,"held-proof-before",vec![held.clone()],vec![]);
    let ordinary=store.acquire_revision(store.revision().unwrap()).unwrap();
    assert!(store.lww_backup_unit_values(&ordinary.lease).is_err()); store.release_revision(&ordinary.lease).unwrap();
    let (lease,_)=store.lww_acquire_backup_capture(store.revision().unwrap()).unwrap();
    assert!(store.revision_leases[&lease.lease].connection.is_readonly("backup_device").unwrap());
    store.device_store().unwrap().connection().execute("UPDATE lww_receive SET authority='999',digest='invalid' WHERE request_id='held-proof-before'",[]).unwrap();
    assert_eq!(store.lww_backup_unit_values(&lease.lease).unwrap()[&held.key],held.value);
    store.release_revision(&lease.lease).unwrap();
    let (lease,_)=store.lww_acquire_backup_capture(store.revision().unwrap()).unwrap();
    assert!(!store.lww_backup_unit_values(&lease.lease).unwrap().contains_key(&held.key)); store.release_revision(&lease.lease).unwrap();
    receive(&mut store,"held-proof-one",vec![held.clone()],vec![]);
    receive(&mut store,"held-proof-conflict",vec![change(&["character","missing","name"],2,serde_json::json!("conflicting"))],vec![]);
    let (lease,_)=store.lww_acquire_backup_capture(store.revision().unwrap()).unwrap();
    assert!(store.lww_backup_unit_values(&lease.lease).is_err()); store.release_revision(&lease.lease).unwrap();
    store.device_store().unwrap().connection().execute("UPDATE lww_receive SET authority='999' WHERE request_id='held-proof-conflict'",[]).unwrap();
    store.device_store().unwrap().connection().execute("UPDATE lww_receive SET digest='invalid' WHERE request_id='held-proof-one'",[]).unwrap();
    let (lease,_)=store.lww_acquire_backup_capture(store.revision().unwrap()).unwrap();
    assert!(store.lww_backup_unit_values(&lease.lease).is_err()); store.release_revision(&lease.lease).unwrap();
    assert!(store.revision_leases.is_empty()); assert_eq!(store.active_readers.active_count(),0);
}

#[test]
fn backup_device_attachment_is_readonly_with_readwrite_revision_reader_flags() {
    let (_dir,store)=store();
    let library=Connection::open_with_flags(&store.database_path,super::super::snapshot::revision_reader_open_flags_for_target(true)).unwrap();
    library.execute_batch("PRAGMA query_only=ON; BEGIN").unwrap();
    let path:String=store.device_store().unwrap().connection().query_row("SELECT file FROM pragma_database_list WHERE name='main'",[],|row|row.get(0)).unwrap();
    pin_backup_device_snapshot(&library,&path).unwrap();
    assert!(library.is_readonly("backup_device").unwrap());
    assert!(library.execute("INSERT INTO backup_device.device_settings VALUES('accountst','null')",[]).is_err());
    library.execute_batch("ROLLBACK; DETACH DATABASE backup_device").unwrap();
    assert!(!library.query_row("SELECT EXISTS(SELECT 1 FROM pragma_database_list WHERE name='backup_device')",[],|row|row.get::<_,bool>(0)).unwrap());
}

#[test]
fn backup_archived_parent_holds_restore_until_explicit_unarchive_without_child_restamping() {
    use crate::logical_records::{decode_logical_record,encode_logical_record_key,LogicalRecordLocator};
    let (dir,mut store)=store(); create_conversation(&mut store,"char","chat");
    save(&mut store,vec![mutation(&["character","char","name"],serde_json::json!("before"))]);
    store.archive_character("char",store.revision().unwrap(),10).unwrap();
    let name=change(&["character","char","name"],store.lww_clock_state().unwrap().issued.unwrap().physical_ms.0+1,serde_json::json!("held-archive-name"));
    assert_eq!(receive(&mut store,"archive-backup-held",vec![name.clone()],vec![]).held_keys,vec![name.key.clone()]);
    let (lease,sections)=store.lww_acquire_backup_capture(store.revision().unwrap()).unwrap();
    let units=store.lww_backup_unit_values(&lease.lease).unwrap(); assert_eq!(units[&name.key],name.value);
    let reader=&store.revision_leases[&lease.lease];
    let locator=LogicalRecordLocator::Character{character_id:"char".into()}; let key=encode_logical_record_key(&locator).unwrap();
    let cas=crate::asset_repository::PayloadCas::new(&store.repository_root).unwrap();
    let encoded=super::super::record_projection::reconstruct_record_with_owner_objects(&reader.connection,&cas,&reader.target.generation,&key,vec![],|_|Ok(()),&|hash|Ok(cas.stat_object(hash)?.unwrap())).unwrap();
    let envelope=decode_logical_record(&encoded).unwrap(); let root=store.read_root(Some(&lease.lease)).unwrap().value;
    store.release_revision(&lease.lease).unwrap();
    store.restore_character("char",store.revision().unwrap()).unwrap();
    save(&mut store,vec![mutation(&["character","char","name"],serde_json::json!("later-local"))]);
    let stage=store.replace_begin().unwrap().staging_id; store.replace_put_root(&stage,&root).unwrap(); store.replace_put_presets(&stage,&[]).unwrap();
    let tx=store.connection.transaction().unwrap(); super::super::record_apply::apply_record_rows(&tx,&stage,&key,&locator,&envelope,0).unwrap(); tx.commit().unwrap();
    let header=Header{binding_authority:0.into(),request_id:"archive-held-full-restore".into()};
    store.lww_commit_replacement_with_device_sections(&header,&stage,Some(&units),&sections.iter().collect::<Vec<_>>()).unwrap();
    let restored=read_unit(&store.connection,&name.key).unwrap().unwrap(); assert!(restored.0>name.stamp); assert_eq!(restored.1,name.value);
    assert_eq!(parent_status(&store.connection,&name.key).unwrap(),"held");
    assert_eq!(store.connection.query_row("SELECT status FROM lww_receive_rows WHERE request_id=?1 AND key=?2",params![header.request_id,name.key.as_str()],|row|row.get::<_,String>(0)).unwrap(),"held");
    drop(store); let mut store=PersistentStore::open(dir.path()).unwrap();
    assert!(super::super::archive::read_archived_object(&store.connection,&active_generation(&store.connection).unwrap(),"char").unwrap().is_some());
    store.restore_character("char",store.revision().unwrap()).unwrap();
    assert_eq!(store.read_character("char",None).unwrap().unwrap().value["name"],"held-archive-name");
    store.lww_drain_deferred(&ApplyReceive{header:Header{binding_authority:0.into(),request_id:"drain-unarchived-local-held".into()},generating:vec![]}).unwrap();
    assert_eq!(read_unit(&store.connection,&name.key).unwrap().unwrap(),restored);
    assert!(store.lww_read_outbox(0.into(),100).unwrap().entries.iter().any(|entry|entry.key==name.key&&entry.stamp==restored.0&&entry.version==header.request_id));
}

#[test]
fn commit_intent_replays_once_after_restart() {
    let (dir, mut store) = store();
    let header = Header {
        binding_authority: 0.into(),
        request_id: "crash-local".into(),
    };
    let input = WorkingSetCommit {
        expected_revision: 0,
        unit_mutations: Some(vec![mutation(
            &["root", "language"],
            serde_json::json!("ko"),
        )]),
        ..Default::default()
    };
    let intent = Intent::Commit {
        commit: input.clone(),
        aliases: vec![],
    };
    let (issued, _) = store.reserve_intent(&header, &intent).unwrap();
    drop(store);
    let mut store = PersistentStore::open(dir.path()).unwrap();
    assert_eq!(store.read_root(None).unwrap().value["language"], "ko");
    let intents: i64 = store.device_store().unwrap().connection()
        .query_row("SELECT count(*) FROM lww_intents", [], |row| row.get(0)).unwrap();
    assert_eq!(intents, 0, "a replayed write leaves no intent behind");
    let revision = store.revision().unwrap();
    store.lww_recover_intents().unwrap();
    assert_eq!(store.revision().unwrap(), revision);
    let entry = store
        .lww_read_outbox(0.into(), 10)
        .unwrap()
        .entries
        .remove(0);
    assert_eq!(entry.stamp, issued);
}

#[test]
fn receive_progress_moves_only_after_durable_apply_and_exact_finish() {
    let (dir, mut store) = store();
    let header = Header {
        binding_authority: 0.into(),
        request_id: "durable-receive".into(),
    };
    let staged = StageReceive {
        header: header.clone(),
        changes: vec![change(&["root", "language"], 10, serde_json::json!("ko"))],
        progress: Progress {
            kind: "external".into(),
            cursor: 9.into(),
            writer_id: Some(stamp(1).writer_id),
        },
        admitted_time_upper_ms: 100.into(),
    };
    store.lww_stage_receive(&staged).unwrap();
    assert!(store.lww_finish_receive(&header).is_err());
    assert!(store.lww_receive_progress(0.into()).unwrap().is_empty());
    store
        .lww_apply_receive(&ApplyReceive {
            header: header.clone(),
            generating: vec![],
        })
        .unwrap();
    drop(store);
    let mut store = PersistentStore::open(dir.path()).unwrap();
    assert_eq!(store.read_root(None).unwrap().value["language"], "ko");
    assert!(store.lww_receive_progress(0.into()).unwrap().is_empty());
    store.lww_finish_receive(&header).unwrap();
    store.lww_finish_receive(&header).unwrap();
    assert_eq!(
        store.lww_receive_progress(0.into()).unwrap()[0].cursor,
        9.into()
    );
}

#[test]
fn independent_fields_converge_in_both_delivery_orders() {
    let (_a_directory, mut a) = store();
    let (_b_directory, mut b) = store();
    let one = change(&["root", "language"], 10, serde_json::json!("ko"));
    let two = change(&["root", "username"], 20, serde_json::json!("synthetic"));
    receive(&mut a, "one", vec![one.clone()], vec![]);
    receive(&mut a, "two", vec![two.clone()], vec![]);
    receive(&mut b, "two", vec![two], vec![]);
    receive(&mut b, "one", vec![one], vec![]);
    assert_eq!(
        a.read_root(None).unwrap().value,
        b.read_root(None).unwrap().value
    );
}

#[test]
fn a_received_persona_joins_the_root_with_its_membership_unit() {
    let (_dir, mut store) = store();
    let persona = |store: &PersistentStore| {
        store.read_root(None).unwrap().value["personas"].as_array().cloned().unwrap_or_default()
            .into_iter().find(|value| value["id"] == "received-persona")
    };
    // A replacement pages its units in key order, so membership can arrive
    // in an earlier receive than any field of the record.
    receive(&mut store, "membership", vec![change(&["exists", "persona", "received-persona"], 10, serde_json::json!(true))], vec![]);
    assert_eq!(persona(&store), Some(serde_json::json!({"id": "received-persona"})));
    receive(&mut store, "fields", vec![change(&["persona", "received-persona", "name"], 10, serde_json::json!("Received"))], vec![]);
    receive(&mut store, "repeat", vec![change(&["exists", "persona", "received-persona"], 20, serde_json::json!(true))], vec![]);
    assert_eq!(persona(&store), Some(serde_json::json!({"id": "received-persona", "name": "Received"})));
    receive(&mut store, "removal", vec![Change {
        key: unit_key(&["exists", "persona", "received-persona"]).unwrap(), stamp: stamp(30), value: UnitValue::Deleted,
    }], vec![]);
    assert_eq!(persona(&store), None);
}

#[test]
fn echo_and_empty_receives_keep_the_revision_and_still_acknowledge() {
    let (_dir, mut store) = store();
    save(
        &mut store,
        vec![mutation(&["root", "openAIKey"], serde_json::json!("a"))],
    );
    let own = store
        .lww_read_outbox(0.into(), 10)
        .unwrap()
        .entries
        .remove(0);
    let before = store.revision().unwrap();

    let empty = receive(&mut store, "empty", vec![], vec![]);
    assert!(empty.affected_keys.is_empty());
    assert_eq!(empty.revision, before);

    let echo = receive(
        &mut store,
        "echo",
        vec![Change {
            key: own.key.clone(),
            stamp: own.stamp.clone(),
            value: own.value.clone(),
        }],
        vec![],
    );
    assert!(echo.affected_keys.is_empty());
    assert_eq!(echo.revision, before);
    assert_eq!(store.revision().unwrap(), before);
    assert!(store.lww_read_outbox(0.into(), 10).unwrap().entries.is_empty());
    let count = |db: &Connection, sql: &str| -> i64 { db.query_row(sql, [], |r| r.get(0)).unwrap() };
    for db in [&store.connection, store.device_store().unwrap().connection()] {
        assert_eq!(count(db, "SELECT count(*) FROM lww_receive_rows"), 0);
    }
    let device = store.device_store().unwrap().connection();
    assert_eq!(count(device, "SELECT count(*) FROM lww_receive WHERE request_id='echo' AND finished=1"), 1);
    assert_eq!(
        store.lww_receive_progress(0.into()).unwrap()[0].cursor,
        1.into()
    );

    // The change context closed, so the next local write opens a new one.
    let next = save(
        &mut store,
        vec![mutation(&["root", "openAIKey"], serde_json::json!("b"))],
    );
    assert_eq!(next.revision, before + 1);
    let remote = receive(
        &mut store,
        "remote",
        vec![change(&["root", "language"], 10, serde_json::json!("ko"))],
        vec![],
    );
    assert_eq!(remote.affected_keys.len(), 1);
    assert_eq!(remote.revision, before + 2);
}

fn create_conversation(store: &mut PersistentStore, char_id: &str, conv_id: &str) {
    save(
        store,
        vec![
            mutation(
                &["exists", "character", char_id],
                serde_json::json!({"type":"character"}),
            ),
            mutation(
                &["exists", "conversation", char_id, conv_id],
                serde_json::json!(true),
            ),
        ],
    );
}
fn copy_objects(source: &PersistentStore, target: &PersistentStore) {
    let mut statement = source
        .connection
        .prepare("SELECT hash,body FROM message_page_objects")
        .unwrap();
    for row in statement
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })
        .unwrap()
    {
        let (hash, body) = row.unwrap();
        target.lww_put_object(&hash, &body).unwrap();
    }
}

#[test]
fn incremental_message_receive_staging_and_generation_deferral_reuse_after_restart() {
    use crate::persistent_store::hash_work::{reset_hash_work,take_hash_work,DomainWork};
    for generating in [false,true] {
        let (_sender_directory,mut sender) = store(); let (directory,mut target) = store();
        create_conversation(&mut sender,"char","chat"); create_conversation(&mut target,"char","chat");
        let messages = (0..1024).map(|i| serde_json::json!({"chatId":format!("m-{i}"),"data":format!("synthetic-{i}")})).collect::<Vec<_>>();
        let replace = |store: &mut PersistentStore,start,delete_count,messages| {
            store.commit(&WorkingSetCommit { expected_revision:store.revision().unwrap(),
                conversations:Some(vec![super::super::ConversationMutation::ReplaceRange {
                    character_id:"char".into(),conversation_id:"chat".into(),start,delete_count,messages,
                    conversation:None,configured_index:None,
                }]),..Default::default() }).unwrap();
        };
        replace(&mut sender,0,0,messages.clone()); replace(&mut target,0,0,messages);
        let start = if generating { 500 } else { 1024 };
        let id = if generating { "m-500" } else { "appended" };
        replace(&mut sender,start,i64::from(generating),vec![serde_json::json!({"chatId":id,"data":"remote-update"})]);
        copy_objects(&sender,&target);
        let key = UnitKey::new(&["messages","char","chat"]).unwrap();
        let entry = sender.lww_read_outbox(0.into(),100).unwrap().entries.into_iter().find(|e| e.key==key).unwrap();
        let expected = entry.value.clone();
        let UnitValue::Object { descriptor,.. } = &expected else { unreachable!() };
        assert!(target.lww_verified_object_present(&descriptor.object_hash).unwrap());
        assert!(!target.lww_verified_object_present(&"00".repeat(32)).unwrap());
        let manifest = risunest_external_storage_format::message_pages::MessageManifest::decode(
            &target.lww_object_body(&descriptor.object_hash).unwrap().unwrap()).unwrap();
        let new_pages = manifest.pages.iter().filter(|page| !target.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM message_page_proofs WHERE hash=?1)",[&page.hash],|r|r.get::<_,bool>(0)).unwrap()).collect::<Vec<_>>();
        let message_calls = new_pages.iter().map(|p|p.message_count as u64).sum::<u64>();
        let message_bytes = new_pages.iter().map(|p|p.byte_length.0-(
            risunest_external_storage_format::message_pages::PAGE_PREFIX.len()+
            risunest_external_storage_format::message_pages::PAGE_SUFFIX.len()) as u64-(p.message_count as u64-1)).sum::<u64>();
        let change = Change { key:key.clone(),stamp:stamp(u64::MAX-1),value:entry.value };
        let header = Header { binding_authority:target.lww_binding_authority().unwrap(),request_id:format!("incremental-{generating}") };
        let original_rowids = target.connection.prepare("SELECT message_id,rowid FROM messages ORDER BY message_index").unwrap()
            .query_map([],|r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?))).unwrap()
            .collect::<Result<std::collections::BTreeMap<_,_>,_>>().unwrap();
        target.connection.execute_batch("CREATE TABLE page_receive_writes(kind TEXT);
            CREATE TRIGGER page_receive_insert AFTER INSERT ON messages BEGIN INSERT INTO page_receive_writes VALUES('insert'); END;
            CREATE TRIGGER page_receive_delete AFTER DELETE ON messages BEGIN INSERT INTO page_receive_writes VALUES('delete'); END;").unwrap();
        reset_hash_work();
        target.lww_stage_receive(&StageReceive { header:header.clone(),changes:vec![change.clone()],
            progress:Progress { kind:"server".into(),cursor:1.into(),writer_id:None },admitted_time_upper_ms:u64::MAX.into() }).unwrap();
        let admission = take_hash_work();
        assert_eq!(admission.domains["native_message_verify"],DomainWork { calls:message_calls,bytes:message_bytes });
        assert_eq!(admission.domains["native_page_decode_identity"],DomainWork {
            calls:new_pages.len() as u64,bytes:new_pages.iter().map(|p|p.byte_length.0).sum(),
        });
        assert!(message_calls<256,"fixed fixture must reuse unchanged pages");
        assert!(admission.incomplete.is_empty());
        drop(target); let mut target = PersistentStore::open(directory.path()).unwrap();
        reset_hash_work();
        let applied = target.lww_apply_receive(&ApplyReceive { header:header.clone(),generating:if generating {
            vec![MessageLocator { character_id:"char".into(),conversation_id:"chat".into(),start:None }]
        } else { vec![] } }).unwrap();
        let work = take_hash_work();
        for domain in ["native_message_verify","native_page_decode_identity","native_repage_message_verify","native_repage_page_identity"] {
            assert!(!work.domains.contains_key(domain),"restart apply must skip {domain}: {work:?}");
        }
        assert!(work.incomplete.is_empty());
        target.lww_finish_receive(&header).unwrap();
        if generating {
            assert_eq!(applied.deferred_keys,vec![key.clone()]);
            assert_eq!(target.connection.query_row("SELECT count(*) FROM page_receive_writes",[],|r|r.get::<_,i64>(0)).unwrap(),0);
            drop(target); target = PersistentStore::open(directory.path()).unwrap();
            reset_hash_work();
            let drained = target.lww_drain_deferred(&ApplyReceive { header:Header {
                binding_authority:target.lww_binding_authority().unwrap(),request_id:"incremental-drain".into(),
            },generating:vec![] }).unwrap();
            assert!(drained.affected_keys.contains(&key));
            let work = take_hash_work();
            for domain in ["native_message_verify","native_page_decode_identity","native_repage_message_verify","native_repage_page_identity"] {
                assert!(!work.domains.contains_key(domain),"deferred drain must skip {domain}: {work:?}");
            }
            assert!(work.incomplete.is_empty());
        } else { assert!(applied.affected_keys.contains(&key)); }
        let counts = |kind| target.connection.query_row("SELECT count(*) FROM page_receive_writes WHERE kind=?1",[kind],|r|r.get::<_,i64>(0)).unwrap();
        assert_eq!(counts("insert"),1); assert_eq!(counts("delete"),i64::from(generating));
        for (id,rowid) in target.connection.prepare("SELECT message_id,rowid FROM messages ORDER BY message_index").unwrap()
            .query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?))).unwrap().collect::<Result<Vec<_>,_>>().unwrap() {
            if id=="m-500" && generating { continue; }
            if let Some(old) = original_rowids.get(&id) { assert_eq!(*old,rowid,"unchanged {id}"); }
        }
        assert_eq!(read_unit(&target.connection,&key).unwrap(),Some((change.stamp,expected)));
        let generation = super::super::active_generation(&target.connection).unwrap();
        assert_eq!(target.connection.query_row("SELECT value FROM messages WHERE generation=?1 AND message_index=?2",params![generation,start],
            |r|r.get::<_,String>(0)).unwrap(),format!("{{\"chatId\":\"{id}\",\"data\":\"remote-update\"}}"));
    }
}

#[test]
fn every_generating_message_list_is_deferred_but_metadata_applies() {
    let (_sender_directory, mut sender) = store();
    let (dir, mut target) = store();
    for conv in ["one", "two"] {
        create_conversation(&mut sender, "char", conv);
        create_conversation(&mut target, "char", conv);
    }
    let first = sender.lww_read_outbox(0.into(), 100).unwrap();
    sender
        .lww_ack_outbox(
            &Header {
                binding_authority: 0.into(),
                request_id: "clear".into(),
            },
            &first
                .entries
                .iter()
                .map(|e| AckEntry {
                    key: e.key.clone(),
                    version: e.version.clone(),
                    stamp: e.stamp.clone(),
                    value_identity: e.value.identity().unwrap(),
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
    for conv in ["one", "two"] {
        sender
            .commit(&WorkingSetCommit {
                expected_revision: sender.revision().unwrap(),
                conversations: Some(vec![super::super::ConversationMutation::ReplaceRange {
                    character_id: "char".into(),
                    conversation_id: conv.into(),
                    start: 99,
                    delete_count: 99,
                    messages: vec![serde_json::json!({"data":conv,"chatId":"kept"})],
                    conversation: None,
                    configured_index: None,
                }]),
                ..Default::default()
            })
            .unwrap();
    }
    copy_objects(&sender, &target);
    let generating = ["one", "two"]
        .map(|id| MessageLocator {
            character_id: "char".into(),
            conversation_id: id.into(),
            start: None,
        })
        .to_vec();
    let mut changes = sender
        .lww_read_outbox(0.into(), 100)
        .unwrap()
        .entries
        .into_iter()
        .map(|e| Change {
            key: e.key,
            stamp: e.stamp,
            value: e.value,
        })
        .collect::<Vec<_>>();
    changes.push(change(
        &["conversation", "char", "one", "name"],
        u64::MAX - 1,
        serde_json::json!("remote-name"),
    ));
    let result = receive(&mut target, "generating", changes, generating.clone());
    assert_eq!(result.deferred_keys.len(), 2);
    assert!(target
        .lww_read_outbox_generating(0.into(), 100, &generating)
        .unwrap()
        .entries
        .iter()
        .all(|e| e.key.components()[0] != "messages"));
    drop(target);
    let mut target = PersistentStore::open(dir.path()).unwrap();
    let drained = target
        .lww_drain_deferred(&ApplyReceive {
            header: Header {
                binding_authority: 0.into(),
                request_id: "drain".into(),
            },
            generating: vec![],
        })
        .unwrap();
    assert_eq!(
        drained
            .affected_keys
            .iter()
            .filter(|key| key.components()[0] == "messages")
            .count(),
        2
    );
    let count: i64 = target
        .connection
        .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn order_preserves_folders_filters_retired_ids_and_appends_concurrent_additions() {
    let (_store_directory, mut store) = store();
    let changes = vec![
        change(
            &["exists", "character", "a"],
            1,
            serde_json::json!({"type":"character"}),
        ),
        change(
            &["exists", "character", "b"],
            2,
            serde_json::json!({"type":"character"}),
        ),
        change(
            &["exists", "character", "c"],
            3,
            serde_json::json!({"type":"character"}),
        ),
        change(
            &["order", "characters"],
            10,
            serde_json::json!([{"name":"folder","data":["b","missing","b"]},"a"]),
        ),
    ];
    receive(&mut store, "order", changes, vec![]);
    assert_eq!(
        store.read_root(None).unwrap().value["characterOrder"],
        serde_json::json!([{"name":"folder","data":["b"]},"a","c"])
    );
    receive(
        &mut store,
        "retire",
        vec![Change {
            key: unit_key(&["exists", "character", "b"]).unwrap(),
            stamp: stamp(11),
            value: UnitValue::Deleted,
        }],
        vec![],
    );
    assert_eq!(
        store.read_root(None).unwrap().value["characterOrder"][0]["data"],
        serde_json::json!([])
    );
}

fn character_order(store: &PersistentStore) -> Value {
    store.read_root(None).unwrap().value["characterOrder"].clone()
}

#[test]
fn trashed_characters_stay_out_of_the_character_order() {
    let (_store_directory, mut store) = store();
    save(&mut store, vec![
        mutation(&["exists", "character", "a"], serde_json::json!({"type":"character"})),
        mutation(&["exists", "character", "b"], serde_json::json!({"type":"character"})),
        mutation(&["exists", "character", "c"], serde_json::json!({"type":"character"})),
        mutation(&["order", "characters"], serde_json::json!([{"name":"folder","data":["c"]},"a","b"])),
    ]);
    assert_eq!(character_order(&store), serde_json::json!([{"name":"folder","data":["c"]},"a","b"]));
    // The renderer trashes a character by dropping it from the order.
    save(&mut store, vec![
        mutation(&["character", "b", "trashTime"], serde_json::json!(1)),
        mutation(&["order", "characters"], serde_json::json!([{"name":"folder","data":["c"]},"a"])),
    ]);
    assert_eq!(character_order(&store), serde_json::json!([{"name":"folder","data":["c"]},"a"]));
    // A trashed character the order still lists stays out as well.
    save(&mut store, vec![mutation(&["character", "c", "trashTime"], serde_json::json!(2))]);
    assert_eq!(character_order(&store), serde_json::json!([{"name":"folder","data":[]},"a"]));
    // Restoring from the trash brings the character back.
    save(&mut store, vec![
        UnitMutation::Delete { key: unit_key(&["character", "b", "trashTime"]).unwrap() },
        mutation(&["order", "characters"], serde_json::json!([{"name":"folder","data":["c"]},"a","b"])),
    ]);
    assert_eq!(character_order(&store), serde_json::json!([{"name":"folder","data":[]},"a","b"]));
    let trash = store.query_characters(&crate::persistent_store::CharacterQuery {
        search: None, order: crate::persistent_store::QueryOrder::Configured, trash: true, limit: 10, cursor: None,
    }, None).unwrap();
    assert_eq!(trash.items.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(), ["c"]);
}

#[test]
fn a_received_trash_leaves_the_character_order() {
    let (_store_directory, mut store) = store();
    receive(&mut store, "library", vec![
        change(&["exists", "character", "a"], 1, serde_json::json!({"type":"character"})),
        change(&["exists", "character", "b"], 2, serde_json::json!({"type":"character"})),
        change(&["exists", "character", "c"], 3, serde_json::json!({"type":"character"})),
        change(&["order", "characters"], 4, serde_json::json!(["a", "b", "c"])),
    ], vec![]);
    assert_eq!(character_order(&store), serde_json::json!(["a", "b", "c"]));
    receive(&mut store, "trash", vec![
        change(&["character", "c", "trashTime"], 5, serde_json::json!(5)),
        change(&["order", "characters"], 5, serde_json::json!(["a", "b"])),
    ], vec![]);
    assert_eq!(character_order(&store), serde_json::json!(["a", "b"]));
    receive(&mut store, "trash-listed", vec![
        change(&["character", "b", "trashTime"], 6, serde_json::json!(6)),
    ], vec![]);
    assert_eq!(character_order(&store), serde_json::json!(["a"]));
}

#[test]
fn local_and_opaque_fields_persist_without_publication() {
    let (_store_directory, mut store) = store();
    create_conversation(&mut store, "char", "conv");
    save(
        &mut store,
        vec![
            mutation(&["character", "char", "chatPage"], serde_json::json!(7)),
            mutation(
                &["character", "char", "futureOwnField"],
                serde_json::json!({"x":1}),
            ),
            mutation(
                &["conversation", "char", "conv", "futureOwnField"],
                serde_json::json!(true),
            ),
        ],
    );
    let entries = store.lww_read_outbox(0.into(), 100).unwrap().entries;
    assert!(entries.iter().all(|e| !matches!(
        e.key.components().last().unwrap().as_str(),
        "chatPage" | "futureOwnField"
    )));
    assert_eq!(
        store.read_character("char", None).unwrap().unwrap().value["chatPage"],
        7
    );
}

#[test]
fn realm_block_loadout_and_onboarding_fields_publish_and_apply_on_a_peer() {
    let (_source_dir, mut source) = store();
    let (peer_dir, mut peer) = store();
    create_conversation(&mut source, "char", "conv");
    let blocked = serde_json::json!([{"id":"synthetic-creator","name":"Synthetic creator"}]);
    let options = serde_json::json!({"modules":false,"globalVariables":true,"preset":false,"persona":true});
    let onboarding = serde_json::json!({"enabled":true,"mode":"form","questions":[]});
    save(
        &mut source,
        vec![
            mutation(&["root", "blockedRealmCreators"], blocked.clone()),
            mutation(&["root", "loadoutApplyOptions"], options.clone()),
            mutation(&["character", "char", "onboarding"], onboarding.clone()),
            mutation(&["conversation", "char", "conv", "onboardingDone"], serde_json::json!(true)),
        ],
    );
    let outbox = source.lww_read_outbox(0.into(), 100).unwrap().entries;
    copy_objects(&source, &peer);
    receive(
        &mut peer,
        "shared-fields",
        outbox
            .into_iter()
            .map(|entry| Change { key: entry.key, stamp: entry.stamp, value: entry.value })
            .collect(),
        vec![],
    );
    drop(peer);
    let peer = PersistentStore::open(peer_dir.path()).unwrap();
    let root = peer.read_root(None).unwrap().value;
    assert_eq!(root["blockedRealmCreators"], blocked);
    assert_eq!(root["loadoutApplyOptions"], options);
    assert_eq!(peer.read_character("char", None).unwrap().unwrap().value["onboarding"], onboarding);
    assert_eq!(
        peer.read_conversation("char", "conv", None).unwrap().unwrap().value["onboardingDone"],
        true
    );
}

#[test]
fn plugin_record_deletion_is_reusable_and_generic_record_deletion_is_permanent() {
    let (_store_directory, mut store) = store();
    receive(
        &mut store,
        "plugin",
        vec![change(
            &["record", "plugins", "synthetic"],
            1,
            serde_json::json!({"name":"synthetic","script":"a"}),
        )],
        vec![],
    );
    receive(
        &mut store,
        "plugin-delete",
        vec![Change {
            key: unit_key(&["record", "plugins", "synthetic"]).unwrap(),
            stamp: stamp(2),
            value: UnitValue::Deleted,
        }],
        vec![],
    );
    receive(
        &mut store,
        "plugin-reinstall",
        vec![change(
            &["record", "plugins", "synthetic"],
            3,
            serde_json::json!({"name":"synthetic","script":"b"}),
        )],
        vec![],
    );
    assert_eq!(
        store.read_root(None).unwrap().value["plugins"][0]["script"],
        "b"
    );
    receive(
        &mut store,
        "module",
        vec![
            change(&["exists", "modules", "module"], 1, serde_json::json!(true)),
            change(
                &["record", "modules", "module"],
                1,
                serde_json::json!({"id":"module"}),
            ),
        ],
        vec![],
    );
    receive(
        &mut store,
        "module-delete",
        vec![Change {
            key: unit_key(&["exists", "modules", "module"]).unwrap(),
            stamp: stamp(2),
            value: UnitValue::Deleted,
        }],
        vec![],
    );
    receive(
        &mut store,
        "module-stale",
        vec![change(
            &["record", "modules", "module"],
            3,
            serde_json::json!({"id":"module","name":"bad"}),
        )],
        vec![],
    );
    assert_eq!(
        store.read_root(None).unwrap().value["modules"],
        serde_json::json!([])
    );
}

#[test]
fn plugin_local_participation_filters_and_one_batch_shares_stamp() {
    use super::super::device_store::{plugin_values::PluginDeviceMutation, Section};
    let (_store_directory, mut store) = store();
    store
        .device_store_mut()
        .unwrap()
        .write_plugin_device_values(
            "orphan",
            &[
                PluginDeviceMutation::Set {
                    space: "string".into(),
                    key: "a".into(),
                    value: "1".into(),
                },
                PluginDeviceMutation::Set {
                    space: "json".into(),
                    key: "b".into(),
                    value: "null".into(),
                },
            ],
        )
        .unwrap();
    assert!(store
        .lww_read_outbox(0.into(), 100)
        .unwrap()
        .entries
        .is_empty());
    store
        .device_store_mut()
        .unwrap()
        .set_section_participating(Section::LocalPlugins, true)
        .unwrap();
    let out = store.lww_read_outbox(0.into(), 100).unwrap();
    assert_eq!(out.entries.len(), 2);
    assert_eq!(out.entries[0].stamp, out.entries[1].stamp);
    store
        .device_store_mut()
        .unwrap()
        .write_plugin_device_values(
            "orphan",
            &[PluginDeviceMutation::Set {
                space: "string".into(),
                key: "a".into(),
                value: "1".into(),
            }],
        )
        .unwrap();
    assert_eq!(
        store.lww_read_outbox(0.into(), 100).unwrap().entries[0].stamp,
        out.entries[0].stamp
    );
    assert!(store
        .device_store_mut()
        .unwrap()
        .set_section_participating(Section::Hypa, false)
        .is_err());
    store
        .device_store_mut()
        .unwrap()
        .set_section_participating(Section::LocalPlugins, false)
        .unwrap();
    assert!(store
        .lww_read_outbox(0.into(), 100)
        .unwrap()
        .entries
        .is_empty());
}

#[test]
fn unpublished_future_clock_repair_requires_native_proof_and_preserves_accepted_clock() {
    let (_store_directory, mut store) = store();
    let future = stamp(device_store::now_ms().unwrap() as u64 + 1_000_000);
    let accepted = stamp(1);
    store
        .device_store()
        .unwrap()
        .connection()
        .execute(
            "UPDATE lww_clock SET issued=?1,accepted=?2",
            params![
                serde_json::to_string(&future).unwrap(),
                serde_json::to_string(&accepted).unwrap()
            ],
        )
        .unwrap();
    save(
        &mut store,
        vec![mutation(&["root", "language"], serde_json::json!("ko"))],
    );
    let old = store.lww_read_outbox(0.into(), 100).unwrap().entries;
    let header = Header {
        binding_authority: 0.into(),
        request_id: "clock-repair".into(),
    };
    let corrected = DecimalU64(device_store::now_ms().unwrap() as u64);
    assert!(store
        .lww_retry_unpublished(&header, "missing", corrected)
        .is_err());
    store
        .lww_record_unpublished_proof(
            &header,
            "proof",
            &old.iter()
                .map(|e| AckEntry {
                    key: e.key.clone(),
                    version: e.version.clone(),
                    stamp: e.stamp.clone(),
                    value_identity: e.value.identity().unwrap(),
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let before = store.lww_clock_state().unwrap().accepted;
    assert_eq!(before, Some(accepted.clone()));
    store
        .lww_retry_unpublished(&header, "proof", corrected)
        .unwrap();
    let repaired = store.lww_read_outbox(0.into(), 100).unwrap().entries;
    assert_eq!(repaired.len(), 1);
    assert!(repaired[0].stamp.physical_ms < old[0].stamp.physical_ms);
    assert_eq!(repaired[0].value, old[0].value);
    assert_eq!(store.lww_clock_state().unwrap().accepted, before);
    store
        .lww_retry_unpublished(&header, "proof", corrected)
        .unwrap();
    assert_eq!(
        store.lww_read_outbox(0.into(), 100).unwrap().entries,
        repaired
    );

    for phase in ["reserved", "library"] {
        let (directory, mut repairing) = self::store();
        repairing.device_store_mut().unwrap().set_section_participating(device_store::Section::LocalPlugins, true).unwrap();
        clear_outbox(&mut repairing);
        let future = stamp(device_store::now_ms().unwrap() as u64 + 1_000_000);
        repairing.device_store().unwrap().connection().execute(
            "UPDATE lww_clock SET issued=?1,accepted=?2", params![serde_json::to_string(&future).unwrap(), serde_json::to_string(&accepted).unwrap()]).unwrap();
        save(&mut repairing, vec![mutation(&["root", "language"], serde_json::json!("repair-both"))]);
        repairing.device_store_mut().unwrap().write_plugin_device_values("repair", &[
            device_store::plugin_values::PluginDeviceMutation::Set { space: "string".into(), key: "key".into(), value: "device-value".into() },
        ]).unwrap();
        let entries = repairing.lww_read_outbox(0.into(), 100).unwrap().entries;
        assert_eq!(entries.iter().map(|entry| entry.key.clone()).collect::<BTreeSet<_>>(),
            BTreeSet::from([unit_key(&["root", "language"]).unwrap(), unit_key(&["plugin-local", "repair", "string", "key"]).unwrap()]));
        assert_eq!(repairing.read_root(None).unwrap().value["language"], "repair-both");
        assert_eq!(repairing.device_store().unwrap().read_plugin_device_value("repair", "string", "key").unwrap().as_deref(), Some("device-value"));
        let header = Header { binding_authority: 0.into(), request_id: format!("repair-{phase}") };
        repairing.lww_record_unpublished_proof(&header, "both-proof", &entries.iter().map(|entry| AckEntry {
            key: entry.key.clone(), version: entry.version.clone(), stamp: entry.stamp.clone(), value_identity: entry.value.identity().unwrap(),
        }).collect::<Vec<_>>()).unwrap();
        let mut peer = PersistentStore::open(directory.path()).unwrap();
        let request = header.request_id.clone();
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let copy = captured.clone();
        let callback: Box<dyn FnOnce()> = Box::new(move || {
            assert!(matches!(peer.lww_recover_intents(), Err(StoreError::CommitBusy)));
            assert_admission_begin_busy(&mut peer);
            let frozen = admission_intent_state(&peer, &request).unwrap();
            assert_eq!(peer.device_store().unwrap().connection().query_row(
                "SELECT count(*) FROM lww_intent_failures WHERE request_id=?1", [&request], |row| row.get::<_, i64>(0)).unwrap(), 0);
            *copy.borrow_mut() = Some(frozen);
            panic!("synthetic Repair interruption after {phase}");
        });
        if phase == "reserved" { AFTER_REPAIR_RESERVATION.with(|hook| *hook.borrow_mut() = Some(callback)); }
        else { AFTER_REPAIR_LIBRARY.with(|hook| *hook.borrow_mut() = Some(callback)); }
        let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            repairing.lww_retry_unpublished(&header, "both-proof", corrected)
        }));
        assert!(stopped.is_err());
        assert_eq!(admission_intent_state(&repairing, &header.request_id), *captured.borrow());
        let issued = repairing.lww_clock_state().unwrap().issued.unwrap();
        assert_eq!(repairing.lww_clock_state().unwrap().accepted, Some(accepted.clone()));
        assert!(!repairing.lww_request_recorded(&header.request_id).unwrap());
        drop(repairing);
        let mut reopened = PersistentStore::open(directory.path()).unwrap();
        let after = reopened.lww_read_outbox(0.into(), 100).unwrap().entries;
        assert_eq!(after.len(), entries.len());
        for original in &entries {
            let actual = after.iter().find(|entry| entry.key == original.key).unwrap();
            assert_eq!(actual.value, original.value);
            assert_eq!(actual.version, header.request_id);
            assert_eq!(actual.stamp, issued);
        }
        assert_eq!(reopened.lww_clock_state().unwrap().accepted, Some(accepted.clone()));
        assert_eq!(reopened.lww_clock_state().unwrap().issued, Some(issued));
        assert!(!reopened.lww_request_recorded(&header.request_id).unwrap());
        assert!(reopened.device_store().unwrap().connection().query_row(
            "SELECT complete FROM lww_intents WHERE request_id=?1", [&header.request_id], |row| row.get::<_, bool>(0)).unwrap());
        reopened.lww_retry_unpublished(&header, "both-proof", corrected).unwrap();
        assert_eq!(reopened.lww_read_outbox(0.into(), 100).unwrap().entries, after);
        assert!(reopened.lww_quarantined_intents().unwrap().is_empty());
    }
}

fn clear_outbox(store: &mut PersistentStore) {
    let entries = store
        .lww_read_outbox(store.lww_binding_authority().unwrap(), 1000)
        .unwrap()
        .entries;
    store
        .lww_ack_outbox(
            &Header {
                binding_authority: store.lww_binding_authority().unwrap(),
                request_id: "clear-check".into(),
            },
            &entries
                .into_iter()
                .map(|entry| AckEntry {
                    key: entry.key,
                    stamp: entry.stamp,
                    value_identity: entry.value.identity().unwrap(),
                    version: entry.version,
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
}
#[test]
fn same_batch_parent_creation_precedes_message_range_and_local_statics_never_publish() {
    let (_store_directory, mut store) = store();
    store
        .commit(&WorkingSetCommit {
            expected_revision: 0,
            unit_mutations: Some(vec![
                mutation(
                    &["exists", "character", "new"],
                    serde_json::json!({"type":"character"}),
                ),
                mutation(
                    &["exists", "conversation", "new", "chat"],
                    serde_json::json!(true),
                ),
                mutation(
                    &["conversation", "new", "chat", "name"],
                    serde_json::json!("created"),
                ),
            ]),
            conversations: Some(vec![super::super::ConversationMutation::ReplaceRange {
                character_id: "new".into(),
                conversation_id: "chat".into(),
                start: 0,
                delete_count: 0,
                messages: vec![serde_json::json!({"data":"synthetic","chatId":"kept"})],
                conversation: None,
                configured_index: None,
            }]),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        store
            .read_conversation("new", "chat", None)
            .unwrap()
            .unwrap()
            .value["message"][0]["chatId"],
        "kept"
    );
    save(
        &mut store,
        vec![mutation(
            &["character", "new", "statics"],
            serde_json::json!({"messages":[1],"value":2}),
        )],
    );
    clear_outbox(&mut store);
    save(
        &mut store,
        vec![
            mutation(
                &["character", "new", "statics"],
                serde_json::json!({"messages":[3],"value":2}),
            ),
            mutation(
                &["conversation", "new", "chat", "futureOwnField"],
                serde_json::json!("local"),
            ),
        ],
    );
    assert!(store
        .lww_read_outbox(0.into(), 100)
        .unwrap()
        .entries
        .is_empty());
    let value = store.read_character("new", None).unwrap().unwrap().value;
    assert_eq!(value["statics"]["messages"], serde_json::json!([3]));
    assert_eq!(
        store
            .read_conversation("new", "chat", None)
            .unwrap()
            .unwrap()
            .value["futureOwnField"],
        "local"
    );
}
#[test]
fn archive_publishes_one_state_unit_and_shared_payload_excludes_local_fields() {
    use std::io::Read;
    let (_directory, mut store) = store();
    create_conversation(&mut store, "char", "chat");
    save(
        &mut store,
        vec![
            mutation(
                &["character", "char", "name"],
                serde_json::json!("synthetic"),
            ),
            mutation(
                &["character", "char", "localFuture"],
                serde_json::json!("private"),
            ),
            mutation(
                &["character", "char", "statics"],
                serde_json::json!({"messages":["private"],"value":1}),
            ),
            mutation(
                &["conversation", "char", "chat", "localFuture"],
                serde_json::json!("private-chat"),
            ),
            mutation(
                &["character", "char", "onboarding"],
                serde_json::json!({"enabled":true,"mode":"html","questions":[],"html":"<form></form>"}),
            ),
            mutation(
                &["conversation", "char", "chat", "onboardingDone"],
                serde_json::json!(true),
            ),
        ],
    );
    clear_outbox(&mut store);
    let revision = store.revision().unwrap();
    store.archive_character("char", revision, 10).unwrap();
    let out = store.lww_read_outbox(0.into(), 100).unwrap().entries;
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].key.components(), ["archive", "char"]);
    let retired: i64 = store
        .connection
        .query_row("SELECT COUNT(*) FROM lww_retired", [], |r| r.get(0))
        .unwrap();
    assert_eq!(retired, 0);
    let generation = active_generation(&store.connection).unwrap();
    let archived =
        super::super::archive::read_archived_object(&store.connection, &generation, "char")
            .unwrap()
            .unwrap();
    assert_ne!(archived.object_hash, archived.shared_object_hash);
    let body = store
        .lww_object_body(&archived.shared_object_hash)
        .unwrap()
        .unwrap();
    let mut decoded = String::new();
    flate2::read::GzDecoder::new(body.as_slice())
        .read_to_string(&mut decoded)
        .unwrap();
    let shared: Value = serde_json::from_str(&decoded).unwrap();
    assert!(shared["detail"].get("localFuture").is_none());
    assert!(shared["detail"]["statics"].get("messages").is_none());
    assert!(shared["conversations"][0]["detail"]
        .get("localFuture")
        .is_none());
    assert_eq!(shared["detail"]["onboarding"]["html"], "<form></form>");
    assert_eq!(shared["conversations"][0]["detail"]["onboardingDone"], true);
    clear_outbox(&mut store);
    let revision = store.revision().unwrap();
    store.restore_character("char", revision).unwrap();
    let restored = store.read_character("char", None).unwrap().unwrap().value;
    assert_eq!(restored["localFuture"], "private");
    assert_eq!(
        restored["statics"]["messages"],
        serde_json::json!(["private"])
    );
    assert_eq!(
        store
            .read_conversation("char", "chat", None)
            .unwrap()
            .unwrap()
            .value["localFuture"],
        "private-chat"
    );
    assert_eq!(
        store.lww_read_outbox(0.into(), 100).unwrap().entries.len(),
        1
    );
}
#[test]
fn received_archive_retains_local_fields_and_holds_children_until_restore() {
    let (_source_directory, mut source) = store();
    let (dir, mut target) = store();
    create_conversation(&mut source, "char", "chat");
    create_conversation(&mut target, "char", "chat");
    save(
        &mut target,
        vec![mutation(
            &["character", "char", "privateOwn"],
            serde_json::json!("retained"),
        )],
    );
    clear_outbox(&mut source);
    let revision = source.revision().unwrap();
    source.archive_character("char", revision, 10).unwrap();
    copy_objects(&source, &target);
    let entry = source
        .lww_read_outbox(0.into(), 100)
        .unwrap()
        .entries
        .remove(0);
    let time = device_store::now_ms().unwrap() as u64 + 100_000;
    receive(
        &mut target,
        "remote-archive",
        vec![Change {
            key: entry.key.clone(),
            stamp: stamp(time),
            value: entry.value,
        }],
        vec![],
    );
    let result = receive(
        &mut target,
        "held-field",
        vec![change(
            &["character", "char", "name"],
            time + 1,
            serde_json::json!("remote-name"),
        )],
        vec![],
    );
    assert_eq!(result.held_keys.len(), 1);
    drop(target);
    let mut target = PersistentStore::open(dir.path()).unwrap();
    let result = receive(
        &mut target,
        "remote-restore",
        vec![Change {
            key: entry.key,
            stamp: stamp(time + 2),
            value: UnitValue::Deleted,
        }],
        vec![],
    );
    assert!(result
        .affected_keys
        .iter()
        .any(|k| k.components()[0] == "archive"));
    let value = target.read_character("char", None).unwrap().unwrap().value;
    assert_eq!(value["privateOwn"], "retained");
    assert_eq!(value["name"], "remote-name");
    assert_eq!(
        target
            .read_conversation("char", "chat", None)
            .unwrap()
            .unwrap()
            .value["id"],
        "chat"
    );
}
#[test]
fn plugin_local_received_while_disabled_activates_on_enable_without_publication() {
    let (_store_directory, mut store) = store();
    use device_store::Section;
    store
        .device_store_mut()
        .unwrap()
        .set_section_participating(Section::LocalPlugins, false)
        .unwrap();
    receive(
        &mut store,
        "opaque-plugin",
        vec![change(
            &["plugin-local", "orphan", "string", "key"],
            10,
            serde_json::json!("remote"),
        )],
        vec![],
    );
    assert!(store
        .device_store()
        .unwrap()
        .read_plugin_device_value("orphan", "string", "key")
        .unwrap()
        .is_none());
    store
        .device_store_mut()
        .unwrap()
        .set_section_participating(Section::LocalPlugins, true)
        .unwrap();
    assert_eq!(
        store
            .device_store()
            .unwrap()
            .read_plugin_device_value("orphan", "string", "key")
            .unwrap()
            .as_deref(),
        Some("remote")
    );
    assert!(store
        .lww_read_outbox(0.into(), 100)
        .unwrap()
        .entries
        .is_empty());
}

#[test]
fn archive_record_restore_keeps_both_bodies_but_publishes_filtered_body_only() {
    use crate::logical_records::{
        decode_logical_record, encode_logical_record_key, LogicalRecordEnvelope,
        LogicalRecordLocator,
    };
    let (_directory, mut store) = store();
    create_conversation(&mut store, "char", "chat");
    save(
        &mut store,
        vec![
            mutation(&["character", "char", "name"], serde_json::json!("before")),
            mutation(
                &["character", "char", "privateOwn"],
                serde_json::json!("local"),
            ),
        ],
    );
    let revision = store.revision().unwrap();
    store.archive_character("char", revision, 10).unwrap();
    let generation = active_generation(&store.connection).unwrap();
    let cas = crate::asset_repository::PayloadCas::new(&store.repository_root).unwrap();
    let locator = LogicalRecordLocator::Character {
        character_id: "char".into(),
    };
    let key = encode_logical_record_key(&locator).unwrap();
    let encoded = super::super::record_projection::reconstruct_record_with_owner_objects(
        &store.connection,
        &cas,
        &generation,
        &key,
        vec![],
        |_| Ok(()),
        &|hash| Ok(cas.stat_object(hash)?.unwrap()),
    )
    .unwrap();
    let envelope = decode_logical_record(&encoded).unwrap();
    let LogicalRecordEnvelope::ArchivedCharacter {
        archive_object_hash,
        shared_archive_object_hash,
        ..
    } = &envelope
    else {
        panic!("archive record")
    };
    assert_ne!(archive_object_hash, shared_archive_object_hash);
    assert!(envelope.dependency_hashes().contains(archive_object_hash));
    assert!(envelope
        .dependency_hashes()
        .contains(shared_archive_object_hash));
    let backup_units: BTreeMap<UnitKey, UnitValue> = {
        let mut q = store
            .connection
            .prepare("SELECT key,value FROM lww_units")
            .unwrap();
        q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .map(|row| {
                let (key, value) = row.unwrap();
                (
                    key.try_into().unwrap(),
                    serde_json::from_str(&value).unwrap(),
                )
            })
            .collect()
    };
    let revision = store.revision().unwrap();
    store.restore_character("char", revision).unwrap();
    save(
        &mut store,
        vec![mutation(
            &["character", "char", "name"],
            serde_json::json!("after"),
        )],
    );
    clear_outbox(&mut store);
    let staging = store.replace_begin().unwrap().staging_id;
    let root = store.read_root(None).unwrap().value;
    store.replace_put_root(&staging, &root).unwrap();
    store.replace_put_presets(&staging, &[]).unwrap();
    let tx = store.connection.transaction().unwrap();
    super::super::record_apply::apply_record_rows(&tx, &staging, &key, &locator, &envelope, 0)
        .unwrap();
    tx.commit().unwrap();
    let header = Header {
        binding_authority: 0.into(),
        request_id: "archive-backup-restore".into(),
    };
    let result = store
        .lww_commit_replacement_units(&header, &staging, Some(&backup_units))
        .unwrap();
    let out = store.lww_read_outbox(0.into(), 100).unwrap().entries;
    assert!(!out
        .iter()
        .any(|entry| entry.key.components()[0] == "exists"
            && matches!(entry.value, UnitValue::Deleted)));
    let archive = out
        .iter()
        .find(|entry| entry.key.components()[0] == "archive")
        .unwrap();
    let metadata = projection::archive_metadata(&store.connection, &archive.value).unwrap();
    assert_eq!(&metadata.object_hash, shared_archive_object_hash);
    assert_ne!(&metadata.object_hash, archive_object_hash);
    assert_eq!(
        store
            .lww_commit_replacement_units(&header, &staging, Some(&backup_units))
            .unwrap()
            .revision,
        result.revision
    );
    let mut altered = backup_units;
    altered.insert(
        unit_key(&["root", "language"]).unwrap(),
        inline(&serde_json::json!("altered")).unwrap(),
    );
    assert!(store
        .lww_commit_replacement_units(&header, &staging, Some(&altered))
        .is_err());
    let revision = store.revision().unwrap();
    store.restore_character("char", revision).unwrap();
    let value = store.read_character("char", None).unwrap().unwrap().value;
    assert_eq!(value["name"], "before");
    assert_eq!(value["privateOwn"], "local");
    assert!(store
        .read_conversation("char", "chat", None)
        .unwrap()
        .is_some());
}

fn detach_for_new_authority(store: &mut PersistentStore) -> DecimalU64 {
    let selection = super::super::sync_selection::read(&store.connection).unwrap();
    let binding = store.lww_binding_authority().unwrap();
    let change = super::super::sync_selection::BindingSelectionChange {
        initial_publication: false,
        expected_epoch: selection.epoch,
        new_epoch: Uuid::new_v4().to_string(),
        target: super::super::sync_selection::SyncTarget::None,
        library_id: None,
        target_id: None,
        inspection_id: None,
    };
    store
        .lww_switch_target(
            &Header {
                binding_authority: binding,
                request_id: Uuid::new_v4().to_string(),
            },
            &change,
        )
        .unwrap()
}
#[test]
fn default_binding_sends_nothing_then_changed_child_initializes_only_its_owners() {
    let (_source_directory, mut source) = store();
    let (_target_directory, mut target) = store();
    create_conversation(&mut source, "char", "chat");
    create_conversation(&mut source, "unrelated", "other");
    save(
        &mut source,
        vec![
            mutation(
                &["character", "char", "name"],
                serde_json::json!("baseline"),
            ),
            mutation(
                &["character", "char", "privateOwn"],
                serde_json::json!("private"),
            ),
            mutation(
                &["conversation", "char", "chat", "name"],
                serde_json::json!("before"),
            ),
            mutation(
                &["character", "unrelated", "name"],
                serde_json::json!("unrelated"),
            ),
        ],
    );
    let old_parent = read_unit(
        &source.connection,
        &unit_key(&["exists", "character", "char"]).unwrap(),
    )
    .unwrap()
    .unwrap();
    let old_name = read_unit(
        &source.connection,
        &unit_key(&["character", "char", "name"]).unwrap(),
    )
    .unwrap()
    .unwrap();
    let binding = detach_for_new_authority(&mut source);
    assert!(source
        .lww_read_outbox(binding, 100)
        .unwrap()
        .entries
        .is_empty());
    save(
        &mut source,
        vec![mutation(
            &["conversation", "char", "chat", "name"],
            serde_json::json!("changed"),
        )],
    );
    let out = source.lww_read_outbox(binding, 100).unwrap().entries;
    assert_eq!(
        out.iter()
            .find(|entry| entry.key.components() == ["exists", "character", "char"])
            .unwrap()
            .stamp,
        old_parent.0
    );
    assert_eq!(
        out.iter()
            .find(|entry| entry.key.components() == ["character", "char", "name"])
            .unwrap()
            .stamp,
        old_name.0
    );
    assert!(!out.iter().any(|entry| entry
        .key
        .components()
        .iter()
        .any(|component| component == "unrelated" || component == "privateOwn")));
    copy_objects(&source, &target);
    let result = receive(
        &mut target,
        "initialized-child",
        out.into_iter()
            .map(|entry| Change {
                key: entry.key,
                stamp: entry.stamp,
                value: entry.value,
            })
            .collect(),
        vec![],
    );
    assert!(result.held_keys.is_empty());
    assert_eq!(
        target.read_character("char", None).unwrap().unwrap().value["name"],
        "baseline"
    );
    assert_eq!(
        target
            .read_conversation("char", "chat", None)
            .unwrap()
            .unwrap()
            .value["name"],
        "changed"
    );
    clear_outbox(&mut source);
    save(
        &mut source,
        vec![mutation(
            &["conversation", "char", "chat", "name"],
            serde_json::json!("next"),
        )],
    );
    let out = source.lww_read_outbox(binding, 100).unwrap().entries;
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].key.components(),
        ["conversation", "char", "chat", "name"]
    );
}
#[test]
fn first_message_edit_initializes_default_parents_and_control_values_without_restamping() {
    let (_source_directory, mut source) = store();
    let (_target_directory, mut target) = store();
    create_conversation(&mut source, "char", "chat");
    save(
        &mut source,
        vec![mutation(
            &["conversation", "char", "chat", "name"],
            serde_json::json!("baseline"),
        )],
    );
    let parent = read_unit(
        &source.connection,
        &unit_key(&["exists", "conversation", "char", "chat"]).unwrap(),
    )
    .unwrap()
    .unwrap();
    let binding = detach_for_new_authority(&mut source);
    source
        .commit(&WorkingSetCommit {
            expected_revision: source.revision().unwrap(),
            conversations: Some(vec![super::super::ConversationMutation::ReplaceRange {
                character_id: "char".into(),
                conversation_id: "chat".into(),
                start: 0,
                delete_count: 0,
                messages: vec![serde_json::json!({"data":"synthetic","chatId":"kept"})],
                conversation: None,
                configured_index: None,
            }]),
            ..Default::default()
        })
        .unwrap();
    let out = source.lww_read_outbox(binding, 100).unwrap().entries;
    assert_eq!(
        out.iter()
            .find(|entry| entry.key.components() == ["exists", "conversation", "char", "chat"])
            .unwrap()
            .stamp,
        parent.0
    );
    copy_objects(&source, &target);
    let result = receive(
        &mut target,
        "first-message",
        out.into_iter()
            .map(|entry| Change {
                key: entry.key,
                stamp: entry.stamp,
                value: entry.value,
            })
            .collect(),
        vec![],
    );
    assert!(result.held_keys.is_empty());
    let conversation = target
        .read_conversation("char", "chat", None)
        .unwrap()
        .unwrap()
        .value;
    assert_eq!(conversation["name"], "baseline");
    assert_eq!(conversation["message"][0]["chatId"], "kept");
}
#[test]
fn explicit_nondefault_initialization_pages_existing_state_without_new_stamps() {
    let (_source_directory, mut source) = store();
    create_conversation(&mut source, "char", "chat");
    save(
        &mut source,
        vec![mutation(&["root", "language"], serde_json::json!("ko"))],
    );
    let binding = detach_for_new_authority(&mut source);
    let issued = source.lww_clock_state().unwrap().issued;
    let header = Header {
        binding_authority: binding,
        request_id: "explicit-init".into(),
    };
    let mut cursor = None;
    let mut exported = Vec::new();
    loop {
        let page = source
            .lww_queue_unit_state_page(&header, cursor.as_ref(), 1)
            .unwrap();
        exported.extend(page.entries);
        cursor = page.after_key;
        if !page.has_more {
            break;
        }
    }
    assert_eq!(source.lww_clock_state().unwrap().issued, issued);
    let out = source.lww_read_outbox(binding, 100).unwrap().entries;
    assert_eq!(out.len(), exported.len());
    for entry in out {
        let state = exported
            .iter()
            .find(|state| state.key == entry.key)
            .unwrap();
        assert_eq!(entry.stamp, state.stamp);
        assert_eq!(entry.value, state.value);
        assert_eq!(entry.version, state.version);
    }
    clear_outbox(&mut source);
    let page = source
        .lww_queue_unit_state_page(&header, None, 100)
        .unwrap();
    assert!(!page.entries.is_empty());
    assert!(source
        .lww_read_outbox(binding, 100)
        .unwrap()
        .entries
        .is_empty());
}

#[test]
fn future_clock_admission_blocks_transport_but_local_messages_keep_committing() {
    let (_source_directory, mut source) = store();
    let (_target_directory, mut target) = store();
    create_conversation(&mut source, "char", "chat");
    let future = stamp(device_store::now_ms().unwrap() as u64 + 1_000_000);
    source
        .device_store()
        .unwrap()
        .connection()
        .execute(
            "UPDATE lww_clock SET issued=?1",
            [serde_json::to_string(&future).unwrap()],
        )
        .unwrap();
    for (position, text) in [(0, "before"), (1, "after")] {
        source
            .commit(&WorkingSetCommit {
                expected_revision: source.revision().unwrap(),
                conversations: Some(vec![super::super::ConversationMutation::ReplaceRange {
                    character_id: "char".into(),
                    conversation_id: "chat".into(),
                    start: position,
                    delete_count: 0,
                    messages: vec![serde_json::json!({"data":text})],
                    conversation: None,
                    configured_index: None,
                }]),
                ..Default::default()
            })
            .unwrap();
        if position == 0 {
            copy_objects(&source, &target);
            let entries = source.lww_read_outbox(0.into(), 100).unwrap().entries;
            let revision = target.revision().unwrap();
            assert!(target
                .lww_stage_receive(&StageReceive {
                    header: Header {
                        binding_authority: 0.into(),
                        request_id: "future-denied".into()
                    },
                    changes: entries
                        .into_iter()
                        .map(|entry| Change {
                            key: entry.key,
                            stamp: entry.stamp,
                            value: entry.value
                        })
                        .collect(),
                    progress: Progress {
                        kind: "external".into(),
                        cursor: 1.into(),
                        writer_id: None
                    },
                    admitted_time_upper_ms: DecimalU64(device_store::now_ms().unwrap() as u64),
                })
                .is_err());
            assert_eq!(target.revision().unwrap(), revision);
        }
    }
    let conversation = source
        .read_conversation("char", "chat", None)
        .unwrap()
        .unwrap()
        .value;
    assert_eq!(conversation["message"].as_array().unwrap().len(), 2);
    assert_eq!(conversation["message"][1]["data"], "after");
    assert!(source
        .lww_read_outbox(0.into(), 100)
        .unwrap()
        .entries
        .iter()
        .any(|entry| entry.key.components()[0] == "messages"
            && entry.stamp.physical_ms.0 >= future.physical_ms.0));
}

fn new_device_stage(store: &mut PersistentStore) -> (Header, String, Vec<Change>) {
    use super::super::sync_selection::SyncTarget;
    let target = SyncTarget::Server("fresh-connection".into());
    let state = store.lww_binding_state().unwrap();
    let inspection = store
        .register_lww_binding_inspection(state.target_authority, &target, "target", "library")
        .unwrap();
    let changes = vec![change(&["root", "language"], 10, serde_json::json!("en"))];
    let request_id = Uuid::new_v4().to_string();
    let header = Header { binding_authority:state.target_authority,request_id:request_id.clone() };
    let staging = store.lww_stage_binding_units(&header,&inspection,&changes,10.into()).unwrap().staging_id;
    (
        Header {
            binding_authority: state.target_authority,
            request_id,
        },
        staging,
        changes,
    )
}
#[test]
fn adopting_a_fresh_writer_keeps_the_clock_unsent_versions_and_receive_progress() {
    let (_store_directory, mut store) = store();
    save(&mut store, vec![mutation(&["root", "language"], serde_json::json!("ko"))]);
    let authority = store.lww_binding_authority().unwrap();
    store
        .device_store()
        .unwrap()
        .connection()
        .execute(
            "INSERT INTO lww_progress VALUES('server','','9',?1)",
            [authority.0.to_string()],
        )
        .unwrap();
    let before = store.lww_clock_state().unwrap();
    let unsent = |store: &PersistentStore| -> Vec<(UnitKey, Stamp)> {
        store
            .lww_read_outbox(authority, 256)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| (entry.key, entry.stamp))
            .collect()
    };
    let retained = unsent(&store);
    assert_eq!(retained.len(), 1);
    let fresh = Uuid::new_v4().to_string();
    let refused = |result: StoreResult<()>| match result {
        Err(StoreError::Validation { message }) => message,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        refused(store.lww_adopt_fresh_writer(DecimalU64(authority.0 + 1), &before.writer_id, &fresh)),
        "binding-authority-changed"
    );
    assert_eq!(
        refused(store.lww_adopt_fresh_writer(authority, &fresh, &Uuid::new_v4().to_string())),
        "fresh-writer-changed"
    );
    for _ in 0..2 {
        store.lww_adopt_fresh_writer(authority, &before.writer_id, &fresh).unwrap();
    }
    let after = store.lww_clock_state().unwrap();
    assert_eq!(after.writer_id, fresh);
    assert_eq!((after.issued.as_ref(), after.accepted.as_ref()), (before.issued.as_ref(), before.accepted.as_ref()));
    assert_eq!(after.binding_authority, authority);
    assert_eq!(unsent(&store), retained);
    assert_eq!(store.lww_binding_state().unwrap().progress, serde_json::json!([{"kind": "server", "writerId": "", "cursor": "9"}]));
    save(&mut store, vec![mutation(&["root", "askRemoval"], serde_json::json!(true))]);
    let next = unsent(&store);
    let edit = next.iter().find(|(key, _)| key.components()[1] == "askRemoval").unwrap();
    assert_eq!(edit.1.writer_id, fresh);
    assert!(edit.1.physical_ms >= before.issued.unwrap().physical_ms);
    assert!(next.contains(&retained[0]));
}
#[test]
fn explicit_new_device_replaces_before_identity_reset_and_completed_retry_preserves_later_edits() {
    use super::super::device_store::plugin_values::PluginDeviceMutation;
    let (_store_directory, mut store) = store();
    save(
        &mut store,
        vec![mutation(&["root", "language"], serde_json::json!("old"))],
    );
    let (header, staging, _) = new_device_stage(&mut store);
    let before = store.lww_clock_state().unwrap();
    let accepted = stamp(500);
    let db = store.device_store().unwrap().connection();
    db.execute(
        "UPDATE lww_clock SET accepted=?1",
        [serde_json::to_string(&accepted).unwrap()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO lww_unpublished_proofs VALUES('old-publication',?1,'[]')",
        [header.binding_authority.0.to_string()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO lww_progress VALUES('external','','9',?1)",
        [header.binding_authority.0.to_string()],
    )
    .unwrap();
    store
        .device_store_mut()
        .unwrap()
        .write_setting("hub", &serde_json::json!("local-setting"))
        .unwrap();
    store
        .device_store_mut()
        .unwrap()
        .write_plugin_device_values(
            "orphan",
            &[
                PluginDeviceMutation::Set {
                    space: "json".into(),
                    key: "old".into(),
                    value: "null".into(),
                },
                PluginDeviceMutation::Set {
                    space: "string".into(),
                    key: "old".into(),
                    value: "old".into(),
                },
            ],
        )
        .unwrap();
    let preparation = store.prepare_lww_new_device(&header, &staging).unwrap();
    assert_eq!(
        store.prepare_lww_new_device(&header, &staging).unwrap(),
        preparation
    );
    assert!(store
        .lww_replace_target_as_new_device(&header, &staging, &preparation.authorization_id)
        .is_err());
    assert_eq!(store.lww_clock_state().unwrap().writer_id, before.writer_id);
    store
        .authorize_lww_new_device(&preparation.authorization_id)
        .unwrap();
    let guard = store.acquire_intent_admission().unwrap();
    assert!(matches!(store.lww_replace_target_as_new_device(&header, &staging, &preparation.authorization_id), Err(StoreError::CommitBusy)));
    assert!(store.lww_request_unreserved(&header.request_id).unwrap());
    assert_eq!(store.lww_clock_state().unwrap().writer_id, before.writer_id);
    drop(guard);
    let result = store
        .lww_replace_target_as_new_device(&header, &staging, &preparation.authorization_id)
        .unwrap();
    assert_ne!(result.writer_id, before.writer_id);
    assert_eq!(result.writer_id, preparation.writer_id);
    assert_eq!(result.binding_authority.0, header.binding_authority.0 + 1);
    let after = store.lww_clock_state().unwrap();
    assert_eq!(after.writer_id, result.writer_id);
    assert_eq!(after.accepted, Some(accepted));
    assert!(after.issued.is_none());
    assert_eq!(store.read_root(None).unwrap().value["language"], "en");
    assert_eq!(
        store.lww_binding_state().unwrap().target_authority,
        result.binding_authority
    );
    assert_eq!(
        store.lww_binding_state().unwrap().library_id,
        Some("library".into())
    );
    assert_eq!(
        store.device_store().unwrap().read_setting("hub").unwrap(),
        Some(serde_json::json!("local-setting"))
    );
    assert_eq!(
        store
            .device_store()
            .unwrap()
            .connection()
            .query_row("SELECT count(*) FROM plugin_device_storage", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .device_store()
            .unwrap()
            .connection()
            .query_row(
                "SELECT count(*) FROM lww_unpublished_proofs WHERE proof_id='old-publication'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert!(store
        .lww_receive_progress(result.binding_authority)
        .unwrap()
        .is_empty());
    assert!(store.lww_read_outbox(header.binding_authority, 10).is_err());
    store
        .device_store_mut()
        .unwrap()
        .write_plugin_device_values(
            "orphan",
            &[PluginDeviceMutation::Set {
                space: "string".into(),
                key: "later".into(),
                value: "kept".into(),
            }],
        )
        .unwrap();
    save(
        &mut store,
        vec![mutation(&["root", "language"], serde_json::json!("later"))],
    );
    let current = store.lww_clock_state().unwrap();
    let revision = store.revision().unwrap();
    let guard = store.acquire_intent_admission().unwrap();
    assert_eq!(
        store
            .lww_replace_target_as_new_device(&header, &staging, &preparation.authorization_id)
            .unwrap(),
        result
    );
    drop(guard);
    assert_eq!(store.revision().unwrap(), revision);
    assert_eq!(store.lww_clock_state().unwrap().issued, current.issued);
    assert_eq!(store.read_root(None).unwrap().value["language"], "later");
    assert_eq!(
        store
            .device_store()
            .unwrap()
            .connection()
            .query_row(
                "SELECT value FROM plugin_device_storage WHERE key='later'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "kept"
    );
    assert!(store
        .lww_replace_target_as_new_device(&header, "different-stage", &preparation.authorization_id)
        .is_err());
}
#[test]
fn explicit_new_device_recovery_finishes_after_library_activation_without_exposing_old_edits() {
    let (directory, mut store) = store();
    save(
        &mut store,
        vec![mutation(&["root", "language"], serde_json::json!("old"))],
    );
    let (header, staging, changes) = new_device_stage(&mut store);
    let mut delayed = PersistentStore::open(directory.path()).unwrap();
    let mut receiving = PersistentStore::open(directory.path()).unwrap();
    let preparation = store.prepare_lww_new_device(&header, &staging).unwrap();
    store
        .authorize_lww_new_device(&preparation.authorization_id)
        .unwrap();
    let state = store.lww_clock_state().unwrap();
    let writer_id = preparation.writer_id.clone();
    let selection_change=serde_json::from_str::<super::super::sync_selection::BindingSelectionChange>(&store.device_store().unwrap().connection().query_row("SELECT selection_change FROM lww_new_device_authorizations WHERE authorization_id=?1",[&preparation.authorization_id],|r|r.get::<_,String>(0)).unwrap()).unwrap();
    let new_authority = DecimalU64(header.binding_authority.0 + 1);
    let intent = Intent::NewDevice {
        device_revision: device_revision(store.device_store().unwrap().connection()).unwrap(),
        authorization_id: preparation.authorization_id.clone(),
        selection_change: selection_change.clone(),
        staging_id: staging.clone(),
        changes: changes.clone(),
        old_writer_id: state.writer_id.clone(),
        writer_id: writer_id.clone(),
        new_authority,
    };
    let body = serde_json::to_string(&intent).unwrap();
    let digest = risunest_sync_wire::hash(body.as_bytes());
    let stamp = state.issued.unwrap();
    store
        .device_store()
        .unwrap()
        .connection()
        .execute(
            "INSERT INTO lww_intents VALUES(?1,?2,?3,?4,?5,0)",
            params![
                header.request_id,
                header.binding_authority.0.to_string(),
                serde_json::to_string(&stamp).unwrap(),
                body,
                digest
            ],
        )
        .unwrap();
    let captured: (String, String, String, String) = store.device_store().unwrap().connection().query_row(
        "SELECT authority,stamp,body,digest FROM lww_intents WHERE request_id=?1", [&header.request_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap();
    let guard = store.acquire_intent_admission().unwrap();
    assert!(matches!(delayed.lww_recover_intents(), Err(StoreError::CommitBusy)));
    assert_admission_begin_busy(&mut receiving);
    drop(guard);
    let activation = commit::replace_commit_lww(
        &mut store.connection,
        &store.repository_root,
        &staging,
        &header,
        &stamp,
        &digest,
        &[],
        true,
        &changes,
        Some(new_authority),
        Some(&selection_change),
        None,
    )
    .unwrap();
    assert_eq!(store.lww_clock_state().unwrap().writer_id, state.writer_id);
    drop(store);
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let result = store
        .lww_replace_target_as_new_device(&header, &staging, &preparation.authorization_id)
        .unwrap();
    assert_eq!(result.revision, activation.revision);
    assert_eq!(result.writer_id, writer_id);
    assert_eq!(result.binding_authority, new_authority);
    assert_eq!(store.lww_clock_state().unwrap().writer_id, writer_id);
    assert_eq!(store.read_root(None).unwrap().value["language"], "en");
    assert!(store
        .lww_read_outbox(new_authority, 100)
        .unwrap()
        .entries
        .is_empty());

    use crate::persistent_store::receive_batches as batches;
    let page = StageReceive { header: Header { binding_authority: new_authority, request_id: "after-new-device".into() },
        changes: vec![change(&["root", "language"], 20, serde_json::json!("later"))],
        progress: Progress { kind: "server".into(), cursor: 9.into(), writer_id: None }, admitted_time_upper_ms: u64::MAX.into() };
    receiving.lww_stage_receive(&page).unwrap();
    let source = batches::Source::Staged { header: page.header.clone() };
    let begin = batches::BeginRequest { candidate: receiving.prepare_native_receive(&source).unwrap(), source, native_baseline: None };
    let admitted = serde_json::to_value(receiving.begin_local_native_receive(&begin).unwrap()).unwrap();
    let rows = receiving.lww_receive_row_counts(&page.header.request_id).unwrap();
    let root = receiving.read_root(None).unwrap();
    let progress = serde_json::to_value(receiving.lww_receive_progress(new_authority).unwrap()).unwrap();
    let proof = admission_intent_state(&receiving, &header.request_id);
    for fault in ["none", "corrupt", "missing"] {
        if fault == "corrupt" {
            delayed.connection.execute("UPDATE lww_requests SET digest='synthetic-new-device-corruption' WHERE request_id=?1", [&header.request_id]).unwrap();
        } else if fault == "missing" {
            delayed.connection.execute("DELETE FROM lww_requests WHERE request_id=?1", [&header.request_id]).unwrap();
        }
        let result = delayed.recover_intent(header.binding_authority, &header.request_id,
            captured.0.clone(), &captured.1, &captured.2, &captured.3);
        if fault == "none" { result.unwrap(); }
        else {
            let expected = if fault == "missing" { "request-receipt-missing" } else { "request-id-integrity" };
            assert!(matches!(result, Err(StoreError::Validation { message }) if message == expected));
        }
        assert_eq!(admission_intent_state(&receiving, &header.request_id), proof);
        assert_eq!(receiving.lww_receive_row_counts(&page.header.request_id).unwrap(), rows);
        assert_eq!(serde_json::to_value(receiving.begin_local_native_receive(&begin).unwrap()).unwrap()["state"], admitted["state"]);
        assert_eq!(serde_json::to_value(receiving.lww_receive_progress(new_authority).unwrap()).unwrap(), progress);
        assert_eq!(receiving.read_root(None).unwrap().value, root.value);
        assert_eq!(receiving.revision().unwrap(), root.revision);
        assert_eq!(receiving.lww_clock_state().unwrap().writer_id, writer_id);
        assert_eq!(receiving.lww_binding_authority().unwrap(), new_authority);
        assert!(receiving.lww_quarantined_intents().unwrap().is_empty());
    }
}

#[test]
fn explicit_new_device_rejects_mutated_or_unattested_stage_without_identity_change() {
    let (_store_directory, mut store) = store();
    let stage = store.replace_begin().unwrap().staging_id;
    store
        .replace_put_root(&stage, &serde_json::json!({}))
        .unwrap();
    let before = store.lww_clock_state().unwrap();
    assert!(store
        .lww_replace_target_as_new_device(
            &Header {
                binding_authority: before.binding_authority,
                request_id: "unattested".into()
            },
            &stage,
            "missing"
        )
        .is_err());
    assert_eq!(store.lww_clock_state().unwrap().writer_id, before.writer_id);
    let (header, staging, _) = new_device_stage(&mut store);
    let preparation = store.prepare_lww_new_device(&header, &staging).unwrap();
    store
        .authorize_lww_new_device(&preparation.authorization_id)
        .unwrap();
    store
        .replace_put_root(&staging, &serde_json::json!({"language":"changed"}))
        .unwrap();
    let before = store.lww_clock_state().unwrap();
    assert!(store
        .lww_replace_target_as_new_device(&header, &staging, &preparation.authorization_id)
        .is_err());
    let after = store.lww_clock_state().unwrap();
    assert_eq!(after.writer_id, before.writer_id);
    assert_eq!(after.binding_authority, before.binding_authority);
    assert_eq!(after.issued, before.issued);
}

#[test]
fn explicit_new_device_empty_target_activates_empty_stage_without_republishing_local_units() {
    use super::super::sync_selection::SyncTarget;
    let (_store_directory, mut store) = store();
    save(
        &mut store,
        vec![mutation(&["root", "language"], serde_json::json!("local"))],
    );
    let state = store.lww_binding_state().unwrap();
    let target = SyncTarget::Server("empty-target".into());
    let inspection = store
        .register_lww_binding_inspection(state.target_authority, &target, "target", "empty-library")
        .unwrap();
    let header = Header {
        binding_authority: state.target_authority,
        request_id: Uuid::new_v4().to_string(),
    };
    let staging = store.lww_stage_binding_units(&header,&inspection,&[],0.into()).unwrap().staging_id;
    let preparation = store.prepare_lww_new_device(&header, &staging).unwrap();
    assert_eq!(
        store.lww_binding_state().unwrap().target_authority,
        state.target_authority
    );
    assert_eq!(store.read_root(None).unwrap().value["language"], "local");
    store
        .authorize_lww_new_device(&preparation.authorization_id)
        .unwrap();
    let result = store
        .lww_replace_target_as_new_device(&header, &staging, &preparation.authorization_id)
        .unwrap();
    assert_eq!(result.writer_id, preparation.writer_id);
    assert!(store
        .read_root(None)
        .unwrap()
        .value
        .get("language")
        .is_none());
    assert_eq!(store.lww_binding_state().unwrap().target, target);
    assert!(store
        .lww_read_outbox(result.binding_authority, 100)
        .unwrap()
        .entries
        .is_empty());
    assert!(store
        .lww_read_unit_state(result.binding_authority, None, 100)
        .unwrap()
        .entries
        .is_empty());
}

#[test]
fn an_intent_that_can_no_longer_apply_is_closed_and_later_writes_proceed() {
    let (dir, mut store) = store();
    save(&mut store, vec![mutation(&["root", "language"], serde_json::json!("en"))]);
    let revision = store.revision().unwrap();
    let stale = Header { binding_authority: 0.into(), request_id: "stale-commit".into() };
    let intent = Intent::Commit {
        commit: WorkingSetCommit {
            expected_revision: revision - 1,
            unit_mutations: Some(vec![mutation(&["root", "language"], serde_json::json!("ko"))]),
            ..Default::default()
        },
        aliases: vec![],
    };
    store.reserve_intent(&stale, &intent).unwrap();
    save(&mut store, vec![mutation(&["root", "language"], serde_json::json!("ja"))]);
    assert_eq!(store.read_root(None).unwrap().value["language"], "ja");
    let kept: bool = store.device_store().unwrap().connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM lww_intents WHERE request_id=?1)", [&stale.request_id], |row| row.get(0),
    ).unwrap();
    assert!(!kept);
    let committed: bool = store.connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM lww_requests WHERE request_id=?1)", [&stale.request_id], |row| row.get(0),
    ).unwrap();
    assert!(!committed);
    drop(store);
    let store = PersistentStore::open(dir.path()).unwrap();
    assert_eq!(store.read_root(None).unwrap().value["language"], "ja");
}

#[test]
fn a_character_field_edit_does_not_reproject_the_character_order() {
    let (_store_directory, mut store) = store();
    save(&mut store, vec![
        mutation(&["exists", "character", "a"], serde_json::json!({"type":"character"})),
        mutation(&["exists", "character", "b"], serde_json::json!({"type":"character"})),
        mutation(&["order", "characters"], serde_json::json!(["b", "a"])),
    ]);
    assert_eq!(store.read_root(None).unwrap().value["characterOrder"], serde_json::json!(["b", "a"]));
    store.connection.execute_batch("CREATE TEMP TABLE root_writes(n INTEGER);
        CREATE TEMP TRIGGER count_root_writes AFTER UPDATE ON main.root BEGIN INSERT INTO root_writes VALUES(1); END;").unwrap();
    save(&mut store, vec![mutation(&["character", "a", "name"], serde_json::json!("renamed"))]);
    let writes: i64 = store.connection.query_row("SELECT count(*) FROM root_writes", [], |row| row.get(0)).unwrap();
    assert_eq!(writes, 0);
    save(&mut store, vec![mutation(&["exists", "character", "c"], serde_json::json!({"type":"character"}))]);
    assert_eq!(store.read_root(None).unwrap().value["characterOrder"], serde_json::json!(["b", "a", "c"]));
}

#[test]
fn a_drain_with_no_work_leaves_both_revisions_unchanged() {
    let (_store_directory, mut store) = store();
    let result = receive(&mut store, "held", vec![change(&["conversation", "missing", "chat", "name"], 1, serde_json::json!("held"))], vec![]);
    assert_eq!(result.held_keys.len(), 1);
    let device_revision = |store: &PersistentStore| -> i64 {
        store.device_store().unwrap().connection().query_row("SELECT revision FROM device_meta WHERE singleton=1", [], |row| row.get(0)).unwrap()
    };
    let (revision, device) = (store.revision().unwrap(), device_revision(&store));
    let header = Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: "drain".into() };
    let drained = store.lww_drain_deferred(&ApplyReceive { header, generating: vec![] }).unwrap();
    assert_eq!(drained.held_keys.len(), 1);
    assert!(drained.affected_keys.is_empty());
    assert_eq!(drained.revision, revision);
    assert_eq!(store.revision().unwrap(), revision);
    assert_eq!(device_revision(&store), device);
}

#[test]
fn ranges_for_one_conversation_in_one_commit_capture_once_from_their_pages() {
    use super::super::message_pages::{capture_manifest, reset_capture_work, take_capture_work};
    let message = |i: usize| serde_json::json!({"chatId":format!("m-{i}"),"data":format!("synthetic-{i}")});
    let range = |start: i64, delete_count: i64, messages: Vec<Value>| super::super::ConversationMutation::ReplaceRange {
        character_id: "char".into(), conversation_id: "chat".into(), start, delete_count, messages,
        conversation: None, configured_index: None,
    };
    let count = 1024usize;
    let end = count as i64;
    let shapes: Vec<(&str, Vec<super::super::ConversationMutation>, usize)> = vec![
        ("windowed", vec![range(0, 0, vec![]), range(end, 0, vec![message(count)])], 256),
        ("two appends", vec![range(end, 0, vec![message(count)]), range(end + 1, 0, vec![message(count + 1)])], 256),
        ("clamped appends", vec![range(end + 50, 3, vec![message(count)]), range(end + 50, 0, vec![message(count + 1)])], 256),
        ("edit then append", vec![range(5, 1, vec![serde_json::json!({"chatId":"m-5","data":"edited"})]), range(end, 0, vec![message(count)])], 512),
        ("delete then insert", vec![range(5, 1, vec![]), range(600, 0, vec![message(count), message(count + 1)])], 512),
        ("insert then delete", vec![range(600, 0, vec![message(count), message(count + 1)]), range(5, 2, vec![])], 512),
    ];
    for (shape, ranges, bound) in shapes {
        let (_dir, mut store) = store();
        create_conversation(&mut store, "char", "chat");
        store.commit(&WorkingSetCommit {
            expected_revision: store.revision().unwrap(),
            conversations: Some(vec![range(0, 0, (0..count).map(message).collect())]),
            ..Default::default()
        }).unwrap();
        reset_capture_work();
        store.commit(&WorkingSetCommit {
            expected_revision: store.revision().unwrap(),
            conversations: Some(ranges),
            ..Default::default()
        }).unwrap_or_else(|error| panic!("{shape}: {error:?}"));
        let work = take_capture_work();
        assert_eq!((work.successful_captures, work.failed_captures), (1, 0), "{shape}");
        assert!(work.work.messages_read <= bound, "{shape}: {work:?}");
        let key = UnitKey::new(&["messages", "char", "chat"]).unwrap();
        let stored = read_unit(&store.connection, &key).unwrap().unwrap().1;
        let tx = store.connection.transaction().unwrap();
        let generation = active_generation(&tx).unwrap();
        let fresh = capture_manifest(&tx, &generation, "char", "chat", None).unwrap();
        drop(tx);
        assert_eq!(stored, fresh, "{shape}");
    }
}

#[test]
fn superseded_message_pages_are_collected_only_after_the_grace_period() {
    let (_dir, mut store) = store();
    create_conversation(&mut store, "char", "chat");
    let message = |i: usize| serde_json::json!({"chatId":format!("m-{i}"),"data":format!("synthetic-{i}")});
    let append = |store: &mut PersistentStore, start: usize, messages: Vec<Value>| {
        store.commit(&WorkingSetCommit {
            expected_revision: store.revision().unwrap(),
            conversations: Some(vec![super::super::ConversationMutation::ReplaceRange {
                character_id: "char".into(), conversation_id: "chat".into(), start: start as i64, delete_count: 0,
                messages, conversation: None, configured_index: None,
            }]),
            ..Default::default()
        }).unwrap();
    };
    let referenced = |store: &PersistentStore| -> BTreeSet<String> {
        let mut hashes = store.connection.prepare("SELECT hash FROM message_page_indexes").unwrap()
            .query_map([], |row| row.get::<_, String>(0)).unwrap().collect::<Result<BTreeSet<_>, _>>().unwrap();
        let body: Vec<u8> = store.connection.query_row("SELECT body FROM message_page_manifests", [], |row| row.get(0)).unwrap();
        hashes.insert(risunest_sync_wire::hash(&body));
        hashes
    };
    let present = |store: &PersistentStore, hash: &str| -> bool {
        store.connection.query_row("SELECT EXISTS(SELECT 1 FROM message_page_objects WHERE hash=?1)", [hash], |row| row.get(0)).unwrap()
    };
    append(&mut store, 0, (0..40).map(message).collect());
    let before = referenced(&store);
    append(&mut store, 40, vec![message(40)]);
    let after = referenced(&store);
    let superseded = before.difference(&after).cloned().collect::<Vec<_>>();
    assert!(!superseded.is_empty());
    let now = 1_900_000_000_000i64;
    store.sweep_message_page_objects(super::super::MessageObjectStore::Library, now, super::super::MESSAGE_PAGE_SWEEP_LIMIT).unwrap();
    assert!(superseded.iter().all(|hash| present(&store, hash)));
    store.sweep_message_page_objects(super::super::MessageObjectStore::Library, now + super::super::ASSET_GC_PRODUCT_MINIMUM_GRACE_MS - 1, super::super::MESSAGE_PAGE_SWEEP_LIMIT).unwrap();
    assert!(superseded.iter().all(|hash| present(&store, hash)));
    store.sweep_message_page_objects(super::super::MessageObjectStore::Library, now + super::super::ASSET_GC_PRODUCT_MINIMUM_GRACE_MS, super::super::MESSAGE_PAGE_SWEEP_LIMIT).unwrap();
    for hash in &superseded {
        assert!(!present(&store, hash), "superseded object {hash} survived maintenance");
        let proofs: i64 = store.connection.query_row(
            "SELECT (SELECT count(*) FROM message_page_proofs WHERE hash=?1)+(SELECT count(*) FROM message_page_verified_objects WHERE hash=?1)",
            [hash], |row| row.get(0),
        ).unwrap();
        assert_eq!(proofs, 0);
    }
    assert!(after.iter().all(|hash| present(&store, hash)));
    let key = UnitKey::new(&["messages", "char", "chat"]).unwrap();
    let value = read_unit(&store.connection, &key).unwrap().unwrap().1;
    let UnitValue::Object { descriptor_hash, descriptor } = &value else { panic!() };
    assert!(present(&store, descriptor_hash) && present(&store, &descriptor.object_hash));
    super::super::message_pages::validate_manifest(&store.connection, &value).unwrap();
}

#[test]
fn the_message_object_sweep_keeps_what_held_rows_archives_and_leases_need() {
    let grace = super::super::ASSET_GC_PRODUCT_MINIMUM_GRACE_MS;
    let limit = super::super::MESSAGE_PAGE_SWEEP_LIMIT;
    let (_dir, mut store) = store();
    let message = |owner: &str, i: usize| serde_json::json!({"chatId":format!("{owner}-{i}"),"data":format!("synthetic-{owner}-{i}")});
    let append = |store: &mut PersistentStore, character: &str, start: usize, messages: Vec<Value>| {
        store.commit(&WorkingSetCommit {
            expected_revision: store.revision().unwrap(),
            conversations: Some(vec![super::super::ConversationMutation::ReplaceRange {
                character_id: character.into(), conversation_id: "chat".into(), start: start as i64, delete_count: 0,
                messages, conversation: None, configured_index: None,
            }]),
            ..Default::default()
        }).unwrap();
    };
    let held = Change {
        key: unit_key(&["messages", "waiting", "chat"]).unwrap(),
        stamp: stamp(2),
        value: incoming_message_value(&store, "waiting", "chat", "held-message"),
    };
    let staged = receive(&mut store, "held", vec![change(&["exists", "conversation", "waiting", "chat"], 2, serde_json::json!(true)), held.clone()], vec![]);
    assert!(staged.held_keys.contains(&held.key));
    create_conversation(&mut store, "archived", "chat");
    append(&mut store, "archived", 0, (0..40).map(|i| message("archived", i)).collect());
    store.archive_character("archived", store.revision().unwrap(), 10).unwrap();
    create_conversation(&mut store, "leased", "chat");
    append(&mut store, "leased", 0, (0..40).map(|i| message("leased", i)).collect());
    let lease = store.acquire_revision(store.revision().unwrap()).unwrap();
    append(&mut store, "leased", 40, vec![message("leased", 40)]);
    let leased: Vec<String> = {
        let reader = &store.revision_leases[&lease.lease].connection;
        let mut hashes = reader.prepare("SELECT hash FROM message_page_indexes WHERE character_id='leased'").unwrap()
            .query_map([], |row| row.get::<_, String>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        let body: Vec<u8> = reader.query_row("SELECT body FROM message_page_manifests WHERE character_id='leased'", [], |row| row.get(0)).unwrap();
        hashes.push(risunest_sync_wire::hash(&body));
        hashes
    };
    let present = |store: &PersistentStore, hash: &str| -> bool {
        store.connection.query_row("SELECT EXISTS(SELECT 1 FROM message_page_objects WHERE hash=?1)", [hash], |row| row.get(0)).unwrap()
    };
    let orphan = b"synthetic-unreferenced-object".to_vec();
    let orphan_hash = risunest_sync_wire::hash(&orphan);
    store.lww_put_object(&orphan_hash, &orphan).unwrap();

    let now = 1_900_000_000_000i64;
    store.sweep_message_page_objects(super::super::MessageObjectStore::Library, now, limit).unwrap();
    assert!(!store.lww_verified_object_present(&orphan_hash).unwrap());
    store.lww_put_object(&orphan_hash, &orphan).unwrap();
    assert!(store.lww_verified_object_present(&orphan_hash).unwrap());
    store.sweep_message_page_objects(super::super::MessageObjectStore::Library, now + grace, limit).unwrap();
    assert!(present(&store, &orphan_hash), "a put after marking restarts the grace period");
    assert!(leased.iter().all(|hash| present(&store, hash)), "a revision lease still reads these pages");
    store.release_revision(&lease.lease).unwrap();

    store.restore_character("archived", store.revision().unwrap()).unwrap();
    let generation = active_generation(&store.connection).unwrap();
    let restored: i64 = store.connection.query_row(
        "SELECT count(*) FROM messages WHERE generation=?1 AND character_id='archived'", [&generation], |row| row.get(0),
    ).unwrap();
    assert_eq!(restored, 40);
    let parent = receive(&mut store, "parent", vec![change(&["exists", "character", "waiting"], 3, serde_json::json!({"type":"character"}))], vec![]);
    assert!(parent.affected_keys.contains(&held.key));
    let projected: String = store.connection.query_row(
        "SELECT value FROM messages WHERE generation=?1 AND character_id='waiting'",
        [active_generation(&store.connection).unwrap()], |row| row.get(0),
    ).unwrap();
    assert!(projected.contains("held-message"));
    for character in ["archived", "leased", "waiting"] {
        let value = read_unit(&store.connection, &unit_key(&["messages", character, "chat"]).unwrap()).unwrap().unwrap().1;
        super::super::message_pages::validate_manifest(&store.connection, &value).unwrap();
    }

    store.sweep_message_page_objects(super::super::MessageObjectStore::Library, now + 2 * grace, limit).unwrap();
    store.sweep_message_page_objects(super::super::MessageObjectStore::Library, now + 3 * grace, limit).unwrap();
    assert!(!present(&store, &orphan_hash));
    let current: BTreeSet<String> = store.connection.prepare("SELECT hash FROM message_page_indexes WHERE character_id='leased'").unwrap()
        .query_map([], |row| row.get::<_, String>(0)).unwrap().collect::<Result<_, _>>().unwrap();
    assert!(leased.iter().any(|hash| !current.contains(hash)));
    for hash in leased.iter().filter(|hash| !current.contains(*hash)) {
        assert!(!present(&store, hash), "{hash} outlived its lease");
    }
}

#[test]
fn an_unreadable_reference_node_stops_the_message_object_sweep() {
    use risunest_sync_wire::descriptor::RecordDescriptor;
    let grace = super::super::ASSET_GC_PRODUCT_MINIMUM_GRACE_MS;
    let limit = super::super::MESSAGE_PAGE_SWEEP_LIMIT;
    let (_dir, mut store) = store();
    let put = |store: &PersistentStore, body: &[u8]| -> String {
        let hash = risunest_sync_wire::hash(body);
        store.lww_put_object(&hash, body).unwrap();
        hash
    };
    let present = |store: &PersistentStore, hash: &str| -> bool {
        store.connection.query_row("SELECT EXISTS(SELECT 1 FROM message_page_objects WHERE hash=?1)", [hash], |row| row.get(0)).unwrap()
    };
    let node = put(&store, b"synthetic-not-a-reference-page");
    let body = put(&store, b"synthetic-record-body");
    let orphan = put(&store, b"synthetic-unreferenced-object");
    let value = UnitValue::object(RecordDescriptor {
        object_hash: body.clone(), dependency_root: Some(node.clone()), relation_root: None,
        dependencies: vec![], relations: vec![], scopes: vec![],
    }).unwrap();
    store.connection.execute(
        "INSERT INTO snapshot_original_units(key,value) VALUES('synthetic',?1)", [serde_json::to_string(&value).unwrap()],
    ).unwrap();
    let now = 1_900_000_000_000i64;
    assert!(store.sweep_message_page_objects(super::super::MessageObjectStore::Library, now, limit).is_err());
    assert!(store.sweep_message_page_objects(super::super::MessageObjectStore::Library, now + grace, limit).is_err());
    let marks: i64 = store.connection.query_row("SELECT count(*) FROM message_page_object_marks", [], |row| row.get(0)).unwrap();
    assert_eq!(marks, 0);
    assert!([&node, &body, &orphan].iter().all(|hash| present(&store, hash)));
    store.connection.execute("DELETE FROM snapshot_original_units", []).unwrap();
    store.sweep_message_page_objects(super::super::MessageObjectStore::Library, now + 2 * grace, limit).unwrap();
    store.sweep_message_page_objects(super::super::MessageObjectStore::Library, now + 3 * grace, limit).unwrap();
    assert!([&node, &body, &orphan].iter().all(|hash| !present(&store, hash)));
}

#[test]
fn gallery_pending_guard_survives_intent_recovery_and_acknowledges_committed_deletes() {
    for committed in [false, true] {
        let (dir, mut store) = store();
        let attachment: crate::persistent_store::AssetAlias = serde_json::from_value(serde_json::json!({
            "key":"guarded", "objectHash":null, "kind":"inlay", "size":0, "mime":"image/png",
            "name":"guarded", "ext":"png", "inlayType":"image", "metadata":{}
        })).unwrap();
        store.commit_asset_alias(&attachment, store.revision().unwrap()).unwrap();
        let revision = store.revision().unwrap();
        let header = Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: "guarded-gallery-delete".into() };
        let input = WorkingSetCommit {
            expected_revision: revision,
            gallery_pending_fence: Some(crate::persistent_store::gallery::pending_fence(&store.connection).unwrap()),
            unit_mutations: Some(vec![UnitMutation::Delete { key: unit_key(&["inlay", "guarded"]).unwrap() }]),
            ..Default::default()
        };
        let (issued, digest) = store.reserve_intent(&header, &Intent::for_commit(&input, &[])).unwrap();
        if committed { commit::commit_lww(&mut store.connection, &input, &[], &header, &issued, &digest).unwrap(); }
        let mut peer = store.open_native_job_store().unwrap();
        receive(&mut peer, "guarded-pending", vec![change(&["character", "missing", "desc"], 1, serde_json::json!("{{inlay::guarded}}"))], vec![]);
        assert_eq!(peer.revision().unwrap(), revision + i64::from(committed));
        if !committed {
            assert!(matches!(commit::commit_lww(&mut store.connection, &input, &[], &header, &issued, &digest), Err(StoreError::RevisionConflict {..})));
            assert!(store.read_asset_alias("inlay", "guarded", None).unwrap().is_some());
        }
        drop(peer);
        drop(store);
        let mut reopened = PersistentStore::open(dir.path()).unwrap();
        reopened.lww_recover_intents().unwrap();
        assert_eq!(reopened.revision().unwrap(), revision + i64::from(committed));
        assert_eq!(reopened.read_asset_alias("inlay", "guarded", None).unwrap().is_none(), committed);
        assert_eq!(reopened.lww_request_recorded(&header.request_id).unwrap(), committed);
        let pending: bool = reopened.device_store().unwrap().connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM lww_intents WHERE request_id=?1)", [&header.request_id], |row| row.get(0),
        ).unwrap();
        assert!(!pending);
    }
}

// Frozen serde declarations from app-v1.0.6. The downgrade contract depends on
// that reader rejecting the new intent kind before it can replay a deletion.
#[allow(dead_code)]
mod released_app_v1_0_6_intents {
    use super::*;
    use crate::persistent_store::{AssetOwnerHead, ConversationMutation, PluginStorageMutation, RootMutation};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "camelCase")]
    pub(super) enum Intent<'a> {
        Commit {
            commit: WorkingSetCommit,
            aliases: Vec<crate::persistent_store::AssetAlias>,
        },
        PluginClaim {
            owner: String,
            key: String,
            import_batch_id: String,
            assigned_at: i64,
            expected_revision: i64,
        },
        PluginAssign {
            sources: Vec<(String, String)>,
            to_owner: String,
            collision: commit::AssignCollision,
            assigned_at: i64,
            expected_revision: i64,
        },
        Replacement {
            device_revision: i64,
            staging_id: String,
            base_revision: i64,
            staging_digest: String,
            /// Rows of the changes, each marked when a source unit replaced the
            /// value captured from the stage.
            changes: intent_rows::IntentRows,
            /// Recognizes a retry with the same source units; nothing replays them.
            source_units: Option<intent_rows::IntentRows>,
            device_sections: Option<Cow<'a, device_store::sections::FrozenBackupSections>>,
            /// Fixed settings and permissions do not advance device_revision.
            device_settings_digest: Option<String>,
            device_changes: Cow<'a, [(UnitKey, UnitValue)]>,
        },
        Target {
            device_revision: i64,
            staging_id: String,
            changes: intent_rows::IntentRows,
        },
        NewDevice {
            device_revision: i64,
            authorization_id: String,
            staging_id: String,
            changes: Vec<Change>,
            old_writer_id: String,
            writer_id: String,
            new_authority: DecimalU64,
            selection_change: crate::persistent_store::sync_selection::BindingSelectionChange,
        },
        Repair {
            entries: Vec<OutboxEntry>,
        },
        Archive {
            character_id: String,
            expected_revision: i64,
            at_ms: i64,
            restore: bool,
            incoming: Option<Change>,
        },
        Switch {
            device_revision: i64,
            change: crate::persistent_store::sync_selection::BindingSelectionChange,
            new_authority: DecimalU64,
        },
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct WorkingSetCommit {
        pub(crate) expected_revision: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) root: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) root_mutations: Option<Vec<RootMutation>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) replace_presets: Option<Vec<Value>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) character: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) character_details: Option<Vec<Value>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) replace_character: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) add_character: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) conversations: Option<Vec<ConversationMutation>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) delete_character_ids: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) plugin_storage: Option<Vec<PluginStorageMutation>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) asset_owner_heads: Option<Vec<AssetOwnerHead>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) unit_mutations: Option<Vec<crate::persistent_store::lww::UnitMutation>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) messages_changed: Option<Vec<crate::persistent_store::lww::MessageLocator>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) binding_authority: Option<risunest_sync_wire::stamp::DecimalU64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) request_id: Option<String>,
    }
}

#[test]
fn released_reader_rejects_guarded_gallery_intents_without_changing_ordinary_commits() {
    use crate::persistent_store::hash_work::{reset_commit_intent_inputs, take_commit_intent_inputs};
    let (_dir, mut store) = store();
    for guarded in [false, true] {
        let input = WorkingSetCommit {
            expected_revision: store.revision().unwrap(),
            gallery_pending_fence: guarded.then(|| crate::persistent_store::gallery::pending_fence(&store.connection).unwrap()),
            unit_mutations: Some(vec![UnitMutation::Delete { key: unit_key(&["inlay", "synthetic"]).unwrap() }]),
            ..Default::default()
        };
        reset_commit_intent_inputs();
        store.commit(&input).unwrap();
        let bodies = take_commit_intent_inputs();
        assert_eq!(bodies.len(), 1);
        let body = String::from_utf8(bodies.into_iter().next().unwrap()).unwrap();
        let released = serde_json::from_str::<released_app_v1_0_6_intents::Intent>(&body);
        if guarded {
            assert!(released.unwrap_err().to_string().contains("unknown variant"));
            // The old reader accepts the guard as an unknown field on ordinary Commit.
            let unsafe_body = serde_json::to_string(&Intent::Commit { commit: input, aliases: vec![] }).unwrap();
            assert!(serde_json::from_str::<released_app_v1_0_6_intents::Intent>(&unsafe_body).is_ok());
        } else {
            let old_body = serde_json::to_string(&released.unwrap()).unwrap();
            assert_eq!(old_body, body);
            assert!(serde_json::from_str::<Intent>(&old_body).is_ok());
        }
    }
}

#[test]
fn reconciled_archive_units_restore_on_a_peer_using_the_existing_receive_path() {
    use crate::persistent_store::archive_reconcile::{ArchiveReconcileRequest, ArchiveReconcileResult};
    let (_source_dir, mut source) = store();
    let (_target_dir, mut target) = store();
    create_conversation(&mut source, "char", "chat");
    create_conversation(&mut target, "char", "chat");
    save(&mut source, vec![mutation(&["character", "char", "privateOwn"], serde_json::json!("source-only"))]);
    save(&mut target, vec![mutation(&["character", "char", "privateOwn"], serde_json::json!("target-only"))]);
    let lease = source.acquire_revision(source.revision().unwrap()).unwrap().lease;
    let histories = source.archive_reconcile_histories(&lease, "char", &["chat".into()]).unwrap();
    source.release_revision(&lease).unwrap();
    source.archive_character("char", source.revision().unwrap(), 10).unwrap();
    target.archive_character("char", target.revision().unwrap(), 10).unwrap();
    clear_outbox(&mut source);
    let binding = source.lww_binding_state().unwrap();
    let request = ArchiveReconcileRequest {
        header: Header { binding_authority: binding.target_authority, request_id: "peer-archive-reconcile".into() },
        expected_revision: source.revision().unwrap(), selection_epoch: binding.selection_epoch,
        character_id: "char".into(), histories,
        unit_mutations: vec![mutation(&["character", "char", "name"], serde_json::json!("shared edit")),
            mutation(&["character", "char", "privateOwn"], serde_json::json!("source-edited"))],
        conversations: vec![crate::persistent_store::ConversationMutation::ReplaceRange {
            character_id: "char".into(), conversation_id: "chat".into(), start: 0, delete_count: 0,
            messages: vec![serde_json::json!({"chatId":"saved-output","role":"char","data":"saved generation output","futureField":{"keep":true}})],
            conversation: None, configured_index: None,
        }], asset_owner_heads: vec![],
    };
    assert!(matches!(source.reconcile_archived_character(&request, &|| false).unwrap(), ArchiveReconcileResult::Committed { .. }));
    let changes = source.lww_read_outbox(binding.target_authority, 100).unwrap().entries.into_iter()
        .map(|entry| Change { key: entry.key, stamp: entry.stamp, value: entry.value }).collect::<Vec<_>>();
    assert!(changes.iter().any(|change| change.key.components()[0] == "archive"));
    assert!(changes.iter().any(|change| change.key.components()[0] == "messages"));
    assert!(!changes.iter().any(|change| change.key.components().last().is_some_and(|field| field == "privateOwn")));
    copy_objects(&source, &target);
    let received = receive(&mut target, "peer-reconciled-units", changes, vec![]);
    assert!(received.held_keys.contains(&unit_key(&["messages", "char", "chat"]).unwrap()));
    assert!(target.read_character("char", None).is_err());
    target.restore_character("char", target.revision().unwrap()).unwrap();
    let drained = target.lww_drain_deferred(&ApplyReceive {
        header: Header { binding_authority: target.lww_binding_authority().unwrap(), request_id: "peer-reconciled-drain".into() },
        generating: vec![],
    }).unwrap();
    assert!(drained.affected_keys.contains(&unit_key(&["messages", "char", "chat"]).unwrap()));
    let detail = target.read_character("char", None).unwrap().unwrap().value;
    assert_eq!(detail["name"], "shared edit");
    assert_eq!(detail["privateOwn"], "target-only");
    let chat = target.read_conversation("char", "chat", None).unwrap().unwrap().value;
    assert_eq!(chat["message"], serde_json::json!([{"chatId":"saved-output","role":"char","data":"saved generation output","futureField":{"keep":true}}]));
}


#[test]
fn recorded_receive_recovers_both_partitions_and_released_held_keys_after_interruption() {
    let (dir, mut store) = store();
    let held = change(&["character", "missing", "name"], 10, serde_json::json!("held remote name"));
    assert_eq!(receive(&mut store, "older-held-page", vec![held.clone()], vec![]).held_keys, vec![held.key.clone()]);
    let header = Header { binding_authority: 0.into(), request_id: "recorded-partitions".into() };
    let changes = vec![
        change(&["exists", "character", "missing"], 11, serde_json::json!({"type":"character"})),
        change(&["plugin", "synthetic", "setting"], 11, serde_json::json!("library value")),
        change(&["plugin-local", "synthetic", "string", "setting"], 11, serde_json::json!("device value")),
    ];
    store.lww_stage_receive(&StageReceive { header: header.clone(), changes, progress: Progress { kind: "server".into(), cursor: 2.into(), writer_id: None }, admitted_time_upper_ms: 100.into() }).unwrap();
    let request = ApplyReceive { header: header.clone(), generating: vec![] };
    store.device_store().unwrap().connection().execute_batch("CREATE TEMP TRIGGER fail_receive_receipt BEFORE INSERT ON device_settings WHEN NEW.key='native-server-receive-receipt.v1' BEGIN SELECT RAISE(ABORT,'synthetic partition interruption'); END").unwrap();
    assert!(store.lww_apply_receive_recorded(&request).is_err());
    let committed_revision = store.revision().unwrap();
    assert_eq!(store.read_character("missing", None).unwrap().unwrap().value["name"], "held remote name");
    drop(store);
    let mut store = PersistentStore::open(dir.path()).unwrap();
    let result = store.lww_apply_receive_recorded(&request).unwrap();
    assert_eq!(result.revision, committed_revision);
    assert!(result.affected_keys.contains(&held.key));
    assert!(result.affected_keys.contains(&unit_key(&["plugin-local", "synthetic", "string", "setting"]).unwrap()));
    assert_eq!(result.plugin_storage_changed_keys, vec![unit_key(&["plugin", "synthetic", "setting"]).unwrap()]);
    assert!(result.held_keys.is_empty());
    store.lww_finish_receive(&header).unwrap();
    drop(store);
    let mut store = PersistentStore::open(dir.path()).unwrap();
    assert_eq!(serde_json::to_value(store.lww_apply_receive_recorded(&request).unwrap()).unwrap(), serde_json::to_value(&result).unwrap());
    assert_eq!(store.revision().unwrap(), committed_revision);
    let wrong = Header { request_id: "other-page".into(), ..header.clone() };
    store.clear_native_receive_receipt(&wrong).unwrap();
    assert!(read_native_receive_receipt(&store.connection, false, &header).unwrap().is_some());
    store.device_store().unwrap().connection().execute("DELETE FROM device_settings WHERE key=?1", [NATIVE_RECEIVE_RECEIPT_SETTING]).unwrap();
    let partial = store.lww_apply_receive_recorded(&request).unwrap();
    assert!(partial.current_state_recovery_required);
    assert_eq!(partial.revision, committed_revision);
    store.clear_native_receive_receipt(&header).unwrap();
    assert!(read_native_receive_receipt(&store.connection, false, &header).unwrap().is_none());
    assert!(read_native_receive_receipt(store.device_store().unwrap().connection(), true, &header).unwrap().is_none());
    let recovered = store.lww_apply_receive_recorded(&request).unwrap();
    assert!(recovered.current_state_recovery_required);
    assert_eq!(recovered.revision, committed_revision);
    assert!(recovered.affected_keys.is_empty());
    assert_eq!(store.read_character("missing", None).unwrap().unwrap().value["name"], "held remote name");
}

#[test]
fn recorded_receive_noop_keeps_revisions_and_preserves_deferred_result_on_retry() {
    let (_dir, mut store) = store();
    create_conversation(&mut store, "char", "chat");
    let time = store.lww_clock_state().unwrap().issued.unwrap().physical_ms.0 + 1;
    let messages = Change { key: unit_key(&["messages", "char", "chat"]).unwrap(), stamp: stamp(time), value: incoming_message_value(&store, "char", "chat", "deferred recorded message") };
    let header = Header { binding_authority: 0.into(), request_id: "recorded-deferred".into() };
    store.lww_stage_receive(&StageReceive { header: header.clone(), changes: vec![messages.clone()], progress: Progress { kind: "server".into(), cursor: 1.into(), writer_id: None }, admitted_time_upper_ms: u64::MAX.into() }).unwrap();
    let revision = store.revision().unwrap();
    let request = ApplyReceive { header: header.clone(), generating: vec![MessageLocator { character_id: "char".into(), conversation_id: "chat".into(), start: None }] };
    let result = store.lww_apply_receive_recorded(&request).unwrap();
    assert_eq!(result.revision, revision);
    assert_eq!(result.deferred_keys, vec![messages.key]);
    assert!(result.affected_keys.is_empty());
    let retry = store.lww_apply_receive_recorded(&ApplyReceive { header: header.clone(), generating: vec![] }).unwrap();
    let mut replay = result.clone();
    replay.current_state_recovery_required = true;
    assert_eq!(serde_json::to_value(retry).unwrap(), serde_json::to_value(replay).unwrap());
    assert_eq!(store.revision().unwrap(), revision);
    store.lww_finish_receive(&header).unwrap();
    let drained = store.lww_drain_deferred(&ApplyReceive { header: Header { request_id: "drain-recorded".into(), ..header }, generating: vec![] }).unwrap();
    assert!(!drained.affected_keys.is_empty());
    assert!(drained.deferred_keys.is_empty());
}


#[test]
fn recorded_receive_recovers_archive_effect_after_its_separate_commit() {
    let (_source_directory, mut source) = store();
    let (directory, mut target) = store();
    create_conversation(&mut source, "char", "chat");
    create_conversation(&mut target, "char", "chat");
    clear_outbox(&mut source);
    source.archive_character("char", source.revision().unwrap(), 10).unwrap();
    copy_objects(&source, &target);
    let archive = source.lww_read_outbox(0.into(), 100).unwrap().entries.remove(0);
    let time = device_store::now_ms().unwrap() as u64 + 100_000;
    receive(&mut target, "before-recorded-restore", vec![Change { key: archive.key.clone(), stamp: stamp(time), value: archive.value }], vec![]);
    let header = Header { binding_authority: 0.into(), request_id: "recorded-restore".into() };
    target.lww_stage_receive(&StageReceive { header: header.clone(), changes: vec![Change { key: archive.key.clone(), stamp: stamp(time + 1), value: UnitValue::Deleted }], progress: Progress { kind: "server".into(), cursor: 2.into(), writer_id: None }, admitted_time_upper_ms: u64::MAX.into() }).unwrap();
    target.connection.execute_batch("CREATE TEMP TRIGGER fail_library_receive_receipt BEFORE UPDATE ON app_kv WHEN NEW.key='native-server-receive-receipt.v1' AND json_type(NEW.value,'$.result')='object' BEGIN SELECT RAISE(ABORT,'synthetic post-archive interruption'); END").unwrap();
    let request = ApplyReceive { header, generating: vec![] };
    assert!(target.lww_apply_receive_recorded(&request).is_err());
    assert!(target.read_conversation("char", "chat", None).unwrap().is_some());
    let revision = target.revision().unwrap();
    drop(target);
    let mut target = PersistentStore::open(directory.path()).unwrap();
    let result = target.lww_apply_receive_recorded(&request).unwrap();
    assert_eq!(result.revision, revision);
    assert_eq!(result.affected_keys, vec![archive.key]);
    let mut replay = result.clone();
    replay.current_state_recovery_required = true;
    assert_eq!(serde_json::to_value(target.lww_apply_receive_recorded(&request).unwrap()).unwrap(), serde_json::to_value(replay).unwrap());
}

#[test]
fn recorded_receive_provenance_distinguishes_own_changes_from_unrelated_native_writes() {
    for intervening in ["none", "library", "device", "hypa", "old-evidence"] {
        let (_dir, mut store) = store();
        store.device_store_mut().unwrap().set_section_participating(device_store::Section::LocalPlugins, true).unwrap();
        let lease = store.acquire_revision(store.revision().unwrap()).unwrap().lease;
        let mut baseline = store.read_native_receive_evidence(&lease).unwrap();
        if intervening == "old-evidence" { baseline.device_revision = None; }
        if intervening == "library" {
            receive(&mut store, "native-library-write", vec![change(&["plugin", "unrelated", "key"], 10, serde_json::json!("native"))], vec![]);
        } else if intervening == "device" {
            receive(&mut store, "native-device-write", vec![change(&["plugin-local", "unrelated", "string", "key"], 10, serde_json::json!("native"))], vec![]);
            assert_eq!(store.device_store().unwrap().read_plugin_device_value("unrelated", "string", "key").unwrap().as_deref(), Some("native"));
        }
        if intervening == "hypa" {
            store.device_store_mut().unwrap().write_hypa_embeddings(&[device_store::hypa::HypaEmbeddingWrite {
                cache_key: "a".repeat(64), producer: "synthetic".into(), model: "synthetic".into(), endpoint: None,
                preprocess_version: 1, dimensions: 1, vector: 1.0f32.to_le_bytes().to_vec(), metadata: None,
            }]).unwrap();
            assert_eq!(store.device_store().unwrap().read_hypa_embeddings(&["a".repeat(64)]).unwrap()[0].vector,
                Some(1.0f32.to_le_bytes().to_vec()));
            assert_eq!(crate::persistent_store::receive_evidence::device_fence(store.device_store().unwrap().connection()).unwrap(), baseline.device_plugins);
        }
        let own = change(&["plugin", "page", "key"], 11, serde_json::json!("received"));
        let own_device = change(&["plugin-local", "page", "string", "key"], 11, serde_json::json!("received-device"));
        let header = Header { binding_authority: 0.into(), request_id: "observed-page".into() };
        store.lww_stage_receive(&StageReceive { header: header.clone(), changes: vec![own.clone(), own_device.clone()],
            progress: Progress { kind: "server".into(), cursor: 1.into(), writer_id: None }, admitted_time_upper_ms: 100.into() }).unwrap();
        let result = store.lww_apply_receive_recorded_with_baseline(&ApplyReceive { header, generating: vec![] }, Some(&baseline)).unwrap();
        assert_eq!(result.current_state_recovery_required, intervening != "none", "{intervening}");
        assert_eq!(result.plugin_storage_changed_keys, vec![own.key, own_device.key]);
        assert_eq!(store.read_plugin_storage("page", "key", None).unwrap().unwrap().value, serde_json::json!("received"));
        assert_eq!(store.device_store().unwrap().read_plugin_device_value("page", "string", "key").unwrap().as_deref(), Some("received-device"));
        assert_ne!(result.native_device_plugin_fence.as_ref(), Some(&baseline.device_plugins));
        let current = store.acquire_revision(result.revision).unwrap().lease;
        assert_eq!(result.native_device_plugin_fence, Some(store.read_native_receive_evidence(&current).unwrap().device_plugins));
        store.release_revision(&current).unwrap();
        store.release_revision(&lease).unwrap();
    }
}

#[test]
fn receive_evidence_keeps_decimal_device_revision_and_rejects_stale_or_released_library_lease() {
    let (_dir, mut store) = store();
    store.device_store().unwrap().connection().execute("UPDATE plugin_gc_state SET revision=9007199254740993 WHERE singleton=1", []).unwrap();
    store.device_store().unwrap().connection().execute("UPDATE device_meta SET revision=9007199254740995 WHERE singleton=1", []).unwrap();
    let lease = store.acquire_revision(store.revision().unwrap()).unwrap().lease;
    let evidence = store.read_native_receive_evidence(&lease).unwrap();
    assert_eq!(serde_json::to_value(&evidence).unwrap()["devicePlugins"]["revision"], "9007199254740993");
    assert_eq!(serde_json::to_value(&evidence).unwrap()["deviceRevision"], "9007199254740995");
    receive(&mut store, "evidence-library-write", vec![change(&["plugin", "native", "key"], 10, serde_json::json!("new"))], vec![]);
    assert!(matches!(store.read_native_receive_evidence(&lease), Err(StoreError::RevisionConflict { .. })));
    store.release_revision(&lease).unwrap();
    assert!(matches!(store.read_native_receive_evidence(&lease), Err(StoreError::SnapshotReleased)));
}

#[test]
fn recorded_receive_detects_device_write_after_its_partition_commits() {
    let (directory, mut store) = store();
    store.device_store_mut().unwrap().set_section_participating(device_store::Section::LocalPlugins, true).unwrap();
    let mut peer = PersistentStore::open(directory.path()).unwrap();
    let lease = store.acquire_revision(store.revision().unwrap()).unwrap().lease;
    let baseline = store.read_native_receive_evidence(&lease).unwrap();
    let header = Header { binding_authority: 0.into(), request_id: "device-gap-page".into() };
    store.lww_stage_receive(&StageReceive { header: header.clone(), changes: vec![change(&["plugin", "page", "key"], 11, serde_json::json!("received"))],
        progress: Progress { kind: "server".into(), cursor: 1.into(), writer_id: None }, admitted_time_upper_ms: 100.into() }).unwrap();
    crate::persistent_store::receive_evidence::test_hooks::set_after_device(move || {
        peer.device_store_mut().unwrap().write_plugin_device_values("other", &[
            device_store::plugin_values::PluginDeviceMutation::Set { space: "string".into(), key: "key".into(), value: "native".into() },
        ]).unwrap();
    });
    let result = store.lww_apply_receive_recorded_with_baseline(&ApplyReceive { header, generating: vec![] }, Some(&baseline)).unwrap();
    assert!(result.current_state_recovery_required);
    assert_ne!(result.native_device_plugin_fence, Some(baseline.device_plugins));
    assert_eq!(store.read_plugin_storage("page", "key", None).unwrap().unwrap().value, serde_json::json!("received"));
    assert_eq!(store.device_store().unwrap().read_plugin_device_value("other", "string", "key").unwrap().as_deref(), Some("native"));
    store.release_revision(&lease).unwrap();
}

#[test]
fn recorded_receive_archive_revision_chain_rejects_a_foreign_commit_between_steps() {
    for intervening in [false, true] {
        let (_source_directory, mut source) = store();
        let (directory, mut target) = store();
        for owner in ["first", "second"] {
            create_conversation(&mut source, owner, "chat");
            create_conversation(&mut target, owner, "chat");
        }
        clear_outbox(&mut source);
        for owner in ["first", "second"] {
            source.archive_character(owner, source.revision().unwrap(), 10).unwrap();
        }
        copy_objects(&source, &target);
        let archives = source.lww_read_outbox(0.into(), 100).unwrap().entries;
        assert_eq!(archives.len(), 2);
        let time = device_store::now_ms().unwrap() as u64 + 100_000;
        let peer = intervening.then(|| PersistentStore::open(directory.path()).unwrap());
        let lease = target.acquire_revision(target.revision().unwrap()).unwrap().lease;
        let baseline = target.read_native_receive_evidence(&lease).unwrap();
        let header = Header { binding_authority: 0.into(), request_id: "archive-provenance-page".into() };
        target.lww_stage_receive(&StageReceive { header: header.clone(), changes: archives.iter().map(|archive| Change { key: archive.key.clone(), stamp: stamp(time), value: archive.value.clone() }).collect(),
            progress: Progress { kind: "server".into(), cursor: 1.into(), writer_id: None }, admitted_time_upper_ms: u64::MAX.into() }).unwrap();
        if let Some(mut peer) = peer {
            crate::persistent_store::receive_evidence::test_hooks::set_after_archive(move || {
                // A foreign writer does not participate in the new admission File protocol.
                let header = Header { binding_authority: peer.lww_binding_authority().unwrap(), request_id: "foreign-library-write".into() };
                let input = WorkingSetCommit { expected_revision: peer.revision().unwrap(), request_id: Some(header.request_id.clone()),
                    unit_mutations: Some(vec![mutation(&["plugin", "other", "key"], serde_json::json!("native"))]), ..Default::default() };
                let stamp = peer.reserve_commit_only_stamp(&header).unwrap();
                let digest = commit_intent_digest(&input).unwrap();
                commit::commit_lww(&mut peer.connection, &input, &[], &header, &stamp, &digest).unwrap();
            });
        }
        let result = target.lww_apply_receive_recorded_with_baseline(&ApplyReceive { header, generating: vec![] }, Some(&baseline)).unwrap();
        assert_eq!(result.current_state_recovery_required, intervening);
        for archive in &archives { assert!(result.affected_keys.contains(&archive.key)); }
        for owner in ["first", "second"] { assert!(target.archive_preview(owner, None).unwrap().archived); }
        if intervening { assert_eq!(target.read_plugin_storage("other", "key", None).unwrap().unwrap().value, serde_json::json!("native")); }
        target.release_revision(&lease).unwrap();
    }
}


fn admission_intent_state(store: &PersistentStore, request: &str) -> Option<String> {
    store.device_store().unwrap().connection().query_row(
        "SELECT json_array(authority,stamp,body,digest,complete,
            (SELECT identity FROM lww_intent_proofs WHERE request_id=i.request_id),
            (SELECT json_array(failures,error,quarantined) FROM lww_intent_failures WHERE request_id=i.request_id),
            (SELECT json_group_array(json_array(ordinal,key,stamp,value,source_override)) FROM
                (SELECT * FROM lww_intent_rows WHERE request_id=i.request_id ORDER BY ordinal)))
         FROM lww_intents i WHERE request_id=?1", [request], |row| row.get(0)).optional().unwrap()
}

fn assert_admission_begin_busy(store: &mut PersistentStore) {
    use crate::persistent_store::receive_batches as batches;
    let source = batches::Source::Drain { binding_authority: store.lww_binding_authority().unwrap() };
    let request = batches::BeginRequest { candidate: store.prepare_native_receive(&source).unwrap(), source, native_baseline: None };
    assert!(matches!(store.begin_local_native_receive(&request), Err(StoreError::CommitBusy)));
}

fn quarantined_commit(store: &mut PersistentStore, request: &str) -> QuarantinedIntent {
    let header = Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: request.into() };
    let input = WorkingSetCommit { expected_revision: store.revision().unwrap(), request_id: Some(request.into()),
        unit_mutations: Some(vec![mutation(&["root", "language"], serde_json::json!("uncommitted"))]), ..Default::default() };
    store.reserve_intent(&header, &Intent::for_commit(&input, &[])).unwrap();
    for _ in 0..3 { store.record_intent_failure(request, &error("synthetic-quarantined-input")).unwrap(); }
    store.lww_quarantined_intents().unwrap().into_iter().find(|entry| entry.request_id == request).unwrap()
}

#[test]
fn intent_admission_contends_across_handles_and_preserves_pending_phase_failure() {
    let (directory, mut owner) = store();
    let mut peer = PersistentStore::open(directory.path()).unwrap();
    let (_independent_directory, mut independent) = store();
    let revision = owner.revision().unwrap();
    let guard = owner.acquire_intent_admission().unwrap();
    assert!(matches!(peer.lww_recover_intents(), Err(StoreError::CommitBusy)));
    assert!(!peer.lww_recover_intents_at_open().unwrap());
    let input = WorkingSetCommit { expected_revision: revision, request_id: Some("guarded-live".into()),
        unit_mutations: Some(vec![mutation(&["root", "language"], serde_json::json!("received"))]), ..Default::default() };
    assert!(matches!(peer.commit(&input), Err(StoreError::CommitBusy)));
    assert!(peer.lww_request_unreserved("guarded-live").unwrap());
    save(&mut independent, vec![mutation(&["root", "language"], serde_json::json!("independent"))]);
    drop(guard);

    let mut active_peer = PersistentStore::open(directory.path()).unwrap();
    BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
        assert!(matches!(active_peer.lww_recover_intents(), Err(StoreError::CommitBusy)));
        assert!(!active_peer.lww_request_unreserved("guarded-live").unwrap());
        FAIL_ADMISSION_PHASE.with(|failure| failure.set(true));
    })));
    let failure = owner.commit(&input).unwrap_err();
    assert!(matches!(failure, StoreError::Validation { ref message } if message == "intent-admission-synthetic-phase"));
    assert_eq!(owner.revision().unwrap(), revision);
    assert!(!owner.lww_request_recorded("guarded-live").unwrap());
    let db = owner.device_store().unwrap().connection();
    let pending: (bool, i64, i64) = db.query_row(
        "SELECT complete,(SELECT count(*) FROM lww_intent_proofs WHERE request_id=i.request_id),
            (SELECT count(*) FROM lww_intent_failures WHERE request_id=i.request_id)
            FROM lww_intents i WHERE request_id=?1", ["guarded-live"],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
    assert_eq!(pending, (false, 1, 0));
    let frozen = admission_intent_state(&owner, "guarded-live");
    BEFORE_REPLAY_PHASE.with(|hook| *hook.borrow_mut() = Some(Box::new(|| {
        FAIL_ADMISSION_PHASE.with(|failure| failure.set(true));
    })));
    assert!(matches!(peer.lww_recover_intents(), Err(StoreError::Validation { message }) if message == "intent-admission-synthetic-phase"));
    assert_eq!(admission_intent_state(&peer, "guarded-live"), frozen);
    assert_eq!(peer.revision().unwrap(), revision);
    assert!(!peer.lww_request_recorded("guarded-live").unwrap());
    assert!(peer.lww_quarantined_intents().unwrap().is_empty());
    peer.lww_recover_intents().unwrap();
    assert_eq!(peer.read_root(None).unwrap().value["language"], "received");
    assert_eq!(peer.commit(&input).unwrap().revision, revision + 1);
    assert_eq!(peer.revision().unwrap(), revision + 1);
}

#[test]
fn stale_switch_replay_cannot_erase_a_new_receive_after_another_handle_completed_it() {
    use crate::persistent_store::{receive_batches as batches, sync_selection::{SwitchBindingRequest, SyncTarget}};
    let (directory, mut delayed) = store();
    let mut completing = PersistentStore::open(directory.path()).unwrap();
    let mut receiving = PersistentStore::open(directory.path()).unwrap();
    let binding = delayed.lww_binding_state().unwrap();
    let target = SyncTarget::External("serialization-target".into());
    let inspection = delayed.register_lww_binding_inspection(binding.target_authority, &target,
        "serialization-target-id", "serialization-library-id").unwrap();
    let request = SwitchBindingRequest { initial_publication: false,
        header: Header { binding_authority: binding.target_authority, request_id: "serialization-switch".into() },
        expected_selection_epoch: binding.selection_epoch, target, inspection_id: Some(inspection) };
    delayed.stop_next_switch_after_library_commit();
    assert!(delayed.switch_lww_binding(&request).is_err());
    let captured: (String, String, String, String) = delayed.device_store().unwrap().connection().query_row(
        "SELECT authority,stamp,body,digest FROM lww_intents WHERE request_id=?1", [&request.header.request_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap();
    let old_source = batches::Source::Drain { binding_authority: request.header.binding_authority };
    let old_candidate = receiving.prepare_native_receive(&old_source).unwrap();
    let guard = delayed.acquire_intent_admission().unwrap();
    assert!(matches!(completing.lww_recover_intents(), Err(StoreError::CommitBusy)));
    assert!(matches!(receiving.begin_local_native_receive(&batches::BeginRequest {
        candidate: old_candidate, source: old_source, native_baseline: None }), Err(StoreError::CommitBusy)));
    assert_eq!(receiving.lww_binding_authority().unwrap(), request.header.binding_authority);
    drop(guard);
    completing.lww_recover_intents().unwrap();
    let current = receiving.lww_binding_authority().unwrap();
    assert_eq!(current.0, request.header.binding_authority.0 + 1);
    let page = StageReceive { header: Header { binding_authority: current, request_id: "after-completed-switch".into() },
        changes: vec![change(&["root", "language"], 10, serde_json::json!("after"))],
        progress: Progress { kind: "server".into(), cursor: 8.into(), writer_id: None }, admitted_time_upper_ms: u64::MAX.into() };
    receiving.lww_stage_receive(&page).unwrap();
    let source = batches::Source::Staged { header: page.header.clone() };
    let begin = batches::BeginRequest { candidate: receiving.prepare_native_receive(&source).unwrap(), source, native_baseline: None };
    let first = receiving.begin_local_native_receive(&begin).unwrap();
    let before = serde_json::to_value(&first).unwrap();
    let rows = receiving.lww_receive_row_counts(&page.header.request_id).unwrap();
    assert_eq!(rows, (1, 0));
    delayed.recover_intent(request.header.binding_authority, &request.header.request_id,
        captured.0.clone(), &captured.1, &captured.2, &captured.3).unwrap();
    assert_eq!(receiving.lww_receive_row_counts(&page.header.request_id).unwrap(), rows);
    let retry = serde_json::to_value(receiving.begin_local_native_receive(&begin).unwrap()).unwrap();
    assert_eq!(retry["state"], before["state"]);
    assert_eq!(receiving.lww_binding_authority().unwrap(), current);
    assert!(receiving.device_store().unwrap().connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM lww_receive WHERE request_id=?1)", [&page.header.request_id], |row| row.get::<_, bool>(0)).unwrap());
    for missing in [false, true] {
        if missing {
            delayed.connection.execute("DELETE FROM lww_requests WHERE request_id=?1", [&request.header.request_id]).unwrap();
        } else {
            delayed.connection.execute("UPDATE lww_requests SET digest='synthetic-corrupt-receipt' WHERE request_id=?1", [&request.header.request_id]).unwrap();
        }
        let failure = delayed.recover_intent(request.header.binding_authority, &request.header.request_id,
            captured.0.clone(), &captured.1, &captured.2, &captured.3).unwrap_err();
        let expected = if missing { "request-receipt-missing" } else { "request-id-integrity" };
        assert!(matches!(failure, StoreError::Validation { message } if message == expected));
        assert_eq!(receiving.lww_receive_row_counts(&page.header.request_id).unwrap(), rows);
        assert_eq!(serde_json::to_value(receiving.begin_local_native_receive(&begin).unwrap()).unwrap()["state"], before["state"]);
        assert_eq!(receiving.lww_binding_authority().unwrap(), current);
    }

}

#[test]
fn direct_quarantine_completion_and_discard_cannot_pass_a_live_admission_guard() {
    use crate::persistent_store::sync_selection::{SwitchBindingRequest, SyncTarget};
    let (directory, mut owner) = store();
    let mut peer = PersistentStore::open(directory.path()).unwrap();
    save(&mut owner, vec![mutation(&["root", "language"], serde_json::json!("retained"))]);
    let binding = owner.lww_binding_state().unwrap();
    let target = SyncTarget::External("quarantine-admission-target".into());
    let inspection = owner.register_lww_binding_inspection(binding.target_authority, &target,
        "quarantine-admission-target-id", "quarantine-admission-library-id").unwrap();
    let request = SwitchBindingRequest { initial_publication: false,
        header: Header { binding_authority: binding.target_authority, request_id: "guarded-quarantine".into() },
        expected_selection_epoch: binding.selection_epoch, target, inspection_id: Some(inspection) };
    owner.stop_next_switch_after_library_commit();
    assert!(owner.switch_lww_binding(&request).is_err());
    for _ in 0..3 { owner.record_intent_failure(&request.header.request_id, &error("synthetic-cross-store-stop")).unwrap(); }
    let selected = owner.lww_quarantined_intents().unwrap().into_iter().find(|item| item.request_id == request.header.request_id).unwrap();
    assert!(selected.recoverable && !selected.discardable);
    let guard = owner.acquire_intent_admission().unwrap();
    let revision = owner.revision().unwrap();
    assert!(matches!(peer.lww_complete_quarantined_intent(&selected.request_id, &selected.token, revision), Err(StoreError::CommitBusy)));
    assert!(matches!(peer.lww_discard_quarantined_intent(&selected.request_id, &selected.token, revision), Err(StoreError::CommitBusy)));
    assert_eq!(peer.device_store().unwrap().connection().query_row(
        "SELECT failures FROM lww_intent_failures WHERE request_id=?1", [&request.header.request_id], |row| row.get::<_, i64>(0)).unwrap(), 3);
    assert_eq!(peer.lww_binding_authority().unwrap(), request.header.binding_authority);
    drop(guard);
    assert!(peer.lww_complete_quarantined_intent(&selected.request_id, &selected.token, revision).unwrap());
    assert_eq!(peer.read_root(None).unwrap().value["language"], "retained");
    assert_eq!(peer.revision().unwrap(), revision);
    assert_eq!(peer.lww_binding_authority().unwrap().0, request.header.binding_authority.0 + 1);
    assert!(peer.lww_quarantined_intents().unwrap().is_empty());

    let selected = quarantined_commit(&mut owner, "discardable-commit");
    assert!(selected.discardable && !selected.recoverable);
    let other = quarantined_commit(&mut owner, "retained-quarantine");
    let frozen = admission_intent_state(&owner, &selected.request_id);
    let unrelated = admission_intent_state(&owner, &other.request_id);
    let root = owner.read_root(None).unwrap();
    assert!(!owner.lww_request_recorded(&selected.request_id).unwrap());
    let guard = owner.acquire_intent_admission().unwrap();
    assert!(matches!(peer.lww_discard_quarantined_intent(&selected.request_id, &selected.token, root.revision), Err(StoreError::CommitBusy)));
    assert_eq!(admission_intent_state(&peer, &selected.request_id), frozen);
    assert_eq!(admission_intent_state(&peer, &other.request_id), unrelated);
    drop(guard);
    assert!(peer.lww_discard_quarantined_intent(&selected.request_id, &selected.token, root.revision).unwrap());
    assert_eq!(admission_intent_state(&peer, &selected.request_id), None);
    for table in ["lww_intent_proofs", "lww_intent_rows", "lww_intent_failures"] {
        assert_eq!(peer.device_store().unwrap().connection().query_row(
            &format!("SELECT count(*) FROM {table} WHERE request_id=?1"), [&selected.request_id], |row| row.get::<_, i64>(0)).unwrap(), 0);
    }
    assert_eq!(admission_intent_state(&peer, &other.request_id), unrelated);
    assert_eq!(peer.read_root(None).unwrap().value, root.value);
    assert_eq!(peer.revision().unwrap(), root.revision);
}


#[test]
fn intent_admission_aliases_share_one_lock_and_invalid_leaf_fails_closed() {
    let (_owner_directory, owner) = store();
    let device = owner.device_store().unwrap().connection();
    let device_path = std::path::Path::new(device.path().unwrap());
    let alias = Connection::open(device_path.parent().unwrap().join(".").join(device_store::DEVICE_DATABASE_FILE)).unwrap();
    let guard = owner.acquire_intent_admission().unwrap();
    assert!(matches!(IntentAdmissionGuard::acquire(&alias, &owner.connection), Err(StoreError::CommitBusy)));
    let lock_path = guard.path.clone();
    #[cfg(windows)]
    {
        let parent = device_path.parent().unwrap().to_string_lossy().to_uppercase();
        let alias = Connection::open_with_flags(std::path::Path::new(&parent).join(device_store::DEVICE_DATABASE_FILE), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert!(matches!(IntentAdmissionGuard::acquire(&alias, &owner.connection), Err(StoreError::CommitBusy)));
    }
    #[cfg(unix)]
    {
        let aliases = tempfile::tempdir().unwrap();
        let directory = aliases.path().join("directory");
        std::os::unix::fs::symlink(device_path.parent().unwrap(), &directory).unwrap();
        let file_directory = aliases.path().join("file");
        std::fs::create_dir(&file_directory).unwrap();
        let file = file_directory.join(device_store::DEVICE_DATABASE_FILE);
        std::os::unix::fs::symlink(device_path, &file).unwrap();
        for path in [directory.join(device_store::DEVICE_DATABASE_FILE), file] {
            let alias = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            assert!(matches!(IntentAdmissionGuard::acquire(&alias, &owner.connection), Err(StoreError::CommitBusy)));
        }
    }
    drop(guard);
    let alias_guard = IntentAdmissionGuard::acquire(&alias, &owner.connection).unwrap();
    assert_eq!(alias_guard.path, lock_path);
    drop(alias_guard);
    let revision = owner.revision().unwrap();
    let issued = owner.lww_clock_state().unwrap().issued;
    std::fs::remove_file(&lock_path).unwrap();
    std::fs::create_dir(&lock_path).unwrap();
    assert!(matches!(owner.acquire_intent_admission(), Err(failure) if admission_failure(&failure)));
    std::fs::remove_dir(&lock_path).unwrap();
    #[cfg(unix)]
    {
        let target = lock_path.with_extension("synthetic-target");
        std::fs::write(&target, b"retained synthetic target").unwrap();
        std::os::unix::fs::symlink(&target, &lock_path).unwrap();
        assert!(matches!(owner.acquire_intent_admission(), Err(failure) if admission_failure(&failure)));
        assert_eq!(std::fs::read(&target).unwrap(), b"retained synthetic target");
        std::fs::remove_file(&lock_path).unwrap();
    }
    assert_eq!(owner.revision().unwrap(), revision);
    assert_eq!(owner.lww_clock_state().unwrap().issued, issued);
    std::fs::write(&lock_path, b"invalid synthetic lock contents").unwrap();
    assert!(matches!(owner.acquire_intent_admission(), Err(StoreError::Validation { message }) if message == "intent-admission-file-integrity"));
    assert_eq!(std::fs::read(&lock_path).unwrap(), b"invalid synthetic lock contents");
}

#[test]
fn intent_admission_child_process() {
    let Some(root) = std::env::var_os("RISUNEST_TEST_INTENT_ADMISSION_ROOT") else { return };
    let store = PersistentStore::open(std::path::Path::new(&root)).unwrap();
    let _guard = store.acquire_intent_admission().unwrap();
    std::fs::write(std::path::Path::new(&root).join("intent-admission-ready"), b"ready").unwrap();
    let mut byte = [0u8; 1];
    std::io::Read::read_exact(&mut std::io::stdin(), &mut byte).unwrap();
}

#[test]
fn intent_admission_process_exit_releases_only_its_owned_os_lock() {
    use std::{process::{Command, Stdio}, time::{Duration, Instant}};
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
    }
    for terminate in [false, true] {
        let (directory, owner) = store();
        let mut child = OwnedChild(Command::new(std::env::current_exe().unwrap())
            .arg("persistent_store::lww::tests::intent_admission_child_process").arg("--exact").arg("--nocapture")
            .env("RISUNEST_TEST_INTENT_ADMISSION_ROOT", directory.path())
            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !directory.path().join("intent-admission-ready").exists() {
            assert!(child.0.try_wait().unwrap().is_none(), "owned lock child exited before readiness");
            assert!(Instant::now() < deadline, "owned lock child did not report readiness");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(owner.acquire_intent_admission(), Err(StoreError::CommitBusy)));
        if terminate { child.0.kill().unwrap(); }
        else { std::io::Write::write_all(child.0.stdin.as_mut().unwrap(), b"x").unwrap(); }
        let exit = loop {
            if let Some(exit) = child.0.try_wait().unwrap() { break exit; }
            assert!(Instant::now() < deadline, "owned lock child did not exit");
            std::thread::sleep(Duration::from_millis(5));
        };
        if !terminate { assert!(exit.success()); }
        let guard = owner.acquire_intent_admission().unwrap();
        assert_eq!(std::fs::metadata(&guard.path).unwrap().len(), 0);
    }
}


#[test]
fn generation_checkpoint_admission_busy_preserves_attempt_until_retry_or_cancel() {
    use crate::native_generation::operation::{Checkpoint, Phase, Start, Target};
    for cancel in [false, true] {
        let (directory, mut generating) = store();
        create_conversation(&mut generating, "generation-owner", "generation-chat");
        let locker = PersistentStore::open(directory.path()).unwrap();
        let revision = generating.revision().unwrap();
        let ticket = generating.generation_reserve(Target { character_id: "generation-owner".into(),
            conversation_id: "generation-chat".into(), expected_revision: revision,
            library_identity: generating.library_identity().unwrap(), binding_authority: generating.lww_binding_authority().unwrap() }).unwrap();
        let Start::Started { owner, .. } = generating.generation_start(&ticket).unwrap() else { panic!("owner not started") };
        let guard = locker.acquire_intent_admission().unwrap();
        let checkpoint = Checkpoint { sequence: 1, text: "synthetic output".into() };
        assert!(matches!(generating.generation_checkpoint(&owner, &checkpoint), Err(StoreError::CommitBusy)));
        let pending = generating.generation_status(&ticket).unwrap();
        assert!(pending.pending_commit);
        assert_eq!(pending.checkpoint.0, 0);
        assert_eq!(generating.revision().unwrap(), revision);
        assert!(matches!(generating.generation_finish(&owner, cancel), Err(StoreError::CommitBusy)));
        assert!(generating.generation_status(&ticket).unwrap().pending_commit);
        drop(guard);
        if cancel {
            let status = generating.generation_finish(&owner, true).unwrap();
            assert_eq!(status.phase, Phase::Cancelled);
            assert!(!status.pending_commit);
            assert_eq!(generating.revision().unwrap(), revision);
        } else {
            let status = generating.generation_checkpoint(&owner, &checkpoint).unwrap();
            assert_eq!(status.checkpoint.0, 1);
            assert!(!status.pending_commit);
            assert_eq!(status.committed_revision, Some(revision + 1));
            let chat = generating.read_conversation("generation-owner", "generation-chat", None).unwrap().unwrap().value;
            assert_eq!(chat["message"].as_array().unwrap().len(), 1);
            assert_eq!(chat["message"][0]["data"], "synthetic output");
            assert_eq!(generating.generation_finish(&owner, false).unwrap().phase, Phase::Completed);
        }
    }
}


#[test]
fn switch_reservation_refreshes_device_revision_after_admission() {
    let (directory, mut store) = store();
    let mut peer = PersistentStore::open(directory.path()).unwrap();
    let selection = crate::persistent_store::sync_selection::read(&store.connection).unwrap();
    let header = Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: "switch-after-peer-write".into() };
    let change = crate::persistent_store::sync_selection::BindingSelectionChange { initial_publication: false,
        expected_epoch: selection.epoch, new_epoch: Uuid::new_v4().to_string(),
        target: crate::persistent_store::sync_selection::SyncTarget::None,
        library_id: None, target_id: None, inspection_id: None };
    let before = device_revision(store.device_store().unwrap().connection()).unwrap();
    let observed = std::rc::Rc::new(std::cell::Cell::new(before));
    let callback_observed = observed.clone();
    BEFORE_SWITCH_ADMISSION.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
        peer.device_store_mut().unwrap().write_plugin_device_values("peer", &[
            device_store::plugin_values::PluginDeviceMutation::Set { space: "string".into(), key: "admission".into(), value: "retained-peer-write".into() },
        ]).unwrap();
        callback_observed.set(device_revision(peer.device_store().unwrap().connection()).unwrap());
    })));
    let current = store.lww_switch_target(&header, &change).unwrap();
    assert!(observed.get() > before);
    assert_eq!(current.0, header.binding_authority.0 + 1);
    let (body, complete): (String, bool) = store.device_store().unwrap().connection().query_row(
        "SELECT body,complete FROM lww_intents WHERE request_id=?1", [&header.request_id],
        |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    let Intent::Switch { device_revision, .. } = serde_json::from_str(&body).unwrap() else { panic!("switch intent missing") };
    assert_eq!(device_revision, observed.get());
    assert!(complete);
    assert_eq!(store.device_store().unwrap().read_plugin_device_value("peer", "string", "admission").unwrap().as_deref(), Some("retained-peer-write"));
    assert!(store.lww_request_recorded(&header.request_id).unwrap());
    assert_eq!(store.lww_switch_target(&header, &change).unwrap(), current);
    assert_eq!(store.device_store().unwrap().connection().query_row(
        "SELECT count(*) FROM lww_intent_failures WHERE request_id=?1", [&header.request_id], |row| row.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn generation_normalization_admission_busy_preserves_frozen_ids_until_retry_or_cancel() {
    use crate::native_generation::{inputs::OllamaIntent, operation::{Phase, Start, Target}, service::{self, GenerationError, PreparedGeneration}};
    for cancel in [false, true] {
        let (directory, mut owner) = store();
        let fixture: Value = serde_json::from_str(include_str!("../../../tests/fixtures/native-generation/ollama-text.json")).unwrap();
        let mut character = fixture["character"].clone();
        let mut conversation = fixture["conversation"].clone();
        conversation["message"] = fixture["effectMessages"].clone();
        character["chats"] = serde_json::json!([conversation]);
        let stage = owner.replace_begin().unwrap();
        owner.replace_put_root(&stage.staging_id, &fixture["root"]).unwrap();
        owner.replace_put_presets(&stage.staging_id, &[fixture["preset"].clone()]).unwrap();
        owner.replace_add_characters(&stage.staging_id, &[character]).unwrap();
        owner.replace_commit(&stage.staging_id, Some(0)).unwrap();
        let revision = owner.revision().unwrap();
        let intent = OllamaIntent { effective_preset_id: "preset-a".into(), host: None, target: Target {
            character_id: "character".into(), conversation_id: "conversation".into(), expected_revision: revision,
            library_identity: owner.library_identity().unwrap(), binding_authority: owner.lww_binding_authority().unwrap(),
        }};
        let locker = std::rc::Rc::new(PersistentStore::open(directory.path()).unwrap());
        let ticket = service::reserve_ollama(&mut owner, &intent).unwrap();
        let Start::Started { owner: generation, .. } = owner.generation_start_preparation(&ticket).unwrap() else { panic!("preparation not started") };
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None::<WorkingSetCommit>));
        let held = std::rc::Rc::new(std::cell::RefCell::new(None));
        let (copy, hold, lock) = (captured.clone(), held.clone(), locker.clone());
        // The interaction effect commits first, so admission is taken only before the normalization commit.
        BEFORE_COMMIT_ADMISSION.with(|hook| *hook.borrow_mut() = Some(Box::new(move |input| {
            assert!(input.conversations.is_none() && input.unit_mutations.is_some());
            BEFORE_COMMIT_ADMISSION.with(|hook| *hook.borrow_mut() = Some(Box::new(move |input| {
                *copy.borrow_mut() = Some(input.clone());
                *hold.borrow_mut() = Some(lock.acquire_intent_admission().unwrap());
            })));
        })));
        assert!(matches!(owner.generation_ensure_history_ids(&generation), Err(GenerationError::Store(StoreError::CommitBusy))));
        assert!(owner.generation_status(&ticket).unwrap().pending_commit);
        assert!(matches!(owner.generation_finish(&generation, cancel), Err(StoreError::CommitBusy)));
        assert!(owner.generation_status(&ticket).unwrap().pending_commit);
        assert_eq!(owner.revision().unwrap(), revision + 1);
        assert_eq!(owner.read_conversation("character", "conversation", None).unwrap().unwrap().value, conversation);
        let submitted = captured.borrow().as_ref().unwrap().clone();
        let ranges = submitted.conversations.as_ref().expect("normalization input was captured");
        assert!(!ranges.is_empty() && ranges.iter().all(|range| matches!(range, crate::persistent_store::ConversationMutation::ReplaceRange { .. })));
        assert!(held.borrow_mut().take().is_some());
        if cancel {
            let status = owner.generation_finish(&generation, true).unwrap();
            assert_eq!(status.phase, Phase::Cancelled);
            assert!(!status.pending_commit);
            assert_eq!(owner.revision().unwrap(), revision + 1);
            assert_eq!(owner.read_conversation("character", "conversation", None).unwrap().unwrap().value, conversation);
            continue;
        }
        let expected_ranges = serde_json::to_value(&submitted.conversations).unwrap();
        let observed = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = observed.clone();
        BEFORE_COMMIT_ADMISSION.with(|hook| *hook.borrow_mut() = Some(Box::new(move |input| {
            assert_eq!(serde_json::to_value(&input.conversations).unwrap(), expected_ranges);
            seen.set(true);
        })));
        let status = owner.generation_ensure_history_ids(&generation).unwrap();
        assert!(observed.get());
        assert!(!status.pending_commit && !status.preparation_ready);
        assert_eq!(status.committed_revision, Some(revision + 2));
        let mut expected = conversation.clone();
        for range in submitted.conversations.as_ref().unwrap() {
            let crate::persistent_store::ConversationMutation::ReplaceRange { start, delete_count, messages, .. } = range else { panic!("normalization used a different mutation") };
            let start = usize::try_from(*start).unwrap();
            assert_eq!(usize::try_from(*delete_count).unwrap(), messages.len());
            for (offset, row) in messages.iter().enumerate() {
                let before = &conversation["message"][start + offset];
                assert!(before["chatId"].is_null());
                let id = row["chatId"].as_str().unwrap();
                assert_eq!(Uuid::parse_str(id).unwrap().to_string(), id);
                let mut unchanged = before.clone(); unchanged["chatId"] = Value::String(id.into());
                assert_eq!(*row, unchanged);
                expected["message"][start + offset] = row.clone();
            }
        }
        assert_eq!(owner.read_conversation("character", "conversation", None).unwrap().unwrap().value, expected);
        assert!(matches!(service::prepare_reserved_ollama(&mut owner, &ticket).unwrap(), PreparedGeneration::Ready { status, .. } if status.preparation_ready));
        assert_eq!(owner.generation_finish(&generation, false).unwrap().phase, Phase::Completed);
        assert_eq!(owner.revision().unwrap(), revision + 2);
        assert_eq!(owner.read_conversation("character", "conversation", None).unwrap().unwrap().value, expected);
    }
}

fn admission_input_rows(store: &PersistentStore, request: &str) -> String {
    store.device_store().unwrap().connection().query_row(
        "SELECT json_group_array(json_array(ordinal,key,stamp,value,source_override)) FROM
            (SELECT * FROM lww_intent_rows WHERE request_id=?1 ORDER BY ordinal)", [request], |row| row.get(0)).unwrap()
}

#[test]
fn replacement_input_rows_are_owned_before_reservation_and_retry_after_interruption() {
    for target in [false, true] {
        for interrupt in [false, true] {
            let (directory, mut owner) = store();
            let mut peer = PersistentStore::open(directory.path()).unwrap();
            let inspecting = PersistentStore::open(directory.path()).unwrap();
            let (header, staging, changes) = if target { new_device_stage(&mut owner) }
                else { (Header { binding_authority: 0.into(), request_id: "replacement-input-owner".into() }, restore_stage(&mut owner, "replacement"), vec![]) };
            let unrelated = Header { binding_authority: 0.into(), request_id: "quarantined-input-owner".into() };
            let unrelated_stage = restore_stage(&mut owner, "quarantined");
            let row = intent_rows::target_row(&change(&["root", "language"], 7, serde_json::json!("quarantined")));
            let rows = intent_rows::write(&mut owner.device_store_mut().unwrap().connection, &unrelated.request_id, "target", std::iter::once(row)).unwrap();
            let intent = Intent::Target { device_revision: device_revision(owner.device_store().unwrap().connection()).unwrap(), staging_id: unrelated_stage, changes: rows };
            owner.reserve_intent(&unrelated, &intent).unwrap();
            for _ in 0..3 { owner.record_intent_failure(&unrelated.request_id, &error("synthetic-held-input")).unwrap(); }
            let retained = admission_intent_state(&owner, &unrelated.request_id);
            let root = owner.read_root(None).unwrap();
            let clock = owner.lww_clock_state().unwrap().issued;
            let request = header.request_id.clone();
            let observed = std::rc::Rc::new(std::cell::RefCell::new(None));
            let copy = observed.clone();
            let retained_in_hook = retained.clone();
            AFTER_REPLACEMENT_INPUT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                let rows = admission_input_rows(&peer, &request);
                assert_ne!(rows, "[]");
                assert!(peer.lww_request_unreserved(&request).unwrap());
                assert!(matches!(peer.lww_recover_intents(), Err(StoreError::CommitBusy)));
                assert_admission_begin_busy(&mut peer);
                assert_eq!(admission_input_rows(&peer, &request), rows);
                assert_eq!(admission_intent_state(&peer, "quarantined-input-owner"), retained_in_hook);
                assert_eq!(peer.lww_clock_state().unwrap().issued, clock);
                assert_eq!(peer.revision().unwrap(), root.revision);
                *copy.borrow_mut() = Some(rows);
                if interrupt { panic!("synthetic input-before-reservation interruption"); }
            })));
            let invoke = |store: &mut PersistentStore| if target {
                store.lww_replace_target(&header, &staging, &changes)
            } else { store.lww_commit_replacement(&header, &staging) };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| invoke(&mut owner)));
            assert!(observed.borrow().is_some());
            let committed = if interrupt {
                assert!(result.is_err());
                assert_eq!(owner.revision().unwrap(), root.revision);
                assert!(owner.lww_request_unreserved(&header.request_id).unwrap());
                owner.lww_recover_intents().unwrap();
                assert_eq!(admission_input_rows(&owner, &header.request_id), "[]");
                assert_eq!(admission_intent_state(&owner, &unrelated.request_id), retained);
                let expected = observed.borrow().clone().unwrap();
                let request = header.request_id.clone();
                AFTER_REPLACEMENT_INPUT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                    assert_eq!(admission_input_rows(&inspecting, &request), expected);
                })));
                invoke(&mut owner).unwrap()
            } else { result.unwrap().unwrap() };
            assert_eq!(committed.revision, root.revision + 1);
            let expected = if target { "en" } else { "replacement" };
            assert_eq!(owner.read_root(None).unwrap().value["language"], expected);
            assert!(owner.lww_request_recorded(&header.request_id).unwrap());
            assert_eq!(admission_intent_state(&owner, &unrelated.request_id), retained);
            assert_eq!(admission_input_rows(&owner, &header.request_id), "[]");
            let outbox = owner.lww_read_outbox(0.into(), 100).unwrap().entries;
            assert_eq!(invoke(&mut owner).unwrap().revision, committed.revision);
            assert_eq!(owner.lww_read_outbox(0.into(), 100).unwrap().entries, outbox);
            assert_eq!(owner.revision().unwrap(), committed.revision);
        }
    }
}

#[test]
fn intent_admission_replacement_is_denied_or_detected_before_phase_write() {
    let (directory, mut owner) = store();
    let peer = PersistentStore::open(directory.path()).unwrap();
    let path = IntentAdmissionGuard::path(owner.device_store().unwrap().connection()).unwrap();
    let retired = path.with_extension("synthetic-retired");
    let hook_path = path.clone();
    let hook_retired = retired.clone();
    let revision = owner.revision().unwrap();
    let input = WorkingSetCommit { expected_revision: revision, request_id: Some("sidecar-phase".into()),
        unit_mutations: Some(vec![mutation(&["root", "language"], serde_json::json!("sidecar-retained"))]), ..Default::default() };
    let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
    let copy = captured.clone();
    BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
        *copy.borrow_mut() = admission_intent_state(&peer, "sidecar-phase");
        #[cfg(windows)]
        {
            assert!(std::fs::rename(&hook_path, &hook_retired).is_err());
            assert!(std::fs::remove_file(&hook_path).is_err());
            assert!(!hook_retired.exists());
            assert!(matches!(peer.acquire_intent_admission(), Err(StoreError::CommitBusy)));
        }
        #[cfg(unix)]
        {
            std::fs::rename(&hook_path, &hook_retired).unwrap();
            std::fs::write(&hook_path, b"").unwrap();
        }
    })));
    let result = owner.commit(&input);
    assert!(captured.borrow().is_some());
    #[cfg(windows)]
    {
        assert_eq!(result.unwrap().revision, revision + 1);
        assert!(!retired.exists());
    }
    #[cfg(unix)]
    {
        assert!(matches!(result, Err(failure) if admission_failure(&failure)));
        assert_eq!(owner.revision().unwrap(), revision);
        assert_eq!(admission_intent_state(&owner, "sidecar-phase"), *captured.borrow());
        assert!(!owner.lww_request_recorded("sidecar-phase").unwrap());
        assert!(owner.lww_quarantined_intents().unwrap().is_empty());
        let stamp = owner.lww_clock_state().unwrap().issued;
        owner.lww_recover_intents().unwrap();
        assert_eq!(owner.lww_clock_state().unwrap().issued, stamp);
        assert_eq!(owner.revision().unwrap(), revision + 1);
        std::fs::remove_file(&retired).unwrap();
    }
    assert_eq!(owner.read_root(None).unwrap().value["language"], "sidecar-retained");
    let root = owner.read_root(None).unwrap();
    let issued = owner.lww_clock_state().unwrap().issued;
    drop(owner);
    std::fs::remove_file(&path).unwrap();
    let reopened = PersistentStore::open(directory.path()).unwrap();
    let guard = reopened.acquire_intent_admission().unwrap();
    assert_eq!(std::fs::metadata(&guard.path).unwrap().len(), 0);
    assert_eq!(reopened.read_root(None).unwrap().value, root.value);
    assert_eq!(reopened.revision().unwrap(), root.revision);
    assert_eq!(reopened.lww_clock_state().unwrap().issued, issued);
}

fn stage_kernel_rows(store: &mut PersistentStore, id: &str, changes: Vec<Change>) -> Header {
    let header = Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: id.into() };
    store.lww_stage_receive(&StageReceive { header: header.clone(), changes,
        progress: Progress { kind: "server".into(), cursor: 1.into(), writer_id: None }, admitted_time_upper_ms: u64::MAX.into() }).unwrap();
    header
}
fn begin_kernel(store: &mut PersistentStore) -> crate::persistent_store::receive_batches::Capability {
    let source = crate::persistent_store::receive_batches::Source::Drain { binding_authority: store.lww_binding_authority().unwrap() };
    begin_kernel_from(store, source)
}
fn begin_kernel_from(store: &mut PersistentStore, source: crate::persistent_store::receive_batches::Source)
    -> crate::persistent_store::receive_batches::Capability {
    use crate::persistent_store::receive_batches::{BeginRequest, BeginResult};
    let lease = store.acquire_revision(store.revision().unwrap()).unwrap().lease;
    let baseline = store.read_native_receive_evidence(&lease).unwrap();
    store.release_revision(&lease).unwrap();
    let candidate = store.prepare_native_receive(&source).unwrap();
    let BeginResult::Admitted { state } = store.begin_local_native_receive(&BeginRequest { candidate, source, native_baseline: Some(baseline) }).unwrap()
        else { panic!("fresh kernel admission expected") };
    state.capability
}
fn step_kernel(store: &PersistentStore, cap: &crate::persistent_store::receive_batches::Capability)
    -> StoreResult<crate::persistent_store::receive_batches::AdvanceResult> {
    use crate::persistent_store::receive_batches::AdvanceRequest;
    let state = store.read_native_receive_state(cap)?;
    store.advance_local_native_receive(&AdvanceRequest { capability: cap.clone(), expected_progress_token: state.progress_token, generating: vec![] })
}
fn kernel_manifest(store: &PersistentStore, cap: &crate::persistent_store::receive_batches::Capability,
    partition: crate::persistent_store::receive_batches::Partition) -> crate::persistent_store::receive_batches::Manifest {
    use crate::persistent_store::receive_batches::{read_manifest, Partition};
    read_manifest(match partition { Partition::Library => &store.connection, Partition::Device => store.device_store().unwrap().connection() }, partition, cap).unwrap().unwrap()
}
/// Steps the real settlement path until both partitions seal, so local writes are admitted again
/// and a test can restore what a receive installed.
fn seal_kernel(store: &PersistentStore, cap: &crate::persistent_store::receive_batches::Capability)
    -> crate::persistent_store::receive_batches::Outcome {
    use crate::persistent_store::receive_batches::Phase;
    for _ in 0..1000 {
        let state = store.read_native_receive_state(cap).unwrap();
        if state.phase == Phase::Sealed { return state.outcome.unwrap(); }
        step_kernel(store, cap).unwrap();
    }
    panic!("the receive must seal");
}
fn kernel_ready_for_settlement(store: &PersistentStore, cap: &crate::persistent_store::receive_batches::Capability) -> bool {
    use crate::persistent_store::receive_batches::{Partition, ScanPhase};
    [Partition::Library, Partition::Device].into_iter().all(|partition| {
        let manifest = kernel_manifest(store, cap, partition);
        manifest.ticket.is_none() && manifest.cursor.phase == ScanPhase::Dispositions
    })
}

#[test]
fn bounded_receive_kernel_preserves_every_effect_and_repeated_device_fence() {
    use crate::persistent_store::receive_batches::{self as batches, AdvanceResult, Partition, Phase};
    let (_directory, mut store) = store();
    store.device_store_mut().unwrap().set_section_participating(device_store::Section::LocalPlugins, true).unwrap();
    let first = "k".repeat(risunest_sync_wire::unit::MAX_UNIT_KEY_BYTES - 256);
    let second = "m".repeat(risunest_sync_wire::unit::MAX_UNIT_KEY_BYTES - 256);
    let time = device_store::now_ms().unwrap() as u64 + 100_000;
    stage_kernel_rows(&mut store, "kernel-large", vec![change(&["plugin", "bounded", &first], time, serde_json::json!("first")),
        change(&["plugin", "bounded", &second], time, serde_json::json!("second"))]);
    stage_kernel_rows(&mut store, "kernel-device-first", vec![change(&["plugin-local", "bounded", "string", "key"], time, serde_json::json!("one"))]);
    stage_kernel_rows(&mut store, "kernel-device-next", vec![change(&["plugin-local", "bounded", "string", "key"], time + 1, serde_json::json!("two"))]);
    let cap = begin_kernel(&mut store);
    let _limits = batches::kernel_test::Limits::set(128, 100_000);
    let mut library_count = 0;
    let mut device_fences = 0;
    let mut device_limit = None;
    for _ in 0..100 {
        let AdvanceResult::Advanced { state, fence } = step_kernel(&store, &cap).unwrap() else { panic!("current token was stale") };
        assert_eq!(state.phase, Phase::Producing);
        assert!(state.outcome.is_none());
        assert!(!fence.unknown_library && !fence.unknown_device, "known own-only progress must not request broad invalidation");
        let count = kernel_manifest(&store, &cap, Partition::Library).affected_count;
        assert!(count - library_count <= 1, "the encoded byte bound must split these large keys");
        library_count = count;
        let changed_device = fence.keys.iter().filter(|key| key.components()[0] == "plugin-local").count();
        assert!(changed_device <= 1, "each device step exposes its own fence after an intervening read");
        if changed_device != 0 {
            let observed = store.device_store().unwrap().read_plugin_device_value("bounded", "string", "key").unwrap();
            assert_eq!(observed.as_deref(), Some(if device_fences == 0 { "one" } else { "two" }));
        }
        device_fences += changed_device;
        if device_limit.is_none() && kernel_manifest(&store, &cap, Partition::Library).cursor.phase == batches::ScanPhase::Dispositions {
            device_limit = Some(batches::kernel_test::Limits::set(1, 100_000));
        }
        if kernel_ready_for_settlement(&store, &cap) { break; }
    }
    assert!(kernel_ready_for_settlement(&store, &cap));
    assert_eq!(library_count, 2);
    assert_eq!(device_fences, 2, "historical dedupe must not suppress the second invalidation");
    let device = kernel_manifest(&store, &cap, Partition::Device);
    assert_eq!((device.affected_count, device.plugin_changed_count), (1, 1));
    assert_eq!(store.read_plugin_storage("bounded", &first, None).unwrap().unwrap().value, "first");
    assert_eq!(store.read_plugin_storage("bounded", &second, None).unwrap().unwrap().value, "second");
    assert_eq!(store.device_store().unwrap().read_plugin_device_value("bounded", "string", "key").unwrap().as_deref(), Some("two"));
    let before = (store.revision().unwrap(), device_revision(store.device_store().unwrap().connection()).unwrap());
    let AdvanceResult::Advanced { fence, .. } = step_kernel(&store, &cap).unwrap() else { panic!("current suspension token") };
    assert!(!fence.unknown_library && !fence.unknown_device);
    assert_eq!((store.revision().unwrap(), device_revision(store.device_store().unwrap().connection()).unwrap()), before);
}

#[test]
fn receive_kernel_clock_survives_library_commit_source_removal_and_stale_retry() {
    use crate::persistent_store::receive_batches::{self as batches, AdvanceRequest, AdvanceResult, Partition};
    for interrupt in [1, 2] {
        let (directory, mut store) = store();
        let time = device_store::now_ms().unwrap() as u64 + 100_000;
        let mut changes = vec![change(&["root", "language"], time, serde_json::json!("kernel-language")),
            change(&["root", "additionalPrompt"], time, serde_json::json!("kernel-prompt"))];
        for change in &mut changes { change.stamp.logical = 7; }
        changes[1].stamp.writer_id = "00000000-0000-4000-8000-000000000002".into();
        let maximum = changes[1].stamp.clone();
        let header = stage_kernel_rows(&mut store, "kernel-clock", changes);
        let cap = begin_kernel(&mut store);
        let original = store.read_native_receive_state(&cap).unwrap();
        let before_clock = store.lww_clock_state().unwrap();
        let before_library = serde_json::to_value(kernel_manifest(&store, &cap, Partition::Library)).unwrap();
        let before_device = serde_json::to_value(kernel_manifest(&store, &cap, Partition::Device)).unwrap();
        let AdvanceResult::Stale { fence, .. } = store.advance_local_native_receive(&AdvanceRequest { capability: cap.clone(),
            expected_progress_token: "obsolete".into(), generating: vec![] }).unwrap() else { panic!("stale token accepted") };
        assert!(fence.unknown_library && fence.unknown_device);
        assert_eq!(serde_json::to_value(store.lww_clock_state().unwrap()).unwrap(), serde_json::to_value(before_clock).unwrap());
        assert_eq!(serde_json::to_value(kernel_manifest(&store, &cap, Partition::Library)).unwrap(), before_library);
        assert_eq!(serde_json::to_value(kernel_manifest(&store, &cap, Partition::Device)).unwrap(), before_device);
        let revision = store.revision().unwrap();
        batches::kernel_test::hook(interrupt, || {});
        let mut interrupted = false;
        for _ in 0..50 {
            match step_kernel(&store, &cap) {
                Err(StoreError::Validation { message }) if message == "synthetic-kernel-interruption" => { interrupted = true; break; }
                Ok(_) => {}, other => panic!("unexpected kernel outcome: {other:?}"),
            }
        }
        assert!(interrupted);
        assert_eq!(store.lww_clock_state().unwrap().accepted, Some(maximum.clone()));
        assert_eq!(store.revision().unwrap(), revision + i64::from(interrupt == 2));
        if interrupt == 2 {
            assert_eq!(store.read_root(None).unwrap().value["language"], "kernel-language");
            assert_eq!(kernel_manifest(&store, &cap, Partition::Library).affected_count, 2);
            // Inject foreign removal, independent of the not-yet-implemented released APPLY route.
            store.connection.execute("DELETE FROM lww_receive_rows WHERE request_id=?1", [&header.request_id]).unwrap();
            store.device_store().unwrap().connection().execute("DELETE FROM lww_receive WHERE request_id=?1", [&header.request_id]).unwrap();
        }
        drop(store);
        let store = PersistentStore::open(directory.path()).unwrap();
        let AdvanceResult::Stale { fence, .. } = store.advance_local_native_receive(&AdvanceRequest { capability: cap.clone(),
            expected_progress_token: original.progress_token, generating: vec![] }).unwrap() else { panic!("old progress token mutated reopened operation") };
        assert!(fence.unknown_library && fence.unknown_device);
        for _ in 0..100 { if kernel_ready_for_settlement(&store, &cap) { break; } step_kernel(&store, &cap).unwrap(); }
        assert!(kernel_ready_for_settlement(&store, &cap));
        assert_eq!(store.lww_clock_state().unwrap().accepted, Some(maximum.clone()));
        assert_eq!(kernel_manifest(&store, &cap, Partition::Library).affected_count, 2);
        assert_eq!(store.read_root(None).unwrap().value["additionalPrompt"], "kernel-prompt");
        let tx = Transaction::new_unchecked(store.device_store().unwrap().connection(), TransactionBehavior::Immediate).unwrap();
        assert!(reserve_stamp(&tx).unwrap() > maximum);
        tx.commit().unwrap();
    }
    {
        let (directory, mut store) = store();
        store.device_store_mut().unwrap().set_section_participating(device_store::Section::LocalPlugins, true).unwrap();
        let received = change(&["plugin-local", "rollback", "string", "key"], device_store::now_ms().unwrap() as u64 + 100_000, serde_json::json!("device-value"));
        stage_kernel_rows(&mut store, "kernel-device-rollback", vec![received.clone()]);
        let cap = begin_kernel(&mut store);
        let before = (store.revision().unwrap(), device_revision(store.device_store().unwrap().connection()).unwrap());
        batches::kernel_test::hook(2, || {});
        let mut interrupted = false;
        for _ in 0..50 {
            match step_kernel(&store, &cap) {
                Err(StoreError::Validation { message }) if message == "synthetic-kernel-interruption" => { interrupted = true; break; }
                Ok(_) => {}, other => panic!("unexpected device outcome: {other:?}"),
            }
        }
        assert!(interrupted);
        assert_eq!(store.lww_clock_state().unwrap().accepted, Some(received.stamp.clone()));
        assert_eq!((store.revision().unwrap(), device_revision(store.device_store().unwrap().connection()).unwrap()), before);
        assert!(store.device_store().unwrap().read_plugin_device_value("rollback", "string", "key").unwrap().is_none());
        assert!(read_unit(store.device_store().unwrap().connection(), &received.key).unwrap().is_none());
        assert_eq!(kernel_manifest(&store, &cap, Partition::Device).affected_count, 0);
        assert_eq!(store.device_store().unwrap().connection().query_row("SELECT status FROM lww_receive_rows WHERE key=?1", [received.key.as_str()], |r| r.get::<_,String>(0)).unwrap(), "staged");
        drop(store);
        let store = PersistentStore::open(directory.path()).unwrap();
        for _ in 0..100 { if kernel_ready_for_settlement(&store, &cap) { break; } step_kernel(&store, &cap).unwrap(); }
        assert!(kernel_ready_for_settlement(&store, &cap));
        assert_eq!(store.device_store().unwrap().read_plugin_device_value("rollback", "string", "key").unwrap().as_deref(), Some("device-value"));
        assert_eq!(kernel_manifest(&store, &cap, Partition::Device).affected_count, 1);
        assert_eq!(store.revision().unwrap(), before.0);
        assert_eq!(device_revision(store.device_store().unwrap().connection()).unwrap(), before.1 + 1);
    }
    for remove_row in [false, true] {
        let (directory, mut store) = store();
        let mut peer = PersistentStore::open(directory.path()).unwrap();
        let received = change(&["root", "language"], device_store::now_ms().unwrap() as u64 + 100_000, serde_json::json!("after-drift"));
        let header = stage_kernel_rows(&mut store, "kernel-observe-drift", vec![received.clone()]);
        let cap = begin_kernel(&mut store);
        let before = store.revision().unwrap();
        let observed = std::rc::Rc::new(std::cell::Cell::new(false));
        let callback_observed = observed.clone();
        batches::kernel_test::inspect(1, move || {
            // This is a real independent local commit, not an old-binary APPLY simulation.
            let foreign = Header { binding_authority: peer.lww_binding_authority().unwrap(), request_id: "kernel-foreign-commit".into() };
            let input = WorkingSetCommit { expected_revision: peer.revision().unwrap(), request_id: Some(foreign.request_id.clone()),
                unit_mutations: Some(vec![mutation(&["root", "additionalPrompt"], serde_json::json!("foreign-value"))]), ..Default::default() };
            let stamp = peer.reserve_commit_only_stamp(&foreign).unwrap();
            let digest = commit_intent_digest(&input).unwrap();
            commit::commit_lww(&mut peer.connection, &input, &[], &foreign, &stamp, &digest).unwrap();
            if remove_row { peer.connection.execute("DELETE FROM lww_receive_rows WHERE request_id=?1", [&header.request_id]).unwrap(); }
            callback_observed.set(true);
        });
        for _ in 0..50 {
            let result = step_kernel(&store, &cap).unwrap();
            if observed.get() {
                let AdvanceResult::Advanced { fence, .. } = result else { panic!("observation outcome incorrectly classified precommit") };
                assert!(fence.unknown_library && fence.unknown_device);
                break;
            }
        }
        assert!(observed.get());
        assert_eq!(store.revision().unwrap(), before + 1);
        assert_eq!(store.read_root(None).unwrap().value["additionalPrompt"], "foreign-value");
        assert!(read_unit(&store.connection, &received.key).unwrap().is_none());
        assert_eq!(kernel_manifest(&store, &cap, Partition::Library).affected_count, 0);
        assert_eq!(store.lww_clock_state().unwrap().accepted, Some(received.stamp));
        for _ in 0..100 { if kernel_ready_for_settlement(&store, &cap) { break; } step_kernel(&store, &cap).unwrap(); }
        assert!(kernel_ready_for_settlement(&store, &cap));
        let AdvanceResult::Advanced { fence, .. } = step_kernel(&store, &cap).unwrap() else { panic!("current suspension token") };
        assert!(fence.unknown_library && !fence.unknown_device, "suspension keeps only the real foreign library reason");
        assert_eq!(kernel_manifest(&store, &cap, Partition::Library).affected_count, u64::from(!remove_row));
        assert_eq!(store.read_root(None).unwrap().value["language"].as_str() == Some("after-drift"), !remove_row);
        assert_eq!(store.read_root(None).unwrap().value["additionalPrompt"], "foreign-value");
    }
    for corrupt in [true, false] {
        // A reset revalidates the sealed history one page per step before it scans again.
        let (directory, mut store) = store();
        let mut peer = PersistentStore::open(directory.path()).unwrap();
        let time = device_store::now_ms().unwrap() as u64 + 100_000;
        let keys: Vec<String> = (0..5).map(|index| format!("key-{index}")).collect();
        stage_kernel_rows(&mut store, "kernel-revalidation", keys.iter()
            .map(|key| change(&["plugin", "revalidated", key], time, serde_json::json!(key))).collect());
        let cap = begin_kernel(&mut store);
        let _limits = batches::kernel_test::Limits::set(1, 100_000);
        for _ in 0..50 {
            if kernel_manifest(&store, &cap, Partition::Library).effect_pages == 4 { break; }
            step_kernel(&store, &cap).unwrap();
        }
        let sealed = kernel_manifest(&store, &cap, Partition::Library);
        assert_eq!((sealed.effect_pages, sealed.cursor.phase), (4, batches::ScanPhase::Remaining));
        let foreign = Header { binding_authority: peer.lww_binding_authority().unwrap(), request_id: "kernel-revalidation-foreign".into() };
        let input = WorkingSetCommit { expected_revision: peer.revision().unwrap(), request_id: Some(foreign.request_id.clone()),
            unit_mutations: Some(vec![mutation(&["root", "additionalPrompt"], serde_json::json!("foreign-value"))]), ..Default::default() };
        let stamp = peer.reserve_commit_only_stamp(&foreign).unwrap();
        let digest = commit_intent_digest(&input).unwrap();
        commit::commit_lww(&mut peer.connection, &input, &[], &foreign, &stamp, &digest).unwrap();
        fn row_statuses(store: &PersistentStore) -> Vec<(String, String)> {
            store.connection.prepare("SELECT key,status FROM lww_receive_rows ORDER BY rowid").unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().collect::<Result<_, _>>().unwrap()
        }
        let statuses = row_statuses(&store);
        assert_eq!(statuses.iter().filter(|(_, status)| status == "staged").count(), 1);
        let revision = store.revision().unwrap();
        if corrupt {
            let pages: Vec<(String, rusqlite::types::Value)> = [0u64, 1].into_iter().map(|ordinal| store.connection.query_row(
                "SELECT key,value FROM app_kv WHERE key LIKE ?1", [format!("%effect:{ordinal:020}")], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()).collect();
            store.connection.execute("UPDATE app_kv SET value=?2 WHERE key=?1", rusqlite::params![pages[0].0, pages[1].1]).unwrap();
            store.connection.execute("UPDATE app_kv SET value=?2 WHERE key=?1", rusqlite::params![pages[1].0, pages[0].1]).unwrap();
            for _ in 0..2 {
                assert!(matches!(step_kernel(&store, &cap), Err(StoreError::Validation { message }) if message == "native-receive-effect-page-integrity"));
            }
            assert_eq!(row_statuses(&store), statuses);
            assert_eq!(store.revision().unwrap(), revision);
            continue;
        }
        for step in 1..=4u64 {
            if step == 2 {
                drop(store);
                store = PersistentStore::open(directory.path()).unwrap();
                assert_eq!(serde_json::to_value(kernel_manifest(&store, &cap, Partition::Library)).unwrap()["validation"]["effectOrdinal"], 1);
            }
            let AdvanceResult::Advanced { fence, .. } = step_kernel(&store, &cap).unwrap() else { panic!("current token was stale") };
            let manifest = kernel_manifest(&store, &cap, Partition::Library);
            assert!(manifest.foreign.library && !manifest.foreign.device);
            if step < 4 {
                assert!(fence.unknown_library && !fence.unknown_device && fence.keys.is_empty());
                assert_eq!(manifest.cursor.epoch, sealed.cursor.epoch);
                assert_eq!(serde_json::to_value(&manifest).unwrap()["validation"]["effectOrdinal"], step);
            } else {
                assert!(fence.unknown_library && fence.unknown_device);
                assert_eq!(manifest.cursor.epoch, sealed.cursor.epoch + 1);
                assert!(manifest.validation.is_none() && manifest.validated.is_some());
            }
            assert_eq!(store.revision().unwrap(), revision);
            assert_eq!(row_statuses(&store), statuses);
        }
        for _ in 0..100 { if kernel_ready_for_settlement(&store, &cap) { break; } step_kernel(&store, &cap).unwrap(); }
        assert!(kernel_ready_for_settlement(&store, &cap));
        let settled = kernel_manifest(&store, &cap, Partition::Library);
        assert_eq!((settled.effect_pages, settled.affected_count), (5, 5));
        assert_eq!(store.revision().unwrap(), revision + 1);
        assert!(row_statuses(&store).iter().all(|(_, status)| status == "done"));
        for key in &keys { assert_eq!(store.read_plugin_storage("revalidated", key, None).unwrap().unwrap().value, key.as_str()); }
    }
    {
        let (directory, mut store) = store();
        let peer = std::rc::Rc::new(PersistentStore::open(directory.path()).unwrap());
        for connection in [&store.connection, store.device_store().unwrap().connection(), &peer.connection, peer.device_store().unwrap().connection()] {
            connection.busy_timeout(std::time::Duration::ZERO).unwrap();
        }
        let time = device_store::now_ms().unwrap() as u64 + 100_000;
        let plugin = change(&["plugin", "guarded", "key"], time, serde_json::json!("guarded-plugin"));
        stage_kernel_rows(&mut store, "kernel-guarded-commit", vec![change(&["root", "language"], time, serde_json::json!("guarded")), plugin.clone()]);
        let cap = begin_kernel(&mut store);
        let request = |store: &PersistentStore| AdvanceRequest { capability: cap.clone(),
            expected_progress_token: store.read_native_receive_state(&cap).unwrap().progress_token, generating: vec![] };
        // A peer write lock at either guard is retryable contention, before and after the clock observation.
        for held in [peer.device_store().unwrap().connection(), &peer.connection] {
            let request = request(&store);
            held.execute_batch("BEGIN IMMEDIATE").unwrap();
            assert!(matches!(store.advance_local_native_receive(&request), Err(StoreError::CommitBusy)));
            held.execute_batch("ROLLBACK").unwrap();
        }
        let held = peer.clone();
        batches::kernel_test::inspect(1, move || held.connection.execute_batch("BEGIN IMMEDIATE").unwrap());
        assert!((0..50).any(|_| match store.advance_local_native_receive(&request(&store)) {
            Err(StoreError::CommitBusy) => true, result => { result.unwrap(); false }
        }));
        peer.connection.execute_batch("ROLLBACK").unwrap();
        let phases = std::rc::Rc::new(std::cell::Cell::new(0));
        let callback_phases = phases.clone();
        let callback_peer = peer.clone();
        batches::kernel_test::inspect(3, move || {
            let peer = callback_peer;
            for connection in [&peer.connection, peer.device_store().unwrap().connection()] {
                assert!(matches!(Transaction::new_unchecked(connection, TransactionBehavior::Immediate),
                    Err(rusqlite::Error::SqliteFailure(error, _)) if error.code == rusqlite::ErrorCode::DatabaseBusy));
            }
            callback_phases.set(1);
            batches::kernel_test::inspect(2, move || {
                // The released library lock is taken and held past the device commit.
                peer.connection.execute_batch("BEGIN IMMEDIATE").unwrap();
                assert!(matches!(Transaction::new_unchecked(peer.device_store().unwrap().connection(), TransactionBehavior::Immediate),
                    Err(rusqlite::Error::SqliteFailure(error, _)) if error.code == rusqlite::ErrorCode::DatabaseBusy));
                assert_eq!(peer.read_root(None).unwrap().value["language"], "guarded");
                callback_phases.set(2);
            });
        });
        let AdvanceResult::Advanced { state, fence } = store.advance_local_native_receive(&request(&store)).unwrap()
            else { panic!("current token was stale") };
        assert_eq!(phases.get(), 2);
        assert_eq!(fence.keys, vec![plugin.key.clone()], "a committed step keeps its fence while a peer holds the library lock");
        assert!(!fence.unknown_library && !fence.unknown_device);
        peer.connection.execute_batch("ROLLBACK").unwrap();
        assert_eq!(state.progress_token, store.read_native_receive_state(&cap).unwrap().progress_token);
        for _ in 0..100 { if kernel_ready_for_settlement(&store, &cap) { break; } step_kernel(&store, &cap).unwrap(); }
        assert!(kernel_ready_for_settlement(&store, &cap));
        assert_eq!(store.read_root(None).unwrap().value["language"], "guarded");
        assert_eq!(store.read_plugin_storage("guarded", "key", None).unwrap().unwrap().value, "guarded-plugin");
        assert_eq!(kernel_manifest(&store, &cap, Partition::Library).affected_count, 2);
    }
}

#[test]
fn receive_kernel_installs_both_archive_directions_through_commit_only_cas() {
    use crate::asset_repository::{job_pins::durable_cas_job_ids, PayloadCas};
    use crate::native_generation::operation::{Start, Target};
    use crate::persistent_store::archive::{is_archived, read_archived_object};
    use crate::persistent_store::receive_batches::{self as batches, AdvanceRequest, Partition, Source, TransitionDecision};
    for mode in ["archive", "restore", "protected", "released", "same-state", "parent", "identical-archive", "identical-restore",
        "cross", "missing-cas", "generation-blocked", "generation-raced", "renderer"] {
        let identical = mode.starts_with("identical-");
        let restore = matches!(mode, "restore" | "identical-restore" | "missing-cas");
        let (_source_directory, mut source) = store();
        let (directory, mut target) = store();
        create_conversation(&mut source, "char", "chat");
        if mode != "parent" {
            create_conversation(&mut target, "char", "chat");
            // Only this device's own archive payload carries these fields.
            save(&mut target, vec![mutation(&["character", "char", "localFuture"], serde_json::json!("target-private")),
                mutation(&["conversation", "char", "chat", "localFuture"], serde_json::json!("target-chat"))]);
        }
        clear_outbox(&mut source);
        source.archive_character("char", source.revision().unwrap(), 10).unwrap();
        copy_objects(&source, &target);
        let archive = source.lww_read_outbox(0.into(), 100).unwrap().entries.into_iter().find(|entry| entry.key.components()[0] == "archive").unwrap();
        let incoming = archive_metadata(&target.connection, &archive.value).unwrap();
        let local_archive = if restore || mode == "same-state" {
            target.archive_character("char", target.revision().unwrap(), 9).unwrap();
            Some(read_archived_object(&target.connection, &active_generation(&target.connection).unwrap(), "char").unwrap().unwrap())
        } else { None };
        let cas = PayloadCas::new(target.repository_root()).unwrap();
        let missing = (mode == "missing-cas").then(|| {
            let hash = &local_archive.as_ref().unwrap().object_hash;
            let bytes = cas.read_object(hash).unwrap().unwrap();
            std::fs::remove_file(cas.object_path(hash).unwrap().unwrap()).unwrap();
            bytes
        });
        let generation_target = |store: &PersistentStore| Target { character_id: "char".into(), conversation_id: "chat".into(),
            expected_revision: store.revision().unwrap(), library_identity: store.library_identity().unwrap(),
            binding_authority: store.lww_binding_authority().unwrap() };
        let owner = if matches!(mode, "protected" | "released") {
            let ticket = target.generation_reserve(generation_target(&target)).unwrap();
            let Start::Started { owner, .. } = target.generation_start(&ticket).unwrap() else { panic!("generation start") };
            Some(owner)
        } else { None };
        // A second handle starts the generation through its own device guard.
        let peer = matches!(mode, "generation-blocked" | "generation-raced").then(|| {
            let peer = PersistentStore::open(directory.path()).unwrap();
            for connection in [&peer.connection, peer.device_store().unwrap().connection()] {
                connection.busy_timeout(std::time::Duration::ZERO).unwrap();
            }
            std::rc::Rc::new(std::cell::RefCell::new(peer))
        });
        let reserved = peer.as_ref().map(|peer| {
            let generation = generation_target(&peer.borrow());
            peer.borrow_mut().generation_reserve(generation).unwrap()
        });
        let time = device_store::now_ms().unwrap() as u64 + 100_000;
        let mut changes = vec![Change { key: archive.key.clone(), stamp: stamp(time),
            value: if restore { UnitValue::Deleted } else { archive.value.clone() } },
            change(&["root", "language"], time, serde_json::json!("unrelated"))];
        if mode == "parent" {
            changes.push(change(&["exists", "character", "char"], time, serde_json::json!({"type":"character"})));
            changes.push(change(&["exists", "conversation", "char", "chat"], time, serde_json::json!(true)));
        }
        if mode == "restore" {
            // The restore installs before Exists, so a new conversation and its field are never held behind it.
            changes.push(change(&["exists", "conversation", "char", "fresh"], time, serde_json::json!(true)));
            changes.push(change(&["conversation", "char", "fresh", "name"], time, serde_json::json!("fresh-name")));
        }
        let request = if identical {
            let local = target.lww_read_outbox(0.into(), 100).unwrap().entries.into_iter().next().unwrap();
            assert!(target.lww_request_recorded(&local.version).unwrap());
            let tx = Transaction::new_unchecked(&target.connection, TransactionBehavior::Immediate).unwrap();
            // Restored held units can own local publication before residency catches up.
            put_unit(&tx, &changes[0].key, &changes[0].stamp, &changes[0].value, &local.version, Some(target.lww_binding_authority().unwrap())).unwrap();
            tx.commit().unwrap();
            local.version
        } else { format!("archive-{mode}") };
        let header = stage_kernel_rows(&mut target, &request, changes);
        if identical {
            target.connection.execute("UPDATE lww_receive_rows SET status=?2 WHERE request_id=?1 AND key=?3",
                params![header.request_id, if restore { "deferred" } else { "held" }, archive.key.as_str()]).unwrap();
        }
        let before_unit = read_unit(&target.connection, &archive.key).unwrap();
        let before_outbox = serde_json::to_value(target.lww_read_outbox(0.into(), 100).unwrap()).unwrap();
        let before_row: String = target.connection.query_row("SELECT json_array(request_id,key,stamp,value,status) FROM lww_receive_rows WHERE request_id=?1 AND key=?2",
            params![header.request_id, archive.key.as_str()], |row| row.get(0)).unwrap();
        let cap = if mode == "cross" {
            // Another staged receive drives the archive row the first one left staged.
            let trigger = stage_kernel_rows(&mut target, "archive-cross-trigger", vec![change(&["root", "additionalPrompt"], time, serde_json::json!("trigger"))]);
            begin_kernel_from(&mut target, Source::Staged { header: trigger })
        } else if matches!(mode, "archive" | "restore") {
            let source = Source::Drain { binding_authority: target.lww_binding_authority().unwrap() };
            let candidate = target.prepare_native_receive(&source).unwrap();
            let batches::BeginResult::Admitted { state } = target.begin_local_native_receive(&batches::BeginRequest {
                candidate, source, native_baseline: None }).unwrap() else { panic!("fresh kernel admission expected") };
            assert!(state.foreign.scope);
            state.capability
        } else { begin_kernel(&mut target) };
        let _limits = batches::kernel_test::Limits::set(1, 1024);
        let started = std::rc::Rc::new(std::cell::RefCell::new(None));
        let blocked = std::rc::Rc::new(std::cell::Cell::new(false));
        if let (Some(peer), Some(reserved)) = (peer.clone(), reserved.clone()) {
            if mode == "generation-raced" {
                let started = started.clone();
                // The generation starts after staging and before the install takes its guards.
                batches::kernel_test::inspect(4, move || {
                    let Start::Started { owner, .. } = peer.borrow_mut().generation_start(&reserved).unwrap() else { panic!("generation start") };
                    *started.borrow_mut() = Some(owner);
                });
            } else {
                let blocked = blocked.clone();
                // The install holds the device guard through its library commit.
                batches::kernel_test::inspect(5, move || {
                    assert!(peer.borrow_mut().generation_start(&reserved).is_err());
                    blocked.set(true);
                });
            }
        }
        let mut parked = None;
        let mut missing_failed = false;
        let mut released = false;
        let (mut renderer, mut renderer_deferred) = (false, false);
        for _ in 0..100 {
            // A renderer generation that starts after the flip parked keeps the row for a later phase.
            let generating = if std::mem::take(&mut renderer) {
                vec![MessageLocator { character_id: "char".into(), conversation_id: "chat".into(), start: None }]
            } else { vec![] };
            let rendering = !generating.is_empty();
            let step = target.read_native_receive_state(&cap).and_then(|state| target.advance_local_native_receive(
                &AdvanceRequest { capability: cap.clone(), expected_progress_token: state.progress_token, generating }));
            match step {
                Err(StoreError::Validation { message }) if message == "archive-object-missing" && missing.is_some() && !missing_failed => {
                    // Nothing changes until the archive body is here again; the incoming stamp is already observed.
                    missing_failed = true;
                    assert_eq!(serde_json::to_value(&kernel_manifest(&target, &cap, Partition::Library).ticket).unwrap(), serde_json::to_value(&parked).unwrap());
                    assert!(is_archived(&target.connection, &active_generation(&target.connection).unwrap(), "char").unwrap());
                    assert_eq!(target.lww_clock_state().unwrap().accepted, Some(stamp(time)));
                    cas.prepare_bytes(missing.as_ref().unwrap()).unwrap();
                }
                result => { result.unwrap(); }
            }
            let status: String = target.connection.query_row("SELECT status FROM lww_receive_rows WHERE request_id=?1 AND key=?2", params![header.request_id,archive.key.as_str()], |row| row.get(0)).unwrap();
            if rendering {
                assert_eq!(status, "deferred");
                assert!(kernel_manifest(&target, &cap, Partition::Library).ticket.is_none());
                assert_eq!(is_archived(&target.connection, &active_generation(&target.connection).unwrap(), "char").unwrap(), restore);
                renderer_deferred = true;
            }
            if status == "deferred" && !released {
                if mode == "released" { target.generation_finish(owner.as_ref().unwrap(), true).unwrap(); released = true; }
                if let (Some(owner), Some(peer)) = (started.borrow_mut().take(), peer.as_ref()) {
                    peer.borrow_mut().generation_finish(&owner, true).unwrap();
                    released = true;
                }
            }
            let manifest = kernel_manifest(&target, &cap, Partition::Library);
            if let (None, Some(ticket)) = (&parked, &manifest.ticket) {
                // A parked flip changes nothing before its install.
                assert_eq!(is_archived(&target.connection, &active_generation(&target.connection).unwrap(), "char").unwrap(), restore);
                if identical {
                    assert_eq!(read_unit(&target.connection, &archive.key).unwrap(), before_unit);
                    assert_eq!(serde_json::to_value(target.lww_read_outbox(0.into(), 100).unwrap()).unwrap(), before_outbox);
                    assert_eq!(target.connection.query_row("SELECT json_array(request_id,key,stamp,value,status) FROM lww_receive_rows WHERE request_id=?1 AND key=?2",
                        params![header.request_id, archive.key.as_str()], |row| row.get::<_,String>(0)).unwrap(), before_row);
                }
                if mode == "parent" { assert!(target.read_conversation("char", "chat", None).unwrap().is_some()); }
                renderer = mode == "renderer";
                parked = Some(ticket.clone());
            }
            if kernel_ready_for_settlement(&target, &cap) { break; }
        }
        assert!(kernel_ready_for_settlement(&target, &cap));
        assert_eq!(missing_failed, mode == "missing-cas");
        assert_eq!(released, matches!(mode, "released" | "generation-raced"));
        assert_eq!(blocked.get(), mode == "generation-blocked");
        assert_eq!(renderer_deferred, mode == "renderer");
        let manifest = kernel_manifest(&target, &cap, Partition::Library);
        let generation = active_generation(&target.connection).unwrap();
        let status: String = target.connection.query_row("SELECT status FROM lww_receive_rows WHERE request_id=?1 AND key=?2", params![header.request_id,archive.key.as_str()], |row| row.get(0)).unwrap();
        // The receipt keeps the identity of the receive that staged the row, whichever operation installs it.
        let receipt = format!("archive-transition:{}:{}", header.request_id, risunest_sync_wire::hash(archive.key.as_str().as_bytes()));
        let receipted: bool = target.connection.query_row("SELECT EXISTS(SELECT 1 FROM lww_requests WHERE request_id=?1)", [&receipt], |row| row.get(0)).unwrap();
        assert!(durable_cas_job_ids(directory.path()).unwrap().is_empty());
        assert_eq!(target.read_root(None).unwrap().value["language"], "unrelated");
        if matches!(mode, "protected" | "same-state") {
            assert!(parked.is_none() && !receipted);
            if mode == "protected" {
                assert_eq!(status, "deferred");
                assert!(target.read_conversation("char", "chat", None).unwrap().is_some());
                assert_eq!(manifest.affected_count, 1);
            } else {
                // The local payload stays; only the shared descriptor follows the incoming archive.
                let current = read_archived_object(&target.connection, &generation, "char").unwrap().unwrap();
                let local = local_archive.as_ref().unwrap();
                assert_eq!((&current.object_hash, &current.asset_hashes), (&local.object_hash, &local.asset_hashes));
                assert_eq!((&current.shared_object_hash, current.archived_at), (&incoming.shared_object_hash, incoming.archived_at));
                assert_eq!(manifest.affected_count, 2);
            }
        } else {
            let ticket = parked.expect("unsafe archive flip must park before its install");
            assert_eq!(ticket.restore, restore);
            assert_eq!(ticket.decision, if identical { TransitionDecision::Identical } else { TransitionDecision::ApplyRemote });
            assert_eq!(ticket.archive_key, archive.key.as_str());
            assert!(manifest.ticket.is_none() && manifest.outcome.is_none());
            assert_eq!(status, "done");
            assert_eq!(is_archived(&target.connection, &generation, "char").unwrap(), !restore);
            assert_eq!(receipted, !identical);
            if identical {
                assert_eq!(read_unit(&target.connection, &archive.key).unwrap(), before_unit);
                // The page revision follows the install commits; every local entry stays exact.
                assert_eq!(serde_json::to_value(target.lww_read_outbox(0.into(), 100).unwrap()).unwrap()["entries"], before_outbox["entries"]);
            } else {
                assert_eq!(read_unit(&target.connection, &archive.key).unwrap(),
                    Some((stamp(time), if restore { UnitValue::Deleted } else { archive.value.clone() })));
            }
            assert_eq!(manifest.affected_count, match mode { "restore" | "parent" => 4, "cross" => 3, _ => 2 });
            if !restore {
                // This device's own payload is published and cataloged with the archive pointer.
                let archived = read_archived_object(&target.connection, &generation, "char").unwrap().unwrap();
                assert!(cas.stat_object(&archived.object_hash).unwrap().is_some());
                assert_eq!(target.connection.query_row("SELECT count(*) FROM asset_objects WHERE object_hash=?1", [&archived.object_hash], |row| row.get::<_, i64>(0)).unwrap(), 1);
                assert_ne!(archived.object_hash, archived.shared_object_hash);
                assert_eq!((&archived.shared_object_hash, archived.archived_at), (&incoming.object_hash, incoming.archived_at));
            }
        }
        if let (Some(peer), Some(reserved)) = (peer.as_ref(), reserved.as_ref()) {
            if mode == "generation-blocked" {
                assert!(!matches!(peer.borrow_mut().generation_start(reserved), Ok(Start::Started { .. })), "a generation must not start on the installed archive");
            }
        }
        if mode == "cross" { assert_eq!(target.read_root(None).unwrap().value["additionalPrompt"], "trigger"); }
        if mode == "restore" { assert_eq!(target.read_conversation("char", "fresh", None).unwrap().unwrap().value["name"], "fresh-name"); }
        assert_eq!(seal_kernel(&target, &cap), crate::persistent_store::receive_batches::Outcome::Processed);
        if is_archived(&target.connection, &generation, "char").unwrap() {
            target.restore_character("char", target.revision().unwrap()).unwrap();
        }
        assert!(target.read_conversation("char", "chat", None).unwrap().is_some());
        if mode != "parent" {
            assert_eq!(target.read_character("char", None).unwrap().unwrap().value["localFuture"], "target-private");
            assert_eq!(target.read_conversation("char", "chat", None).unwrap().unwrap().value["localFuture"], "target-chat");
        }
    }
}

#[test]
fn receive_kernel_resumes_an_interrupted_first_archive_after_reopen_in_both_directions() {
    use crate::asset_repository::{job_pins::durable_cas_job_ids, PayloadCas};
    use crate::persistent_store::archive::{is_archived, read_archived_object};
    use crate::persistent_store::receive_batches::{self as batches, BeginRequest, BeginResult, Partition, Source};
    let owners = ["first", "second"];
    for restore in [false, true] {
        // 5 fails after the first payload is published and before its library commit; 6 fails after that commit.
        for interrupt in [5u8, 6] {
            let (_source_directory, mut source) = store();
            let (directory, mut target) = store();
            for owner in owners {
                create_conversation(&mut source, owner, "chat");
                create_conversation(&mut target, owner, "chat");
                save(&mut target, vec![mutation(&["character", owner, "localFuture"], serde_json::json!(format!("{owner}-private"))),
                    mutation(&["conversation", owner, "chat", "localFuture"], serde_json::json!(format!("{owner}-chat")))]);
            }
            clear_outbox(&mut source);
            for owner in owners { source.archive_character(owner, source.revision().unwrap(), 10).unwrap(); }
            copy_objects(&source, &target);
            if restore {
                for owner in owners { target.archive_character(owner, target.revision().unwrap(), 9).unwrap(); }
            }
            let entries = source.lww_read_outbox(0.into(), 100).unwrap().entries;
            let time = device_store::now_ms().unwrap() as u64 + 100_000;
            stage_kernel_rows(&mut target, "interrupted-archives", owners.iter().map(|owner| {
                let entry = entries.iter().find(|entry| entry.key.components() == ["archive", owner]).unwrap();
                Change { key: entry.key.clone(), stamp: stamp(time), value: if restore { UnitValue::Deleted } else { entry.value.clone() } }
            }).collect());
            let cap = begin_kernel(&mut target);
            let revision = target.revision().unwrap();
            batches::kernel_test::hook(interrupt, || {});
            let mut interrupted = false;
            for _ in 0..20 {
                match step_kernel(&target, &cap) {
                    Err(StoreError::Validation { message }) if message == "synthetic-kernel-interruption" => { interrupted = true; break; }
                    result => { result.unwrap(); }
                }
            }
            assert!(interrupted);
            let generation = active_generation(&target.connection).unwrap();
            let flipped = |store: &PersistentStore, owner: &str| is_archived(&store.connection, &generation, owner).unwrap() != restore;
            assert_eq!(flipped(&target, "first"), interrupt == 6);
            assert!(!flipped(&target, "second"));
            assert_eq!(target.revision().unwrap(), revision + i64::from(interrupt == 6));
            let library = kernel_manifest(&target, &cap, Partition::Library);
            assert_eq!((library.ticket.is_some(), library.affected_count), (interrupt == 5, u64::from(interrupt == 6)));
            // The pre-install observation is durable, and an aborted publication leaves no live journal.
            assert_eq!(target.lww_clock_state().unwrap().accepted, Some(stamp(time)));
            assert!(durable_cas_job_ids(directory.path()).unwrap().is_empty());
            drop(target);
            let mut target = PersistentStore::open(directory.path()).unwrap();
            assert_eq!(target.lww_clock_state().unwrap().accepted, Some(stamp(time)));
            // A startup drain helps the existing operation instead of projecting its rows raw.
            let drain = Source::Drain { binding_authority: target.lww_binding_authority().unwrap() };
            let candidate = target.prepare_native_receive(&drain).unwrap();
            let BeginResult::Recovery { state } = target.begin_local_native_receive(&BeginRequest { candidate, source: drain, native_baseline: None }).unwrap()
                else { panic!("the interrupted operation must stay the producer") };
            assert_eq!(state.capability, cap);
            for _ in 0..100 { if kernel_ready_for_settlement(&target, &cap) { break; } step_kernel(&target, &cap).unwrap(); }
            assert!(kernel_ready_for_settlement(&target, &cap));
            assert_eq!(kernel_manifest(&target, &cap, Partition::Library).affected_count, 2);
            assert_eq!(target.revision().unwrap(), revision + 2);
            assert!(durable_cas_job_ids(directory.path()).unwrap().is_empty());
            assert_eq!(seal_kernel(&target, &cap), crate::persistent_store::receive_batches::Outcome::Processed);
            let cas = PayloadCas::new(target.repository_root()).unwrap();
            for owner in owners {
                assert!(flipped(&target, owner));
                if !restore {
                    let archived = read_archived_object(&target.connection, &generation, owner).unwrap().unwrap();
                    assert!(cas.stat_object(&archived.object_hash).unwrap().is_some());
                    target.restore_character(owner, target.revision().unwrap()).unwrap();
                }
                assert_eq!(target.read_character(owner, None).unwrap().unwrap().value["localFuture"], format!("{owner}-private"));
                assert_eq!(target.read_conversation(owner, "chat", None).unwrap().unwrap().value["localFuture"], format!("{owner}-chat"));
            }
        }
    }
}

#[test]
fn receive_kernel_identical_local_pending_and_retired_children_keep_their_exact_contract() {
    use crate::persistent_store::receive_batches::Partition;
    let (_directory, mut store) = store();
    create_conversation(&mut store, "retired", "chat");
    save(&mut store, vec![mutation(&["root", "language"], serde_json::json!("local"))]);
    let local = store.lww_read_outbox(0.into(), 100).unwrap().entries.into_iter().find(|entry| entry.key == unit_key(&["root", "language"]).unwrap()).unwrap();
    stage_kernel_rows(&mut store, &local.version, vec![Change { key: local.key.clone(), stamp: local.stamp.clone(), value: local.value.clone() }]);
    store.connection.execute("UPDATE lww_receive_rows SET status='held' WHERE request_id=?1", [&local.version]).unwrap();
    stage_kernel_rows(&mut store, "kernel-old-delete", vec![Change { key: unit_key(&["exists", "character", "retired"]).unwrap(), stamp: stamp(1), value: UnitValue::Deleted },
        change(&["conversation", "retired", "chat", "note"], 2, serde_json::json!("must-not-project"))]);
    let original_unit: (String, String, String) = store.connection.query_row("SELECT stamp,value,version FROM lww_units WHERE key=?1", [local.key.as_str()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    let cap = begin_kernel(&mut store);
    for _ in 0..100 { if kernel_ready_for_settlement(&store, &cap) { break; } step_kernel(&store, &cap).unwrap(); }
    assert!(kernel_ready_for_settlement(&store, &cap));
    assert_eq!(store.read_root(None).unwrap().value["language"], "local");
    assert_eq!(store.connection.query_row("SELECT stamp,value,version FROM lww_units WHERE key=?1", [local.key.as_str()], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).unwrap(), original_unit);
    assert!(store.lww_read_outbox(0.into(), 100).unwrap().entries.iter().any(|entry| entry.key == local.key && entry.version == local.version && entry.stamp == local.stamp && entry.value == local.value));
    assert!(store.read_character("retired", None).unwrap().is_none());
    assert!(matches!(read_unit(&store.connection, &unit_key(&["exists", "character", "retired"]).unwrap()).unwrap(), Some((_, UnitValue::Deleted))));
    assert!(read_unit(&store.connection, &unit_key(&["conversation", "retired", "chat", "note"]).unwrap()).unwrap().is_none());
    assert_eq!(kernel_manifest(&store, &cap, Partition::Library).affected_count, 2);
}
