use super::*;
use crate::asset_repository::PayloadCas;
use crate::persistent_store::lww;
use std::cell::Cell;

fn archive_store() -> (tempfile::TempDir, PersistentStore, PayloadCas) {
    let directory = tempfile::tempdir().expect("create archive directory");
    let mut store = PersistentStore::open(directory.path()).expect("open persistent store");
    let cas = PayloadCas::new(directory.path()).expect("open payload CAS");
    let staging = store.replace_begin().expect("begin replacement").staging_id;
    store
        .replace_put_root(
            &staging,
            &json!({
                "username": "Archive",
                "account": { "id": "account-1", "token": "secret" },
                "modules": [],
                "loadouts": [],
                "plugins": []
            }),
        )
        .expect("stage root");
    store.replace_put_presets(&staging, &[]).expect("stage presets");
    store
        .replace_add_characters(
            &staging,
            &[
                json!({
                    "type": "character",
                    "chaId": "first-active",
                    "name": "First Active",
                    "chats": [{
                        "id": "first-chat",
                        "name": "First Chat",
                        "message": [{ "role": "user", "data": "one", "chatId": "m1" }]
                    }]
                }),
                json!({
                    "type": "character",
                    "chaId": "middle-archived",
                    "name": "Middle Archived",
                    "image": "middle-asset-key",
                    "lastInteraction": 42,
                    "chats": [
                        {
                            "id": "middle-chat",
                            "name": "Middle Chat",
                            "message": [
                                { "role": "user", "data": "hello", "chatId": "m2" },
                                { "role": "char", "data": "world", "chatId": "m3" }
                            ]
                        },
                        {
                            "id": "middle-second",
                            "name": "Middle Second",
                            "message": [{ "role": "user", "data": "again", "chatId": "m4" }]
                        }
                    ]
                }),
                json!({
                    "type": "character",
                    "chaId": "last-active",
                    "name": "Last Active",
                    "chats": [{ "id": "last-chat", "name": "Last Chat", "message": [] }]
                }),
            ],
        )
        .expect("stage characters");
    store.replace_commit(&staging, None).expect("commit fixture");
    (directory, store, cas)
}

fn seed_character_asset(
    store: &PersistentStore,
    cas: &PayloadCas,
    logical_key: &str,
) -> crate::asset_repository::PreparedPayload {
    let prepared = cas
        .prepare_bytes(logical_key.as_bytes())
        .expect("prepare asset payload");
    let generation = active_generation(&store.connection).expect("read active generation");
    store
        .connection
        .execute(
            "INSERT INTO asset_aliases (
                generation, logical_key, object_hash, kind, size, mime, name, ext, metadata
             ) VALUES (?1, ?2, ?3, 'asset', ?4, 'image/png', 'middle', 'png', '{}')",
            params![
                generation,
                logical_key,
                prepared.content_hash,
                prepared.byte_size as i64
            ],
        )
        .expect("seed character asset alias");
    prepared
}

fn archived_object(store: &PersistentStore, character_id: &str) -> archive::ArchivedObject {
    let generation = active_generation(&store.connection).expect("read active generation");
    archive::read_archived_object(&store.connection, &generation, character_id)
        .expect("read archived object")
        .expect("character is archived")
}

fn character_ids_in_order(store: &PersistentStore) -> Vec<String> {
    let page = store
        .query_characters(
            &CharacterQuery {
                search: None,
                order: QueryOrder::Configured,
                trash: false,
                limit: 64,
                cursor: None,
            },
            None,
        )
        .expect("query characters");
    page.items.into_iter().map(|item| item.id).collect()
}

/// Invariant 7. The row never leaves `characters`, so the list keeps it in place.
#[test]
fn archiving_keeps_the_character_listed_in_the_same_position_with_archived_residency() {
    let (_directory, mut store, _cas) = archive_store();
    let before = character_ids_in_order(&store);
    assert_eq!(before, ["first-active", "middle-archived", "last-active"]);

    let revision = store.revision().expect("read revision");
    store
        .archive_character("middle-archived", revision, 1_700_000_000_000)
        .expect("archive the character");

    let page = store
        .query_characters(
            &CharacterQuery {
                search: None,
                order: QueryOrder::Configured,
                trash: false,
                limit: 64,
                cursor: None,
            },
            None,
        )
        .expect("query characters after archiving");
    let ids = page
        .items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["first-active", "middle-archived", "last-active"]);
    let archived = page
        .items
        .iter()
        .find(|item| item.id == "middle-archived")
        .expect("archived character stays listed");
    let summary = archived
        .archived
        .as_ref()
        .expect("archived characters carry an archived summary");
    assert_eq!(summary.conversation_count, 2);
    assert_eq!(summary.message_count, 3);
    assert_eq!(summary.archived_at, 1_700_000_000_000);
    assert_eq!(archived.configured_index, 1);
    assert_eq!(archived.name, "Middle Archived");
    assert!(page
        .items
        .iter()
        .filter(|item| item.id != "middle-archived")
        .all(|item| item.archived.is_none()));
}

/// Invariant 8. Returning the marker would look like an empty character.
#[test]
fn reading_an_archived_character_fails_instead_of_returning_the_marker() {
    let (_directory, mut store, _cas) = archive_store();
    let revision = store.revision().expect("read revision");
    store
        .archive_character("middle-archived", revision, 10)
        .expect("archive the character");

    let error = store
        .read_character("middle-archived", None)
        .expect_err("archived detail is not readable");
    assert!(matches!(error, StoreError::Validation { .. }));

    let generation = active_generation(&store.connection).expect("read active generation");
    let stored: String = store
        .connection
        .query_row(
            "SELECT detail FROM characters WHERE generation = ?1 AND character_id = ?2",
            params![generation, "middle-archived"],
            |row| row.get(0),
        )
        .expect("read stored detail");
    let stored: Value = serde_json::from_str(&stored).expect("parse stored detail");
    assert!(archive::is_marker_detail(&stored));
    assert!(store
        .read_character("first-active", None)
        .expect("active detail stays readable")
        .is_some());
}

#[test]
fn archiving_moves_conversations_into_one_object_and_restoring_puts_them_back() {
    let (_directory, mut store, _cas) = archive_store();
    let before = store
        .read_character("middle-archived", None)
        .expect("read active detail")
        .expect("character exists")
        .value;
    let revision = store.revision().expect("read revision");
    store
        .archive_character("middle-archived", revision, 10)
        .expect("archive the character");

    let generation = active_generation(&store.connection).expect("read active generation");
    for table in ["conversations", "messages"] {
        let remaining: i64 = store
            .connection
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM {table} WHERE generation = ?1 AND character_id = ?2"
                ),
                params![generation, "middle-archived"],
                |row| row.get(0),
            )
            .expect("count archived rows");
        assert_eq!(remaining, 0, "{table} rows stay behind after archiving");
    }
    let conversation_count: i64 = store
        .connection
        .query_row(
            "SELECT conversation_count FROM characters
             WHERE generation = ?1 AND character_id = ?2",
            params![generation, "middle-archived"],
            |row| row.get(0),
        )
        .expect("read archived conversation count");
    assert_eq!(conversation_count, 0);

    let revision = store.revision().expect("read revision");
    store
        .restore_character("middle-archived", revision)
        .expect("restore the character");
    let after = store
        .read_character("middle-archived", None)
        .expect("read restored detail")
        .expect("character exists")
        .value;
    assert_eq!(after, before);
    let conversations = store
        .query_conversations(
            &ConversationQuery {
                character_id: "middle-archived".to_owned(),
                order: QueryOrder::Configured,
                limit: 64,
                cursor: None,
            },
            None,
        )
        .expect("query restored conversations");
    assert_eq!(
        conversations
            .items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["middle-chat", "middle-second"]
    );
    assert_eq!(
        store
            .read_conversation("middle-archived", "middle-chat", None)
            .expect("read restored conversation")
            .expect("conversation exists")
            .value["message"]
            .as_array()
            .expect("restored messages")
            .len(),
        2
    );
    let generation = active_generation(&store.connection).expect("read active generation");
    assert!(
        archive::read_archived_object(&store.connection, &generation, "middle-archived")
            .expect("read archived object")
            .is_none()
    );
}

#[test]
fn cancelling_or_oversizing_streamed_archive_keeps_the_live_character_and_revision() {
    for cancel in [true, false] {
        let (_directory, mut store, cas) = archive_store();
        let before = store.materialize(None).unwrap();
        let revision = store.revision().expect("read revision");
        let checks = Cell::new(0_u32);
        let is_cancelled = || {
            let next = checks.get() + 1;
            checks.set(next);
            cancel && next >= 6
        };
        let error = if cancel {
            store.archive_character_with_cancellation(
                "middle-archived", revision, 10, &is_cancelled, None,
            ).expect_err("cancel archive while streaming")
        } else {
            super::super::archive::archive_character_with_limit(
                &mut store.connection, &cas, "middle-archived", revision, 10,
                &is_cancelled, None, 128,
            ).expect_err("reject an archive above the decoded limit")
        };
        assert!(error.to_string().contains(if cancel { "cancelled" } else { "decoded size limit" }));
        assert_eq!(store.revision().expect("read revision after failure"), revision);
        assert_eq!(store.materialize(None).unwrap(), before);
        assert!(store.read_character("middle-archived", None).unwrap().is_some());
    }
}

#[test]
fn cancelling_streamed_restore_keeps_the_archive_object_and_cleans_staging() {
    let (_directory, mut store, cas) = archive_store();
    let revision = store.revision().expect("read revision");
    store
        .archive_character("middle-archived", revision, 10)
        .expect("archive character");
    let archived = archived_object(&store, "middle-archived");
    let revision = store.revision().expect("read archived revision");
    let checks = Cell::new(0_u32);
    let is_cancelled = || {
        let next = checks.get() + 1;
        checks.set(next);
        next >= 5
    };

    let error = store
        .restore_character_with_cancellation("middle-archived", revision, &is_cancelled, None)
        .expect_err("cancel restore while staging");

    assert!(error.to_string().contains("cancelled"));
    assert_eq!(store.revision().expect("read revision after cancellation"), revision);
    assert_eq!(archived_object(&store, "middle-archived"), archived);
    assert!(cas
        .stat_object(&archived.object_hash)
        .expect("stat retained archive")
        .is_some());
    let staging_tables: i64 = store
        .connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_database_list WHERE name = 'archive_restore'",
            [],
            |row| row.get(0),
        )
        .expect("count restore staging tables");
    assert_eq!(staging_tables, 0);

    store
        .restore_character("middle-archived", revision)
        .expect("retained archive remains restorable");
}

#[test]
fn malformed_streamed_restore_keeps_the_archive_and_cleans_partial_staging() {
    let (_directory, mut store, cas) = archive_store();
    let revision = store.revision().expect("read revision");
    store
        .archive_character("middle-archived", revision, 10)
        .expect("archive the character");
    let original = archived_object(&store, "middle-archived");
    let original_bytes = cas
        .read_object(&original.object_hash)
        .expect("read original archive")
        .expect("archive object exists");
    let mut decoder = flate2::read::GzDecoder::new(original_bytes.as_slice());
    let mut json_bytes = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut json_bytes).expect("decode archive");
    let mut payload: Value = serde_json::from_slice(&json_bytes).expect("parse archive");
    payload["conversations"]
        .as_array_mut()
        .expect("conversation array")
        .push(json!({ "unexpected": true }));
    let mut encoder =
        flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    serde_json::to_writer(&mut encoder, &payload).expect("encode malformed payload");
    let malformed = cas
        .prepare_bytes(&encoder.finish().expect("finish malformed archive"))
        .expect("prepare malformed archive");
    let mut redirected = original.clone();
    redirected.object_hash = malformed.content_hash;
    let generation = active_generation(&store.connection).expect("read active generation");
    store
        .connection
        .execute(
            "UPDATE characters SET archived_object = ?3
             WHERE generation = ?1 AND character_id = ?2",
            params![
                generation,
                "middle-archived",
                serde_json::to_string(&redirected).expect("encode archive metadata")
            ],
        )
        .expect("point fixture at malformed archive");

    let before_revision = store.revision().expect("read revision before restore");
    assert!(store
        .restore_character("middle-archived", before_revision)
        .is_err());
    assert_eq!(
        store.revision().expect("read revision after failed restore"),
        before_revision
    );
    let retained = archived_object(&store, "middle-archived");
    assert_eq!(retained.object_hash, redirected.object_hash);
    assert!(cas
        .stat_object(&original.object_hash)
        .expect("stat original archive")
        .is_some());
    let active_rows: i64 = store
        .connection
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM conversations
                 WHERE generation = ?1 AND character_id = ?2) +
                (SELECT COUNT(*) FROM messages
                 WHERE generation = ?1 AND character_id = ?2)",
            params![generation, "middle-archived"],
            |row| row.get(0),
        )
        .expect("count active rows");
    assert_eq!(active_rows, 0);
    let staging_tables: i64 = store
        .connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_database_list WHERE name = 'archive_restore'",
            [],
            |row| row.get(0),
        )
        .expect("count restore staging tables");
    assert_eq!(staging_tables, 0);
}

#[test]
#[ignore = "synthetic memory benchmark"]
fn large_character_archive_and_restore_memory_measurement() {
    const MESSAGE_COUNT: i64 = 16_384;
    const MESSAGE_BYTES: usize = 2 * 1024;
    let (_directory, mut store, _cas) = archive_store();
    let generation = active_generation(&store.connection).expect("read active generation");
    let transaction = store.connection.transaction().expect("begin fixture transaction");
    transaction.execute(
        "DELETE FROM messages
         WHERE generation=?1 AND character_id='middle-archived' AND conversation_id='middle-chat'",
        [&generation],
    ).expect("clear fixture messages");
    {
        let mut statement = transaction.prepare(
            "INSERT INTO messages (
                generation, character_id, conversation_id, message_index, message_id, value
             ) VALUES (?1, 'middle-archived', 'middle-chat', ?2, ?3, ?4)",
        ).expect("prepare message insert");
        let body = "x".repeat(MESSAGE_BYTES);
        for index in 0..MESSAGE_COUNT {
            let message_id = format!("scale-{index}");
            let value = serde_json::to_string(&json!({
                "role": if index % 2 == 0 { "user" } else { "char" },
                "data": body,
                "chatId": message_id,
            })).expect("encode message");
            statement.execute(params![generation, index, message_id, value])
                .expect("insert message");
        }
    }
    transaction.execute(
        "UPDATE conversations SET message_count=?2
         WHERE generation=?1 AND character_id='middle-archived' AND conversation_id='middle-chat'",
        params![generation, MESSAGE_COUNT],
    ).expect("update message count");
    transaction.commit().expect("commit scale fixture");

    let archive_revision = store.revision().expect("read archive revision");
    let archive = crate::test_memory::measure_working_set(|| {
        store.archive_character("middle-archived", archive_revision, 10)
    });
    archive.value.expect("archive large character");
    let restore_revision = store.revision().expect("read restore revision");
    let restore = crate::test_memory::measure_working_set(|| {
        store.restore_character("middle-archived", restore_revision)
    });
    restore.value.expect("restore large character");

    println!(
        "BOUNDED_ARCHIVE_MEMORY {{\"messageCount\":{MESSAGE_COUNT},\"messageBytes\":{MESSAGE_BYTES},\"archiveBaselineWorkingSetBytes\":{:?},\"archivePeakWorkingSetBytes\":{:?},\"archiveRetainedWorkingSetBytes\":{:?},\"restoreBaselineWorkingSetBytes\":{:?},\"restorePeakWorkingSetBytes\":{:?},\"restoreRetainedWorkingSetBytes\":{:?}}}",
        archive.baseline_working_set_bytes,
        archive.peak_working_set_bytes,
        archive.retained_working_set_bytes,
        restore.baseline_working_set_bytes,
        restore.peak_working_set_bytes,
        restore.retained_working_set_bytes,
    );
}

#[test]
fn an_archived_character_rejects_every_mutation_except_deleting_it() {
    let (_directory, mut store, _cas) = archive_store();
    let revision = store.revision().expect("read revision");
    store
        .archive_character("middle-archived", revision, 10)
        .expect("archive the character");
    let revision = store.revision().expect("read revision");

    let mut detail_commit = empty_working_set_commit(revision);
    detail_commit.character = Some(json!({ "chaId": "middle-archived", "name": "Renamed" }));
    assert!(matches!(
        store.commit(&detail_commit).expect_err("detail write is refused"),
        StoreError::Validation { .. }
    ));

    let mut conversation_commit = empty_working_set_commit(revision);
    conversation_commit.conversations = Some(vec![ConversationMutation::Delete {
        character_id: "middle-archived".to_owned(),
        conversation_id: "middle-chat".to_owned(),
    }]);
    assert!(matches!(
        store
            .commit(&conversation_commit)
            .expect_err("conversation write is refused"),
        StoreError::Validation { .. }
    ));

    let mut archive_again = empty_working_set_commit(revision);
    archive_again.replace_character = Some(json!({
        "chaId": "middle-archived",
        "name": "Middle Archived",
        "chats": []
    }));
    assert!(store.commit(&archive_again).is_err());

    // Soft trash is a character detail write, so the archive list deletes
    // permanently instead.
    let mut trash_commit = empty_working_set_commit(revision);
    trash_commit.character = Some(json!({
        "chaId": "middle-archived",
        "name": "Middle Archived",
        "trashTime": 10,
    }));
    assert!(matches!(
        store
            .commit(&trash_commit)
            .expect_err("soft trash is refused"),
        StoreError::Validation { .. }
    ));

    let mut delete_commit = empty_working_set_commit(revision);
    delete_commit.delete_character_ids = Some(vec!["middle-archived".to_owned()]);
    store
        .commit(&delete_commit)
        .expect("deleting an archived character stays allowed");
    assert_eq!(
        character_ids_in_order(&store),
        ["first-active", "last-active"]
    );
}

/// Invariant 5. The recorded hashes are the pin, so the sweep keeps them even
/// when nothing else in the library still points at the bytes.
#[test]
fn asset_gc_keeps_every_asset_hash_an_archived_character_recorded() {
    use super::asset_object_catalog::AssetObjectRegistration;

    let (_directory, mut store, cas) = archive_store();
    let asset = seed_character_asset(&store, &cas, "middle-asset-key");
    let asset_hash = asset.content_hash.clone();
    let unrelated = cas
        .prepare_bytes(b"unrelated object")
        .expect("prepare unrelated object");
    store
        .asset_object_catalog()
        .register(
            &[
                AssetObjectRegistration {
                    object_hash: asset_hash.clone(),
                    byte_size: asset.byte_size,
                },
                AssetObjectRegistration {
                    object_hash: unrelated.content_hash.clone(),
                    byte_size: unrelated.byte_size,
                },
            ],
            0,
        )
        .expect("register GC candidates");

    let revision = store.revision().expect("read revision");
    store
        .archive_character("middle-archived", revision, 10)
        .expect("archive the character");
    let archived = archived_object(&store, "middle-archived");
    assert!(
        archived.asset_hashes.contains(&asset_hash),
        "the archived record names the character asset"
    );

    // Drop the alias row so the archived record is the only thing holding the bytes.
    let generation = active_generation(&store.connection).expect("read active generation");
    store
        .connection
        .execute(
            "DELETE FROM asset_aliases WHERE generation = ?1 AND logical_key = ?2",
            params![generation, "middle-asset-key"],
        )
        .expect("remove the alias row");

    let swept = store
        .asset_gc_delete_page(64, None, 1_000_000, 10)
        .expect("run the sweep");
    assert!(swept.report.deletion_enabled, "the sweep actually deletes");
    for hash in archived
        .asset_hashes
        .iter()
        .chain(std::iter::once(&archived.object_hash))
    {
        assert!(
            cas.stat_object(hash).expect("stat pinned object").is_some(),
            "archived hash {hash} survives the sweep"
        );
    }
    assert!(
        cas.stat_object(&unrelated.content_hash)
            .expect("stat unreferenced object")
            .is_none(),
        "an unreferenced object is still collected"
    );
}

/// Invariant 6. An upstream database has no archive, and receiving one must not
/// be able to erase what the archive holds.
#[test]
fn replacing_from_an_upstream_database_preserves_archived_rows_objects_and_pins() {
    let (_directory, mut store, cas) = archive_store();
    let asset_hash = seed_character_asset(&store, &cas, "middle-asset-key").content_hash;
    let generation = active_generation(&store.connection).expect("read active generation");
    store
        .connection
        .execute(
            "INSERT INTO asset_owner_heads (
                generation, owner_kind, owner_locator, present, manifest_hash, entry_count
             ) VALUES (?1, 'character-additional-assets', ?2, 1, ?3, 1)",
            params![generation, "middle-archived", asset_hash],
        )
        .expect("seed the character owner head");

    let revision = store.revision().expect("read revision");
    store
        .archive_character("middle-archived", revision, 10)
        .expect("archive the character");
    let archived = archived_object(&store, "middle-archived");

    let staging = store.replace_begin().expect("begin replacement").staging_id;
    store
        .replace_put_root(&staging, &json!({ "username": "Received" }))
        .expect("stage received root");
    store.replace_put_presets(&staging, &[]).expect("stage presets");
    store
        .replace_add_characters(
            &staging,
            &[
                json!({
                    "type": "character",
                    "chaId": "received-first",
                    "name": "Received First",
                    "chats": []
                }),
                json!({
                    "type": "character",
                    "chaId": "received-last",
                    "name": "Received Last",
                    "chats": []
                }),
            ],
        )
        .expect("stage received characters");
    let preserved_revision = store
        .replace_preserve_repositories(&staging, None)
        .expect("preserve repositories")
        .revision;
    store
        .replace_commit(&staging, Some(preserved_revision))
        .expect("commit the replacement");

    let ids = character_ids_in_order(&store);
    assert_eq!(
        ids,
        ["received-first", "middle-archived", "received-last"],
        "the archived character survives at its saved position"
    );
    assert!(!ids.contains(&"first-active".to_owned()));
    let survived = archived_object(&store, "middle-archived");
    assert_eq!(survived, archived);
    assert!(cas
        .stat_object(&archived.object_hash)
        .expect("stat the archived object")
        .is_some());
    let generation = active_generation(&store.connection).expect("read active generation");
    let heads: i64 = store
        .connection
        .query_row(
            "SELECT COUNT(*) FROM asset_owner_heads
             WHERE generation = ?1 AND owner_kind = 'character-additional-assets'
               AND owner_locator = ?2",
            params![generation, "middle-archived"],
            |row| row.get(0),
        )
        .expect("count preserved owner heads");
    assert_eq!(heads, 1, "the archived asset pin is preserved");
}

#[test]
fn replacing_from_a_database_that_carries_an_archived_character_reports_a_conflict() {
    let (_directory, mut store, _cas) = archive_store();
    let revision = store.revision().expect("read revision");
    store
        .archive_character("middle-archived", revision, 10)
        .expect("archive the character");

    let staging = store.replace_begin().expect("begin replacement").staging_id;
    store
        .replace_put_root(&staging, &json!({ "username": "Received" }))
        .expect("stage received root");
    store.replace_put_presets(&staging, &[]).expect("stage presets");
    store
        .replace_add_characters(
            &staging,
            &[json!({
                "type": "character",
                "chaId": "middle-archived",
                "name": "Middle Archived",
                "chats": []
            })],
        )
        .expect("stage received characters");
    let conflict = store
        .replace_preserve_repositories(&staging, None)
        .expect_err("the incoming character collides with the archive");
    assert!(matches!(conflict, StoreError::Validation { .. }));
    assert!(
        archived_object(&store, "middle-archived").conversation_count == 2,
        "the archive is untouched by the refused replacement"
    );
}

#[test]
fn a_full_database_read_leaves_archived_characters_out() {
    let (_directory, mut store, _cas) = archive_store();
    let revision = store.revision().expect("read revision");
    store
        .archive_character("middle-archived", revision, 10)
        .expect("archive the character");

    let database = store.materialize(None).expect("materialize the database");
    let ids = database["characters"]
        .as_array()
        .expect("materialized characters")
        .iter()
        .map(|character| character["chaId"].as_str().expect("character id"))
        .collect::<Vec<_>>();
    assert_eq!(ids, ["first-active", "last-active"]);
}

fn reconcile_unit(parts: &[&str], value: Value) -> lww::UnitMutation {
    lww::UnitMutation::Set { key: lww::unit_key(parts).unwrap(), value }
}

fn reconcile_request(store: &mut PersistentStore, request_id: &str) -> super::super::archive_reconcile::ArchiveReconcileRequest {
    let owner = "middle-archived";
    let lease = store.acquire_revision(store.revision().unwrap()).unwrap().lease;
    let histories = store.archive_reconcile_histories(&lease, owner, &["middle-chat".into()]).unwrap();
    store.release_revision(&lease).unwrap();
    store.archive_character(owner, store.revision().unwrap(), 123).unwrap();
    let binding = store.lww_binding_state().unwrap();
    super::super::archive_reconcile::ArchiveReconcileRequest {
        header: lww::Header { binding_authority: binding.target_authority, request_id: request_id.into() },
        expected_revision: store.revision().unwrap(), selection_epoch: binding.selection_epoch,
        character_id: owner.into(), histories,
        unit_mutations: vec![reconcile_unit(&["character", owner, "name"], json!("Locally edited"))],
        conversations: vec![ConversationMutation::ReplaceRange { character_id: owner.into(), conversation_id: "middle-chat".into(),
            start: 1, delete_count: 1, messages: vec![json!({"chatId":"m3","role":"char","data":"local result","futureMessage":{"nested":true}})],
            conversation: None, configured_index: None }],
        asset_owner_heads: vec![],
    }
}

#[test]
fn archive_reconciliation_commits_local_edits_without_restoring_the_owner() {
    use super::super::archive_reconcile::ArchiveReconcileResult;
    let (_directory, mut store, _cas) = archive_store();
    store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), unit_mutations: Some(vec![
        reconcile_unit(&["character", "middle-archived", "privateOwn"], json!("device-only")),
        reconcile_unit(&["character", "middle-archived", "futureCharacter"], json!({"unrecognized":[1,"value"]})),
        reconcile_unit(&["conversation", "middle-archived", "middle-chat", "futureConversation"], json!({"kept":true})),
    ]), ..Default::default() }).unwrap();
    let unrelated = store.read_conversation("first-active", "first-chat", None).unwrap().unwrap().value;
    let request = reconcile_request(&mut store, "archive-reconcile-edits");
    let result = store.reconcile_archived_character(&request, &|| false).unwrap();
    let ArchiveReconcileResult::Committed { revision, archive, histories } = result else { panic!("expected commit") };
    assert_eq!(revision, request.expected_revision + 1);
    assert!(archive.archived);
    assert_eq!(archive.name, "Locally edited");
    assert_ne!(histories[0].history_hash, request.histories[0].history_hash);
    assert_eq!(histories[0].message_count, Some(2));
    assert_eq!(archived_object(&store, "middle-archived").archived_at, 123);
    assert!(store.read_character("middle-archived", None).is_err());
    assert_eq!(store.read_conversation("first-active", "first-chat", None).unwrap().unwrap().value, unrelated);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM conversations WHERE character_id='middle-archived'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    store.restore_character("middle-archived", revision).unwrap();
    let detail = store.read_character("middle-archived", None).unwrap().unwrap().value;
    assert_eq!(detail["name"], "Locally edited");
    assert_eq!(detail["privateOwn"], "device-only");
    assert_eq!(detail["futureCharacter"], json!({"unrecognized":[1,"value"]}));
    let chat = store.read_conversation("middle-archived", "middle-chat", None).unwrap().unwrap().value;
    assert_eq!(chat["futureConversation"], json!({"kept":true}));
    assert_eq!(chat["message"][1]["data"], "local result");
    assert_eq!(chat["message"][1]["futureMessage"], json!({"nested":true}));
}

#[test]
fn archive_reconciliation_receipt_survives_reopen_and_later_restore() {
    use super::super::archive_reconcile::ArchiveReconcileResult;
    let (directory, mut store, _cas) = archive_store();
    let request = reconcile_request(&mut store, "archive-reconcile-retry");
    store.reconcile_archived_character(&request, &|| false).unwrap();
    let committed_revision = store.revision().unwrap();
    drop(store);
    let mut store = PersistentStore::open(directory.path()).unwrap();
    assert!(matches!(store.reconcile_archived_character(&request, &|| false).unwrap(),
        ArchiveReconcileResult::Replayed { committed_revision: prior } if prior == committed_revision));
    assert_eq!(store.revision().unwrap(), committed_revision);
    store.restore_character("middle-archived", committed_revision).unwrap();
    let restored_revision = store.revision().unwrap();
    assert!(matches!(store.reconcile_archived_character(&request, &|| false).unwrap(), ArchiveReconcileResult::Replayed { .. }));
    assert_eq!(store.revision().unwrap(), restored_revision);
    assert_eq!(store.read_conversation("middle-archived", "middle-chat", None).unwrap().unwrap().value["message"].as_array().unwrap().len(), 2);
    let mut changed = request.clone();
    changed.conversations.clear();
    changed.histories.clear();
    assert!(store.reconcile_archived_character(&changed, &|| false).unwrap_err().to_string().contains("request-id-integrity"));
}

#[test]
fn archive_reconciliation_commands_keep_witnesses_and_replay_before_unrelated_intent_recovery() {
    use super::super::{archive_reconcile::ArchiveReconcileResult, commands};
    use tauri::Manager;
    let (_directory, mut store, _cas) = archive_store();
    let lease = store.acquire_revision(store.revision().unwrap()).unwrap().lease;
    let request = reconcile_request(&mut store, "archive-reconcile-command");
    let app = tauri::test::mock_builder().build(tauri::test::mock_context(tauri::test::noop_assets())).unwrap();
    app.manage(commands::PersistentStoreState::with_test_store(store));
    assert_eq!(commands::pds_archive_reconcile_histories(app.state(), lease.clone(), request.character_id.clone(),
        vec!["middle-chat".into()]).unwrap(), request.histories);
    commands::pds_release_revision(app.state(), lease).unwrap();
    tauri::async_runtime::block_on(commands::hydrate_archive_reconcile(app.handle().clone(),
        request.character_id.clone(), "reconcile-hydrate-command".into())).unwrap();
    let result = tauri::async_runtime::block_on(commands::reconcile_archived_character(app.handle().clone(),
        request.clone(), "reconcile-write-command".into())).unwrap();
    let ArchiveReconcileResult::Committed { revision, archive, .. } = result else { panic!("expected command commit") };
    assert!(archive.archived);
    commands::with_store_mut(app.state(), |store| {
        store.restore_character(&request.character_id, revision)?;
        store.device_store()?.connection().execute(
            "INSERT INTO lww_intents(request_id,authority,stamp,body,digest,complete) VALUES('synthetic-invalid-pending','0','{}','{','synthetic',0)", [],
        )?;
        Ok(())
    }).unwrap();
    assert!(commands::with_store_mut(app.state(), |_| Ok(())).is_err());
    let current_revision = commands::pds_read_revision(app.state()).unwrap();
    let replay = tauri::async_runtime::block_on(commands::reconcile_archived_character(app.handle().clone(),
        request, "reconcile-replay-command".into())).unwrap();
    assert!(matches!(replay, ArchiveReconcileResult::Replayed { committed_revision } if committed_revision == revision));
    assert_eq!(commands::pds_read_revision(app.state()).unwrap(), current_revision);
}
#[test]
fn archive_reconciliation_refuses_changed_history_without_losing_the_archive() {
    use super::super::archive_reconcile::ArchiveReconcileResult;
    for insert in [false, true] {
        let (_directory, mut store, _cas) = archive_store();
        let mut request = reconcile_request(&mut store, "archive-reconcile-conflict");
        store.restore_character("middle-archived", store.revision().unwrap()).unwrap();
        store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), conversations: Some(vec![
            ConversationMutation::ReplaceRange { character_id: "middle-archived".into(), conversation_id: "middle-chat".into(),
                start: 0, delete_count: if insert { 0 } else { 1 }, messages: vec![json!({"chatId":"remote-message","role":"user","data":"remote change"})],
                conversation: None, configured_index: None }
        ]), ..Default::default() }).unwrap();
        store.archive_character("middle-archived", store.revision().unwrap(), 456).unwrap();
        request.expected_revision = store.revision().unwrap();
        let before = archived_object(&store, "middle-archived");
        assert!(matches!(store.reconcile_archived_character(&request, &|| false).unwrap(), ArchiveReconcileResult::HistoryConflict { .. }));
        assert_eq!(store.revision().unwrap(), request.expected_revision);
        assert_eq!(archived_object(&store, "middle-archived"), before);
        assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_requests WHERE request_id=?1", [&request.header.request_id], |row| row.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(store.device_store().unwrap().connection().query_row("SELECT count(*) FROM lww_intents WHERE request_id=?1", [&request.header.request_id], |row| row.get::<_, i64>(0)).unwrap(), 0);
    }
}

#[test]
fn archive_reconciliation_failure_rolls_back_child_units_archive_and_receipt() {
    let (_directory, mut store, _cas) = archive_store();
    let request = reconcile_request(&mut store, "archive-reconcile-rollback");
    let before = archived_object(&store, "middle-archived");
    let outbox = store.lww_read_outbox(request.header.binding_authority, 256).unwrap();
    store.connection.execute_batch("CREATE TEMP TRIGGER reject_archive_reconcile BEFORE INSERT ON main.lww_requests WHEN NEW.request_id='archive-reconcile-rollback' BEGIN SELECT RAISE(ABORT,'synthetic receipt failure'); END;").unwrap();
    assert!(store.reconcile_archived_character(&request, &|| false).is_err());
    assert_eq!(store.revision().unwrap(), request.expected_revision);
    assert_eq!(archived_object(&store, "middle-archived"), before);
    let after = store.lww_read_outbox(request.header.binding_authority, 256).unwrap();
    assert_eq!(after.revision, outbox.revision);
    assert_eq!(after.entries, outbox.entries);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM conversations WHERE character_id='middle-archived'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(store.device_store().unwrap().connection().query_row("SELECT count(*) FROM lww_intents WHERE request_id=?1", [&request.header.request_id], |row| row.get::<_, i64>(0)).unwrap(), 0);
    store.connection.execute_batch("DROP TRIGGER reject_archive_reconcile").unwrap();
    assert!(store.reconcile_archived_character(&request, &|| false).is_ok());
}

#[test]
fn archive_catalog_registration_obeys_the_enclosing_transaction() {
    let (_directory, mut store, cas) = archive_store();
    let object = cas.prepare_bytes(b"synthetic transactional archive body").unwrap();
    let reader = Connection::open(&store.database_path).unwrap();
    let tx = store.connection.transaction().unwrap();
    super::super::asset_object_catalog::register_in_transaction(&tx, &[super::super::asset_object_catalog::AssetObjectRegistration {
        object_hash: object.content_hash.clone(), byte_size: object.byte_size,
    }], 5).unwrap();
    assert_eq!(tx.query_row("SELECT count(*) FROM asset_objects WHERE object_hash=?1", [&object.content_hash], |row| row.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(reader.query_row("SELECT count(*) FROM asset_objects WHERE object_hash=?1", [&object.content_hash], |row| row.get::<_, i64>(0)).unwrap(), 0);
    tx.rollback().unwrap();
    assert_eq!(reader.query_row("SELECT count(*) FROM asset_objects WHERE object_hash=?1", [&object.content_hash], |row| row.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn archive_reconciliation_cancellation_after_first_publication_keeps_recovery_candidates() {
    use crate::asset_repository::job_pins::{collect_durable_cas_job_roots_read_only, durable_cas_job_ids};
    let (directory, mut store, cas) = archive_store();
    let mut request = reconcile_request(&mut store, "archive-reconcile-cancel");
    request.unit_mutations.push(reconcile_unit(&["character", "middle-archived", "privateOwn"], json!("local only")));
    let before = archived_object(&store, "middle-archived");
    let published = std::cell::RefCell::new(Vec::new());
    let error = store.reconcile_archived_character(&request, &|| {
        let roots = collect_durable_cas_job_roots_read_only(directory.path());
        let present = roots.object_hashes.iter().filter(|hash| cas.stat_object(hash).unwrap().is_some()).cloned().collect::<Vec<_>>();
        if present.len() == 1 {
            *published.borrow_mut() = present;
            true
        } else { false }
    }).unwrap_err();
    assert!(error.to_string().contains(super::super::archive::ARCHIVE_CANCELLED_MESSAGE));
    assert_eq!(published.borrow().len(), 1);
    assert_eq!(store.revision().unwrap(), request.expected_revision);
    assert_eq!(archived_object(&store, "middle-archived"), before);
    let hash = published.borrow()[0].clone();
    assert_eq!(store.connection.query_row("SELECT count(*) FROM asset_objects WHERE object_hash=?1", [&hash], |row| row.get::<_, i64>(0)).unwrap(), 1);
    assert!(cas.stat_object(&hash).unwrap().is_some());
    assert!(durable_cas_job_ids(directory.path()).unwrap().is_empty());
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_requests WHERE request_id=?1", [&request.header.request_id], |row| row.get::<_, i64>(0)).unwrap(), 0);
    assert!(store.reconcile_archived_character(&request, &|| false).is_ok());
}

#[test]
fn archive_reconciliation_delete_requires_unchanged_conversation_metadata() {
    use super::super::archive_reconcile::ArchiveReconcileResult;
    let (_directory, mut store, _cas) = archive_store();
    let mut request = reconcile_request(&mut store, "archive-reconcile-delete-conflict");
    request.conversations = vec![ConversationMutation::Delete { character_id: "middle-archived".into(), conversation_id: "middle-chat".into() }];
    store.restore_character("middle-archived", store.revision().unwrap()).unwrap();
    store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), unit_mutations: Some(vec![
        reconcile_unit(&["conversation", "middle-archived", "middle-chat", "name"], json!("renamed remotely")),
    ]), ..Default::default() }).unwrap();
    store.archive_character("middle-archived", store.revision().unwrap(), 456).unwrap();
    request.expected_revision = store.revision().unwrap();
    let before = archived_object(&store, "middle-archived");
    assert!(matches!(store.reconcile_archived_character(&request, &|| false).unwrap(), ArchiveReconcileResult::HistoryConflict { .. }));
    assert_eq!(store.revision().unwrap(), request.expected_revision);
    assert_eq!(archived_object(&store, "middle-archived"), before);
    store.restore_character("middle-archived", store.revision().unwrap()).unwrap();
    assert_eq!(store.read_conversation("middle-archived", "middle-chat", None).unwrap().unwrap().value["name"], "renamed remotely");
}

#[test]
fn archive_reconciliation_rejects_other_owners_and_stale_selection() {
    let (_directory, mut store, _cas) = archive_store();
    let request = reconcile_request(&mut store, "archive-reconcile-scope");
    let before = archived_object(&store, "middle-archived");
    for unit in [
        reconcile_unit(&["character", "first-active", "name"], json!("wrong owner")),
        reconcile_unit(&["root", "username"], json!("outside owner")),
        reconcile_unit(&["exists", "character", "middle-archived"], json!({"type":"character"})),
        reconcile_unit(&["character", "middle-archived", "type"], json!("group")),
    ] {
        let mut invalid = request.clone();
        invalid.unit_mutations = vec![unit];
        assert!(store.reconcile_archived_character(&invalid, &|| false).is_err());
    }
    let mut stale = request.clone();
    stale.selection_epoch = "stale-selection".into();
    assert!(store.reconcile_archived_character(&stale, &|| false).unwrap_err().to_string().contains("Sync selection changed"));
    assert_eq!(store.revision().unwrap(), request.expected_revision);
    assert_eq!(archived_object(&store, "middle-archived"), before);
    assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_requests WHERE request_id=?1", [&request.header.request_id], |row| row.get::<_, i64>(0)).unwrap(), 0);
}
#[test]
fn archive_reconciliation_holds_authority_and_selection_through_the_library_write() {
    use crate::asset_repository::job_pins::durable_cas_job_ids;
    let (directory, mut store, _cas) = archive_store();
    let request = reconcile_request(&mut store, "archive-reconcile-guards");
    let device = Connection::open(directory.path().join("persistent").join(super::super::device_store::DEVICE_DATABASE_FILE)).unwrap();
    device.busy_timeout(std::time::Duration::ZERO).unwrap();
    let library = Connection::open(&store.database_path).unwrap();
    library.busy_timeout(std::time::Duration::ZERO).unwrap();
    let attempted = std::cell::Cell::new(false);
    store.reconcile_archived_character(&request, &|| {
        if !attempted.get() && !durable_cas_job_ids(directory.path()).unwrap().is_empty() {
            attempted.set(true);
            let device_error = device.execute("UPDATE lww_clock SET binding_authority='999' WHERE singleton=1", []).unwrap_err();
            assert_eq!(device_error.sqlite_error_code(), Some(rusqlite::ErrorCode::DatabaseBusy));
            let selection_error = library.execute("UPDATE library_sync_selection SET selection_epoch='concurrent-selection' WHERE singleton=1", []).unwrap_err();
            assert_eq!(selection_error.sqlite_error_code(), Some(rusqlite::ErrorCode::DatabaseBusy));
        }
        false
    }).unwrap();
    assert!(attempted.get());
    assert_eq!(store.lww_binding_authority().unwrap(), request.header.binding_authority);
    assert_eq!(store.lww_binding_state().unwrap().selection_epoch, request.selection_epoch);
    assert_eq!(store.revision().unwrap(), request.expected_revision + 1);
}