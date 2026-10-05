use super::*;
use crate::persistent_store::sync_selection::{SwitchBindingRequest, SyncTarget};

fn quarantine(store: &mut PersistentStore, request_id: &str) -> QuarantinedIntent {
    for _ in 0..3 {
        store.record_intent_failure(request_id, &error("synthetic-cross-store-stop")).unwrap();
    }
    diagnosed(store, request_id)
}

fn diagnosed(store: &PersistentStore, request_id: &str) -> QuarantinedIntent {
    store.lww_quarantined_intents().unwrap().into_iter()
        .find(|intent| intent.request_id == request_id).unwrap()
}

fn failure_evidence(store: &PersistentStore, request_id: &str) -> (i64, String, bool, bool) {
    store.device_store().unwrap().connection().query_row(
        "SELECT f.failures,f.error,f.quarantined,i.complete FROM lww_intent_failures f
         JOIN lww_intents i ON i.request_id=f.request_id WHERE f.request_id=?1",
        [request_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).unwrap()
}

fn partial_switch(retain: bool) -> (tempfile::TempDir, PersistentStore, SwitchBindingRequest) {
    let (dir, mut store) = store();
    save(&mut store, vec![mutation(&["root", "language"], serde_json::json!("before-switch"))]);
    let original = store.lww_binding_state().unwrap();
    let target = if retain { SyncTarget::None } else { SyncTarget::External("synthetic-target".into()) };
    let inspection_id = (!retain).then(|| store.register_lww_binding_inspection(
        original.target_authority, &target, "synthetic-target-id", "synthetic-library-id",
    ).unwrap());
    let request = SwitchBindingRequest {
        initial_publication: false,
        header: Header { binding_authority: original.target_authority, request_id: "quarantined-switch".into() },
        expected_selection_epoch: original.selection_epoch,
        target,
        inspection_id,
    };
    store.stop_next_switch_after_library_commit();
    assert!(store.switch_lww_binding(&request).is_err());
    let selected = quarantine(&mut store, &request.header.request_id);
    assert!(!selected.discardable && selected.recoverable);
    (dir, store, request)
}

#[test]
fn quarantined_switch_reopens_and_completes_once_without_resetting_later_library_edits() {
    for retain in [false, true] {
        let (dir, mut store, request) = partial_switch(retain);
        let old = request.header.binding_authority;
        let new = DecimalU64(old.0 + 1);
        let original = store.lww_read_outbox(new, 100).err();
        assert!(original.is_some(), "the device authority has not committed");
        let log = crate::server_sync::lww_client::OperationLog::open(dir.path()).unwrap();
        let page = StageReceive {
            header: Header { binding_authority: old, request_id: "unrelated-remote-page".into() },
            changes: vec![],
            progress: Progress { kind: "server".into(), cursor: 7.into(), writer_id: None },
            admitted_time_upper_ms: 9.into(),
        };
        log.0.execute("INSERT INTO receive_pages VALUES(?1,?2,NULL,1,0)", params![old.0.to_string(), serde_json::to_string(&page).unwrap()]).unwrap();
        log.0.execute("INSERT INTO bootstrap VALUES(?1,'synthetic-pin',NULL,'7')", [old.0.to_string()]).unwrap();
        drop(log);
        save(&mut store, vec![mutation(&["root", "askRemoval"], serde_json::json!(true))]);
        let later = store.lww_read_outbox(old, 100).unwrap().entries.into_iter()
            .find(|entry| entry.key == unit_key(&["root", "askRemoval"]).unwrap()).unwrap();
        let revision = store.revision().unwrap();
        let selected = diagnosed(&store, &request.header.request_id);
        assert!(selected.recoverable);
        drop(store);
        let mut store = PersistentStore::open(dir.path()).unwrap();
        assert_eq!(store.lww_binding_authority().unwrap(), old);
        assert!(store.lww_complete_quarantined_intent(&selected.request_id, &selected.token, revision).unwrap());
        assert_eq!(store.lww_binding_authority().unwrap(), new);
        assert_eq!(store.revision().unwrap(), revision);
        assert_eq!(store.read_root(None).unwrap().value["askRemoval"], true);
        let entries = store.lww_read_outbox(new, 100).unwrap().entries;
        let completed = entries.iter().find(|entry| entry.key == later.key).unwrap();
        assert_eq!((&completed.stamp, &completed.value, &completed.version), (&later.stamp, &later.value, &later.version));
        assert_eq!(entries.iter().any(|entry| entry.key == unit_key(&["root", "language"]).unwrap()), retain);
        let log = crate::server_sync::lww_client::OperationLog::open(dir.path()).unwrap();
        let (authority, body): (String, String) = log.0.query_row("SELECT authority,body FROM receive_pages", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        let carried = if retain { new } else { old };
        assert_eq!(authority, carried.0.to_string());
        assert_eq!(serde_json::from_str::<StageReceive>(&body).unwrap().header.binding_authority, carried);
        assert_eq!(log.0.query_row("SELECT authority FROM bootstrap", [], |row| row.get::<_, String>(0)).unwrap(), carried.0.to_string());
        assert!(store.lww_quarantined_intents().unwrap().is_empty());
        let after = serde_json::to_value(&entries).unwrap();
        assert!(!store.lww_complete_quarantined_intent(&selected.request_id, &selected.token, revision).unwrap());
        assert_eq!(serde_json::to_value(store.lww_read_outbox(new, 100).unwrap().entries).unwrap(), after);
        assert_eq!(store.revision().unwrap(), revision);
    }
}

#[test]
fn quarantined_switch_rejects_stale_diagnosis_and_changed_inputs_without_losing_evidence() {
    for fault in ["token", "revision", "body", "receipt", "authority", "intent-authority", "stamp", "device-revision"] {
        let (_dir, mut store, request) = partial_switch(true);
        let selected = diagnosed(&store, &request.header.request_id);
        let original_revision = store.revision().unwrap();
        match fault {
            "token" => { store.record_intent_failure(&selected.request_id, &error("another-failed-replay")).unwrap(); }
            "revision" => { save(&mut store, vec![mutation(&["root", "askRemoval"], serde_json::json!(true))]); }
            "body" => { store.device_store().unwrap().connection().execute("UPDATE lww_intents SET body='{}' WHERE request_id=?1", [&selected.request_id]).unwrap(); }
            "receipt" => { store.connection.execute("UPDATE lww_requests SET digest=?1 WHERE request_id=?2", params!["0".repeat(64), selected.request_id]).unwrap(); }
            "authority" => { store.device_store().unwrap().connection().execute("UPDATE lww_clock SET binding_authority='99' WHERE singleton=1", []).unwrap(); }
            "intent-authority" => { store.device_store().unwrap().connection().execute("UPDATE lww_intents SET authority='99' WHERE request_id=?1", [&selected.request_id]).unwrap(); }
            "stamp" => {
                let body: String = store.device_store().unwrap().connection().query_row("SELECT stamp FROM lww_intents WHERE request_id=?1", [&selected.request_id], |row| row.get(0)).unwrap();
                let mut changed: Stamp = serde_json::from_str(&body).unwrap();
                changed.logical += 1;
                store.device_store().unwrap().connection().execute("UPDATE lww_intents SET stamp=?1 WHERE request_id=?2", params![serde_json::to_string(&changed).unwrap(), selected.request_id]).unwrap();
            }
            "device-revision" => {
                let device = store.device_store_mut().unwrap();
                let before = device.revision().unwrap();
                device.write_plugin_device_values("quarantine-fixture", &[device_store::plugin_values::PluginDeviceMutation::Set {
                    space: "string".into(), key: "later".into(), value: "later-device".into(),
                }]).unwrap();
                assert!(device.revision().unwrap() > before);
            }
            _ => unreachable!(),
        }
        let evidence = failure_evidence(&store, &selected.request_id);
        let current = diagnosed(&store, &selected.request_id);
        let token = if fault == "token" { &selected.token } else { &current.token };
        let revision = if fault == "revision" { original_revision } else { store.revision().unwrap() };
        assert!(store.lww_complete_quarantined_intent(&selected.request_id, token, revision).is_err(), "{fault}");
        assert_eq!(failure_evidence(&store, &selected.request_id), evidence, "{fault}");
        if !matches!(fault, "token" | "revision") {
            assert!(!current.recoverable, "{fault}");
        }
    }
}

fn partial_repair() -> (tempfile::TempDir, PersistentStore, Header, Vec<OutboxEntry>) {
    use crate::persistent_store::device_store::{plugin_values::PluginDeviceMutation, Section};
    let (dir, mut store) = store();
    let future = stamp(device_store::now_ms().unwrap() as u64 + 1_000_000);
    store.device_store().unwrap().connection().execute(
        "UPDATE lww_clock SET issued=?1,accepted=?2 WHERE singleton=1",
        params![serde_json::to_string(&future).unwrap(), serde_json::to_string(&stamp(1)).unwrap()],
    ).unwrap();
    save(&mut store, vec![mutation(&["root", "language"], serde_json::json!("repair-preserved"))]);
    store.device_store_mut().unwrap().set_section_participating(Section::LocalPlugins, true).unwrap();
    store.device_store_mut().unwrap().write_plugin_device_values("orphan", &[PluginDeviceMutation::Set {
        space: "string".into(), key: "synthetic-repair".into(), value: "retained-device-value".into(),
    }]).unwrap();
    let entries = store.lww_read_outbox(0.into(), 100).unwrap().entries;
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|entry| entry.stamp.physical_ms >= future.physical_ms));
    let header = Header { binding_authority: 0.into(), request_id: "quarantined-repair".into() };
    store.lww_record_unpublished_proof(&header, "synthetic-proof", &entries.iter().map(|entry| AckEntry {
        key: entry.key.clone(), version: entry.version.clone(), stamp: entry.stamp.clone(), value_identity: entry.value.identity().unwrap(),
    }).collect::<Vec<_>>()).unwrap();
    store.device_store().unwrap().connection().execute_batch(
        "CREATE TRIGGER stop_repair_device BEFORE INSERT ON lww_units
         BEGIN SELECT RAISE(FAIL, 'synthetic-repair-device-stop'); END;",
    ).unwrap();
    let corrected = DecimalU64(device_store::now_ms().unwrap() as u64);
    assert!(store.lww_retry_unpublished(&header, "synthetic-proof", corrected).is_err());
    store.device_store().unwrap().connection().execute_batch("DROP TRIGGER stop_repair_device").unwrap();
    let selected = quarantine(&mut store, &header.request_id);
    assert!(!selected.discardable && selected.recoverable);
    let library_version: String = store.connection.query_row("SELECT version FROM lww_outbox WHERE key=?1", [unit_key(&["root", "language"]).unwrap().as_str()], |row| row.get(0)).unwrap();
    assert_eq!(library_version, header.request_id);
    let device_version: String = store.device_store().unwrap().connection().query_row("SELECT version FROM lww_outbox WHERE key=?1", [unit_key(&["plugin-local", "orphan", "string", "synthetic-repair"]).unwrap().as_str()], |row| row.get(0)).unwrap();
    assert_ne!(device_version, header.request_id);
    (dir, store, header, entries)
}

#[test]
fn quarantined_partial_repair_finishes_once_after_reopen_and_preserves_later_outbox_values() {
    let (dir, mut store, header, original) = partial_repair();
    save(&mut store, vec![mutation(&["root", "askRemoval"], serde_json::json!(true))]);
    let later = store.lww_read_outbox(0.into(), 100).unwrap().entries.into_iter()
        .find(|entry| entry.key == unit_key(&["root", "askRemoval"]).unwrap()).unwrap();
    let revision = store.revision().unwrap();
    let selected = diagnosed(&store, &header.request_id);
    assert!(selected.recoverable);
    let accepted = store.lww_clock_state().unwrap().accepted;
    drop(store);
    let mut store = PersistentStore::open(dir.path()).unwrap();
    assert!(store.lww_complete_quarantined_intent(&selected.request_id, &selected.token, revision).unwrap());
    let repaired = store.lww_read_outbox(0.into(), 100).unwrap().entries;
    for prior in original {
        let actual = repaired.iter().find(|entry| entry.key == prior.key).unwrap();
        assert_eq!(actual.value, prior.value);
        assert_eq!(actual.version, header.request_id);
        assert!(actual.stamp.physical_ms < prior.stamp.physical_ms);
    }
    assert!(repaired.contains(&later));
    assert_eq!(store.lww_clock_state().unwrap().accepted, accepted);
    assert!(store.lww_quarantined_intents().unwrap().is_empty());
    assert!(!store.lww_complete_quarantined_intent(&selected.request_id, &selected.token, revision).unwrap());
    assert_eq!(store.lww_read_outbox(0.into(), 100).unwrap().entries, repaired);
    assert_eq!(store.revision().unwrap(), revision);
}

#[test]
fn partial_repair_rejects_changed_unit_or_outbox_witness_without_clearing_quarantine() {
    for (device, table, column) in [
        (false, "lww_outbox", "value"),
        (false, "lww_units", "value"),
        (true, "lww_outbox", "value"),
        (true, "lww_outbox", "version"),
        (true, "lww_outbox", "authority"),
        (true, "lww_units", "value"),
    ] {
        let (_dir, mut store, header, _) = partial_repair();
        let key = if device { unit_key(&["plugin-local", "orphan", "string", "synthetic-repair"]).unwrap() } else { unit_key(&["root", "language"]).unwrap() };
        let db = if device { store.device_store().unwrap().connection() } else { &store.connection };
        let changed = match column {
            "value" => serde_json::to_string(&inline(&serde_json::json!("changed-after-diagnosis")).unwrap()).unwrap(),
            "authority" => "99".into(),
            _ => "unrelated-version".into(),
        };
        db.execute(&format!("UPDATE {table} SET {column}=?1 WHERE key=?2"), params![changed, key.as_str()]).unwrap();
        let selected = diagnosed(&store, &header.request_id);
        assert!(!selected.recoverable, "device={device}, {table}.{column}");
        let evidence = failure_evidence(&store, &header.request_id);
        let revision = store.revision().unwrap();
        assert!(store.lww_complete_quarantined_intent(&selected.request_id, &selected.token, revision).is_err());
        assert_eq!(failure_evidence(&store, &header.request_id), evidence);
    }
}
