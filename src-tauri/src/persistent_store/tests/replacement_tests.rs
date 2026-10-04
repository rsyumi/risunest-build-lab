use super::*;

#[test]
fn activation_leaves_the_previous_library_rows_in_place_for_the_purge() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let seed = stage_root(&mut store, "Original");
    store.replace_commit(&seed, Some(0)).unwrap();
    let stage = stage_root(&mut store, "Replacement");
    for (generation, value) in [(seed.as_str(), "1"), (stage.as_str(), "2")] {
        let transaction = store.connection.transaction().unwrap();
        for index in 0..513 {
            transaction.execute(
                "INSERT INTO plugin_storage(generation,owner,storage_key,byte_size,ordinal,value) VALUES(?1,'synthetic-plugin',?2,1,?3,?4)",
                params![generation, format!("synthetic-{index:04}"), index, value],
            ).unwrap();
        }
        transaction.commit().unwrap();
    }
    store
        .connection
        .execute_batch(&format!(
            "CREATE TRIGGER reject_previous_update BEFORE UPDATE ON plugin_storage WHEN OLD.generation='{seed}'
             BEGIN SELECT RAISE(ABORT,'synthetic previous library update'); END;
             CREATE TRIGGER reject_previous_delete BEFORE DELETE ON plugin_storage WHEN OLD.generation='{seed}'
             BEGIN SELECT RAISE(ABORT,'synthetic previous library delete'); END;"
        ))
        .unwrap();
    store.replace_commit(&stage, Some(1)).unwrap();
    assert_eq!(store.revision().unwrap(), 2);
    for (generation, value) in [(seed.as_str(), "1"), (stage.as_str(), "2")] {
        let count: i64 = store
            .connection
            .query_row(
                "SELECT count(*) FROM plugin_storage WHERE generation=?1 AND value=?2",
                params![generation, value],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 513);
    }
    store
        .connection
        .execute_batch("DROP TRIGGER reject_previous_update; DROP TRIGGER reject_previous_delete;")
        .unwrap();
    while store.purge_retired_batch(100).unwrap() {}
    assert_eq!(
        store
            .connection
            .query_row::<i64, _, _>(
                "SELECT count(*) FROM plugin_storage WHERE generation=?1",
                [&seed],
                |row| row.get(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .connection
            .query_row::<i64, _, _>(
                "SELECT count(*) FROM plugin_storage WHERE generation=?1 AND value='2'",
                [&stage],
                |row| row.get(0),
            )
            .unwrap(),
        513
    );
}

#[test]
fn ordinary_replacement_retry_returns_its_receipt_without_replacing_later_edits() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let seed = stage_root(&mut store, "Original");
    store.replace_commit(&seed, Some(0)).unwrap();
    let stage = stage_root(&mut store, "Replacement");
    assert_eq!(store.replace_commit(&stage, Some(1)).unwrap().revision, 2);
    store.commit(&WorkingSetCommit {
        root: Some(json!({"username":"Later edit"})),
        ..empty_working_set_commit(2)
    }).unwrap();
    let before = serde_json::to_value(store.lww_clock_state().unwrap()).unwrap();
    let pending = serde_json::to_value(store.lww_read_outbox(store.lww_binding_authority().unwrap(), 100).unwrap()).unwrap();
    assert_eq!(store.replace_commit(&stage, Some(1)).unwrap().revision, 2);
    assert_eq!(store.revision().unwrap(), 3);
    assert_eq!(store.read_root(None).unwrap().value["username"], "Later edit");
    assert_eq!(serde_json::to_value(store.lww_clock_state().unwrap()).unwrap(), before);
    assert_eq!(serde_json::to_value(store.lww_read_outbox(store.lww_binding_authority().unwrap(), 100).unwrap()).unwrap(), pending);
    assert!(store.replace_commit(&stage, Some(2)).is_err());
    assert!(store.replace_put_root(&stage, &json!({"username":"Reused stage"})).is_err());
    assert_eq!(store.replace_commit(&stage, Some(1)).unwrap().revision, 2);
    store.device_store_mut().unwrap().connection().execute(
        "UPDATE lww_clock SET binding_authority='1' WHERE singleton=1", [],
    ).unwrap();
    assert!(store.replace_commit(&stage, Some(1)).is_err());
    assert_eq!(store.read_root(None).unwrap().value["username"], "Later edit");
}

#[test]
fn unfinished_ordinary_replacement_rejects_changed_stage_before_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let seed = stage_root(&mut store, "Original");
    store.replace_commit(&seed, Some(0)).unwrap();
    let stage = stage_root(&mut store, "Replacement");
    store.connection.execute_batch(
        "CREATE TRIGGER reject_replacement BEFORE UPDATE ON generations
         BEGIN SELECT RAISE(ABORT,'synthetic replacement failure'); END;",
    ).unwrap();
    assert!(store.replace_commit(&stage, Some(1)).is_err());
    let before = serde_json::to_value(store.lww_clock_state().unwrap()).unwrap();
    store.replace_put_root(&stage, &json!({"username":"Changed staged input"})).unwrap();
    store.connection.execute_batch("DROP TRIGGER reject_replacement").unwrap();
    assert!(store.replace_commit(&stage, Some(1)).is_err());
    assert_eq!(store.revision().unwrap(), 1);
    assert_eq!(store.read_root(None).unwrap().value["username"], "Original");
    assert_eq!(serde_json::to_value(store.lww_clock_state().unwrap()).unwrap(), before);
    drop(store);
    assert!(PersistentStore::open(directory.path()).is_err());
}

#[test]
fn ordinary_replacement_receipt_rejects_another_source_kind_even_with_matching_digests() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let stage = stage_root(&mut store, "Original");
    store.replace_commit(&stage, Some(0)).unwrap();
    let body: String = store.device_store().unwrap().connection().query_row(
        "SELECT body FROM lww_intents WHERE request_id=?1", [&stage], |row| row.get(0),
    ).unwrap();
    let mut body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["source_units"], Value::Null);
    body["source_units"] = json!({ "rows": 0, "digest": "0".repeat(64) });
    let body = serde_json::to_string(&body).unwrap();
    let digest = risunest_sync_wire::hash(body.as_bytes());
    store.device_store().unwrap().connection().execute(
        "UPDATE lww_intents SET body=?2,digest=?3 WHERE request_id=?1",
        params![stage, body, digest],
    ).unwrap();
    store.connection.execute(
        "UPDATE lww_requests SET digest=?2 WHERE request_id=?1", params![stage, digest],
    ).unwrap();
    assert!(store.replace_commit(&stage, Some(0)).is_err());
    assert_eq!(store.revision().unwrap(), 1);
    assert_eq!(store.read_root(None).unwrap().value["username"], "Original");
}

#[test]
fn replacements_do_not_create_or_require_recovery_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let seed = stage_root(&mut store, "Seed");
    store.replace_commit(&seed, Some(0)).unwrap();
    let replacement = stage_root(&mut store, "Replacement");
    store.replace_commit(&replacement, Some(1)).unwrap();
    assert!(store.snapshot_list().unwrap().is_empty());
    store.snapshots_dir = directory.path().join("blocked-snapshots");
    fs::write(&store.snapshots_dir, b"unavailable snapshot directory").unwrap();
    let replacement = stage_root(&mut store, "Final");
    store.replace_commit(&replacement, Some(2)).unwrap();
    assert_eq!(store.revision().unwrap(), 3);
    assert_eq!(store.read_root(None).unwrap().value["username"], "Final");
}

#[test]
fn prepared_replacement_rechecks_revision_before_activation() {
    let directory = tempfile::tempdir().expect("create temporary directory");
    let mut store = PersistentStore::open(directory.path()).expect("open persistent store");
    let seed = stage_root(&mut store, "Seed");
    store
        .replace_commit(&seed, Some(0))
        .expect("activate revision one");
    let replacement = stage_root(&mut store, "Prepared replacement");

    let prepared = store
        .prepare_replace_commit(&replacement, Some(1))
        .expect("prepare replacement");
    let authorized = prepared;

    let competing = stage_root(&mut store, "Competing revision");
    store
        .replace_commit(&competing, Some(1))
        .expect("activate competing revision");
    let error = store
        .finish_prepared_replace(authorized)
        .expect_err("prepared replacement must recheck revision");

    assert!(matches!(
        error,
        StoreError::RevisionConflict {
            expected: 1,
            actual: 2
        }
    ));
    assert_eq!(store.revision().expect("read current revision"), 2);
    assert_eq!(
        store.read_root(None).expect("read current root").value["username"],
        "Competing revision"
    );
    store
        .replace_abort(&replacement)
        .expect("prepared staging remains abortable");
}

#[test]
fn checkpoint_truncate_reports_busy_and_truncates_when_unblocked() {
    let directory = tempfile::tempdir().expect("create temporary directory");
    let store = PersistentStore::open(directory.path()).expect("open persistent store");
    store
        .connection
        .execute_batch("PRAGMA wal_autocheckpoint = 0; INSERT INTO app_kv VALUES ('first', '1');")
        .expect("create initial WAL frames");
    let database_path = directory.path().join("persistent/persistent.sqlite");
    let wal_path = PathBuf::from(format!("{}-wal", database_path.display()));
    store
        .checkpoint(CheckpointMode::Passive)
        .expect("passive checkpoint");
    assert!(fs::metadata(&wal_path).expect("read passive WAL").len() > 0);

    let reader = rusqlite::Connection::open(&database_path).expect("open blocking reader");
    reader
        .execute_batch("BEGIN; SELECT value FROM app_kv WHERE key = 'first';")
        .expect("hold read snapshot");
    store
        .connection
        .execute("INSERT INTO app_kv VALUES ('second', '2')", [])
        .expect("write newer WAL frame");
    store
        .connection
        .busy_timeout(Duration::ZERO)
        .expect("disable checkpoint wait");
    assert!(store.checkpoint(CheckpointMode::Truncate).is_err());
    reader
        .execute_batch("ROLLBACK")
        .expect("release read snapshot");

    store
        .checkpoint(CheckpointMode::Truncate)
        .expect("truncate checkpoint");
    assert_eq!(
        fs::metadata(&wal_path).expect("read truncated WAL").len(),
        0
    );
}

#[test]
fn store_errors_serialize_to_the_command_contract() {
    assert_eq!(
        serde_json::to_value(StoreError::RevisionConflict {
            expected: 12,
            actual: 13,
        })
        .expect("serialize revision conflict"),
        json!({ "code": "revision-conflict", "expected": 12, "actual": 13 })
    );
    assert_eq!(
        serde_json::to_value(StoreError::SnapshotReleased).expect("serialize released snapshot"),
        json!({ "code": "snapshot-released" })
    );
    assert_eq!(
        serde_json::to_value(ConversationPage {
            revision: 4,
            items: vec![],
            next_cursor: None,
        })
        .expect("serialize empty page"),
        json!({ "revision": 4, "items": [] })
    );
}
