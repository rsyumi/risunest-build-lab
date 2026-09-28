use crate::external_storage::content_store::ObjectSource;
use super::*;
use crate::{
    asset_repository::owner_manifest_codec::{encode_owner_manifest, OwnerManifestEntry},
    logical_records::{
        encode_asset_alias_metadata, encode_logical_record, encode_logical_record_key,
        encode_message_page, LogicalAssetAliasMetadata, LogicalOwnerHead, LogicalOwnerLocator,
        LogicalRecordEnvelope, LogicalRecordLocator,
    },
    persistent_store::{external_storage_state, sync_selection},
};
use risunest_external_storage_format::format::{fingerprint, library_fingerprint_domain};
use serde_json::json;
use std::{collections::BTreeMap, fs, path::Path};



fn open_store() -> (tempfile::TempDir, PersistentStore) {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let stage = store.replace_begin().unwrap();
    store
        .replace_put_root(
            &stage.staging_id,
            &json!({
                "marker": "local",
                "statics": { "messages": ["local-only"] }
            }),
        )
        .unwrap();
    store
        .replace_add_characters(
            &stage.staging_id,
            &[json!({
                "chaId": "character",
                "name": "Local character",
                "chatPage": 7,
                "lastInteraction": 700,
                "chats": []
            })],
        )
        .unwrap();
    store.replace_commit(&stage.staging_id, Some(0)).unwrap();
    (directory, store)
}

fn write_record(
    root: &Path,
    locator: LogicalRecordLocator,
    envelope: LogicalRecordEnvelope,
) -> (ExternalSnapshotRecord, [u8; 32]) {
    let key = encode_logical_record_key(&locator).unwrap();
    let encoded = encode_logical_record(&envelope).unwrap();
    let path = root.join(format!("record-{}", encoded.hash));
    fs::write(&path, &encoded.bytes).unwrap();
    (
        ExternalSnapshotRecord {
            key,
            content_hash: encoded.hash.clone(),
            byte_length: encoded.size,
            source: ObjectSource::File(path),
        },
        hex::decode(encoded.hash).unwrap().try_into().unwrap(),
    )
}

fn write_object(root: &Path, bytes: &[u8]) -> ExternalSnapshotObject {
    let object =
        risunest_external_storage_format::logical_records::encoded_object(bytes.to_vec()).unwrap();
    let path = root.join(format!("object-{}", object.hash));
    fs::write(&path, bytes).unwrap();
    ExternalSnapshotObject {
        content_hash: object.hash,
        byte_length: object.size,
        source: ObjectSource::File(path),
    }
}

fn application<'a>(
    root: &'a Path,
    scope_id: &'a [u8; 32],
    fingerprint: &'a [u8; 32],
    revision: i64,
) -> ExternalSnapshotApplication<'a> {
    ExternalSnapshotApplication {
        expected_revision: revision,
        staging_root: root,
        scope_id,
        fingerprint,
        probe: &crate::local_backup::NeverCancelled,
    }
}

fn root_record(root: &Path, marker: &str) -> (ExternalSnapshotRecord, [u8; 32]) {
    write_record(
        root,
        LogicalRecordLocator::Root,
        LogicalRecordEnvelope::Root {
            value: json!({
                "marker": marker,
                "statics": { "messages": ["remote-device-value"] }
            }),
            owner_heads: vec![],
        },
    )
}

fn stage_count(store: &PersistentStore) -> i64 {
    store
        .connection
        .query_row(
            "SELECT count(*) FROM root WHERE generation LIKE 'staging-%'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn external_snapshot_stages_streamed_records_and_preserves_local_view_fields() {
    let (directory, mut store) = open_store();
    let staging = directory.path().join("download");
    fs::create_dir(&staging).unwrap();
    let page = encode_message_page(&[json!({"chatId":"message","data":"remote"})]).unwrap();
    let page_object = write_object(&staging, &page.bytes);
    let owner_payload = write_object(&staging, b"synthetic owner payload");
    let owner_manifest_bytes = encode_owner_manifest(&[OwnerManifestEntry {
        tuple: ["asset".into(), "owner.bin".into(), "binary".into()],
        payload_hash: Some(
            hex::decode(&owner_payload.content_hash)
                .unwrap()
                .try_into()
                .unwrap(),
        ),
    }])
    .unwrap();
    let owner_manifest = write_object(&staging, &owner_manifest_bytes);
    let (conversation, conversation_hash) = write_record(
        &staging,
        LogicalRecordLocator::Conversation {
            character_id: "character".into(),
            conversation_id: "conversation".into(),
        },
        LogicalRecordEnvelope::Conversation {
            configured_index: 0,
            recent_at: 20,
            detail: json!({"id":"conversation","name":"Remote conversation"}),
            message_page_hashes: vec![page.hash],
        },
    );
    let (root, root_hash) = write_record(
        &staging,
        LogicalRecordLocator::Root,
        LogicalRecordEnvelope::Root {
            value: json!({
                "marker": "remote",
                "statics": { "messages": ["remote-device-value"] },
                "modules": [{"name":"Remote module"}]
            }),
            owner_heads: vec![LogicalOwnerHead::present(
                LogicalOwnerLocator::RootModule { index: 0 },
                owner_manifest.content_hash.clone(),
                1,
                1,
            )
            .unwrap()],
        },
    );
    let (character, character_hash) = write_record(
        &staging,
        LogicalRecordLocator::Character {
            character_id: "character".into(),
        },
        LogicalRecordEnvelope::Character {
            configured_index: 0,
            detail: json!({
                "chaId":"character",
                "name":"Remote character",
                "chatPage":1,
                "lastInteraction":10
            }),
            owner_heads: vec![LogicalOwnerHead::absent(
                LogicalOwnerLocator::CharacterAdditional {
                    character_id: "character".into(),
                },
            )],
        },
    );
    let records = vec![conversation, root, character];
    let mut hashes = BTreeMap::new();
    for (record, hash) in records
        .iter()
        .zip([conversation_hash, root_hash, character_hash])
    {
        hashes.insert(record.key.clone(), hash);
    }
    let scope_id = library_fingerprint_domain();
    let fingerprint = fingerprint(&scope_id, &hashes);
    let prepared = store
        .prepare_external_snapshot_application(
            &application(&staging, &scope_id, &fingerprint, 1),
            records.into_iter().map(Ok),
            [Ok(page_object), Ok(owner_manifest), Ok(owner_payload)],
        )
        .unwrap();

    let staged = store.materialize_staging(&prepared.staging_id).unwrap();
    assert_eq!(staged["marker"], "remote");
    assert_eq!(staged["statics"]["messages"], json!(["local-only"]));
    assert_eq!(
        staged["modules"][0]["assets"],
        json!([["asset", "owner.bin", "binary"]])
    );
    assert_eq!(staged["characters"][0]["name"], "Remote character");
    assert_eq!(staged["characters"][0]["chatPage"], 7);
    assert_eq!(staged["characters"][0]["lastInteraction"], 700);
    assert_eq!(
        staged["characters"][0]["chats"][0]["message"][0]["data"],
        "remote"
    );
    assert_eq!(store.finish_prepared_replace(prepared).unwrap().revision, 2);
}

#[test]
fn external_snapshot_restores_archived_character_state_and_payload_references() {
    let (directory, mut store) = open_store();
    let staging = directory.path().join("archive-download");
    fs::create_dir(&staging).unwrap();
    let archive_object = write_object(&staging, b"synthetic archive payload");
    let asset_object = write_object(&staging, b"synthetic archived asset");
    let (root, root_hash) = root_record(&staging, "remote");
    let (character, character_hash) = write_record(
        &staging,
        LogicalRecordLocator::Character {
            character_id: "character".into(),
        },
        LogicalRecordEnvelope::ArchivedCharacter {
            configured_index: 0,
            recent_at: 700,
            trashed: false,
            name: "Archived character".into(),
            image: Some("archive-asset".into()),
            character_type: "character".into(),
            creator_notes: None,
            trash_time: None,
            archive_object_hash: archive_object.content_hash.clone(),
            archive_object_size: archive_object.byte_length,
            archived_at: 10,
            conversation_count: 1,
            message_count: 2,
            asset_hashes: vec![asset_object.content_hash.clone()],
            owner_heads: vec![],
        },
    );
    let records = vec![root, character];
    let hashes = BTreeMap::from([
        (records[0].key.clone(), root_hash),
        (records[1].key.clone(), character_hash),
    ]);
    let scope_id = library_fingerprint_domain();
    let content_fingerprint = fingerprint(&scope_id, &hashes);
    let prepared = store
        .prepare_external_snapshot_application(
            &application(&staging, &scope_id, &content_fingerprint, 1),
            records.into_iter().map(Ok),
            [Ok(archive_object), Ok(asset_object)],
        )
        .expect("stage archived snapshot");
    let archived = super::super::archive::read_archived_object(
        &store.connection,
        prepared.external_staging_id(),
        "character",
    )
    .unwrap()
    .expect("staged archive metadata");
    assert_eq!(archived.archived_at, 10);
    assert_eq!(archived.conversation_count, 1);
    assert_eq!(archived.message_count, 2);
    assert_eq!(archived.asset_hashes.len(), 1);
    store.finish_prepared_replace(prepared).unwrap();
    assert!(store.read_character("character", None).is_err());
}

#[test]
fn external_snapshot_stage_feeds_atomic_normal_receive_activation() {
    let (directory, mut store) = open_store();
    let staging = directory.path().join("download");
    fs::create_dir(&staging).unwrap();
    let epoch = sync_selection::read(&store.connection).unwrap().epoch;
    let tx = store.connection.transaction().unwrap();
    sync_selection::select(
        &tx,
        &epoch,
        &sync_selection::SyncTarget::External("connection".into()),
    )
    .unwrap();
    tx.commit().unwrap();
    let identity = sync_selection::identity(&store.connection).unwrap();
    let tx = store.connection.transaction().unwrap();
    external_storage_state::prepare_receive(
        &tx,
        &external_storage_state::ReceiveIntent {
            job_id: "receive",
            connection_id: "connection",
            repository_id: "repository",
            snapshot_id: "snapshot",
            commit_id: "commit",
            authenticated_head: "head-token",
            identity: &identity,
        },
    )
    .unwrap();
    tx.commit().unwrap();

    let (root, root_hash) = root_record(&staging, "remote-sync");
    let hashes = BTreeMap::from([(root.key.clone(), root_hash)]);
    let scope_id = library_fingerprint_domain();
    let fingerprint = fingerprint(&scope_id, &hashes);
    let prepared = store
        .prepare_external_snapshot_application(
            &application(&staging, &scope_id, &fingerprint, identity.revision),
            [Ok(root)],
            std::iter::empty(),
        )
        .unwrap();
    store.finish_external_receive(prepared, "receive", &Default::default()).unwrap();

    let after = sync_selection::identity(&store.connection).unwrap();
    assert_eq!(after.revision, identity.revision + 1);
    assert_eq!(after.library_epoch, identity.library_epoch);
    assert!(
        !sync_selection::read(&store.connection)
            .unwrap()
            .decision_required
    );
    let (snapshot, phase): (String, String) = store
        .connection
        .query_row(
            "SELECT snapshot_id,(SELECT phase FROM external_storage_jobs WHERE id='receive') FROM external_storage_bases",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (snapshot.as_str(), phase.as_str()),
        ("snapshot", "complete")
    );
}

/// A local body that was changed under its own name is not silently accepted
/// and not silently replaced: the content store publishes without replacement,
/// so the apply stops at the damaged file and the live library stays where it
/// was.
#[test]
fn a_locally_corrupt_object_stops_the_apply_instead_of_activating() {
    for corrupt in [false, true] {
        let (directory, mut store) = open_store();
        let staging = directory.path().join("download");
        fs::create_dir(&staging).unwrap();
        let object = write_object(&staging, b"asset-body");
        let cas = crate::asset_repository::PayloadCas::new(directory.path()).unwrap();
        let held = cas.prepare_bytes(b"asset-body").unwrap();
        assert_eq!(held.content_hash, object.content_hash);
        if corrupt {
            // The same length under the same name, which is what the catalog's
            // own length check cannot tell apart.
            fs::write(
                cas.object_path(&held.content_hash).unwrap().unwrap(),
                b"asset-bodY",
            )
            .unwrap();
        }
        let (root, root_hash) = root_record(&staging, "remote");
        let hashes = BTreeMap::from([(root.key.clone(), root_hash)]);
        let scope_id = library_fingerprint_domain();
        let fingerprint = fingerprint(&scope_id, &hashes);
        let applied = store.prepare_external_snapshot_application(
            &application(&staging, &scope_id, &fingerprint, 1),
            [Ok(root)],
            [Ok(object)],
        );
        if !corrupt {
            let prepared = applied.unwrap();
            assert_eq!(store.finish_prepared_replace(prepared).unwrap().revision, 2);
            continue;
        }
        let error = applied.err().expect("a damaged body must not apply").to_string();
        assert!(error.contains("collision or corruption"), "{error}");
        assert_eq!(stage_count(&store), 0);
        assert_eq!(store.revision().unwrap(), 1);
        assert_eq!(store.materialize(None).unwrap()["marker"], "local");
        assert_eq!(
            fs::read(cas.object_path(&held.content_hash).unwrap().unwrap()).unwrap(),
            b"asset-bodY",
        );
    }
}

/// A record body now arrives through the content store rather than as a file
/// confined to the staging directory, so the store is where the link rejection
/// has to hold. A body replaced by a link to content outside is not an input,
/// whatever that content is.
#[test]
fn a_linked_record_body_in_the_content_store_is_not_a_snapshot_input() {
    let (directory, mut store) = open_store();
    let staging = directory.path().join("download");
    fs::create_dir(&staging).unwrap();
    let outside = directory.path().join("outside");
    fs::create_dir(&outside).unwrap();
    // Over the small-object threshold, so the store publishes it as a file the
    // fixture can replace.
    let (record, root_hash) = root_record(&staging, &"x".repeat(70 * 1024));
    let bytes = fs::read(record.source.file().unwrap()).unwrap();
    let mut content = crate::external_storage::content_store::ContentStore::open(
        &staging.join("external-storage"),
    )
    .unwrap();
    content.put(&record.content_hash, &bytes).unwrap();
    content.commit().unwrap();
    let held = content
        .file_path(&record.content_hash)
        .unwrap()
        .expect("a body this size is published as a file");
    let target = outside.join("target.bin");
    fs::write(&target, &bytes).unwrap();
    fs::remove_file(&held).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &held).unwrap();
    #[cfg(windows)]
    if let Err(error) = std::os::windows::fs::symlink_file(&target, &held) {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            return;
        }
        panic!("create reparse fixture: {error}");
    }

    let record = ExternalSnapshotRecord {
        source: ObjectSource::Captured(record.content_hash.clone()),
        ..record
    };
    let hashes = BTreeMap::from([(record.key.clone(), root_hash)]);
    let scope_id = library_fingerprint_domain();
    let fingerprint = fingerprint(&scope_id, &hashes);
    let error = store
        .prepare_external_snapshot_application(
            &application(&staging, &scope_id, &fingerprint, 1),
            [Ok(record)],
            std::iter::empty(),
        )
        .err()
        .expect("a linked body must not be an input")
        .to_string();
    assert!(error.contains("link"), "{error}");
    assert_eq!(stage_count(&store), 0);
    assert_eq!(store.revision().unwrap(), 1);
    assert_eq!(store.materialize(None).unwrap()["marker"], "local");
}

#[test]
fn rejected_record_stream_rolls_back_the_whole_stage_and_preserves_live_library() {
    let (directory, mut store) = open_store();
    let staging = directory.path().join("download");
    fs::create_dir(&staging).unwrap();
    let (root, root_hash) = root_record(&staging, "rejected");
    let hashes = BTreeMap::from([(root.key.clone(), root_hash)]);
    let scope_id = library_fingerprint_domain();
    let fingerprint = fingerprint(&scope_id, &hashes);
    let error = match store.prepare_external_snapshot_application(
        &application(&staging, &scope_id, &fingerprint, 1),
        vec![
            Ok(root),
            Err(StoreError::Validation {
                message: "synthetic stream failure".into(),
            }),
        ],
        std::iter::empty(),
    ) {
        Ok(_) => panic!("stream failure must reject the stage"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("synthetic stream failure"));
    assert_eq!(stage_count(&store), 0);
    assert_eq!(store.materialize(None).unwrap()["marker"], "local");
}

#[test]
fn duplicate_key_key_mismatch_missing_payload_and_stale_revision_are_rejected() {
    for case in ["duplicate", "mismatch", "missing-payload", "stale"] {
        let (directory, mut store) = open_store();
        let staging = directory.path().join("download");
        fs::create_dir(&staging).unwrap();
        let (root, root_hash) = root_record(&staging, "rejected");
        let mut records = vec![root.clone()];
        let mut hashes = BTreeMap::from([(root.key.clone(), root_hash)]);
        if case == "duplicate" {
            records.push(root);
        } else if case == "mismatch" {
            let (mut mismatch, hash) = root_record(&staging, "mismatch");
            mismatch.key = encode_logical_record_key(&LogicalRecordLocator::Preset {
                preset_id: "preset".into(),
            })
            .unwrap();
            hashes.clear();
            hashes.insert(mismatch.key.clone(), hash);
            records = vec![mismatch];
        } else if case == "missing-payload" {
            let metadata = encode_asset_alias_metadata(&LogicalAssetAliasMetadata {
                mime: "application/octet-stream".into(),
                name: "payload.bin".into(),
                ext: "bin".into(),
                inlay_type: None,
                width: None,
                height: None,
                metadata: json!({}),
            })
            .unwrap();
            let (asset, hash) = write_record(
                &staging,
                LogicalRecordLocator::Asset {
                    logical_key: "asset".into(),
                },
                LogicalRecordEnvelope::Asset {
                    object_hash: Some("a".repeat(64)),
                    size: 4,
                    metadata,
                },
            );
            hashes.insert(asset.key.clone(), hash);
            records.push(asset);
        }
            let scope_id = library_fingerprint_domain();
        let fingerprint = fingerprint(&scope_id, &hashes);
        assert!(store
            .prepare_external_snapshot_application(
                &application(
                    &staging,
                    &scope_id,
                    &fingerprint,
                    if case == "stale" { 0 } else { 1 },
                ),
                records.into_iter().map(Ok),
                std::iter::empty(),
            )
            .is_err());
        assert_eq!(stage_count(&store), 0);
        assert_eq!(store.materialize(None).unwrap()["marker"], "local");
    }
}

#[test]
fn object_hash_and_scope_mismatches_are_rejected_before_activation() {
    let (directory, mut store) = open_store();
    let staging = directory.path().join("download");
    fs::create_dir(&staging).unwrap();
    let (root, root_hash) = root_record(&staging, "rejected");
    let hashes = BTreeMap::from([(root.key.clone(), root_hash)]);
    let scope_id = library_fingerprint_domain();
    let fingerprint = fingerprint(&scope_id, &hashes);
    let bytes = b"synthetic object";
    let mut object = write_object(&staging, bytes);
    object.content_hash = "b".repeat(64);
    assert!(store
        .prepare_external_snapshot_application(
            &application(&staging, &scope_id, &fingerprint, 1),
            [Ok(root.clone())],
            [Ok(object)],
        )
        .is_err());
    assert_eq!(stage_count(&store), 0);

    let wrong_scope = [9; 32];
    assert!(store
        .prepare_external_snapshot_application(
            &application(&staging, &wrong_scope, &fingerprint, 1),
            [Ok(root)],
            std::iter::empty(),
        )
        .is_err());
    assert_eq!(stage_count(&store), 0);
}

#[test]
fn connection_removal_cancels_local_jobs_releases_roots_and_deselects() {
    let (_directory, mut store) = open_store();
    let initial = sync_selection::read(&store.connection).unwrap();
    let tx = store.connection.transaction().unwrap();
    let selected = sync_selection::select(
        &tx,
        &initial.epoch,
        &sync_selection::SyncTarget::External("connection".into()),
    )
    .unwrap();
    tx.commit().unwrap();
    let identity =
        serde_json::to_string(&sync_selection::identity(&store.connection).unwrap()).unwrap();
    store.connection.execute(
        "INSERT INTO external_storage_jobs VALUES('ready','connection','repository','capture-ready',?1,'backup',NULL,NULL,'point','ready')",
        [&identity],
    ).unwrap();
    store.connection.execute(
        "INSERT INTO external_storage_capture_refs VALUES('capture-ready','ready')",
        [],
    ).unwrap();
    store.connection.execute(
        "INSERT INTO external_storage_bases VALUES('connection','repository','snapshot','commit','observation',?1)",
        [&identity],
    ).unwrap();

    store
        .external_prepare_connection_removal("connection")
        .unwrap();

    let phases: Vec<String> = {
        let mut query = store.connection.prepare(
            "SELECT phase FROM external_storage_jobs WHERE connection_id='connection' ORDER BY id",
        ).unwrap();
        query
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(phases, ["cancelled"]);
    assert_eq!(
        store
            .connection
            .query_row(
                "SELECT count(*) FROM external_storage_capture_refs",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .connection
            .query_row(
                "SELECT count(*) FROM external_storage_bases WHERE connection_id='connection'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    let after = sync_selection::read(&store.connection).unwrap();
    assert_eq!(after.target, sync_selection::SyncTarget::None);
    assert_ne!(after.epoch, selected.epoch);
}

#[test]
fn connection_removal_detaches_unknown_publication_but_keeps_its_recovery_root() {
    for phase in ["publishing", "publicationUnknown"] {
        let (_directory, mut store) = open_store();
        let initial = sync_selection::read(&store.connection).unwrap();
        let tx = store.connection.transaction().unwrap();
        sync_selection::select(
            &tx,
            &initial.epoch,
            &sync_selection::SyncTarget::External("connection".into()),
        )
        .unwrap();
        tx.commit().unwrap();
        let identity =
            serde_json::to_string(&sync_selection::identity(&store.connection).unwrap()).unwrap();
        store.connection.execute(
            "INSERT INTO external_storage_jobs VALUES('unsettled','connection','repository','capture',?1,?2,'cas','head','commit',?3)",
            rusqlite::params![identity, "sync", phase],
        ).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO external_storage_capture_refs VALUES('capture','unsettled')",
                [],
            )
            .unwrap();
        store.connection.execute(
            "INSERT INTO external_storage_bases VALUES('connection','repository','snapshot','commit','observation',?1)",
            [&identity],
        ).unwrap();

        store.external_prepare_connection_removal("connection").unwrap();

        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT phase FROM external_storage_jobs WHERE id='unsettled'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "publicationUnknown"
        );
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT count(*) FROM external_storage_capture_refs",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT count(*) FROM external_storage_bases WHERE connection_id='connection'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        let after = sync_selection::read(&store.connection).unwrap();
        assert_eq!(after.target, sync_selection::SyncTarget::None);
    }

    let (_directory, mut store) = open_store();
    let initial = sync_selection::read(&store.connection).unwrap();
    let tx = store.connection.transaction().unwrap();
    let selected = sync_selection::select(
        &tx,
        &initial.epoch,
        &sync_selection::SyncTarget::External("connection".into()),
    )
    .unwrap();
    tx.commit().unwrap();
    let identity = serde_json::to_string(&sync_selection::identity(&store.connection).unwrap()).unwrap();
    store.connection.execute(
        "INSERT INTO external_storage_jobs VALUES('unsettled','connection','repository','capture',?1,'restore','cas','head','commit','applying')",
        [&identity],
    ).unwrap();
    assert!(store.external_prepare_connection_removal("connection").is_err());
    assert_eq!(sync_selection::read(&store.connection).unwrap().epoch, selected.epoch);
}

struct NeverCancelled;
impl crate::local_backup::CancellationProbe for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

fn captured_record_hashes(store: &mut PersistentStore) -> BTreeMap<String, String> {
    let hydration = store
        .hydrate_external_capture_dependencies("synthetic-consumer", &NeverCancelled)
        .unwrap();
    let capture = store
        .capture_external_library("synthetic-consumer", &hydration, &NeverCancelled)
        .unwrap();
    let mut query = capture
        .catalog
        .db
        .prepare("SELECT key,hash FROM records")
        .unwrap();
    let rows = query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap();
    rows.map(|row| row.unwrap()).collect()
}

/// An apply keeps the local value of every field the local device owns, and a
/// capture encodes what it finds, so the two hashes under one key are the
/// publisher's bytes and this device's bytes. They agree only for a record kind
/// that carries no such field. Comparing a capture against a remote catalog
/// therefore reports a difference where there is no content to receive.
#[test]
fn a_capture_of_a_received_library_renames_every_record_holding_a_local_view_field() {
    let (directory, mut store) = open_store();
    let staging = directory.path().join("download");
    fs::create_dir(&staging).unwrap();
    let page = encode_message_page(&[json!({"chatId":"message","data":"remote"})]).unwrap();
    let page_object = write_object(&staging, &page.bytes);
    let (conversation, conversation_hash) = write_record(
        &staging,
        LogicalRecordLocator::Conversation {
            character_id: "character".into(),
            conversation_id: "conversation".into(),
        },
        LogicalRecordEnvelope::Conversation {
            configured_index: 0,
            recent_at: 20,
            detail: json!({"id":"conversation","name":"Remote conversation"}),
            message_page_hashes: vec![page.hash],
        },
    );
    let (root, root_hash) = root_record(&staging, "remote");
    let (character, character_hash) = write_record(
        &staging,
        LogicalRecordLocator::Character {
            character_id: "character".into(),
        },
        LogicalRecordEnvelope::Character {
            configured_index: 0,
            detail: json!({
                "chaId":"character",
                "name":"Remote character",
                "chatPage":1,
                "lastInteraction":10
            }),
            owner_heads: vec![LogicalOwnerHead::absent(
                LogicalOwnerLocator::CharacterAdditional {
                    character_id: "character".into(),
                },
            )],
        },
    );
    let records = vec![conversation, root, character];
    let mut hashes = BTreeMap::new();
    for (record, hash) in records
        .iter()
        .zip([conversation_hash, root_hash, character_hash])
    {
        hashes.insert(record.key.clone(), hash);
    }
    let scope_id = library_fingerprint_domain();
    let expected = fingerprint(&scope_id, &hashes);
    let prepared = store
        .prepare_external_snapshot_application(
            &application(&staging, &scope_id, &expected, 1),
            records.into_iter().map(Ok),
            [Ok(page_object)],
        )
        .unwrap();
    store.finish_prepared_replace(prepared).unwrap();

    let captured = captured_record_hashes(&mut store);
    let named: BTreeMap<String, String> = hashes
        .iter()
        .map(|(key, hash)| (key.clone(), hex::encode(hash)))
        .collect();
    assert_eq!(
        captured.keys().collect::<Vec<_>>(),
        named.keys().collect::<Vec<_>>()
    );
    let conversation = "r1:conversation:WyJjaGFyYWN0ZXIiLCJjb252ZXJzYXRpb24iXQ";
    assert_eq!(captured[conversation], named[conversation]);
    // The root kept its local statics and the character kept its local chat
    // page and last interaction, so neither encodes to the bytes that arrived.
    let character = "r1:character:WyJjaGFyYWN0ZXIiXQ";
    assert_ne!(captured["r1:root"], named["r1:root"]);
    assert_ne!(captured[character], named[character]);
}

fn prepare_receive_intent(store: &mut PersistentStore) -> sync_selection::CaptureIdentity {
    let epoch = sync_selection::read(&store.connection).unwrap().epoch;
    let tx = store.connection.transaction().unwrap();
    sync_selection::select(
        &tx,
        &epoch,
        &sync_selection::SyncTarget::External("connection".into()),
    )
    .unwrap();
    tx.commit().unwrap();
    let identity = sync_selection::identity(&store.connection).unwrap();
    let tx = store.connection.transaction().unwrap();
    external_storage_state::prepare_receive(
        &tx,
        &external_storage_state::ReceiveIntent {
            job_id: "receive",
            connection_id: "connection",
            repository_id: "repository",
            snapshot_id: "snapshot",
            commit_id: "commit",
            authenticated_head: "head-token",
            identity: &identity,
        },
    )
    .unwrap();
    tx.commit().unwrap();
    identity
}

fn receive_root(store: &mut PersistentStore, staging: &Path, marker: &str) -> BTreeMap<String, String> {
    let identity = prepare_receive_intent(store);
    let (root, root_hash) = root_record(staging, marker);
    let hashes = BTreeMap::from([(root.key.clone(), root_hash)]);
    let scope_id = library_fingerprint_domain();
    let expected = fingerprint(&scope_id, &hashes);
    let map: BTreeMap<String, String> = hashes
        .iter()
        .map(|(key, hash)| (key.clone(), hex::encode(hash)))
        .collect();
    let prepared = store
        .prepare_external_snapshot_application(
            &application(staging, &scope_id, &expected, identity.revision),
            [Ok(root)],
            std::iter::empty(),
        )
        .unwrap();
    store
        .finish_external_receive(prepared, "receive", &map)
        .unwrap();
    map
}

#[test]
fn a_receive_leaves_behind_what_the_snapshot_named_under_each_key() {
    let (directory, mut store) = open_store();
    let staging = directory.path().join("download");
    fs::create_dir(&staging).unwrap();
    let map = receive_root(&mut store, &staging, "remote-sync");
    assert_eq!(store.external_base_records("connection").unwrap(), Some(map));
    // A view is a view of one connection's base, not of the library at large.
    assert_eq!(store.external_base_records("other").unwrap(), None);
}

/// The library no longer holds what the base names once the user changes it,
/// so the rows stop being a view of it and have to read as absent.
#[test]
fn a_local_edit_after_a_receive_leaves_no_view() {
    let (directory, mut store) = open_store();
    let staging = directory.path().join("download");
    fs::create_dir(&staging).unwrap();
    receive_root(&mut store, &staging, "remote-sync");
    let revision = store.revision().unwrap();
    let commit = serde_json::from_value(
        json!({"expectedRevision": revision, "root": {"marker":"newer-local"}}),
    )
    .unwrap();
    store.commit(&commit).unwrap();
    assert_eq!(store.external_base_records("connection").unwrap(), None);
}

/// A snapshot of `characters` characters, each with one conversation of one
/// page, and optionally a conversation whose character it does not carry.
fn wide_snapshot(
    staging: &Path,
    characters: usize,
    orphan: bool,
) -> (Vec<ExternalSnapshotRecord>, Vec<ExternalSnapshotObject>, [u8; 32]) {
    let page = encode_message_page(&[json!({"chatId":"message","data":"remote"})]).unwrap();
    let page_object = write_object(staging, &page.bytes);
    let conversation = |character: &str| {
        write_record(
            staging,
            LogicalRecordLocator::Conversation {
                character_id: character.into(),
                conversation_id: "chat".into(),
            },
            LogicalRecordEnvelope::Conversation {
                configured_index: 0,
                recent_at: 1,
                detail: json!({"id":"chat","name":"chat"}),
                message_page_hashes: vec![page.hash.clone()],
            },
        )
    };
    let mut records = vec![root_record(staging, "wide")];
    for index in 0..characters {
        let id = format!("character-{index:04}");
        records.push(write_record(
            staging,
            LogicalRecordLocator::Character {
                character_id: id.clone(),
            },
            LogicalRecordEnvelope::Character {
                configured_index: index as u64,
                detail: json!({"chaId": id, "name": id}),
                owner_heads: vec![LogicalOwnerHead::absent(
                    LogicalOwnerLocator::CharacterAdditional {
                        character_id: id.clone(),
                    },
                )],
            },
        ));
        records.push(conversation(&id));
    }
    if orphan {
        records.push(conversation("absent"));
    }
    let hashes: BTreeMap<_, _> = records
        .iter()
        .map(|(record, hash)| (record.key.clone(), *hash))
        .collect();
    let fingerprint = fingerprint(&library_fingerprint_domain(), &hashes);
    (
        records.into_iter().map(|(record, _)| record).collect(),
        vec![page_object],
        fingerprint,
    )
}

fn stage_wide(
    store: &mut PersistentStore,
    staging: &Path,
    characters: usize,
    orphan: bool,
) -> StoreResult<PreparedReplaceCommit> {
    let (records, objects, fingerprint) = wide_snapshot(staging, characters, orphan);
    let scope_id = library_fingerprint_domain();
    store.prepare_external_snapshot_application(
        &application(staging, &scope_id, &fingerprint, 1),
        records.into_iter().map(Ok),
        objects.into_iter().map(Ok),
    )
}

/// R05. A stage is written a bounded batch at a time, and what lands is the
/// whole snapshot.
#[test]
fn a_stage_is_written_in_bounded_batches_and_lands_every_record() {
    let (directory, mut store) = open_store();
    let staging = directory.path().join("download");
    fs::create_dir(&staging).unwrap();
    let batches = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = batches.clone();
    super::after_stage_batch(Some(Box::new(move |written, _| seen.set(written))));
    let staged = stage_wide(&mut store, &staging, 600, false);
    super::after_stage_batch(None);
    let prepared = staged.unwrap();
    // A root and 600 characters, then 600 conversations, 512 records a batch.
    assert_eq!(batches.get(), 3);
    let library = store.materialize_staging(&prepared.staging_id).unwrap();
    let characters = library["characters"].as_array().unwrap();
    assert_eq!(characters.len(), 600);
    for character in characters {
        assert_eq!(
            character["chats"][0]["message"],
            json!([{"chatId":"message","data":"remote"}])
        );
    }
    assert_eq!(store.finish_prepared_replace(prepared).unwrap().revision, 2);
    assert_eq!(
        store.materialize(None).unwrap()["characters"]
            .as_array()
            .unwrap()
            .len(),
        600
    );
}

/// R05. A local edit between two batches ends the stage at the next one, and
/// a record refused in a later batch takes the earlier batches with it.
#[test]
fn a_stage_stopped_between_batches_leaves_no_stage_behind() {
    let (directory, mut store) = open_store();
    let staging = directory.path().join("download");
    fs::create_dir(&staging).unwrap();
    // Another job's connection, as a local save would have.
    let mut other = store.open_native_job_store().unwrap();
    super::after_stage_batch(Some(Box::new(move |written, _| {
        if written == 1 {
            let commit = serde_json::from_value(json!({
                "expectedRevision": 1, "root": {"marker":"newer-local"}
            }))
            .unwrap();
            other.commit(&commit).unwrap();
        }
    })));
    let staged = stage_wide(&mut store, &staging, 600, false);
    super::after_stage_batch(None);
    assert!(matches!(
        staged,
        Err(StoreError::RevisionConflict {
            expected: 1,
            actual: 2
        })
    ));
    assert_eq!(stage_count(&store), 0);
    assert_eq!(store.materialize(None).unwrap()["marker"], "newer-local");

    let (directory, mut store) = open_store();
    let staging = directory.path().join("download");
    fs::create_dir(&staging).unwrap();
    let batches = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = batches.clone();
    super::after_stage_batch(Some(Box::new(move |written, _| seen.set(written))));
    let staged = stage_wide(&mut store, &staging, 600, true);
    super::after_stage_batch(None);
    assert!(staged.is_err());
    assert!(batches.get() >= 2, "the refusal came after earlier batches committed");
    assert_eq!(stage_count(&store), 0);
    assert_eq!(store.materialize(None).unwrap()["marker"], "local");
}

/// A root, one character and one conversation of `messages` messages, each
/// carrying `padding` bytes, in pages of at most 128.
fn long_conversation_snapshot(
    staging: &Path,
    messages: usize,
    padding: usize,
) -> (Vec<ExternalSnapshotRecord>, Vec<ExternalSnapshotObject>, [u8; 32]) {
    let filler = "x".repeat(padding);
    let values = (0..messages)
        .map(|index| json!({"chatId": format!("message-{index}"), "data": filler}))
        .collect::<Vec<_>>();
    let mut objects = Vec::new();
    let mut page_hashes = Vec::new();
    for page in values.chunks(128) {
        let page = encode_message_page(page).unwrap();
        objects.push(write_object(staging, &page.bytes));
        page_hashes.push(page.hash);
    }
    let records = vec![
        root_record(staging, "long"),
        write_record(
            staging,
            LogicalRecordLocator::Character {
                character_id: "long".into(),
            },
            LogicalRecordEnvelope::Character {
                configured_index: 0,
                detail: json!({"chaId": "long", "name": "long"}),
                owner_heads: vec![LogicalOwnerHead::absent(
                    LogicalOwnerLocator::CharacterAdditional {
                        character_id: "long".into(),
                    },
                )],
            },
        ),
        write_record(
            staging,
            LogicalRecordLocator::Conversation {
                character_id: "long".into(),
                conversation_id: "chat".into(),
            },
            LogicalRecordEnvelope::Conversation {
                configured_index: 0,
                recent_at: 1,
                detail: json!({"id":"chat","name":"chat"}),
                message_page_hashes: page_hashes,
            },
        ),
    ];
    let hashes: BTreeMap<_, _> = records
        .iter()
        .map(|(record, hash)| (record.key.clone(), *hash))
        .collect();
    let fingerprint = fingerprint(&library_fingerprint_domain(), &hashes);
    (
        records.into_iter().map(|(record, _)| record).collect(),
        objects,
        fingerprint,
    )
}

/// The message rows and their bytes each stage transaction committed, read
/// from another connection after every batch.
fn stage_long_conversation(
    directory: &Path,
    store: &mut PersistentStore,
    messages: usize,
    padding: usize,
    probe: &dyn crate::local_backup::CancellationProbe,
    after_batch: impl FnMut(usize) + 'static,
) -> (StoreResult<PreparedReplaceCommit>, Vec<(i64, i64)>) {
    let staging = directory.join("download");
    fs::create_dir(&staging).unwrap();
    let (records, objects, fingerprint) = long_conversation_snapshot(&staging, messages, padding);
    let observer = rusqlite::Connection::open_with_flags(
        directory.join("persistent").join(super::super::DATABASE_FILE),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let committed = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = committed.clone();
    let mut after_batch = after_batch;
    let mut previous = (0i64, 0i64);
    super::after_stage_batch(Some(Box::new(move |written, _| {
        let now: (i64, i64) = observer
            .query_row(
                "SELECT count(*), coalesce(sum(length(value)),0) FROM messages",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        seen.borrow_mut()
            .push((now.0 - previous.0, now.1 - previous.1));
        previous = now;
        after_batch(written);
    })));
    let scope_id = library_fingerprint_domain();
    let staged = store.prepare_external_snapshot_application(
        &ExternalSnapshotApplication {
            probe,
            ..application(&staging, &scope_id, &fingerprint, 1)
        },
        records.into_iter().map(Ok),
        objects.into_iter().map(Ok),
    );
    super::after_stage_batch(None);
    let committed = committed.borrow().clone();
    (staged, committed)
}

/// F03. One conversation longer than a batch is staged over several bounded
/// transactions, and what lands is every message in order.
#[test]
fn one_long_conversation_is_staged_in_bounded_transactions() {
    const MESSAGES: usize = 20_000;
    let (directory, mut store) = open_store();
    let (staged, committed) =
        stage_long_conversation(directory.path(), &mut store, MESSAGES, 0, &NeverCancelled, |_| {});
    let prepared = staged.unwrap();
    assert!(committed.len() >= 3, "{committed:?}");
    assert!(
        committed
            .iter()
            .all(|(rows, _)| *rows as u64 <= super::STAGE_BATCH_WORK),
        "{committed:?}"
    );
    assert_eq!(
        committed.iter().map(|(rows, _)| rows).sum::<i64>(),
        MESSAGES as i64
    );
    // Nothing of the stage is in the library until it is activated.
    assert_eq!(store.materialize(None).unwrap()["marker"], "local");
    let library = store.materialize_staging(&prepared.staging_id).unwrap();
    let character = library["characters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|character| character["chaId"] == "long")
        .unwrap()
        .clone();
    let landed = character["chats"][0]["message"].as_array().unwrap();
    assert_eq!(landed.len(), MESSAGES);
    for (index, message) in landed.iter().enumerate() {
        assert_eq!(message["chatId"], format!("message-{index}"));
    }
    assert_eq!(store.finish_prepared_replace(prepared).unwrap().revision, 2);
}

/// F03. Heavy messages close a batch by their bytes, even inside one
/// conversation.
#[test]
fn a_conversation_of_heavy_messages_is_staged_within_the_byte_bound() {
    const PADDING: usize = 64 * 1024;
    let (directory, mut store) = open_store();
    let (staged, committed) =
        stage_long_conversation(directory.path(), &mut store, 400, PADDING, &NeverCancelled, |_| {});
    staged.unwrap();
    assert!(committed.len() >= 3, "{committed:?}");
    assert!(
        committed
            .iter()
            .all(|(_, bytes)| *bytes as u64 <= super::STAGE_BATCH_BYTES + 2 * PADDING as u64),
        "{committed:?}"
    );
}

/// F03. A local edit between two batches of one conversation ends the stage,
/// and nothing of it is left behind.
#[test]
fn a_local_edit_between_the_batches_of_one_conversation_ends_the_stage() {
    let (directory, mut store) = open_store();
    let mut other = store.open_native_job_store().unwrap();
    let (staged, committed) =
        stage_long_conversation(directory.path(), &mut store, 20_000, 0, &NeverCancelled, move |written| {
            if written == 2 {
                let commit = serde_json::from_value(json!({
                    "expectedRevision": 1, "root": {"marker":"newer-local"}
                }))
                .unwrap();
                other.commit(&commit).unwrap();
            }
        });
    assert!(matches!(
        staged,
        Err(StoreError::RevisionConflict {
            expected: 1,
            actual: 2
        })
    ));
    assert_eq!(committed.len(), 2);
    assert_eq!(stage_count(&store), 0);
    assert_eq!(store.materialize(None).unwrap()["marker"], "newer-local");
}

/// Answers "cancelled" once `fired` is set, or from its `at`th question on,
/// and counts the questions it answered that way.
#[derive(Default)]
struct Cancel {
    fired: std::rc::Rc<std::cell::Cell<bool>>,
    at: Option<usize>,
    asked: std::cell::Cell<usize>,
    refused: std::cell::Cell<usize>,
}

impl crate::local_backup::CancellationProbe for Cancel {
    fn is_cancelled(&self) -> bool {
        self.asked.set(self.asked.get() + 1);
        if self.at.is_some_and(|at| self.asked.get() >= at) {
            self.fired.set(true);
        }
        if self.fired.get() {
            self.refused.set(self.refused.get() + 1);
        }
        self.fired.get()
    }
}

fn staged_rows(store: &PersistentStore) -> (i64, i64) {
    store
        .connection
        .query_row(
            "SELECT (SELECT count(*) FROM root WHERE generation LIKE 'staging-%'),
                    (SELECT count(*) FROM messages WHERE generation LIKE 'staging-%')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

/// A cancellation between two message batches of one conversation admits no
/// further batch, and the stage it leaves is removed by its own owner.
#[test]
fn cancellation_between_the_batches_of_one_conversation_admits_no_later_batch() {
    let (directory, mut store) = open_store();
    let probe = Cancel::default();
    let fired = probe.fired.clone();
    let (staged, committed) =
        stage_long_conversation(directory.path(), &mut store, 20_000, 0, &probe, move |written| {
            if written == 2 {
                fired.set(true);
            }
        });
    assert!(matches!(staged, Err(StoreError::Validation { .. })));
    assert_eq!(committed.len(), 2);
    // The first question after the cancellation was the last one asked.
    assert_eq!(probe.refused.get(), 1);
    assert_eq!(staged_rows(&store), (0, 0));
    assert_eq!(store.revision().unwrap(), 1);
    assert_eq!(store.materialize(None).unwrap()["marker"], "local");
}

/// A cancellation while one long conversation's messages are being turned
/// into rows stops inside that record, before any of it is written.
#[test]
fn cancellation_inside_one_long_record_stops_before_its_rows_are_written() {
    let (directory, mut store) = open_store();
    // Staging the pages and the two short records asks far fewer than 10,000
    // times; each of the 20,000 messages then asks once.
    let probe = Cancel {
        at: Some(10_000),
        ..Cancel::default()
    };
    let (staged, committed) =
        stage_long_conversation(directory.path(), &mut store, 20_000, 0, &probe, |_| {});
    assert!(staged.is_err());
    assert!(committed.is_empty(), "{committed:?}");
    assert_eq!(probe.asked.get(), 10_000);
    assert_eq!(probe.refused.get(), 1);
    assert_eq!(staged_rows(&store), (0, 0));
    assert_eq!(store.materialize(None).unwrap()["marker"], "local");
}

/// A cancellation part way through reading one long object or one long record
/// body stops at the next chunk: nothing is published and no stage is left.
#[test]
fn cancellation_stops_a_long_checked_read_between_chunks() {
    for long_record in [false, true] {
        let (directory, mut store) = open_store();
        let staging = directory.path().join("download");
        fs::create_dir(&staging).unwrap();
        let object = write_object(&staging, &vec![7u8; 1024 * 1024]);
        let (record, digest) = if long_record {
            write_record(
                &staging,
                LogicalRecordLocator::Root,
                LogicalRecordEnvelope::Root {
                    value: json!({"marker": "x".repeat(1024 * 1024)}),
                    owner_heads: vec![],
                },
            )
        } else {
            root_record(&staging, "remote")
        };
        let fingerprint = fingerprint(
            &library_fingerprint_domain(),
            &BTreeMap::from([(record.key.clone(), digest)]),
        );
        let scope_id = library_fingerprint_domain();
        // Objects come first: one question before the object and one before
        // each read of at most 64 KiB, the last of which finds its end. A long
        // record is read after the whole object, with one question before the
        // record, one as it is staged and one per read.
        let at = if long_record { 1 + 17 + 2 + 3 } else { 1 + 3 };
        let probe = Cancel {
            at: Some(at),
            ..Cancel::default()
        };
        let staged = store.prepare_external_snapshot_application(
            &ExternalSnapshotApplication {
                probe: &probe,
                ..application(&staging, &scope_id, &fingerprint, 1)
            },
            [Ok(record)],
            [Ok(object.clone())],
        );
        assert!(staged.is_err(), "{long_record}");
        assert_eq!(probe.asked.get(), at, "{long_record}");
        assert_eq!(probe.refused.get(), 1, "{long_record}");
        let cas = crate::asset_repository::PayloadCas::new(directory.path()).unwrap();
        let published = cas.stat_object(&object.content_hash).unwrap();
        assert_eq!(published.is_some(), long_record, "{long_record}");
        let leftovers = fs::read_dir(directory.path().join("assets").join("staging"))
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(leftovers, 0, "{long_record}");
        assert_eq!(staged_rows(&store), (0, 0), "{long_record}");
        assert_eq!(store.materialize(None).unwrap()["marker"], "local", "{long_record}");
    }
}

/// One message larger than a whole batch is still staged, whole, under a
/// probe that is asked throughout and never cancels.
#[test]
fn one_message_larger_than_a_batch_still_stages_under_a_live_probe() {
    let padding = super::STAGE_BATCH_BYTES as usize + 1024 * 1024;
    let (directory, mut store) = open_store();
    let probe = Cancel::default();
    let (staged, committed) =
        stage_long_conversation(directory.path(), &mut store, 1, padding, &probe, |_| {});
    let prepared = staged.unwrap();
    assert!(committed.iter().any(|(_, bytes)| *bytes as usize > padding), "{committed:?}");
    // The page holding it was read a bounded chunk at a time.
    assert!(probe.asked.get() as usize > padding / (64 * 1024), "{}", probe.asked.get());
    assert_eq!(probe.refused.get(), 0);
    let library = store.materialize_staging(&prepared.staging_id).unwrap();
    let character = library["characters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|character| character["chaId"] == "long")
        .unwrap()
        .clone();
    let message = &character["chats"][0]["message"][0];
    assert_eq!(message["data"].as_str().unwrap().len(), padding);
    assert_eq!(store.finish_prepared_replace(prepared).unwrap().revision, 2);
}
