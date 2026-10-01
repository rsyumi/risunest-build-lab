use super::*;
use crate::data_health::repair::RepairCandidate;
use crate::persistent_store::active_generation;
use serde_json::json;

fn candidate(action: RepairAction) -> RepairCandidate {
    RepairCandidate {
        id: "0:test".to_owned(),
        action,
        finding: 0,
        preferred: true,
        discards: false,
    }
}

/// A library whose root points at an asset it does not register and at one it does, so a repair
/// has something real to remove and something it must leave alone.
fn fixture() -> (tempfile::TempDir, PersistentStore) {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let staging = store.replace_begin().unwrap().staging_id;
    store
        .replace_put_root(
            &staging,
            &json!({
                "userIcon": "assets/missing.png",
                "customBackground": "assets/kept.png",
                "enabledModules": ["module-a", "module-b"],
            }),
        )
        .unwrap();
    store.replace_commit(&staging, Some(0)).unwrap();
    (directory, store)
}

fn root_of(store: &PersistentStore) -> Value {
    store.read_root(None).unwrap().value
}

fn bind_server_capture(store: &PersistentStore) {
    store.connection.execute_batch(
        "INSERT INTO server_sync_state(singleton,config,full_scan) VALUES(1,'{}',0);
         UPDATE library_sync_selection SET target='server',connection_id='synthetic',decision_required=0;"
    ).unwrap();
}

#[test]
fn repair_and_undo_keep_sync_identity_and_emit_only_changed_records() {
    check_repair_and_undo_sync_identity(false);
}

#[test]
fn externally_selected_repair_and_undo_keep_identity_and_precise_capture() {
    check_repair_and_undo_sync_identity(true);
}

fn check_repair_and_undo_sync_identity(external: bool) {
    let (_directory, mut store) = fixture();
    bind_server_capture(&store);
    if external {
        let epoch = super::super::sync_selection::read(&store.connection).unwrap().epoch;
        store.external_select(&epoch, &super::super::sync_selection::SyncTarget::External("synthetic-external".into())).unwrap();
    }
    let selected = super::super::sync_selection::read(&store.connection).unwrap();
    let before = super::super::sync_selection::identity(&store.connection).unwrap();
    store.connection.execute_batch("DELETE FROM content_changes; DELETE FROM server_sync_dirty;").unwrap();
    let (_, journal) = store.apply_repair(1, &[candidate(RepairAction::DropReference {
        owner: crate::data_health::Owner { kind: "root".into(), id: "database".into() },
        source_path: "$.userIcon".into(), occurrence: 0,
    })], 10).unwrap();
    for revision in [2, 3] {
        if revision == 3 { store.undo_repair(&journal, 2).unwrap(); }
        let after = super::super::sync_selection::identity(&store.connection).unwrap();
        assert_eq!(after.library_epoch, before.library_epoch);
        assert_eq!(after.selection_epoch, before.selection_epoch);
        assert_eq!(after.generation, before.generation);
        let after_selection = super::super::sync_selection::read(&store.connection).unwrap();
        assert!(!after_selection.decision_required);
        assert_eq!(after_selection.target, selected.target);
        for table in ["content_changes", "server_sync_dirty"] {
            let entries: Vec<(String, String, String, i64)> = store.connection.prepare(&format!("SELECT kind,key1,key2,revision FROM {table}")).unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap().collect::<Result<_,_>>().unwrap();
            assert_eq!(entries, vec![("root".into(), String::new(), String::new(), revision)]);
        }
    }
}

#[test]
fn a_reference_removal_rewrites_only_the_field_it_names() {
    let (_directory, mut store) = fixture();
    let (committed, journal) = store
        .apply_repair(
            1,
            &[candidate(RepairAction::DropReference {
                owner: crate::data_health::Owner {
                    kind: "root".to_owned(),
                    id: "database".to_owned(),
                },
                source_path: "$.userIcon".to_owned(),
                occurrence: 0,
            })],
            10,
        )
        .unwrap();
    assert_eq!(committed.revision, 2);
    let root = root_of(&store);
    assert!(root.get("userIcon").is_none());
    assert_eq!(
        root.get("customBackground").and_then(Value::as_str),
        Some("assets/kept.png"),
        "a repair changes only what was selected"
    );
    assert_eq!(journal.from_revision, 1);
    assert_eq!(journal.to_revision, 2);
    assert_eq!(journal.records.len(), 1);
    assert_eq!(journal.records[0].table, "root");
}

#[test]
fn root_normalization_removes_only_separately_stored_fields() {
    let (_directory, mut store) = fixture();
    let generation = active_generation(&store.connection).unwrap();
    store
        .connection
        .execute(
            "UPDATE root SET value=json_set(value, '$.pluginStorageMeta', json('{\"owned\":true}')) WHERE generation=?1",
            [&generation],
        )
        .unwrap();
    store
        .apply_repair(
            1,
            &[candidate(RepairAction::NormalizeRecords {
                table: "root".to_owned(),
            })],
            10,
        )
        .unwrap();
    let root = root_of(&store);
    assert!(root.get("pluginStorageMeta").is_none());
    assert_eq!(
        root.get("customBackground").and_then(Value::as_str),
        Some("assets/kept.png")
    );
}

#[test]
fn several_removals_in_one_record_keep_naming_the_same_elements() {
    let (_directory, mut store) = fixture();
    let owner = crate::data_health::Owner {
        kind: "root".to_owned(),
        id: "database".to_owned(),
    };
    store
        .apply_repair(
            1,
            &[
                candidate(RepairAction::DropReference {
                    owner: owner.clone(),
                    source_path: "$.enabledModules[0]".to_owned(),
                    occurrence: 0,
                }),
                RepairCandidate {
                    id: "1:test".to_owned(),
                    ..candidate(RepairAction::DropReference {
                        owner,
                        source_path: "$.enabledModules[1]".to_owned(),
                        occurrence: 1,
                    })
                },
            ],
            10,
        )
        .unwrap();
    assert_eq!(
        root_of(&store).get("enabledModules"),
        Some(&json!([])),
        "removing the later element first keeps the earlier index meaning what it meant"
    );
}

#[test]
fn an_undo_restores_the_previous_image_and_raises_the_revision() {
    let (_directory, mut store) = fixture();
    let before = root_of(&store);
    let (_, journal) = store
        .apply_repair(
            1,
            &[candidate(RepairAction::DropReference {
                owner: crate::data_health::Owner {
                    kind: "root".to_owned(),
                    id: "database".to_owned(),
                },
                source_path: "$.userIcon".to_owned(),
                occurrence: 0,
            })],
            10,
        )
        .unwrap();
    let (undone, skipped) = store.undo_repair(&journal, 2).unwrap();
    assert_eq!(undone.revision, 3);
    assert!(skipped.is_empty());
    assert_eq!(root_of(&store), before);
}

#[test]
fn an_undo_leaves_a_record_the_reader_changed_after_the_repair() {
    let (_directory, mut store) = fixture();
    bind_server_capture(&store);
    let (_, journal) = store
        .apply_repair(
            1,
            &[candidate(RepairAction::DropReference {
                owner: crate::data_health::Owner {
                    kind: "root".to_owned(),
                    id: "database".to_owned(),
                },
                source_path: "$.userIcon".to_owned(),
                occurrence: 0,
            })],
            10,
        )
        .unwrap();
    store
        .commit(&crate::persistent_store::WorkingSetCommit {
            expected_revision: 2,
            root: Some(json!({ "customBackground": "assets/kept.png", "note": "edited" })),
            root_mutations: None,
            replace_presets: None,
            character: None,
            character_details: None,
            replace_character: None,
            add_character: None,
            conversations: None,
            delete_character_ids: None,
            plugin_storage: None,
            asset_owner_heads: None,
        })
        .unwrap();

    let (_, skipped) = store.undo_repair(&journal, 3).unwrap();
    assert_eq!(skipped, ["root:"], "the later edit is reported, not discarded");
    for table in ["content_changes", "server_sync_dirty"] {
        let revision: i64 = store.connection.query_row(
            &format!("SELECT revision FROM {table} WHERE kind='root' AND key1='' AND key2=''"),
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(revision, 3, "a skipped record must not enter the undo delta in {table}");
        let undo_entries: i64 = store.connection.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE revision>3"), [], |row| row.get(0),
        ).unwrap();
        assert_eq!(undo_entries, 0);
    }
    assert_eq!(
        root_of(&store).get("note").and_then(Value::as_str),
        Some("edited")
    );
}

#[test]
fn a_repair_onto_another_revision_is_refused_before_anything_is_staged() {
    let (_directory, mut store) = fixture();
    let error = store
        .apply_repair(
            0,
            &[candidate(RepairAction::DropReference {
                owner: crate::data_health::Owner {
                    kind: "root".to_owned(),
                    id: "database".to_owned(),
                },
                source_path: "$.userIcon".to_owned(),
                occurrence: 0,
            })],
            10,
        )
        .unwrap_err();
    assert!(matches!(error, StoreError::RevisionConflict { .. }), "{error:?}");
    assert_eq!(store.revision().unwrap(), 1, "nothing was applied");
    assert_eq!(
        root_of(&store).get("userIcon").and_then(Value::as_str),
        Some("assets/missing.png")
    );
}

#[test]
fn a_repair_that_leaves_the_library_refused_is_not_applied() {
    let (_directory, mut store) = fixture();
    let generation = active_generation(&store.connection).unwrap();
    // An alias whose payload is absent blocks activation, and this repair does not answer it.
    store
        .connection
        .execute(
            "INSERT INTO asset_aliases (generation, logical_key, object_hash, kind, size, mime, name, ext)
             VALUES (?1, 'assets/absent.png', ?2, 'asset', 4, 'image/png', 'absent.png', 'png')",
            rusqlite::params![generation, "3c".repeat(32)],
        )
        .unwrap();

    let error = store
        .apply_repair(
            1,
            &[candidate(RepairAction::DropReference {
                owner: crate::data_health::Owner {
                    kind: "root".to_owned(),
                    id: "database".to_owned(),
                },
                source_path: "$.userIcon".to_owned(),
                occurrence: 0,
            })],
            10,
        )
        .unwrap_err();
    assert!(matches!(error, StoreError::Validation { .. }), "{error:?}");
    assert_eq!(store.revision().unwrap(), 1);
    assert_eq!(
        active_generation(&store.connection).unwrap(),
        generation,
        "the live generation is untouched"
    );
    assert_eq!(
        root_of(&store).get("userIcon").and_then(Value::as_str),
        Some("assets/missing.png"),
        "the change the reader selected is not half applied"
    );
}

#[test]
fn a_removed_alias_releases_its_object_into_the_journal() {
    let (_directory, mut store) = fixture();
    let cas = crate::asset_repository::PayloadCas::new(store.repository_root()).unwrap();
    let stored = cas.prepare_bytes(b"synthetic").unwrap();
    let generation = active_generation(&store.connection).unwrap();
    store
        .connection
        .execute(
            "INSERT INTO asset_aliases (generation, logical_key, object_hash, kind, size, mime, name, ext)
             VALUES (?1, 'assets/kept.png', ?2, 'asset', 9, 'image/png', 'kept.png', 'png')",
            rusqlite::params![generation, stored.content_hash],
        )
        .unwrap();

    let (_, journal) = store
        .apply_repair(
            1,
            &[candidate(RepairAction::DropAlias {
                kind: "asset".to_owned(),
                key: "assets/kept.png".to_owned(),
            })],
            10,
        )
        .unwrap();
    assert!(journal.released_objects.contains(&stored.content_hash));
    assert!(
        crate::data_health::journal::roots(store.repository_root())
            .unwrap()
            .object_hashes
            .is_empty(),
        "the journal only holds what is written to disk, which the command does"
    );
}

#[test]
fn a_path_no_record_uses_changes_nothing() {
    let (_directory, mut store) = fixture();
    let before = root_of(&store);
    let error = store
        .apply_repair(
            1,
            &[candidate(RepairAction::DropReference {
                owner: crate::data_health::Owner {
                    kind: "root".to_owned(),
                    id: "database".to_owned(),
                },
                source_path: "$.neverPresent".to_owned(),
                occurrence: 0,
            })],
            10,
        )
        .map(|(committed, _)| committed);
    // The staged copy is identical, so the activation still succeeds and the library is the same.
    assert!(error.is_ok(), "{error:?}");
    assert_eq!(root_of(&store), before);
}

#[test]
fn parse_path_reads_the_shapes_the_reference_graph_emits() {
    assert_eq!(parse_path("$.userIcon"), Some(vec![Step::Key("userIcon".to_owned())]));
    assert_eq!(
        parse_path("$.enabledModules[2]"),
        Some(vec![Step::Key("enabledModules".to_owned()), Step::Index(2)])
    );
    assert_eq!(
        parse_path("$.message[0].data"),
        Some(vec![
            Step::Key("message".to_owned()),
            Step::Index(0),
            Step::Key("data".to_owned()),
        ])
    );
    assert_eq!(parse_path("userIcon"), None);
    assert_eq!(parse_path("$.a[x]"), None);
}

#[test]
fn a_failure_during_repair_activation_preserves_live_data_identity_and_capture() {
    let (_directory, mut store) = fixture();
    bind_server_capture(&store);
    let epoch = super::super::sync_selection::read(&store.connection).unwrap().epoch;
    store.external_select(&epoch, &super::super::sync_selection::SyncTarget::External("synthetic-external".into())).unwrap();
    let before = super::super::sync_selection::identity(&store.connection).unwrap();
    let root = root_of(&store);
    store.connection.execute_batch("DELETE FROM content_changes; DELETE FROM server_sync_dirty;
        CREATE TRIGGER synthetic_repair_activation_failure BEFORE UPDATE ON meta
        WHEN NEW.key='currentRevision' BEGIN SELECT RAISE(ABORT,'synthetic activation failure'); END;").unwrap();
    assert!(store.apply_repair(1, &[candidate(RepairAction::DropReference {
        owner: crate::data_health::Owner { kind: "root".into(), id: "database".into() },
        source_path: "$.userIcon".into(), occurrence: 0,
    })], 10).is_err());
    let after = super::super::sync_selection::identity(&store.connection).unwrap();
    assert_eq!(after.library_epoch, before.library_epoch);
    assert_eq!(after.selection_epoch, before.selection_epoch);
    assert_eq!(after.generation, before.generation);
    assert_eq!(store.revision().unwrap(), 1);
    assert_eq!(root_of(&store), root);
    assert!(!super::super::sync_selection::read(&store.connection).unwrap().decision_required);
    for table in ["content_changes", "server_sync_dirty", "server_sync_context"] {
        let count: i64 = store.connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0, "failed activation must not publish {table}");
    }
}

#[test]
#[ignore = "explicit synthetic repair/undo disk and time measurement"]
fn measures_single_reference_repair_and_undo_on_a_large_library() {
    fn file_bytes(path: &std::path::Path) -> u64 {
        match std::fs::metadata(path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => panic!("read synthetic fixture file size: {error}"),
        }
    }
    fn directory_bytes(path: &std::path::Path) -> u64 {
        std::fs::read_dir(path).unwrap().map(|entry| {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            assert!(!kind.is_symlink());
            if kind.is_dir() { directory_bytes(&entry.path()) } else { file_bytes(&entry.path()) }
        }).sum()
    }

    const MESSAGES: usize = 10_000;
    const MESSAGE_BYTES: usize = 1024;
    let (directory, mut store) = fixture();
    let original = root_of(&store);
    let staging = store.replace_begin().unwrap().staging_id;
    store.replace_put_root(&staging, &original).unwrap();
    let messages: Vec<Value> = (0..MESSAGES).map(|index| json!({
        "chatId": format!("synthetic-message-{index}"), "role": "user", "data": "x".repeat(MESSAGE_BYTES),
    })).collect();
    store.replace_add_characters(&staging, &[json!({
        "chaId": "synthetic-large-character", "name": "Synthetic", "type": "character", "chatPage": 0,
        "chats": [{"id": "synthetic-large-chat", "name": "Synthetic", "message": messages}],
    })]).unwrap();
    store.replace_commit(&staging, Some(1)).unwrap();
    bind_server_capture(&store);
    let epoch = super::super::sync_selection::read(&store.connection).unwrap().epoch;
    store.external_select(&epoch, &super::super::sync_selection::SyncTarget::External("synthetic-external".into())).unwrap();
    let selected = super::super::sync_selection::read(&store.connection).unwrap();
    let identity = super::super::sync_selection::identity(&store.connection).unwrap();
    let (actual_messages, message_json_bytes): (i64, i64) = store.connection.query_row(
        "SELECT COUNT(*),SUM(LENGTH(CAST(value AS BLOB))) FROM messages", [], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(actual_messages, MESSAGES as i64);
    store.connection.execute_batch("DELETE FROM content_changes; DELETE FROM server_sync_dirty; PRAGMA wal_autocheckpoint=0;").unwrap();
    let database = store.database_path.clone();
    let wal = std::path::PathBuf::from(format!("{}-wal", database.display()));
    let footprint = || {
        let database_bytes = file_bytes(&database);
        let wal_bytes = file_bytes(&wal);
        let total_bytes = directory_bytes(directory.path());
        assert!(total_bytes >= database_bytes + wal_bytes, "synthetic fixture footprint omitted database or WAL bytes");
        (total_bytes, database_bytes, wal_bytes)
    };
    let assert_contract = |store: &PersistentStore, revision: i64| {
        let after = super::super::sync_selection::identity(&store.connection).unwrap();
        assert_eq!(after.library_epoch, identity.library_epoch);
        assert_eq!(after.selection_epoch, identity.selection_epoch);
        assert_eq!(after.generation, identity.generation);
        let selection = super::super::sync_selection::read(&store.connection).unwrap();
        assert_eq!(selection.target, selected.target);
        assert!(!selection.decision_required);
        let retained: (i64, i64) = store.connection.query_row(
            "SELECT COUNT(*),SUM(LENGTH(CAST(value AS BLOB))) FROM messages", [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(retained, (actual_messages, message_json_bytes));
        for table in ["content_changes", "server_sync_dirty"] {
            let entries: Vec<(String, String, String, i64)> = store.connection.prepare(&format!("SELECT kind,key1,key2,revision FROM {table}")).unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap().collect::<Result<_,_>>().unwrap();
            assert_eq!(entries, vec![("root".into(), String::new(), String::new(), revision)]);
        }
    };

    store.checkpoint(crate::persistent_store::CheckpointMode::Truncate).unwrap();
    let repair_before = footprint();
    let started = std::time::Instant::now();
    let (repaired, journal) = store.apply_repair(identity.revision, &[candidate(RepairAction::DropReference {
        owner: crate::data_health::Owner { kind: "root".into(), id: "database".into() },
        source_path: "$.userIcon".into(), occurrence: 0,
    })], 10).unwrap();
    let repair_ms = started.elapsed().as_millis();
    let repair_after = footprint();
    assert_eq!(journal.records.len(), 1);
    assert_eq!(journal.records[0].table, "root");
    assert!(root_of(&store).get("userIcon").is_none());
    assert_contract(&store, repaired.revision);
    assert!(repair_after.2 > repair_before.2);
    eprintln!("cr004 phase=repair messages={actual_messages} message_json_bytes={message_json_bytes} changed_records={} elapsed_ms={repair_ms} temporary_root_file_bytes_before={} temporary_root_file_bytes_after={} temporary_root_file_growth_bytes={} database_bytes_before={} database_bytes_after={} wal_bytes_before={} wal_bytes_after={} wal_growth_bytes={}",
        journal.records.len(), repair_before.0, repair_after.0, repair_after.0.saturating_sub(repair_before.0), repair_before.1, repair_after.1,
        repair_before.2, repair_after.2, repair_after.2.saturating_sub(repair_before.2));

    store.checkpoint(crate::persistent_store::CheckpointMode::Truncate).unwrap();
    let undo_before = footprint();
    let started = std::time::Instant::now();
    let (undone, skipped) = store.undo_repair(&journal, repaired.revision).unwrap();
    let undo_ms = started.elapsed().as_millis();
    let undo_after = footprint();
    assert!(skipped.is_empty());
    assert_eq!(root_of(&store), original);
    assert_contract(&store, undone.revision);
    assert!(undo_after.2 > undo_before.2);
    eprintln!("cr004 phase=undo messages={actual_messages} message_json_bytes={message_json_bytes} changed_records={} elapsed_ms={undo_ms} temporary_root_file_bytes_before={} temporary_root_file_bytes_after={} temporary_root_file_growth_bytes={} database_bytes_before={} database_bytes_after={} wal_bytes_before={} wal_bytes_after={} wal_growth_bytes={}",
        journal.records.len(), undo_before.0, undo_after.0, undo_after.0.saturating_sub(undo_before.0), undo_before.1, undo_after.1,
        undo_before.2, undo_after.2, undo_after.2.saturating_sub(undo_before.2));
    store.checkpoint(crate::persistent_store::CheckpointMode::Truncate).unwrap();
    let checkpoint = footprint();
    eprintln!("cr004 phase=checkpoint temporary_root_file_bytes={} database_bytes={} wal_bytes={}", checkpoint.0, checkpoint.1, checkpoint.2);
}
