use super::*;

fn state(store: &PersistentStore, generation: &str) -> Option<String> {
    use rusqlite::OptionalExtension;
    store
        .connection
        .query_row("SELECT state FROM generations WHERE id=?1", [generation], |row| row.get(0))
        .optional()
        .unwrap()
}

fn rows(store: &PersistentStore, generation: &str) -> i64 {
    GENERATION_TABLES
        .iter()
        .map(|(table, _)| {
            store
                .connection
                .query_row(
                    &format!("SELECT count(*) FROM {table} WHERE generation=?1"),
                    [generation],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap()
        })
        .sum()
}

fn purge_all(store: &mut PersistentStore) -> usize {
    let mut batches = 0;
    while store.purge_retired_batch(64).unwrap() {
        batches += 1;
        assert!(batches < 10_000, "retired purge does not converge");
    }
    batches + 1
}

fn activated(store: &mut PersistentStore, username: &str) -> String {
    let stage = stage_root(store, username);
    let revision = store.revision().unwrap();
    store.replace_commit(&stage, Some(revision)).unwrap();
    stage
}

#[test]
fn activation_keeps_the_stage_name_and_retires_the_previous_library() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let initial = active_generation(&store.connection).unwrap();
    assert_eq!(state(&store, &initial).as_deref(), Some("active"));
    let first = activated(&mut store, "First");
    assert_eq!(active_generation(&store.connection).unwrap(), first);
    assert_eq!(state(&store, &first).as_deref(), Some("active"));
    assert_eq!(state(&store, &initial).as_deref(), Some("retired"));
    let second = stage_root(&mut store, "Second");
    assert_eq!(state(&store, &second).as_deref(), Some("staging"));
    store.replace_commit(&second, Some(1)).unwrap();
    assert_eq!(active_generation(&store.connection).unwrap(), second);
    assert_eq!(state(&store, &first).as_deref(), Some("retired"));
    assert!(rows(&store, &first) > 0, "a retired library stays until the purge removes it");
    let activated: Option<String> = store
        .connection
        .query_row(
            "SELECT activated_generation FROM lww_requests WHERE request_id=?1",
            [&second],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(activated.as_deref(), Some(second.as_str()));
    assert_eq!(store.read_root(None).unwrap().value["username"], "Second");
}

#[test]
fn abort_and_generation_delete_refuse_an_activated_generation() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let first = activated(&mut store, "First");
    assert!(store.replace_abort(&first).is_err());
    assert_eq!(store.read_root(None).unwrap().value["username"], "First");
    let second = activated(&mut store, "Second");
    assert!(store.replace_abort(&first).is_err(), "a retired generation is left to the purge");
    assert!(store.replace_abort(&second).is_err());
    {
        let transaction = store.connection.transaction().unwrap();
        assert!(super::super::commit::delete_generation(&transaction, &second).is_err());
        transaction.rollback().unwrap();
    }
    assert_eq!(store.read_root(None).unwrap().value["username"], "Second");
    assert!(store.replace_put_root(&second, &json!({ "username": "reused" })).is_err());
    assert_eq!(store.read_root(None).unwrap().value["username"], "Second");
}

#[test]
fn failed_intent_completion_after_the_library_commit_keeps_the_library() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    activated(&mut store, "First");
    let stage = stage_root(&mut store, "Second");
    store
        .device_store()
        .unwrap()
        .connection()
        .execute_batch(
            "CREATE TRIGGER fail_completion BEFORE UPDATE OF complete ON lww_intents
             BEGIN SELECT RAISE(ABORT,'synthetic completion failure'); END;",
        )
        .unwrap();
    assert!(store.replace_commit(&stage, Some(1)).is_err());
    assert_eq!(store.revision().unwrap(), 2);
    assert_eq!(active_generation(&store.connection).unwrap(), stage);
    assert!(store.replace_abort(&stage).is_err());
    assert_eq!(store.read_root(None).unwrap().value["username"], "Second");
    store
        .device_store()
        .unwrap()
        .connection()
        .execute_batch("DROP TRIGGER fail_completion")
        .unwrap();
    store.lww_recover_intents().unwrap();
    assert_eq!(store.replace_commit(&stage, Some(1)).unwrap().revision, 2);
    assert_eq!(store.read_root(None).unwrap().value["username"], "Second");
}

#[test]
fn interrupted_activation_rolls_back_and_keeps_the_stage() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let first = activated(&mut store, "First");
    let stage = stage_root(&mut store, "Second");
    store
        .connection
        .execute_batch(
            "CREATE TRIGGER interrupt_activation BEFORE INSERT ON lww_requests
             BEGIN SELECT RAISE(ABORT,'synthetic interrupted activation'); END;",
        )
        .unwrap();
    assert!(store.replace_commit(&stage, Some(1)).is_err());
    assert_eq!(store.revision().unwrap(), 1);
    assert_eq!(active_generation(&store.connection).unwrap(), first);
    assert_eq!(state(&store, &first).as_deref(), Some("active"));
    assert_eq!(state(&store, &stage).as_deref(), Some("staging"));
    assert_eq!(store.read_root(None).unwrap().value["username"], "First");
    let staged_rows = rows(&store, &stage);
    assert!(staged_rows > 0);
    store.connection.execute_batch("DROP TRIGGER interrupt_activation").unwrap();
    drop(store);
    let store = PersistentStore::open(directory.path()).unwrap();
    assert_eq!(store.revision().unwrap(), 2);
    assert_eq!(active_generation(&store.connection).unwrap(), stage);
    assert_eq!(store.read_root(None).unwrap().value["username"], "Second");
    assert_eq!(state(&store, &first).as_deref(), Some("retired"));
}

#[test]
fn a_reader_lease_keeps_reading_the_previous_library_across_activation_and_purge() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let first = activated(&mut store, "First");
    let lease = store.acquire_revision(1).unwrap().lease;
    activated(&mut store, "Second");
    purge_all(&mut store);
    assert_eq!(state(&store, &first), None);
    assert_eq!(rows(&store, &first), 0);
    assert_eq!(store.read_root(Some(&lease)).unwrap().value["username"], "First");
    assert_eq!(store.read_root(None).unwrap().value["username"], "Second");
    store.release_revision(&lease).unwrap();
}

#[test]
fn startup_keeps_the_active_generation_and_hands_abandoned_stages_to_the_purge() {
    let directory = tempfile::tempdir().unwrap();
    let (active, abandoned) = {
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let active = activated(&mut store, "Active");
        (active, stage_root(&mut store, "Abandoned"))
    };
    assert!(active.starts_with("staging-"));
    let mut store = PersistentStore::open(directory.path()).unwrap();
    assert_eq!(active_generation(&store.connection).unwrap(), active);
    assert_eq!(store.read_root(None).unwrap().value["username"], "Active");
    assert_eq!(state(&store, &abandoned).as_deref(), Some("retired"));
    assert!(store.replace_commit(&abandoned, None).is_err());
    purge_all(&mut store);
    assert_eq!(rows(&store, &abandoned), 0);
    assert_eq!(state(&store, &abandoned), None);
    assert_eq!(store.read_root(None).unwrap().value["username"], "Active");
}

#[test]
fn retired_purge_is_bounded_per_batch_and_resumes_after_a_failed_batch_and_restart() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let old = activated(&mut store, "Old");
    purge_all(&mut store);
    {
        let transaction = store.connection.transaction().unwrap();
        for index in 0..300 {
            transaction
                .execute(
                    "INSERT INTO plugin_storage(generation,owner,storage_key,byte_size,ordinal,value) VALUES(?1,'synthetic-plugin',?2,1,?3,'1')",
                    params![old, format!("synthetic-{index:04}"), index],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
    }
    activated(&mut store, "New");
    let before = rows(&store, &old);
    assert!(store.purge_retired_batch(100).unwrap());
    assert_eq!(rows(&store, &old), before - 100);
    store
        .connection
        .execute_batch(
            "CREATE TRIGGER fail_purge BEFORE DELETE ON plugin_storage
             WHEN OLD.storage_key='synthetic-0250'
             BEGIN SELECT RAISE(ABORT,'synthetic purge failure'); END;",
        )
        .unwrap();
    let remaining = rows(&store, &old);
    let mut failed = false;
    while !failed {
        match store.purge_retired_batch(100) {
            Ok(more) => assert!(more),
            Err(_) => failed = true,
        }
    }
    let after_failure = rows(&store, &old);
    assert!(after_failure < remaining && after_failure > 0);
    assert_eq!((remaining - after_failure) % 100, 0, "a failed batch deletes nothing");
    assert_eq!(state(&store, &old).as_deref(), Some("retired"));
    store.connection.execute_batch("DROP TRIGGER fail_purge").unwrap();
    drop(store);
    let mut store = PersistentStore::open(directory.path()).unwrap();
    assert_eq!(rows(&store, &old), after_failure);
    purge_all(&mut store);
    assert_eq!(rows(&store, &old), 0);
    assert_eq!(state(&store, &old), None);
    assert_eq!(store.read_root(None).unwrap().value["username"], "New");
}

#[test]
fn retired_library_assets_stay_rooted_until_purged() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let old = activated(&mut store, "Old");
    let hash = "c".repeat(64);
    store
        .connection
        .execute(
            "INSERT INTO asset_aliases (generation, logical_key, object_hash, kind, size, mime, name, ext, inlay_type, width, height, metadata)
             VALUES (?1, 'assets/retired.bin', ?2, 'asset', 1, '', '', '', NULL, NULL, NULL, '{}')",
            params![old, hash],
        )
        .unwrap();
    activated(&mut store, "New");
    let roots = snapshot::collect_asset_roots(&store.connection).unwrap();
    assert!(roots.object_hashes.contains(&hash));
    purge_all(&mut store);
    let roots = snapshot::collect_asset_roots(&store.connection).unwrap();
    assert!(!roots.object_hashes.contains(&hash));
}

#[test]
fn purging_a_retired_library_keeps_the_plugin_gc_revision() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let old = activated(&mut store, "Old");
    purge_all(&mut store);
    store
        .connection
        .execute(
            "INSERT INTO plugin_storage(generation,owner,storage_key,byte_size,ordinal,value) VALUES(?1,'synthetic-plugin','synthetic-key',1,0,'1')",
            [&old],
        )
        .unwrap();
    activated(&mut store, "New");
    let revision = |store: &PersistentStore| -> i64 {
        store
            .connection
            .query_row("SELECT revision FROM plugin_gc_revision WHERE singleton=1", [], |row| row.get(0))
            .unwrap()
    };
    let before = revision(&store);
    purge_all(&mut store);
    assert_eq!(rows(&store, &old), 0);
    assert_eq!(revision(&store), before);
    let stage = stage_root(&mut store, "Staged");
    store
        .connection
        .execute(
            "INSERT INTO plugin_storage(generation,owner,storage_key,byte_size,ordinal,value) VALUES(?1,'synthetic-plugin','synthetic-key',1,0,'1')",
            [&stage],
        )
        .unwrap();
    let staged = revision(&store);
    store.replace_abort(&stage).unwrap();
    assert_eq!(revision(&store), staged + 1);
}

#[test]
fn a_snapshot_taken_during_the_purge_restores_the_active_library() {
    let (_directory, mut store, _) = open_fixture();
    let old = active_generation(&store.connection).unwrap();
    purge_all(&mut store);
    activated(&mut store, "Replacement");
    assert!(store.purge_retired_batch(5).unwrap());
    assert!(rows(&store, &old) > 0);
    let snapshot = store.snapshot_create("mid-purge").unwrap();
    let stage = store.snapshot_restore_stage(&snapshot.id, "restore-mid-purge").unwrap();
    assert_eq!(state(&store, &stage.staging_id).as_deref(), Some("staging"));
}

/// A retired library only waits for the purge, so the size estimates leave its
/// values out while its rows are still in the file.
#[test]
fn size_estimates_leave_out_a_retired_library() {
    const FILLER: usize = 1024 * 1024;
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let replace = |store: &mut PersistentStore, fill: &str| {
        let staging = store.replace_begin().unwrap().staging_id;
        store
            .replace_put_root(&staging, &json!({ "username": "Synthetic", "filler": fill.repeat(FILLER) }))
            .unwrap();
        let revision = store.revision().unwrap();
        store.replace_commit(&staging, Some(revision)).unwrap();
        staging
    };
    let page_bytes = |store: &PersistentStore| -> u64 {
        store
            .connection
            .query_row(
                "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap() as u64
    };
    let estimates = |store: &mut PersistentStore| -> [u64; 3] {
        // The storage figures take the archive lock, so it is released first.
        let snapshot = {
            let archive = super::super::snapshot_archive::Archive::open(&store.snapshots_dir).unwrap();
            snapshot::capture_scratch(store, &archive).unwrap().1
        };
        let objects = store.storage_stats().unwrap().asset_objects.bytes;
        [
            store.storage_stats().unwrap().database_bytes,
            store.portable_export_lower_bound().unwrap() - objects,
            snapshot,
        ]
    };
    let first = replace(&mut store, "a");
    purge_all(&mut store);
    replace(&mut store, "b");
    assert_eq!(state(&store, &first).as_deref(), Some("retired"));
    assert!(rows(&store, &first) > 0);
    let pages = page_bytes(&store);
    for estimate in estimates(&mut store) {
        assert!(estimate >= FILLER as u64, "{estimate}");
        assert!(estimate + FILLER as u64 <= pages, "{estimate} of {pages}");
    }
}

/// Root collection finds generations through the tables that can reference a
/// CAS object. A new generation table belongs in that list or among the ones
/// named here, whose objects are not CAS objects.
#[test]
fn root_collection_covers_every_generation_table_that_can_reference_an_object() {
    let without_cas_objects = [
        "asset_alias_replacement_candidates",
        "message_page_indexes",
        "message_page_manifests",
    ];
    let mut classified = snapshot::ROOT_GENERATION_TABLES
        .iter()
        .chain(without_cas_objects.iter())
        .copied()
        .collect::<Vec<_>>();
    classified.sort_unstable();
    let mut tables = GENERATION_TABLES.iter().map(|(table, _)| *table).collect::<Vec<_>>();
    tables.sort_unstable();
    assert_eq!(classified, tables);
}
