use super::super::snapshot_archive::Archive;
use super::*;

#[test]
fn snapshot_restore_freezes_finished_held_and_deferred_original_values_without_device_progress() {
    use crate::persistent_store::lww::{Header,Change,StageReceive,ApplyReceive,Progress,MessageLocator};
    use risunest_sync_wire::{stamp::Stamp,unit::{UnitKey,UnitValue}};
    let (_directory,mut store,_)=open_fixture();
    let (_source_dir,mut source,_)=open_fixture();
    source.commit(&WorkingSetCommit {expected_revision:source.revision().unwrap(),conversations:Some(vec![ConversationMutation::ReplaceRange {
        character_id:"char-b".into(),conversation_id:"conv-beta".into(),start:0,delete_count:3,
        messages:vec![json!({"role":"user","data":"synthetic deferred snapshot message","chatId":"snapshot-deferred"})],conversation:None,configured_index:None,
    }]),..Default::default()}).unwrap();
    let message_key=UnitKey::new(&["messages","char-b","conv-beta"]).unwrap();
    let message:UnitValue=serde_json::from_str(&source.connection.query_row("SELECT value FROM lww_units WHERE key=?1",[message_key.as_str()],|row|row.get::<_,String>(0)).unwrap()).unwrap();
    let mut controls=source.connection.prepare("SELECT hash,body FROM message_page_objects").unwrap();
    for row in controls.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,Vec<u8>>(1)?))).unwrap() {
        let (hash,body)=row.unwrap();store.lww_put_object(&hash,&body).unwrap();
    }
    let held_key=UnitKey::new(&["character","missing","name"]).unwrap();
    let held=UnitValue::inline(br#""synthetic held snapshot name""#).unwrap();
    let stamp=Stamp {physical_ms:(store.lww_clock_state().unwrap().issued.unwrap().physical_ms.0+1).into(),logical:0,writer_id:"00000000-0000-4000-8000-000000000001".into()};
    let header=Header {binding_authority:store.lww_binding_authority().unwrap(),request_id:"snapshot-held-deferred".into()};
    store.lww_stage_receive(&StageReceive {header:header.clone(),changes:vec![Change {key:message_key.clone(),stamp:stamp.clone(),value:message.clone()},Change {key:held_key.clone(),stamp,value:held.clone()}],progress:Progress {kind:"server".into(),cursor:1.into(),writer_id:None},admitted_time_upper_ms:u64::MAX.into()}).unwrap();
    let applied=store.lww_apply_receive(&ApplyReceive {header:header.clone(),generating:vec![MessageLocator {character_id:"char-b".into(),conversation_id:"conv-beta".into(),start:None}]}).unwrap();
    assert_eq!(applied.held_keys,vec![held_key.clone()]);
    assert_eq!(applied.deferred_keys,vec![message_key.clone()]);
    store.lww_finish_receive(&header).unwrap();
    let progress=serde_json::to_value(store.lww_receive_progress(header.binding_authority.clone()).unwrap()).unwrap();
    let snapshot=store.snapshot_create("held-deferred").unwrap();
    let stage=store.snapshot_restore_stage(&snapshot.id,"restore-held-deferred").unwrap();
    for (key,value) in [(&message_key,&message),(&held_key,&held)] {
        let stored:String=store.connection.query_row("SELECT value FROM replacement_source_units WHERE generation=?1 AND layer=0 AND key=?2",params![stage.staging_id,key.as_str()],|row|row.get(0)).unwrap();
        assert_eq!(serde_json::from_str::<UnitValue>(&stored).unwrap(),*value);
    }
    store.snapshot_restore_activate(&stage.staging_id,store.revision().unwrap(),header.binding_authority.clone()).unwrap();
    assert_eq!(serde_json::to_value(store.lww_receive_progress(header.binding_authority).unwrap()).unwrap(),progress);
    assert_eq!(store.materialize(None).unwrap()["characters"][0]["chats"][0]["message"][0]["data"],"synthetic deferred snapshot message");
}

#[test]
fn committed_snapshot_receipt_does_not_replay_after_later_edit() {
    let (directory, mut store, _) = open_fixture();
    let snapshot=store.snapshot_create("manual").unwrap();
    let stage=store.snapshot_restore_stage(&snapshot.id,"receipt-replay").unwrap();
    let authority=store.lww_binding_authority().unwrap();
    let expected=store.revision().unwrap();
    let receipt=store.snapshot_restore_activate(&stage.staging_id,expected,authority.clone()).unwrap();
    store.commit(&WorkingSetCommit {root:Some(json!({"username":"edit after activation"})),..empty_working_set_commit(receipt.revision)}).unwrap();
    drop(store);
    let mut reopened=PersistentStore::open(directory.path()).unwrap();
    assert_eq!(reopened.snapshot_restore_activate(&stage.staging_id,expected,authority).unwrap().revision,receipt.revision);
    assert_eq!(reopened.read_root(None).unwrap().value["username"],"edit after activation");
    assert_eq!(reopened.revision().unwrap(),receipt.revision+1);
}

#[test]
fn a_snapshot_stage_holds_its_source_units_in_the_store_until_activation_or_abort() {
    let (_directory, mut store, _) = open_fixture();
    let lease=store.lww_acquire_library_backup_capture(store.revision().unwrap()).unwrap().lease;
    let original=store.lww_backup_unit_values(&lease).unwrap();
    store.release_revision(&lease).unwrap();
    assert!(!original.is_empty());
    let snapshot=store.snapshot_create("manual").unwrap();
    let aborted=store.snapshot_restore_stage(&snapshot.id,"source-rows-abort").unwrap();
    assert!(store.replacement_source_rows(&aborted.staging_id).unwrap()>0);
    store.snapshot_restore_abort(&aborted.staging_id).unwrap();
    assert_eq!(store.replacement_source_rows(&aborted.staging_id).unwrap(),0);
    let stage=store.snapshot_restore_stage(&snapshot.id,"source-rows-activate").unwrap();
    let staged=store.replacement_source_units(&stage.staging_id).collect::<crate::persistent_store::StoreResult<std::collections::BTreeMap<_,_>>>().unwrap();
    assert_eq!(staged,original);
    let authority=store.lww_binding_authority().unwrap();
    let receipt=store.snapshot_restore_activate(&stage.staging_id,store.revision().unwrap(),authority.clone()).unwrap();
    assert_eq!(store.replacement_source_rows(&stage.staging_id).unwrap(),0);
    assert_eq!(store.snapshot_restore_activate(&stage.staging_id,0,authority).unwrap().revision,receipt.revision);
}

#[test]
fn staged_snapshot_does_not_activate_on_reopen_and_explicit_retry_is_live() {
    let (directory, mut store, _) = open_fixture();
    let snapshot=store.snapshot_create("manual").unwrap();
    store.commit(&WorkingSetCommit {root:Some(json!({"username":"current"})),..empty_working_set_commit(1)}).unwrap();
    let stage=store.snapshot_restore_stage(&snapshot.id,"staged-reopen").unwrap();
    drop(store);
    let mut reopened=PersistentStore::open(directory.path()).unwrap();
    assert_eq!(reopened.read_root(None).unwrap().value["username"],"current");
    assert_eq!(reopened.snapshot_restore_stage(&snapshot.id,"staged-reopen").unwrap().staging_id,stage.staging_id);
    let revision=reopened.revision().unwrap();
    let authority=reopened.lww_binding_authority().unwrap();
    let receipt=reopened.snapshot_restore_activate(&stage.staging_id,revision,authority).unwrap();
    assert_eq!(receipt.revision,revision+1);
}

#[test]
fn schema_configures_the_documented_sqlite_profile() {
    let directory = tempfile::tempdir().expect("create temporary directory");
    let store = PersistentStore::open(directory.path()).expect("open persistent store");

    let integer_pragma = |name: &str| {
        store
            .connection
            .query_row(&format!("PRAGMA {name}"), [], |row| row.get::<_, i64>(0))
            .expect("read integer pragma")
    };
    let journal_mode: String = store
        .connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("read journal mode");

    assert_eq!(journal_mode, "wal");
    assert_eq!(integer_pragma("synchronous"), 1);
    assert_eq!(integer_pragma("busy_timeout"), 5_000);
    assert_eq!(integer_pragma("cache_size"), -16_000);
    assert_eq!(integer_pragma("temp_store"), 2);
    assert_eq!(integer_pragma("journal_size_limit"), 67_108_864);
    assert_eq!(integer_pragma("foreign_keys"), 0);
    assert_eq!(
        integer_pragma("user_version"),
        i64::from(super::schema::SCHEMA_VERSION)
    );
}

#[test]
fn snapshots_create_list_and_restore_before_reopen() {
    let (directory, mut store, database) = open_fixture();
    let snapshot=store.snapshot_create("contract-test").unwrap();
    assert!(snapshot.bytes>0);
    assert_eq!(store.snapshot_list().unwrap().len(),1);
    store.commit(&WorkingSetCommit {root:Some(json!({"username":"Changed after snapshot"})),..empty_working_set_commit(1)}).unwrap();
    let stage=store.snapshot_restore_stage(&snapshot.id,"contract-restore").unwrap();
    let authority=store.lww_binding_authority().unwrap();
    let receipt=store.snapshot_restore_activate(&stage.staging_id,2,authority).unwrap();
    assert_eq!(receipt.revision,3);
    assert_eq!(store.materialize(None).unwrap(),database);
    drop(store);
    let restored=PersistentStore::open(directory.path()).unwrap();
    assert_eq!(restored.revision().unwrap(),receipt.revision);
    assert_eq!(restored.materialize(None).unwrap(),database);
    assert!(restored.snapshot_list().unwrap().iter().any(|s|s.id==snapshot.id));
}

#[test]
fn source_preservation_snapshot_keeps_invalid_json_and_retains_objects() {
    let (_directory, mut store, _) = open_fixture();
    store
        .connection
        .execute("UPDATE root SET value='synthetic invalid JSON'", [])
        .unwrap();
    assert!(store.materialize(None).is_err());
    let snapshot = store.snapshot_create("preserve-invalid-json").unwrap();
    let (_capture, connection) = reconstruct_snapshot(&store, &snapshot.id);
    let value: String = connection
        .query_row("SELECT value FROM root LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "synthetic invalid JSON");
    let metadata = Archive::open(&store.snapshots_dir)
        .unwrap()
        .metadata(&snapshot.id)
        .unwrap();
    assert!(metadata.roots.retain_all_objects);
    assert!(store.materialize(None).is_err());
}

#[test]
fn source_preservation_snapshot_survives_damaged_storage_classes() {
    for sql in [
        "UPDATE root SET value=x'00'",
        "UPDATE characters SET detail=x'00'",
        "UPDATE characters SET image=x'00' WHERE image IS NOT NULL",
        "UPDATE conversations SET detail=x'00'",
        "UPDATE messages SET value=x'00'",
    ] {
        let (_directory, mut store, _) = open_fixture();
        assert!(
            store.connection.execute(sql, []).unwrap() > 0,
            "synthetic damage did not exercise its target: {sql}"
        );
        let snapshot = store.snapshot_create("preserve-damaged-column").unwrap();
        let metadata = Archive::open(&store.snapshots_dir)
            .unwrap()
            .metadata(&snapshot.id)
            .unwrap();
        assert!(metadata.roots.retain_all_objects, "{sql}");
        assert!(
            metadata.roots.blockers.contains("record-unscannable"),
            "{sql}"
        );
    }
    let (_directory, mut store, _) = open_fixture();
    store
        .connection
        .execute("UPDATE root SET value=x'0001'", [])
        .unwrap();
    let snapshot = store.snapshot_create("preserve-damaged-column").unwrap();
    let (_capture, connection) = reconstruct_snapshot(&store, &snapshot.id);
    let value: Vec<u8> = connection
        .query_row("SELECT value FROM root LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, vec![0, 1]);
}

#[test]
fn snapshot_delete_requires_a_listed_id_and_removes_its_roots() {
    let (_directory, mut store, _) = open_fixture();
    let created = store.snapshot_create("delete-test").unwrap();
    store.snapshot_delete(&created.id).unwrap();
    assert!(store.snapshot_list().unwrap().is_empty());
    assert!(Archive::open(&store.snapshots_dir)
        .unwrap()
        .roots()
        .unwrap()
        .is_empty());
    assert!(store.snapshot_delete("../not-a-snapshot.db").is_err());
    assert!(store
        .snapshot_restore_stage("../not-a-snapshot.db","invalid-id")
        .is_err());
    assert!(store.snapshot_delete(&created.id).is_err());
}

#[test]
fn staged_snapshot_owns_source_after_archive_deletion() {
    let (_directory, mut store, database)=open_fixture();
    let created=store.snapshot_create("staged-delete").unwrap();
    let stage=store.snapshot_restore_stage(&created.id,"staged-delete").unwrap();
    store.snapshot_delete(&created.id).unwrap();
    assert!(store.snapshot_list().unwrap().is_empty());
    let authority=store.lww_binding_authority().unwrap();
    store.snapshot_restore_activate(&stage.staging_id,1,authority).unwrap();
    assert_eq!(store.materialize(None).unwrap(),database);
}

#[test]
fn snapshot_archive_rejects_linked_database() {
    let (directory, store, _) = open_fixture();
    let external = directory.path().join("external.sqlite");
    fs::write(&external, b"external synthetic bytes").unwrap();
    let linked = store.snapshots_dir.join("snapshots.sqlite");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&external, &linked).unwrap();
    #[cfg(windows)]
    match std::os::windows::fs::symlink_file(&external, &linked) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("Windows symlink integration unavailable: {error}");
            return;
        }
        Err(error) => panic!("create symlink: {error}"),
    }
    assert!(store.snapshot_list().is_err());
    assert_eq!(fs::read(&external).unwrap(), b"external synthetic bytes");
}

#[test]
fn snapshot_creation_persists_asset_roots_before_returning() {
    let (directory, mut store, _) = open_fixture();
    let generation = super::active_generation(&store.connection).expect("read active generation");
    let manifest_hash = "a".repeat(64);
    let object_hash = "b".repeat(64);
    store
        .connection
        .execute(
            "UPDATE root SET value = ?2 WHERE generation = ?1",
            rusqlite::params![
                generation,
                serde_json::to_string(&json!({
                    "asset": "assets/exact.bin",
                    "inlay": "{{inlay::kept-inlay}}",
                    "coldStoragedChats": ["cold-chat"]
                }))
                .unwrap()
            ],
        )
        .unwrap();
    store
        .connection
        .execute(
            "INSERT INTO asset_aliases (
                generation, logical_key, object_hash, kind, size, mime, name, ext,
                inlay_type, width, height
             ) VALUES (?1, 'assets/missing.bin', NULL, 'asset', 0,
                'application/octet-stream', 'missing', 'bin', NULL, NULL, NULL)",
            [&generation],
        )
        .unwrap();
    store
        .connection
        .execute(
            "INSERT INTO asset_aliases (
                generation, logical_key, object_hash, kind, size, mime, name, ext,
                inlay_type, width, height
             ) VALUES (?1, 'assets/exact.bin', ?2, 'asset', 1,
                'application/octet-stream', 'exact', 'bin', NULL, NULL, NULL)",
            rusqlite::params![generation, object_hash],
        )
        .unwrap();
    store
        .connection
        .execute(
            "INSERT INTO asset_owner_heads (
                generation, owner_kind, owner_locator, present, manifest_hash, entry_count
             ) VALUES (?1, 'root-module-assets', 'module-1', 1, ?2, 1)",
            rusqlite::params![generation, manifest_hash],
        )
        .unwrap();
    store
        .connection
        .execute(
            "INSERT INTO plugin_storage (generation, owner, storage_key, byte_size, ordinal, value)
             VALUES (?1, 'synthetic-plugin', 'opaque', 2, 0, '{}')",
            [generation],
        )
        .unwrap();

    let snapshot = store.snapshot_create("asset-roots").unwrap();
    let metadata = Archive::open(&store.snapshots_dir)
        .unwrap()
        .metadata(&snapshot.id)
        .unwrap();

    assert_eq!(metadata.revision, 1);
    assert_eq!(metadata.roots.manifest_hashes, [manifest_hash].into());
    assert_eq!(metadata.roots.object_hashes, [object_hash].into());
    assert_eq!(
        metadata.roots.legacy_asset_keys,
        ["assets/missing.bin".to_owned()].into()
    );
    assert_eq!(metadata.roots.inlay_ids, ["kept-inlay".to_owned()].into());
    assert_eq!(metadata.roots.cold_keys, ["cold-chat".to_owned()].into());
    assert_eq!(
        metadata.roots.blockers,
        ["cold-payload-unscanned".to_owned()].into()
    );
    assert!(metadata.roots.retain_all_objects);
    drop(directory);
}

#[test]
fn asset_gc_dry_run_keeps_leased_generation_roots_until_release() {
    use super::asset_object_catalog::AssetObjectRegistration;

    let directory = tempfile::tempdir().expect("create temporary directory");
    let mut store = PersistentStore::open(directory.path()).expect("open persistent store");
    let cas = crate::asset_repository::PayloadCas::new(directory.path()).unwrap();
    let original_payload = cas.prepare_bytes(b"original").unwrap();
    let replacement_payload = cas.prepare_bytes(b"replacement").unwrap();
    let collectable_payload = cas.prepare_bytes(b"collectable").unwrap();
    let original = AssetAlias {
        key: "assets/leased.bin".to_owned(),
        object_hash: Some(original_payload.content_hash.clone()),
        kind: "asset".to_owned(),
        size: original_payload.byte_size as i64,
        mime: "application/octet-stream".to_owned(),
        name: "Leased".to_owned(),
        ext: "bin".to_owned(),
        inlay_type: None,
        width: None,
        height: None,
        metadata: json!({}),
    };
    let first = store.commit_asset_alias(&original, 0).unwrap();
    let lease = store.acquire_revision(first.revision).unwrap();
    let replacement = AssetAlias {
        object_hash: Some(replacement_payload.content_hash.clone()),
        size: replacement_payload.byte_size as i64,
        ..original
    };
    store
        .commit_asset_alias(&replacement, first.revision)
        .unwrap();
    store
        .asset_object_catalog()
        .register(
            &[
                AssetObjectRegistration {
                    object_hash: original_payload.content_hash.clone(),
                    byte_size: original_payload.byte_size,
                },
                AssetObjectRegistration {
                    object_hash: replacement_payload.content_hash.clone(),
                    byte_size: replacement_payload.byte_size,
                },
                AssetObjectRegistration {
                    object_hash: collectable_payload.content_hash.clone(),
                    byte_size: collectable_payload.byte_size,
                },
            ],
            0,
        )
        .expect("register GC candidates");

    let leased = store.asset_gc_dry_run(16, None, 100, 10).unwrap().report;
    assert!(leased
        .marked_hashes
        .contains(&original_payload.content_hash));
    assert!(leased
        .marked_hashes
        .contains(&replacement_payload.content_hash));
    assert_eq!(
        leased.potential_delete_hashes,
        vec![collectable_payload.content_hash.clone()]
    );

    store.release_revision(&lease.lease).unwrap();
    let released = store.asset_gc_dry_run(16, None, 100, 10).unwrap().report;
    assert_eq!(
        released.marked_hashes,
        vec![replacement_payload.content_hash]
    );
    assert_eq!(
        released
            .potential_delete_hashes
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        [
            collectable_payload.content_hash,
            original_payload.content_hash
        ]
        .into_iter()
        .collect()
    );
    assert!(!released.deletion_enabled);
}

#[test]
fn asset_gc_dry_run_scans_plugin_references_without_retaining_unreferenced_objects() {
    use super::asset_object_catalog::AssetObjectRegistration;

    let directory = tempfile::tempdir().expect("create temporary directory");
    let mut store = PersistentStore::open(directory.path()).expect("open persistent store");
    let cas = crate::asset_repository::PayloadCas::new(directory.path()).unwrap();
    let first = cas.prepare_bytes(b"plugin-private-a").unwrap();
    let second = cas.prepare_bytes(b"plugin-private-b").unwrap();
    let generation = super::active_generation(&store.connection).expect("read generation");
    store
        .connection
        .execute(
            "INSERT INTO plugin_storage (generation, owner, storage_key, byte_size, ordinal, value)
             VALUES (?1, 'synthetic-plugin', 'opaque-plugin', 22, 0, ?2)",
            rusqlite::params![
                generation,
                serde_json::to_string(&json!({
                    "asset": first.content_hash
                }))
                .unwrap()
            ],
        )
        .unwrap();
    store
        .asset_object_catalog()
        .register(
            &[
                AssetObjectRegistration {
                    object_hash: first.content_hash.clone(),
                    byte_size: first.byte_size,
                },
                AssetObjectRegistration {
                    object_hash: second.content_hash.clone(),
                    byte_size: second.byte_size,
                },
            ],
            0,
        )
        .expect("register plugin liveness candidates");

    let first_page = store.asset_gc_dry_run(1, None, 100, 10).unwrap();
    let second_page = store
        .asset_gc_dry_run(1, first_page.next_cursor.as_deref(), 100, 10)
        .unwrap();
    let mut marked = first_page.report.marked_hashes.clone();
    marked.extend(second_page.report.marked_hashes.clone());
    marked.sort();
    marked.dedup();

    assert_eq!(
        marked,
        vec![first.content_hash.clone()]
    );
    let mut candidates = first_page.report.potential_delete_hashes.clone();
    candidates.extend(second_page.report.potential_delete_hashes.clone());
    assert_eq!(candidates, vec![second.content_hash]);
    assert!(first_page.next_cursor.is_some());
    assert!(second_page.next_cursor.is_none());
    for report in [first_page.report, second_page.report] {
        assert!(!report.blockers.contains(&"plugin-storage-opaque".to_owned()));
        assert!(!report.deletion_enabled);
    }
}

#[test]
fn asset_gc_dry_run_keeps_detached_export_roots_until_reader_release() {
    use super::asset_object_catalog::AssetObjectRegistration;

    let directory = tempfile::tempdir().expect("create temporary directory");
    let mut store = PersistentStore::open(directory.path()).expect("open persistent store");
    let cas = crate::asset_repository::PayloadCas::new(directory.path()).unwrap();
    let original_payload = cas.prepare_bytes(b"detached-original").unwrap();
    let replacement_payload = cas.prepare_bytes(b"detached-replacement").unwrap();
    let original = AssetAlias {
        key: "assets/detached.bin".to_owned(),
        object_hash: Some(original_payload.content_hash.clone()),
        kind: "asset".to_owned(),
        size: original_payload.byte_size as i64,
        mime: "application/octet-stream".to_owned(),
        name: "Detached".to_owned(),
        ext: "bin".to_owned(),
        inlay_type: None,
        width: None,
        height: None,
        metadata: json!({}),
    };
    let first = store.commit_asset_alias(&original, 0).unwrap();
    let mut prepared = store.prepare_risu_save_export(first.revision).unwrap();
    let replacement = AssetAlias {
        object_hash: Some(replacement_payload.content_hash.clone()),
        size: replacement_payload.byte_size as i64,
        ..original
    };
    store
        .commit_asset_alias(&replacement, first.revision)
        .unwrap();
    store
        .asset_object_catalog()
        .register(
            &[
                AssetObjectRegistration {
                    object_hash: original_payload.content_hash.clone(),
                    byte_size: original_payload.byte_size,
                },
                AssetObjectRegistration {
                    object_hash: replacement_payload.content_hash.clone(),
                    byte_size: replacement_payload.byte_size,
                },
            ],
            0,
        )
        .expect("register GC candidates");

    let detached = store.asset_gc_dry_run(16, None, 100, 10).unwrap().report;
    assert!(detached
        .marked_hashes
        .contains(&original_payload.content_hash));
    assert!(detached
        .marked_hashes
        .contains(&replacement_payload.content_hash));
    assert!(detached.potential_delete_hashes.is_empty());

    let reader = prepared.take_reader().expect("take detached reader");
    prepared.release(reader).expect("release detached reader");
    let released = store.asset_gc_dry_run(16, None, 100, 10).unwrap().report;
    assert_eq!(
        released.marked_hashes,
        vec![replacement_payload.content_hash]
    );
    assert_eq!(
        released.potential_delete_hashes,
        vec![original_payload.content_hash]
    );
}

#[test]
fn detached_export_registry_keeps_exact_v8_roots_until_reader_release() {
    let directory = tempfile::tempdir().expect("create temporary directory");
    let mut store = PersistentStore::open(directory.path()).expect("open persistent store");
    let generation = super::active_generation(&store.connection).expect("read generation");
    store
        .connection
        .execute(
            "INSERT INTO asset_aliases (
                generation, logical_key, object_hash, kind, size, mime, name, ext,
                inlay_type, width, height, metadata
             ) VALUES (?1, 'assets/detached.bin', ?2, 'asset', 1,
                'application/octet-stream', 'Detached', 'bin', NULL, NULL, NULL, '{}')",
            rusqlite::params![generation, "ab".repeat(32)],
        )
        .expect("insert detached asset alias root");
    store
        .connection
        .execute(
            "INSERT INTO asset_owner_heads (
                generation, owner_kind, owner_locator, present, manifest_hash, entry_count
             ) VALUES (?1, 'root-module-assets', '0', 1, ?2, 1)",
            rusqlite::params![generation, "cd".repeat(32)],
        )
        .expect("insert detached owner head root");

    let mut prepared = store
        .prepare_risu_save_export(0)
        .expect("prepare detached reader with v8 roots");
    let roots = store
        .active_readers
        .detached_asset_roots()
        .expect("read detached roots");
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].manifest_hashes, ["cd".repeat(32)].into());
    assert_eq!(roots[0].object_hashes, ["ab".repeat(32)].into());

    let reader = prepared.take_reader().expect("take detached reader");
    prepared.release(reader).expect("release detached reader");
    assert!(store
        .active_readers
        .detached_asset_roots()
        .expect("read roots after detached release")
        .is_empty());
}

#[test]
fn android_revision_reader_starts_writable_before_query_only_snapshot_pinning() {
    let flags = super::snapshot::revision_reader_open_flags_for_target(true);

    assert!(flags.contains(rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE));
    assert!(!flags.contains(rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY));
}

#[test]
fn checkpoints_accept_both_documented_modes() {
    let (_directory, store, _) = open_fixture();

    store
        .checkpoint(CheckpointMode::Passive)
        .expect("passive checkpoint");
    store
        .checkpoint(CheckpointMode::Truncate)
        .expect("truncate checkpoint");
}

#[test]
fn active_lease_rejects_truncate_and_final_release_truncates_the_wal() {
    let (directory, mut store, _) = open_fixture();
    store
        .connection
        .execute_batch("PRAGMA wal_autocheckpoint = 0")
        .expect("disable automatic checkpoints");
    let lease = store.acquire_revision(1).expect("acquire WAL reader");
    store
        .set_app_kv("device-backup-commit:after-lease", &json!(true))
        .expect("append WAL frame after lease");
    store
        .checkpoint(CheckpointMode::Passive)
        .expect("passive checkpoint with active lease");

    let started = std::time::Instant::now();
    let error = store
        .checkpoint(CheckpointMode::Truncate)
        .expect_err("truncate must reject an active lease");
    // Far above scheduler/disk jitter, comfortably below the 5s SQLite busy timeout:
    // proves the call rejected promptly instead of waiting out the busy handler.
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(matches!(
        error,
        StoreError::Store { message } if message.contains("active read lease")
    ));

    store
        .release_revision(&lease.lease)
        .expect("release final lease and truncate WAL");
    let database_path = directory.path().join("persistent/persistent.sqlite");
    let wal_path = PathBuf::from(format!("{}-wal", database_path.display()));
    assert_eq!(fs::metadata(wal_path).expect("read final WAL").len(), 0);
}

#[test]
fn renderer_session_release_closes_every_attached_lease_and_truncates_the_wal() {
    let (directory, mut store, _) = open_fixture();
    store
        .connection
        .execute_batch("PRAGMA wal_autocheckpoint = 0")
        .expect("disable automatic checkpoints");
    let first = store.acquire_revision(1).expect("acquire first reader");
    let second = store.acquire_revision(1).expect("acquire second reader");
    store
        .set_app_kv("device-backup-commit:after-session-readers", &json!(true))
        .expect("append WAL frame after readers");

    store
        .release_all_revision_leases()
        .expect("release renderer session readers");

    assert_eq!(store.active_readers.active_count(), 0);
    assert!(matches!(
        store.read_root(Some(&first.lease)),
        Err(StoreError::SnapshotReleased)
    ));
    assert!(matches!(
        store.read_root(Some(&second.lease)),
        Err(StoreError::SnapshotReleased)
    ));
    let database_path = directory.path().join("persistent/persistent.sqlite");
    let wal_path = PathBuf::from(format!("{}-wal", database_path.display()));
    assert_eq!(fs::metadata(wal_path).expect("read final WAL").len(), 0);
}

#[test]
fn attached_export_can_leave_the_store_lock_and_restore_the_same_lease() {
    let (_directory, mut store, _) = open_fixture();
    let lease = store.acquire_revision(1).expect("acquire export reader");

    let prepared = store
        .detach_risu_save_export(&lease.lease)
        .expect("detach export reader");
    assert!(matches!(
        store.read_root(Some(&lease.lease)),
        Err(StoreError::SnapshotReleased)
    ));
    assert_eq!(prepared.reader().unwrap().target.revision, 1);
    let exported = prepared
        .create_attached_export(false, None)
        .expect("export through detached attached reader");

    store
        .reattach_risu_save_export(prepared)
        .expect("reattach export reader");
    assert_eq!(store.read_root(Some(&lease.lease)).unwrap().revision, 1);
    store
        .cleanup_risu_save_export(Path::new(&exported.path))
        .unwrap();
    store.release_revision(&lease.lease).unwrap();
}

#[test]
fn detached_export_reader_rejects_truncate_until_it_is_released() {
    let (directory, mut store, _) = open_fixture();
    store
        .connection
        .execute_batch("PRAGMA wal_autocheckpoint = 0")
        .expect("disable automatic checkpoints");
    let mut prepared = store
        .prepare_risu_save_export(1)
        .expect("prepare detached export reader");
    store
        .set_app_kv("device-backup-commit:after-detached-export", &json!(true))
        .expect("append WAL frame after detached reader");

    let started = std::time::Instant::now();
    let error = store
        .checkpoint(CheckpointMode::Truncate)
        .expect_err("truncate must reject a detached export reader");
    // Far above scheduler/disk jitter, comfortably below the 5s SQLite busy timeout.
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(matches!(
        error,
        StoreError::Store { message } if message.contains("active read lease")
    ));

    let reader = prepared.take_reader().expect("take detached export reader");
    prepared
        .release(reader)
        .expect("release detached export reader");
    let database_path = directory.path().join("persistent/persistent.sqlite");
    let wal_path = PathBuf::from(format!("{}-wal", database_path.display()));
    assert_eq!(fs::metadata(wal_path).expect("read final WAL").len(), 0);
}

#[test]
fn renderer_session_release_does_not_close_a_detached_native_job_reader() {
    let (_directory, mut store, _) = open_fixture();
    let mut prepared = store
        .prepare_risu_save_export(1)
        .expect("prepare detached native job reader");
    let attached = store.acquire_revision(1).expect("acquire renderer reader");

    store
        .release_all_revision_leases()
        .expect("release renderer readers");

    assert_eq!(store.active_readers.active_count(), 1);
    assert!(matches!(
        store.read_root(Some(&attached.lease)),
        Err(StoreError::SnapshotReleased)
    ));
    assert_eq!(prepared.reader().unwrap().target.revision, 1);
    prepared.release_reader().unwrap();
    assert_eq!(store.active_readers.active_count(), 0);
}

#[test]
fn detached_export_release_stays_prompt_while_an_attached_reader_remains() {
    let (directory, mut store, _) = open_fixture();
    store
        .connection
        .execute_batch("PRAGMA wal_autocheckpoint = 0")
        .expect("disable automatic checkpoints");
    let attached = store.acquire_revision(1).expect("acquire attached reader");
    let mut prepared = store
        .prepare_risu_save_export(1)
        .expect("prepare detached export reader");
    store
        .set_app_kv("device-backup-commit:after-two-readers", &json!(true))
        .expect("append WAL frame after both readers");

    let reader = prepared.take_reader().expect("take detached export reader");
    let started = std::time::Instant::now();
    prepared
        .release(reader)
        .expect("release detached reader with attached reader remaining");
    // Far above scheduler/disk jitter, comfortably below the 5s SQLite busy timeout.
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        store
            .read_root(Some(&attached.lease))
            .expect("attached reader remains pinned")
            .revision,
        1
    );
    let database_path = directory.path().join("persistent/persistent.sqlite");
    let wal_path = PathBuf::from(format!("{}-wal", database_path.display()));
    assert!(fs::metadata(&wal_path).expect("read pinned WAL").len() > 0);

    store
        .release_revision(&attached.lease)
        .expect("release final attached reader");
    assert_eq!(fs::metadata(wal_path).expect("read final WAL").len(), 0);
}

#[test]
fn dropping_store_with_active_lease_reopens_latest_state_and_truncates_recovered_wal() {
    let (directory, mut store, _) = open_fixture();
    store
        .connection
        .execute_batch("PRAGMA wal_autocheckpoint = 0")
        .expect("disable automatic checkpoints");
    let lease = store.acquire_revision(1).expect("acquire WAL reader");
    store
        .commit(&WorkingSetCommit {
            root: Some(json!({ "username": "Writer survives lease drop" })),
            ..empty_working_set_commit(1)
        })
        .expect("append writer state while lease is active");
    let database_path = directory.path().join("persistent/persistent.sqlite");
    let wal_path = PathBuf::from(format!("{}-wal", database_path.display()));
    assert!(fs::metadata(&wal_path).expect("read pinned WAL").len() > 0);
    drop(store);

    let reopened = PersistentStore::open(directory.path()).expect("reopen after active lease drop");
    assert_eq!(reopened.revision().expect("read reopened revision"), 2);
    assert_eq!(
        reopened
            .read_root(None)
            .expect("read reopened writer state")
            .value["username"],
        "Writer survives lease drop"
    );
    assert!(matches!(
        reopened.read_root(Some(&lease.lease)),
        Err(StoreError::SnapshotReleased)
    ));
    assert_eq!(
        fs::metadata(&wal_path).expect("read recovered WAL").len(),
        0
    );
}

#[test]
fn ninth_snapshot_removes_the_oldest_and_leaves_eight() {
    let (_directory, mut store, _) = open_fixture();
    let mut ids = Vec::new();
    for index in 0..9 {
        ids.push(
            store
                .snapshot_create(&format!("rotation-{index}"))
                .unwrap()
                .id,
        );
        thread::sleep(Duration::from_millis(10));
    }
    let listed = store.snapshot_list().unwrap();
    assert_eq!(listed.len(), 8);
    assert!(!listed.iter().any(|s| s.id == ids[0]));
    for id in &ids[1..] {
        let (_capture, db) = reconstruct_snapshot(&store, id);
        assert_eq!(
            db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
    }
}

#[test]
fn byte_rotation_keeps_newest_and_durable_stage_owns_rotated_source() {
    let (_directory, mut store, database)=open_fixture();
    let target=store.snapshot_create("target").unwrap();
    let stage=store.snapshot_restore_stage(&target.id,"rotated-stage").unwrap();
    store.snapshot_create("middle").unwrap();
    let latest=store.snapshot_create("latest").unwrap();
    let mut archive=Archive::open(&store.snapshots_dir).unwrap();
    archive.rotate(0,&latest.id).unwrap();
    let ids:Vec<_>=archive.list().unwrap().into_iter().map(|s|s.id).collect();
    assert_eq!(ids,vec![latest.id]);
    let authority=store.lww_binding_authority().unwrap();
    store.snapshot_restore_activate(&stage.staging_id,1,authority).unwrap();
    assert_eq!(store.materialize(None).unwrap(),database);
}

#[test]
fn retention_budget_keeps_the_documented_floor_and_logical_database_multiplier() {
    const MIB: u64 = 1024 * 1024;
    assert_eq!(snapshot::byte_budget(0), 512 * MIB);
    assert_eq!(snapshot::byte_budget(128 * MIB), 512 * MIB);
    assert_eq!(snapshot::byte_budget(140 * MIB), 560 * MIB);
    assert_eq!(snapshot::byte_budget(u64::MAX), u64::MAX);
}

#[test]
#[ignore = "actual-schema synthetic storage measurements"]
fn snapshot_deduplication_actual_schema_measurements() {
    use std::collections::HashMap;
    use std::time::Instant;
    for scenario in [
        "unchanged",
        "append",
        "grow",
        "delete",
        "replace-generation",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let mut database = fixture();
        let mut seed = 614_u64;
        let messages: Vec<_> = (0..4096).map(|index| {
            let data: String = (0..1024).map(|_| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                char::from(b'a' + ((seed >> 32) % 26) as u8)
            }).collect();
            json!({"role":"char", "data":data, "time":index, "chatId":format!("synthetic-{index}")})
        }).collect();
        database["characters"][0]["chats"][0]["message"] = Value::Array(messages);
        let import = |store: &mut PersistentStore, value: &Value| {
            let revision = store.revision().unwrap();
            let staging = store.replace_begin().unwrap();
            store
                .replace_put_root(&staging.staging_id, &staged_root(value))
                .unwrap();
            store
                .replace_put_presets(&staging.staging_id, value["botPresets"].as_array().unwrap())
                .unwrap();
            store
                .replace_add_characters(
                    &staging.staging_id,
                    value["characters"].as_array().unwrap(),
                )
                .unwrap();
            store
                .replace_commit(&staging.staging_id, Some(revision))
                .unwrap();
        };
        import(&mut store, &database);
        let mut expected = HashMap::new();
        let mut create_ms = Vec::new();
        let mut restore_ms = Vec::new();
        for iteration in 0..8 {
            if iteration > 0 && scenario != "unchanged" {
                if scenario == "replace-generation" {
                    database["username"] = json!(format!("synthetic revision {iteration}"));
                    import(&mut store, &database);
                } else {
                    let character_id = database["characters"][0]["chaId"].as_str().unwrap();
                    let chat_id = database["characters"][0]["chats"][0]["id"]
                        .as_str()
                        .unwrap();
                    let generation = active_generation(&store.connection).unwrap();
                    match scenario {
                        "append" => {
                            let index: i64 = store.connection.query_row("SELECT coalesce(max(message_index),-1)+1 FROM messages WHERE generation=?1 AND character_id=?2 AND conversation_id=?3", params![generation,character_id,chat_id], |r| r.get(0)).unwrap();
                            store.connection.execute("INSERT INTO messages(generation,character_id,conversation_id,message_index,message_id,value) VALUES(?1,?2,?3,?4,?5,?6)",
                                params![generation,character_id,chat_id,index,format!("append-{iteration}"),json!({"role":"char","data":"synthetic appended message"}).to_string()]).unwrap();
                        }
                        "grow" => {
                            store.connection.execute("UPDATE messages SET value=json_set(value,'$.data',json_extract(value,'$.data')||?4) WHERE generation=?1 AND character_id=?2 AND conversation_id=?3 AND message_index=2000",params![generation,character_id,chat_id,"x".repeat(120)]).unwrap();
                        }
                        "delete" => {
                            store.connection.execute("DELETE FROM messages WHERE generation=?1 AND character_id=?2 AND conversation_id=?3 AND message_index>=?4 AND message_index<?5",params![generation,character_id,chat_id,(iteration-1)*100,iteration*100]).unwrap();
                        }
                        _ => unreachable!(),
                    }
                    store.connection.execute("UPDATE conversations SET message_count=(SELECT count(*) FROM messages m WHERE m.generation=conversations.generation AND m.character_id=conversations.character_id AND m.conversation_id=conversations.conversation_id) WHERE generation=?1 AND character_id=?2 AND conversation_id=?3",params![generation,character_id,chat_id]).unwrap();
                    store
                        .commit(&WorkingSetCommit {
                            root: Some(json!({"username":format!("synthetic {iteration}")})),
                            ..empty_working_set_commit(store.revision().unwrap())
                        })
                        .unwrap();
                }
            }
            expected.insert(
                store.revision().unwrap(),
                Sha256::digest(serde_json::to_vec(&store.materialize(None).unwrap()).unwrap())
                    .to_vec(),
            );
            let start = Instant::now();
            store.snapshot_create(scenario).unwrap();
            create_ms.push(start.elapsed().as_millis());
        }
        let snapshots = store.snapshot_list().unwrap();
        let logical: u64 = snapshots.iter().map(|s| s.bytes).sum();
        let stored = store.storage_stats().unwrap().snapshot_bytes;
        let unique: i64 = Connection::open(store.snapshots_dir.join("snapshots.sqlite"))
            .unwrap()
            .query_row("SELECT sum(length(data)) FROM chunks", [], |r| r.get(0))
            .unwrap();
        for entry in &snapshots {
            let start = Instant::now();
            let (_capture, connection) = reconstruct_snapshot(&store, &entry.id);
            let actual = super::super::query::materialize(&connection, None).unwrap();
            assert_eq!(
                Sha256::digest(serde_json::to_vec(&actual).unwrap()).to_vec(),
                expected[&current_revision(&connection).unwrap()]
            );
            restore_ms.push(start.elapsed().as_millis());
        }
        assert!(stored < logical, "archive must save space on {scenario}");
        if scenario == "unchanged" {
            assert!(unique as u64 <= snapshots[0].bytes);
        }
        store
            .snapshot_delete(&snapshots.last().unwrap().id)
            .unwrap();
        for entry in store.snapshot_list().unwrap() {
            let (_capture, connection) = reconstruct_snapshot(&store, &entry.id);
            let actual = super::super::query::materialize(&connection, None).unwrap();
            assert_eq!(
                Sha256::digest(serde_json::to_vec(&actual).unwrap()).to_vec(),
                expected[&current_revision(&connection).unwrap()]
            );
        }
        eprintln!(
            "snapshot-dedup-measurement {}",
            json!({"scenario":scenario,"snapshots":snapshots.len(),"logicalBytes":logical,"archiveBytes":stored,"uniquePayloadBytes":unique,"createMs":create_ms,"restoreAndMaterializeMs":restore_ms})
        );
    }
}

#[test]
fn live_snapshot_restore_preserves_old_pinned_revision_until_release() {
    let (_directory, mut store, database)=open_fixture();
    let lease=store.acquire_revision(1).unwrap();
    let snapshot=store.snapshot_create("lease-restore").unwrap();
    store.commit(&WorkingSetCommit {root:Some(json!({"username":"Writer after restore snapshot"})),..empty_working_set_commit(1)}).unwrap();
    let stage=store.snapshot_restore_stage(&snapshot.id,"lease-restore").unwrap();
    let authority=store.lww_binding_authority().unwrap();
    store.snapshot_restore_activate(&stage.staging_id,2,authority).unwrap();
    assert_eq!(store.materialize(None).unwrap(),database);
    assert!(store.read_root(Some(&lease.lease)).is_ok());
    store.release_revision(&lease.lease).unwrap();
    assert!(matches!(store.read_root(Some(&lease.lease)),Err(StoreError::SnapshotReleased)));
}

#[test]
fn live_restore_creates_no_extra_snapshot_and_periodic_snapshots_remain() {
    let (_directory, mut store, database)=open_fixture();
    let target=store.snapshot_create("restore-target").unwrap();
    store.snapshot_create("periodic").unwrap();
    store.commit(&WorkingSetCommit {root:Some(json!({"username":"Current before restore"})),..empty_working_set_commit(1)}).unwrap();
    let before=store.snapshot_list().unwrap().len();
    let stage=store.snapshot_restore_stage(&target.id,"no-pre-restore").unwrap();
    let authority=store.lww_binding_authority().unwrap();
    store.snapshot_restore_activate(&stage.staging_id,2,authority).unwrap();
    assert_eq!(store.materialize(None).unwrap(),database);
    let snapshots=store.snapshot_list().unwrap();
    assert_eq!(snapshots.len(),before);
    assert!(snapshots.iter().any(|s|s.reason=="periodic"));
    assert!(snapshots.iter().all(|s|s.reason!="pre-restore"));
}

#[test]
fn invalid_snapshot_candidates_preserve_current_data_and_operation_state() {
    for wrong_version in [false,true] {
        let (directory,mut store,_)=open_fixture();
        store.commit(&WorkingSetCommit {root:Some(json!({"username":"Current protected data"})),..empty_working_set_commit(1)}).unwrap();
        let expected=store.materialize(None).unwrap();
        let clock=serde_json::to_value(store.lww_clock_state().unwrap()).unwrap();
        let id={
            let mut archive=Archive::open(&store.snapshots_dir).unwrap();
            let scratch=archive.scratch().unwrap();
            if wrong_version {Connection::open(&scratch.path).unwrap().execute_batch("PRAGMA user_version=17;").unwrap();}
            else {fs::write(&scratch.path,b"not a sqlite database").unwrap();}
            archive.insert(&scratch.path,1,"invalid",Default::default()).unwrap().id
        };
        assert!(store.snapshot_restore_stage(&id,"invalid-candidate").is_err());
        assert_eq!(store.materialize(None).unwrap(),expected);
        assert_eq!(serde_json::to_value(store.lww_clock_state().unwrap()).unwrap(),clock);
        drop(store);
        let reopened=PersistentStore::open(directory.path()).unwrap();
        assert_eq!(reopened.materialize(None).unwrap(),expected);
    }
}

#[test]
fn exact_schema_snapshot_with_forged_message_cache_is_rejected_before_activation() {
    let (_directory, mut store, _) = open_fixture();
    for table in ["messages", "message_page_manifests", "message_page_indexes", "message_page_proofs", "message_page_verified_objects"] {
        let count:i64=store.connection.query_row(&format!("SELECT COUNT(*) FROM {table}"),[],|r|r.get(0)).unwrap();
        assert!(count>0,"fixture must contain actual {table} before forgery");
    }
    store.connection.execute("UPDATE messages SET canonical_hash=?1,canonical_size=1",["0".repeat(64)]).unwrap();
    let snapshot = store.snapshot_create("forged-cache").unwrap();
    store.commit(&WorkingSetCommit {
        root: Some(json!({"username":"keep-current"})),
        ..empty_working_set_commit(1)
    }).unwrap();
    let revision = store.revision().unwrap();
    let clock = serde_json::to_value(store.lww_clock_state().unwrap()).unwrap();
    let generation = active_generation(&store.connection).unwrap();
    let error = store.snapshot_restore_stage(&snapshot.id,"forged-snapshot").unwrap_err();
    assert!(error.to_string().contains("identity mismatch"));
    assert_eq!(store.read_root(None).unwrap().value["username"],"keep-current");
    assert_eq!(store.revision().unwrap(),revision);
    assert_eq!(active_generation(&store.connection).unwrap(),generation);
    assert_eq!(serde_json::to_value(store.lww_clock_state().unwrap()).unwrap(),clock);
}

#[test]
fn exact_schema_snapshot_with_forged_page_index_is_rejected_before_activation() {
    let (_directory, mut store, _) = open_fixture();
    let proof_count:i64=store.connection.query_row("SELECT COUNT(*) FROM message_page_proofs",[],|r|r.get(0)).unwrap();
    assert!(proof_count>0);
    store.connection.execute("UPDATE message_page_indexes SET page_start=page_start+1",[]).unwrap();
    let snapshot=store.snapshot_create("forged-index").unwrap();
    let revision=store.revision().unwrap();
    let generation=active_generation(&store.connection).unwrap();
    let clock=serde_json::to_value(store.lww_clock_state().unwrap()).unwrap();
    let library=store.materialize(None).unwrap();
    assert!(store.snapshot_restore_stage(&snapshot.id,"forged-index-stage").is_err());
    assert_eq!(store.revision().unwrap(),revision);
    assert_eq!(active_generation(&store.connection).unwrap(),generation);
    assert_eq!(serde_json::to_value(store.lww_clock_state().unwrap()).unwrap(),clock);
    assert_eq!(store.materialize(None).unwrap(),library);
}

#[test]
fn live_snapshot_restore_recertifies_then_reopens_without_replaying() {
    let (directory, mut store, _) = open_fixture();
    let snapshot = store.snapshot_create("manual").unwrap();
    let stage = store.snapshot_restore_stage(&snapshot.id,"live-snapshot").unwrap();
    let writer = store.lww_clock_state().unwrap().writer_id;
    let authority = store.lww_binding_authority().unwrap();
    let revision = store.revision().unwrap();
    let activated = store.snapshot_restore_activate(&stage.staging_id,revision,authority.clone()).unwrap();
    assert!(activated.revision>revision);
    assert_eq!(store.lww_clock_state().unwrap().writer_id,writer);
    assert_eq!(store.lww_binding_authority().unwrap(),authority);
    let verified:i64 = store.connection.query_row("SELECT COUNT(*) FROM message_page_verified_objects",[],|r|r.get(0)).unwrap();
    assert!(verified>0);
    drop(store);
    let mut reopened = PersistentStore::open(directory.path()).unwrap();
    let replay = reopened.snapshot_restore_activate(&stage.staging_id,revision,authority).unwrap();
    assert_eq!(replay.revision,activated.revision);
    assert_eq!(reopened.revision().unwrap(),activated.revision);
    assert_eq!(reopened.lww_clock_state().unwrap().writer_id,writer);
    assert!(reopened.snapshot_list().unwrap().iter().all(|entry|entry.reason!="pre-restore"));
}
