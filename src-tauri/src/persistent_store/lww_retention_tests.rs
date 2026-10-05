use super::*;

const EXTERNAL_WRITER: &str = "00000000-0000-4000-8000-0000000000aa";

fn device_count(store: &PersistentStore, sql: &str) -> i64 {
    store.device_store().unwrap().connection().query_row(sql, [], |row| row.get(0)).unwrap()
}
fn library_count(store: &PersistentStore, sql: &str) -> i64 {
    store.connection.query_row(sql, [], |row| row.get(0)).unwrap()
}
fn page(store: &PersistentStore, id: &str, kind: &str, writer: Option<&str>, cursor: u64, changes: Vec<Change>) -> StageReceive {
    StageReceive {
        header: Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: id.into() },
        changes,
        progress: Progress { kind: kind.into(), cursor: cursor.into(), writer_id: writer.map(Into::into) },
        admitted_time_upper_ms: u64::MAX.into(),
    }
}
fn deliver(store: &mut PersistentStore, page: &StageReceive, generating: Vec<MessageLocator>) -> ApplyResult {
    store.lww_stage_receive(page).unwrap();
    let applied = store.lww_apply_receive(&ApplyReceive { header: page.header.clone(), generating }).unwrap();
    store.lww_finish_receive(&page.header).unwrap();
    applied
}
fn finished_receives(store: &PersistentStore) -> Vec<String> {
    let device = store.device_store().unwrap().connection();
    let mut statement = device.prepare("SELECT request_id FROM lww_receive WHERE finished=1 ORDER BY request_id").unwrap();
    let ids = statement.query_map([], |row| row.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
    ids
}
fn receive_rows(store: &PersistentStore) -> (i64, i64) {
    let sql = "SELECT count(*) FROM lww_receive_rows";
    (library_count(store, sql), device_count(store, sql))
}
fn next_time(store: &PersistentStore) -> u64 {
    store.lww_clock_state().unwrap().issued.unwrap().physical_ms.0 + 1
}

#[test]
fn completed_writes_leave_no_intent_and_their_receipt_still_answers_a_retry() {
    let (_dir, mut store) = store();
    for index in 0..20 {
        save(&mut store, vec![mutation(&["root", "username"], serde_json::json!(format!("user-{index}")))]);
    }
    assert_eq!(device_count(&store, "SELECT count(*) FROM lww_intents"), 0);
    assert_eq!(device_count(&store, "SELECT count(*) FROM lww_intent_rows"), 0);
    let retried = WorkingSetCommit {
        expected_revision: store.revision().unwrap(),
        request_id: Some("retried-write".into()),
        unit_mutations: Some(vec![mutation(&["root", "language"], serde_json::json!("ko"))]),
        ..Default::default()
    };
    let first = store.commit(&retried).unwrap();
    assert_eq!(device_count(&store, "SELECT count(*) FROM lww_intents"), 0);
    assert_eq!(store.commit(&retried).unwrap().revision, first.revision);
    assert_eq!(store.revision().unwrap(), first.revision);
    let changed = WorkingSetCommit {
        unit_mutations: Some(vec![mutation(&["root", "language"], serde_json::json!("ja"))]),
        ..retried.clone()
    };
    assert!(matches!(store.commit(&changed), Err(StoreError::Validation { message }) if message == "request-id-integrity"));
    assert_eq!(store.read_root(None).unwrap().value["language"], "ko");
    assert_eq!(device_count(&store, "SELECT count(*) FROM lww_intents"), 0);
}

#[test]
fn polling_keeps_only_the_newest_finished_page_of_each_stream() {
    let (_dir, mut store) = store();
    let mut last = None;
    for index in 1..=20u64 {
        let server = page(&store, &format!("server-{index:02}"), "server", None, index,
            vec![change(&["root", "username"], index, serde_json::json!(format!("server-{index}")))]);
        deliver(&mut store, &server, vec![]);
        if index % 5 == 0 {
            let external = page(&store, &format!("external-{index:02}"), "external", Some(EXTERNAL_WRITER), index,
                vec![change(&["root", "language"], index, serde_json::json!(format!("external-{index}")))]);
            deliver(&mut store, &external, vec![]);
        }
        last = Some(server);
    }
    assert_eq!(finished_receives(&store), ["external-20", "server-20"]);
    assert_eq!(receive_rows(&store), (0, 0));
    let last = last.unwrap();
    store.server_assert_receive_finished(&last).unwrap();
    let revision = store.revision().unwrap();
    deliver(&mut store, &last, vec![]);
    assert_eq!(store.revision().unwrap(), revision);
    assert_eq!(finished_receives(&store), ["external-20", "server-20"]);
    assert_eq!(receive_rows(&store), (0, 0));
    store.server_assert_receive_finished(&last).unwrap();
}

#[test]
fn pages_that_repeat_the_stream_cursor_stay_until_a_later_page_moves_it() {
    let (_dir, mut store) = store();
    let part = |store: &PersistentStore, index: u64, cursor: u64| page(store, &format!("group-{index}"), "external", Some(EXTERNAL_WRITER), cursor,
        vec![change(&["root", "language"], index + 1, serde_json::json!(format!("part-{index}")))]);
    let first = part(&store, 0, 4);
    deliver(&mut store, &first, vec![]);
    for index in 1..3u64 {
        let next = part(&store, index, 4);
        deliver(&mut store, &next, vec![]);
    }
    assert_eq!(finished_receives(&store), ["group-0", "group-1", "group-2"]);
    let revision = store.revision().unwrap();
    deliver(&mut store, &first, vec![]);
    assert_eq!(store.revision().unwrap(), revision);
    let last = part(&store, 3, 5);
    deliver(&mut store, &last, vec![]);
    assert_eq!(finished_receives(&store), ["group-3"]);
    assert_eq!(store.read_root(None).unwrap().value["language"], "part-3");
}

#[test]
fn held_and_deferred_rows_keep_their_page_until_the_drain_releases_them() {
    let (_dir, mut store) = store();
    create_conversation(&mut store, "char", "chat");
    let time = next_time(&store);
    let message = Change {
        key: unit_key(&["messages", "char", "chat"]).unwrap(),
        stamp: stamp(time),
        value: incoming_message_value(&store, "char", "chat", "deferred-message"),
    };
    let held = change(&["character", "waiting", "name"], time, serde_json::json!("held-name"));
    let generating = vec![MessageLocator { character_id: "char".into(), conversation_id: "chat".into(), start: None }];
    let deferred_page = page(&store, "deferred-page", "server", None, 1, vec![message.clone()]);
    assert_eq!(deliver(&mut store, &deferred_page, generating).deferred_keys, vec![message.key.clone()]);
    let held_page = page(&store, "held-page", "server", None, 2, vec![held.clone()]);
    assert_eq!(deliver(&mut store, &held_page, vec![]).held_keys, vec![held.key.clone()]);
    for index in 3..=5u64 {
        let later = page(&store, &format!("later-{index}"), "server", None, index, vec![]);
        deliver(&mut store, &later, vec![]);
    }
    assert_eq!(finished_receives(&store), ["deferred-page", "held-page", "later-5"]);
    let lease = store.lww_acquire_library_backup_capture(store.revision().unwrap()).unwrap();
    let units = store.lww_backup_unit_values(&lease.lease).unwrap();
    assert_eq!(units[&held.key], held.value);
    assert_eq!(units[&message.key], message.value);
    store.release_revision(&lease.lease).unwrap();

    let drain = Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: "drain".into() };
    store.lww_drain_deferred(&ApplyReceive { header: drain, generating: vec![] }).unwrap();
    assert_eq!(read_unit(&store.connection, &message.key).unwrap().unwrap().1, message.value);
    assert_eq!(finished_receives(&store), ["held-page", "later-5"]);

    let parent = page(&store, "parent", "server", None, 6,
        vec![change(&["exists", "character", "waiting"], time, serde_json::json!({"type":"character"}))]);
    assert!(deliver(&mut store, &parent, vec![]).affected_keys.contains(&held.key));
    assert_eq!(finished_receives(&store), ["parent"]);
    assert_eq!(receive_rows(&store), (0, 0));
}

#[test]
fn the_deferred_drain_reads_its_rows_through_the_status_index() {
    let (_dir, store) = store();
    for db in [&store.connection, store.device_store().unwrap().connection()] {
        let mut statement = db.prepare(&format!("EXPLAIN QUERY PLAN {DRAIN_ROWS_SQL}")).unwrap();
        let plan: Vec<String> = statement.query_map([], |row| row.get(3)).unwrap().collect::<Result<_, _>>().unwrap();
        assert!(plan.iter().any(|step| step.contains("USING INDEX lww_receive_status (status=?)")), "{plan:?}");
    }
}

#[test]
fn a_replay_that_keeps_failing_with_a_store_error_is_set_aside_and_the_store_opens() {
    let (dir, mut store) = store();
    let header = Header { binding_authority: 0.into(), request_id: "synthetic-stuck".into() };
    let intent = Intent::Commit {
        commit: WorkingSetCommit {
            expected_revision: 0,
            unit_mutations: Some(vec![mutation(&["root", "language"], serde_json::json!("ko"))]),
            ..Default::default()
        },
        aliases: vec![],
    };
    store.reserve_intent(&header, &intent).unwrap();
    store.connection.execute_batch(
        "CREATE TRIGGER synthetic_stuck_receipt BEFORE INSERT ON lww_requests WHEN NEW.request_id='synthetic-stuck'
         BEGIN SELECT RAISE(ABORT,'synthetic store failure'); END;",
    ).unwrap();
    drop(store);
    let failures = |store: &PersistentStore| -> Option<(i64, bool, String)> {
        store.device_store().unwrap().connection().query_row(
            "SELECT failures,quarantined,error FROM lww_intent_failures WHERE request_id='synthetic-stuck'", [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional().unwrap()
    };
    let edit = |store: &mut PersistentStore, name: &str| store.commit(&WorkingSetCommit {
        expected_revision: store.revision().unwrap(),
        unit_mutations: Some(vec![mutation(&["root", "username"], serde_json::json!(name))]),
        ..Default::default()
    });

    let mut store = PersistentStore::open(dir.path()).expect("the store opens while the intent keeps failing");
    assert_eq!(failures(&store), Some((1, false, "synthetic store failure".into())));
    assert_eq!(store.revision().unwrap(), 0);
    assert!(edit(&mut store, "blocked").is_err());
    assert_eq!(failures(&store), Some((2, false, "synthetic store failure".into())));
    let saved = edit(&mut store, "saved").unwrap();
    assert_eq!(failures(&store), Some((3, true, "synthetic store failure".into())));
    assert_eq!(store.revision().unwrap(), saved.revision);
    let root = store.read_root(None).unwrap().value;
    assert_eq!(root["username"], "saved");
    assert_ne!(root["language"], "ko");
    assert_eq!(store.lww_quarantined_intents().unwrap(), vec![QuarantinedIntent {
        request_id: "synthetic-stuck".into(),
        kind: "commit".into(),
        error: "synthetic store failure".into(),
    }]);
    let (lease, _) = store.lww_acquire_backup_capture(store.revision().unwrap()).unwrap();
    store.release_revision(&lease.lease).unwrap();
    let lease = store.acquire_revision(store.revision().unwrap()).unwrap();
    let findings = store.data_health_reader(&lease.lease).unwrap().scan(100, &crate::local_backup::NeverCancelled).unwrap();
    store.release_revision(&lease.lease).unwrap();
    let finding = findings.items.iter().find(|finding| finding.code == "intent-quarantined").expect("the set-aside change is reported");
    assert_eq!(finding.severity, crate::data_health::Severity::Degraded);
    assert_eq!((finding.owner.kind.as_str(), finding.owner.id.as_str()), ("intent", "synthetic-stuck"));
    assert_eq!(finding.detail, "synthetic store failure");
    drop(store);

    let store = PersistentStore::open(dir.path()).unwrap();
    assert_eq!(failures(&store), Some((3, true, "synthetic store failure".into())));
    assert_eq!(store.read_root(None).unwrap().value["username"], "saved");
}

#[test]
fn an_edit_under_a_deleted_record_is_refused_and_a_delete_there_changes_nothing() {
    let (_dir, mut store) = store();
    create_conversation(&mut store, "char", "chat");
    let name = unit_key(&["character", "char", "name"]).unwrap();
    save(&mut store, vec![mutation(&["character", "char", "name"], serde_json::json!("before"))]);
    let time = next_time(&store);
    receive(&mut store, "remote-delete", vec![Change {
        key: unit_key(&["exists", "character", "char"]).unwrap(),
        stamp: stamp(time),
        value: UnitValue::Deleted,
    }], vec![]);
    assert_eq!(parent_status(&store.connection, &name).unwrap(), "retired");
    let revision = store.revision().unwrap();
    let refused = store.commit(&WorkingSetCommit {
        expected_revision: revision,
        unit_mutations: Some(vec![
            mutation(&["root", "username"], serde_json::json!("not-saved")),
            mutation(&["character", "char", "name"], serde_json::json!("after")),
        ]),
        ..Default::default()
    });
    assert!(matches!(refused, Err(StoreError::Validation { ref message }) if message == "retired-record-id"), "{refused:?}");
    assert_eq!(store.revision().unwrap(), revision);
    assert_ne!(store.read_root(None).unwrap().value["username"], "not-saved");
    save(&mut store, vec![UnitMutation::Delete { key: name.clone() }]);
    assert!(store.lww_read_outbox(0.into(), 100).unwrap().entries.iter().all(|entry| entry.key != name));
    assert_eq!(parent_status(&store.connection, &name).unwrap(), "retired");
}

#[test]
fn short_device_connections_leave_the_wal_files_of_the_open_store() {
    use risunest_external_storage_format::section::SectionKind;
    let (_dir, mut store) = store();
    let path = std::path::PathBuf::from(store.device_store().unwrap().connection().path().unwrap());
    let sidecar = |suffix: &str| {
        let mut name = path.clone().into_os_string();
        name.push(suffix);
        std::path::PathBuf::from(name)
    };
    let (wal, shm) = (sidecar("-wal"), sidecar("-shm"));
    let kept = |step: &str| assert!(wal.exists() && shm.exists(), "{step} removed the WAL files of the open device store");
    save(&mut store, vec![mutation(&["root", "username"], serde_json::json!("written"))]);
    kept("a commit");
    let fence = store.device_store().unwrap().visit_plugin_gc_values(|_, _, _, _| Ok(()), |_, _| Ok(())).unwrap();
    kept("the plugin GC snapshot");
    assert!(store.device_store().unwrap().plugin_gc_fence_is_current(&fence).unwrap());
    kept("the plugin GC fence check");
    drop(store.device_store().unwrap().acquire_plugin_gc_barrier(&fence).unwrap());
    kept("the plugin GC barrier");
    store.device_store_mut().unwrap().capture_backup_sections(&[SectionKind::LocalSettings]).unwrap();
    kept("the section capture");
    let (lease, _) = store.lww_acquire_backup_capture(store.revision().unwrap()).unwrap();
    store.release_revision(&lease.lease).unwrap();
    kept("the backup capture");
    drop(store);
    assert!(!wal.exists(), "closing the last connection removes the WAL file");
}
