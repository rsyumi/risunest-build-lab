use super::*;
use crate::persistent_store::plugin_owner::UNOWNED_OWNER;
use crate::persistent_store::PluginStorageValue;

const EMPTY_SCRIPT_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn store_with_rows(rows: &[(&str, &str, Value)]) -> (tempfile::TempDir, PersistentStore) {
    let directory = tempfile::tempdir().expect("create plugin owner directory");
    let mut store = PersistentStore::open(directory.path()).expect("open persistent store");
    let revision = store.revision().expect("read revision");
    store
        .commit(&WorkingSetCommit {
            plugin_storage: Some(
                rows.iter()
                    .map(|(owner, key, value)| PluginStorageMutation::Set {
                        owner: (*owner).to_owned(),
                        key: (*key).to_owned(),
                        value: value.clone(),
                    })
                    .collect(),
            ),
            ..empty_working_set_commit(revision)
        })
        .expect("seed plugin rows");
    (directory, store)
}

/// Invariant 13 rests on the sentinel surviving SQLite untouched. `length()` on
/// this column stops at the first NUL, so the column is compared and never
/// measured.
#[test]
fn the_unowned_sentinel_round_trips_through_the_primary_key_and_change_log() {
    let (_directory, store) = store_with_rows(&[
        (UNOWNED_OWNER, "shared", json!("imported")),
        ("plugin-a", "shared", json!("owned")),
    ]);
    let generation = active_generation(&store.connection).expect("read generation");

    let owners: Vec<String> = store
        .connection
        .prepare("SELECT owner FROM plugin_storage WHERE generation=?1 ORDER BY owner")
        .expect("prepare owners")
        .query_map([&generation], |row| row.get(0))
        .expect("query owners")
        .collect::<Result<_, _>>()
        .expect("collect owners");
    assert_eq!(owners, vec![UNOWNED_OWNER.to_owned(), "plugin-a".to_owned()]);

    let stored: String = store
        .connection
        .query_row(
            "SELECT value FROM plugin_storage
             WHERE generation=?1 AND owner=?2 AND storage_key='shared'",
            params![&generation, UNOWNED_OWNER],
            |row| row.get(0),
        )
        .expect("read sentinel row by equality");
    assert_eq!(
        serde_json::from_str::<Value>(&stored).expect("parse sentinel value"),
        json!("imported")
    );
    assert!(store
        .connection
        .query_row(
            "SELECT COUNT(*) FROM plugin_storage WHERE generation=?1 AND owner=''",
            [&generation],
            |row| row.get::<_, i64>(0),
        )
        .expect("count empty owners")
        == 0);

    let sentinel_changes: i64 = store
        .connection
        .query_row(
            "SELECT COUNT(*) FROM content_changes
             WHERE kind='plugin' AND key1=?1 AND key2='shared'",
            [UNOWNED_OWNER],
            |row| row.get(0),
        )
        .expect("count sentinel change rows");
    assert_eq!(sentinel_changes, 1);
    let named_changes: i64 = store
        .connection
        .query_row(
            "SELECT COUNT(*) FROM content_changes
             WHERE kind='plugin' AND key1='plugin-a' AND key2='shared'",
            [],
            |row| row.get(0),
        )
        .expect("count named change rows");
    assert_eq!(named_changes, 1);

    let round_tripped: Value =
        serde_json::from_str(&serde_json::to_string(&json!({ "owner": UNOWNED_OWNER })).unwrap())
            .expect("json round trip");
    assert_eq!(round_tripped["owner"], json!(UNOWNED_OWNER));
}

/// Invariant 11. One owner never observes another owner's rows.
#[test]
fn a_plugin_reads_and_clears_only_its_own_rows() {
    let (_directory, mut store) = store_with_rows(&[
        ("plugin-a", "a-key", json!("a")),
        ("plugin-b", "b-key", json!("b")),
    ]);

    assert!(store
        .read_plugin_storage("plugin-a", "b-key", None)
        .expect("read across owners")
        .is_none());

    let revision = store.revision().expect("read revision");
    store
        .commit(&WorkingSetCommit {
            plugin_storage: Some(vec![PluginStorageMutation::Clear {
                owner: "plugin-a".to_owned(),
            }]),
            ..empty_working_set_commit(revision)
        })
        .expect("clear one owner");

    assert!(store
        .read_plugin_storage("plugin-a", "a-key", None)
        .expect("read cleared owner")
        .is_none());
    assert_eq!(
        store
            .read_plugin_storage("plugin-b", "b-key", None)
            .expect("read surviving owner")
            .expect("surviving row")
            .value,
        json!("b")
    );
}

/// Invariant 23. A write never moves a sentinel row to the writing plugin.
#[test]
fn writing_the_same_key_leaves_the_unowned_row_untouched() {
    let (_directory, mut store) =
        store_with_rows(&[(UNOWNED_OWNER, "pm_store", json!({ "apiKey": "imported" }))]);

    let revision = store.revision().expect("read revision");
    store
        .commit(&WorkingSetCommit {
            plugin_storage: Some(vec![PluginStorageMutation::Set {
                owner: "provider-manager".to_owned(),
                key: "pm_store".to_owned(),
                value: json!({ "apiKey": "default" }),
            }]),
            ..empty_working_set_commit(revision)
        })
        .expect("write the same key under a real owner");

    assert_eq!(
        store
            .read_plugin_storage(UNOWNED_OWNER, "pm_store", None)
            .expect("read sentinel row")
            .expect("sentinel row survives")
            .value,
        json!({ "apiKey": "imported" })
    );
    assert_eq!(
        store
            .read_plugin_storage("provider-manager", "pm_store", None)
            .expect("read owned row")
            .expect("owned row exists")
            .value,
        json!({ "apiKey": "default" })
    );
}

/// Invariant 26. The list answers with sizes and never with values.
#[test]
fn the_storage_list_reports_sizes_without_carrying_values() {
    let (_directory, store) = store_with_rows(&[
        ("plugin-a", "a-key", json!("aaaaaaaa")),
        ("plugin-b", "b-key", json!({ "nested": "bbbbbbbb" })),
    ]);

    let listed = store
        .list_plugin_storage(None)
        .expect("list plugin storage");
    let serialized = serde_json::to_string(&listed).expect("serialize listing");
    assert!(!serialized.contains("aaaaaaaa"));
    assert!(!serialized.contains("bbbbbbbb"));
    assert!(!serialized.contains("nested"));
    assert_eq!(listed.len(), 2);
    for item in &listed {
        assert!(item.byte_size > 0);
        assert!(item.value_type == "string" || item.value_type == "json");
    }
}

/// Invariant 14. A key two plugins hold leaves the upstream save entirely.
#[test]
fn exporting_drops_only_the_keys_two_plugins_both_hold() {
    let (_directory, store) = store_with_rows(&[
        ("plugin-a", "unique-a", json!("a")),
        ("plugin-b", "unique-b", json!("b")),
        ("plugin-a", "shared", json!("from a")),
        ("plugin-b", "shared", json!("from b")),
    ]);
    let generation = active_generation(&store.connection).expect("read generation");

    let flattened = super::super::export::flattened_plugin_storage(&store.connection, &generation)
        .expect("flatten plugin storage");
    assert_eq!(
        flattened.rows.iter().map(|row| row.key.as_str()).collect::<Vec<_>>(),
        vec!["unique-a", "unique-b"]
    );
    assert_eq!(
        flattened.collisions,
        vec![(
            "shared".to_owned(),
            vec!["plugin-a".to_owned(), "plugin-b".to_owned()]
        )]
    );

    let meta = super::super::export::plugin_storage_meta_value(&flattened.owners);
    assert_eq!(meta["unique-a"]["plugin"], json!("plugin-a"));
    assert_eq!(meta["unique-b"]["plugin"], json!("plugin-b"));
    assert!(meta.get("shared").is_none());

    // Only the export drops the key. The in-app projection still answers with
    // it, taking the later position so the object stays deterministic.
    let projected = store.materialize(None).expect("materialize the working set");
    assert_eq!(
        projected["pluginCustomStorage"],
        json!({ "shared": "from b", "unique-a": "a", "unique-b": "b" })
    );
}

/// The sidecar restores ownership; a save without one lands on the sentinel.
#[test]
fn an_import_restores_ownership_from_the_sidecar_and_otherwise_stays_unowned() {
    let directory = tempfile::tempdir().expect("create import directory");
    let mut store = PersistentStore::open(directory.path()).expect("open persistent store");
    let staging = store.replace_begin().expect("begin replacement");
    store
        .replace_put_root(
            &staging.staging_id,
            &json!({
                "pluginCustomStorage": { "known": "value", "unknown": "value" },
                "pluginStorageMeta": {
                    "known": { "plugin": "plugin-a", "updatedAt": 17 },
                    "forged": { "plugin": "plugin-b", "updatedAt": 17 }
                }
            }),
        )
        .expect("stage imported plugin storage");
    store
        .replace_commit(&staging.staging_id, Some(0))
        .expect("activate import");

    assert!(store
        .read_root(None)
        .expect("read root")
        .value
        .get("pluginStorageMeta")
        .is_none());
    assert_eq!(
        store
            .read_plugin_storage("plugin-a", "known", None)
            .expect("read restored row")
            .expect("restored row exists")
            .value,
        json!("value")
    );
    assert_eq!(
        store
            .read_plugin_storage(UNOWNED_OWNER, "unknown", None)
            .expect("read unowned row")
            .expect("unowned row exists")
            .value,
        json!("value")
    );
    assert!(store
        .read_plugin_storage("plugin-b", "forged", None)
        .expect("read sidecar only key")
        .is_none());
}

/// Invariant 12. A plugin write is durable once the commit returns, so a fresh
/// process reading the same directory finds it without any further step.
#[test]
fn a_committed_plugin_write_survives_reopening_the_store() {
    let directory = tempfile::tempdir().expect("create durability directory");
    let generation;
    {
        let mut store = PersistentStore::open(directory.path()).expect("open persistent store");
        let revision = store.revision().expect("read revision");
        store
            .commit(&WorkingSetCommit {
                plugin_storage: Some(vec![PluginStorageMutation::Set {
                    owner: "plugin-a".to_owned(),
                    key: "durable".to_owned(),
                    value: json!({ "kept": true }),
                }]),
                ..empty_working_set_commit(revision)
            })
            .expect("commit plugin write");
        generation = active_generation(&store.connection).expect("read generation");
    }

    let reopened = PersistentStore::open(directory.path()).expect("reopen persistent store");
    assert_eq!(
        active_generation(&reopened.connection).expect("read generation again"),
        generation
    );
    assert_eq!(
        reopened
            .read_plugin_storage("plugin-a", "durable", None)
            .expect("read durable row")
            .expect("durable row exists")
            .value,
        json!({ "kept": true })
    );
    assert!(reopened
        .read_plugin_storage(UNOWNED_OWNER, "durable", None)
        .expect("read unowned row")
        .is_none());
}

fn imported_store(
    values: Value,
    meta: Value,
) -> (tempfile::TempDir, PersistentStore) {
    let directory = tempfile::tempdir().expect("create claim directory");
    let mut store = PersistentStore::open(directory.path()).expect("open persistent store");
    let staging = store.replace_begin().expect("begin replacement");
    store
        .replace_put_root(
            &staging.staging_id,
            &json!({ "pluginCustomStorage": values, "pluginStorageMeta": meta,
                "plugins": [
                    { "name": "provider-manager", "script": "", "enabled": true, "version": "3.0" },
                    { "name": "plugin-a", "script": "", "enabled": true, "version": "3.0" }
                ] }),
        )
        .expect("stage imported plugin storage");
    store
        .replace_commit(&staging.staging_id, Some(0))
        .expect("activate import");
    (directory, store)
}

/// Every claim and assignment answers with the revision it left, so the
/// renderer's write coordinator can adopt it instead of conflicting.
fn claim(
    store: &mut PersistentStore,
    session: &str,
    owner: &str,
    code_hash: &str,
    runtime: &str,
    key: &str,
) -> Option<Value> {
    let expected = store.revision().expect("read revision");
    let claimed = store
        .claim_plugin_storage_value(session, owner, code_hash, runtime, key, expected)
        .expect("claim plugin value");
    assert_eq!(
        claimed.revision,
        store.revision().expect("read revision after claim")
    );
    assert_eq!(claimed.revision >= expected, true);
    claimed.value
}

fn assign(
    store: &mut PersistentStore,
    sources: &[(String, String)],
    owner: &str,
    collision: crate::persistent_store::commit::AssignCollision,
) -> crate::persistent_store::commit::AssignOutcome {
    let expected = store.revision().expect("read revision");
    let assigned = store
        .assign_plugin_storage(sources, owner, collision, expected)
        .expect("assign plugin storage");
    assert_eq!(
        assigned.revision,
        store.revision().expect("read revision after assignment")
    );
    assigned.outcome
}

/// Invariant 24. One window per import, opened once and never again.
#[test]
fn an_import_offers_each_plugin_one_claim_window_that_never_reopens() {
    let (_directory, mut store) = imported_store(
        json!({ "pm_store": { "apiKey": "imported" }, "other": "value" }),
        json!({}),
    );

    let session = store
        .begin_plugin_claim_session("provider-manager", EMPTY_SCRIPT_HASH, "run-one")
        .expect("open claim session")
        .expect("a waiting import opens a window");
    assert_eq!(
        claim(
            &mut store,
            &session,
            "provider-manager",
            EMPTY_SCRIPT_HASH,
            "run-one",
            "pm_store",
        ),
        Some(json!({ "apiKey": "imported" }))
    );
    assert_eq!(
        store
            .read_plugin_storage("provider-manager", "pm_store", None)
            .expect("read claimed row")
            .expect("claimed row exists")
            .value,
        json!({ "apiKey": "imported" })
    );
    assert!(store
        .read_plugin_storage(UNOWNED_OWNER, "pm_store", None)
        .expect("read unowned row")
        .is_none());

    let claimed = store
        .list_plugin_storage(None)
        .expect("list plugin storage")
        .into_iter()
        .find(|item| item.key == "pm_store")
        .expect("claimed item is listed");
    assert_eq!(claimed.claimed_from.as_deref(), Some("unowned"));
    assert!(claimed.assigned_at.is_some());
    assert!(claimed.import_batch_id.is_some());

    store
        .close_plugin_claim_session(&session)
        .expect("close the window");
    assert!(claim(
        &mut store,
        &session,
        "provider-manager",
        EMPTY_SCRIPT_HASH,
        "run-one",
        "other",
    )
    .is_none());
    // A later run of the same plugin gets no second window for this import.
    assert!(store
        .begin_plugin_claim_session("provider-manager", EMPTY_SCRIPT_HASH, "run-two")
        .expect("reopen attempt")
        .is_none());
    assert!(store
        .read_plugin_storage(UNOWNED_OWNER, "other", None)
        .expect("read remaining unowned row")
        .is_some());
}

#[test]
fn a_claim_refuses_a_key_the_plugin_already_holds_and_a_foreign_caller() {
    let (_directory, mut store) =
        imported_store(json!({ "shared": "imported" }), json!({}));
    let revision = store.revision().expect("read revision");
    store
        .commit(&WorkingSetCommit {
            plugin_storage: Some(vec![PluginStorageMutation::Set {
                owner: "plugin-a".to_owned(),
                key: "shared".to_owned(),
                value: json!("own"),
            }]),
            ..empty_working_set_commit(revision)
        })
        .expect("write the plugin's own value");

    let session = store
        .begin_plugin_claim_session("plugin-a", EMPTY_SCRIPT_HASH, "run-one")
        .expect("open claim session")
        .expect("a waiting import opens a window");
    assert!(claim(&mut store, &session, "plugin-a", EMPTY_SCRIPT_HASH, "run-one", "shared").is_none());
    assert_eq!(
        store
            .read_plugin_storage("plugin-a", "shared", None)
            .expect("read own row")
            .expect("own row exists")
            .value,
        json!("own")
    );
    assert!(store
        .read_plugin_storage(UNOWNED_OWNER, "shared", None)
        .expect("read unowned row")
        .is_some());

    // The window belongs to one plugin, one code and one run.
    for (owner, code_hash, runtime) in [
        ("plugin-b", EMPTY_SCRIPT_HASH, "run-one"),
        ("plugin-a", "hash-two", "run-one"),
        ("plugin-a", EMPTY_SCRIPT_HASH, "run-two"),
    ] {
        assert!(claim(&mut store, &session, owner, code_hash, runtime, "shared").is_none());
    }
}

#[test]
fn a_claim_stamps_only_the_moved_keys_and_orders_and_misses_reserve_nothing() {
    use risunest_sync_wire::unit::{UnitKey, UnitValue};
    use std::collections::{BTreeMap, BTreeSet};
    fn units(store: &PersistentStore) -> BTreeMap<String, (String, String, String)> {
        store.connection.prepare("SELECT key,stamp,value,version FROM lww_units ORDER BY key").unwrap()
            .query_map([], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?, row.get(3)?)))).unwrap()
            .collect::<Result<_, _>>().unwrap()
    }
    let (_directory, mut store) = imported_store(
        json!({"first":1,"claimed":2,"shared":3,"last":4}), json!({}),
    );
    store.commit(&WorkingSetCommit {
        expected_revision: store.revision().unwrap(),
        plugin_storage: Some(vec![PluginStorageMutation::Set {
            owner: "plugin-a".into(), key: "shared".into(), value: json!(5),
        }]), ..Default::default()
    }).unwrap();
    let session = store.begin_plugin_claim_session("plugin-a", EMPTY_SCRIPT_HASH, "run-one").unwrap().unwrap();
    let generation = active_generation(&store.connection).unwrap();
    let original_ordinal: i64 = store.connection.query_row(
        "SELECT ordinal FROM plugin_storage WHERE generation=?1 AND owner=?2 AND storage_key='claimed'",
        params![generation, UNOWNED_OWNER], |row| row.get(0),
    ).unwrap();
    let before = units(&store);
    assert_eq!(claim(&mut store, &session, "plugin-a", EMPTY_SCRIPT_HASH, "run-one", "claimed"), Some(json!(2)));
    let after = units(&store);
    let changed = after.iter().filter(|(key, value)| before.get(*key) != Some(*value)).map(|(key, _)| key.clone()).collect::<BTreeSet<_>>();
    let expected = [
        UnitKey::new(&["plugin", UNOWNED_OWNER, "claimed"]).unwrap(),
        UnitKey::new(&["plugin", "plugin-a", "claimed"]).unwrap(),
        UnitKey::new(&["order", "plugin-storage", UNOWNED_OWNER]).unwrap(),
        UnitKey::new(&["order", "plugin-storage", "plugin-a"]).unwrap(),
    ].map(|key| key.as_str().to_owned()).into_iter().collect::<BTreeSet<_>>();
    assert_eq!(changed, expected);
    let stamps = changed.iter().map(|key| after[key].0.clone()).collect::<BTreeSet<_>>();
    let versions = changed.iter().map(|key| after[key].2.clone()).collect::<BTreeSet<_>>();
    assert_eq!(stamps.len(), 1);
    assert_eq!(versions.len(), 1);
    let old_key = UnitKey::new(&["plugin", UNOWNED_OWNER, "claimed"]).unwrap();
    assert_eq!(serde_json::from_str::<UnitValue>(&after[old_key.as_str()].1).unwrap(), UnitValue::Deleted);
    let claimed_ordinal: i64 = store.connection.query_row(
        "SELECT ordinal FROM plugin_storage WHERE generation=?1 AND owner='plugin-a' AND storage_key='claimed'",
        [&generation], |row| row.get(0),
    ).unwrap();
    assert_eq!(claimed_ordinal, original_ordinal);
    for key in &changed {
        let pending: (String, String, String) = store.connection.query_row(
            "SELECT stamp,value,version FROM lww_outbox WHERE key=?1", [key],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).unwrap();
        assert_eq!(&pending, &after[key]);
    }
    let revision = store.revision().unwrap();
    let clock = serde_json::to_value(store.lww_clock_state().unwrap()).unwrap();
    let intents: i64 = store.device_store().unwrap().connection().query_row("SELECT count(*) FROM lww_intents", [], |row| row.get(0)).unwrap();
    let pending: Vec<(String,String,String,String)> = store.connection.prepare("SELECT key,stamp,value,version FROM lww_outbox ORDER BY key").unwrap()
        .query_map([], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap().collect::<Result<_,_>>().unwrap();
    for key in ["missing", "shared"] {
        assert!(claim(&mut store, &session, "plugin-a", EMPTY_SCRIPT_HASH, "run-one", key).is_none());
    }
    assert_eq!(store.revision().unwrap(), revision);
    assert_eq!(units(&store), after);
    assert_eq!(serde_json::to_value(store.lww_clock_state().unwrap()).unwrap(), clock);
    assert_eq!(store.device_store().unwrap().connection().query_row("SELECT count(*) FROM lww_intents", [], |row| row.get::<_,i64>(0)).unwrap(), intents);
    let after_pending: Vec<(String,String,String,String)> = store.connection.prepare("SELECT key,stamp,value,version FROM lww_outbox ORDER BY key").unwrap()
        .query_map([], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap().collect::<Result<_,_>>().unwrap();
    assert_eq!(after_pending, pending);
}

#[test]
fn a_reserved_claim_recovers_after_reopen_and_a_committed_claim_does_not_replay() {
    let (directory, mut store) = imported_store(json!({"claimed":1,"unowned":2}), json!({}));
    let session = store.begin_plugin_claim_session("plugin-a", EMPTY_SCRIPT_HASH, "run-one").unwrap().unwrap();
    let revision = store.revision().unwrap();
    let before_units: Vec<(String,String,String,String)> = store.connection.prepare("SELECT key,stamp,value,version FROM lww_units ORDER BY key").unwrap()
        .query_map([], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap().collect::<Result<_,_>>().unwrap();
    store.connection.execute_batch("CREATE TRIGGER fail_claim BEFORE INSERT ON lww_requests BEGIN SELECT RAISE(ABORT,'synthetic claim failure'); END;").unwrap();
    assert!(store.claim_plugin_storage_value(&session, "plugin-a", EMPTY_SCRIPT_HASH, "run-one", "claimed", revision).is_err());
    assert_eq!(store.revision().unwrap(), revision);
    assert!(store.read_plugin_storage(UNOWNED_OWNER, "claimed", None).unwrap().is_some());
    assert!(store.read_plugin_storage("plugin-a", "claimed", None).unwrap().is_none());
    let after_units: Vec<(String,String,String,String)> = store.connection.prepare("SELECT key,stamp,value,version FROM lww_units ORDER BY key").unwrap()
        .query_map([], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap().collect::<Result<_,_>>().unwrap();
    assert_eq!(after_units, before_units);
    let (request_id, authority, stamp, body, digest): (String, String, String, String, String) = store.device_store().unwrap().connection().query_row(
        "SELECT request_id,authority,stamp,body,digest FROM lww_intents WHERE complete=0", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
    ).unwrap();
    store.connection.execute_batch("DROP TRIGGER fail_claim;").unwrap();
    drop(store);
    let mut store = PersistentStore::open(directory.path()).unwrap();
    assert_eq!(store.revision().unwrap(), revision + 1);
    assert_eq!(store.read_plugin_storage("plugin-a", "claimed", None).unwrap().unwrap().value, json!(1));
    assert!(store.read_plugin_storage(UNOWNED_OWNER, "claimed", None).unwrap().is_none());
    let unit_key = risunest_sync_wire::unit::UnitKey::new(&["plugin", "plugin-a", "claimed"]).unwrap();
    assert_eq!(store.connection.query_row("SELECT stamp FROM lww_units WHERE key=?1", [unit_key.as_str()], |row| row.get::<_,String>(0)).unwrap(), stamp);
    store.commit(&WorkingSetCommit {
        expected_revision: store.revision().unwrap(),
        plugin_storage: Some(vec![PluginStorageMutation::Set { owner:"plugin-a".into(),key:"claimed".into(),value:json!(3) }]),
        ..Default::default()
    }).unwrap();
    let later_revision = store.revision().unwrap();
    let clock = serde_json::to_value(store.lww_clock_state().unwrap()).unwrap();
    store.device_store().unwrap().connection().execute("INSERT INTO lww_intents VALUES(?1,?2,?3,?4,?5,0)", rusqlite::params![request_id, authority, stamp, body, digest]).unwrap();
    drop(store);
    let store = PersistentStore::open(directory.path()).unwrap();
    assert_eq!(store.revision().unwrap(), later_revision);
    assert_eq!(store.read_plugin_storage("plugin-a", "claimed", None).unwrap().unwrap().value, json!(3));
    assert_eq!(serde_json::to_value(store.lww_clock_state().unwrap()).unwrap(), clock);
    assert_eq!(store.device_store().unwrap().connection().query_row("SELECT count(*) FROM lww_intents WHERE request_id=?1", [&request_id], |row| row.get::<_,i64>(0)).unwrap(), 0);
}

/// A full replacement written later must not mint a fresh window for values a
/// person deliberately left alone.
#[test]
fn a_later_full_replacement_keeps_the_import_a_waiting_value_arrived_in() {
    let (_directory, mut store) = imported_store(json!({ "waiting": "value" }), json!({}));
    let batch = store
        .list_plugin_storage(None)
        .expect("list plugin storage")
        .into_iter()
        .find(|item| item.key == "waiting")
        .expect("waiting item is listed")
        .import_batch_id
        .expect("waiting item carries its import");

    let session = store
        .begin_plugin_claim_session("plugin-a", EMPTY_SCRIPT_HASH, "run-one")
        .expect("open claim session")
        .expect("a waiting import opens a window");
    store
        .close_plugin_claim_session(&session)
        .expect("close without claiming");

    let staging = store.replace_begin().expect("begin replacement");
    store
        .replace_put_root(
            &staging.staging_id,
            &json!({ "pluginCustomStorage": { "waiting": "value" } }),
        )
        .expect("stage a full replacement");
    let revision = store.revision().expect("read revision");
    store
        .replace_commit(&staging.staging_id, Some(revision))
        .expect("activate the replacement");

    assert_eq!(
        store
            .list_plugin_storage(None)
            .expect("list plugin storage")
            .into_iter()
            .find(|item| item.key == "waiting")
            .expect("waiting item is still listed")
            .import_batch_id,
        Some(batch)
    );
    assert!(store
        .begin_plugin_claim_session("plugin-a", EMPTY_SCRIPT_HASH, "run-three")
        .expect("reopen attempt after a replacement")
        .is_none());
}

#[test]
fn full_replacement_preserves_same_key_values_for_different_owners() {
    let (_directory, mut store) = store_with_rows(&[
        ("plugin-a", "shared", json!({ "source": "a" })),
        ("plugin-b", "shared", json!({ "source": "b" })),
    ]);
    let values = vec![
        PluginStorageValue {
            owner: "plugin-a".to_owned(),
            key: "shared".to_owned(),
            value: json!({ "source": "a" }),
        },
        PluginStorageValue {
            owner: "plugin-b".to_owned(),
            key: "shared".to_owned(),
            value: json!({ "source": "b" }),
        },
    ];
    let staging = store.replace_begin().expect("begin replacement");
    store
        .replace_put_root_with_plugin_storage(
            &staging.staging_id,
            &json!({ "pluginCustomStorage": { "shared": { "source": "a" } } }),
            Some(&values),
        )
        .expect("stage owner-scoped plugin values");
    let revision = store.revision().expect("read revision");
    store
        .replace_commit(&staging.staging_id, Some(revision))
        .expect("activate replacement");

    for (owner, source) in [("plugin-a", "a"), ("plugin-b", "b")] {
        assert_eq!(
            store
                .read_plugin_storage(owner, "shared", None)
                .expect("read plugin value")
                .expect("plugin value exists")
                .value,
            json!({ "source": source })
        );
    }
}

#[test]
fn values_that_reached_the_store_outside_an_import_open_no_window() {
    let directory = tempfile::tempdir().expect("create claim directory");
    let mut store = PersistentStore::open(directory.path()).expect("open persistent store");
    let revision = store.revision().expect("read revision");
    store
        .commit(&WorkingSetCommit {
            plugin_storage: Some(vec![PluginStorageMutation::Set {
                owner: UNOWNED_OWNER.to_owned(),
                key: "stray".to_owned(),
                value: json!("value"),
            }]),
            ..empty_working_set_commit(revision)
        })
        .expect("write an unowned value outside an import");

    assert!(store
        .begin_plugin_claim_session("plugin-a", EMPTY_SCRIPT_HASH, "run-one")
        .expect("open claim session")
        .is_none());
}

/// Invariant 26. The manual screen decides what a name collision does, and a
/// value never silently replaces another.
#[test]
fn a_manual_assignment_follows_the_choice_made_for_a_name_collision() {
    use crate::persistent_store::commit::AssignCollision;
    let (_directory, mut store) = store_with_rows(&[
        (UNOWNED_OWNER, "pm_store", json!("imported")),
        (UNOWNED_OWNER, "pm_keys", json!("imported keys")),
        ("provider-manager", "pm_store", json!("own")),
    ]);
    let sources = vec![
        (UNOWNED_OWNER.to_owned(), "pm_store".to_owned()),
        (UNOWNED_OWNER.to_owned(), "pm_keys".to_owned()),
    ];

    let deferred = assign(&mut store, &sources, "provider-manager", AssignCollision::Defer);
    assert_eq!(deferred.moved, 1);
    assert_eq!(deferred.deferred, 1);
    assert_eq!(
        store
            .read_plugin_storage("provider-manager", "pm_store", None)
            .expect("read own row")
            .expect("own row exists")
            .value,
        json!("own")
    );
    assert_eq!(
        store
            .read_plugin_storage("provider-manager", "pm_keys", None)
            .expect("read moved row")
            .expect("moved row exists")
            .value,
        json!("imported keys")
    );
    let moved = store
        .list_plugin_storage(None)
        .expect("list plugin storage")
        .into_iter()
        .find(|item| item.key == "pm_keys")
        .expect("moved item is listed");
    // A manual assignment is not the automatic bucket.
    assert!(moved.claimed_from.is_none());
    assert!(moved.assigned_at.is_some());

    let colliding = store
        .colliding_plugin_storage_keys("provider-manager", &["pm_store".to_owned()])
        .expect("report collisions");
    assert_eq!(colliding, vec!["pm_store".to_owned()]);

    let replaced = assign(
        &mut store,
        &[(UNOWNED_OWNER.to_owned(), "pm_store".to_owned())],
        "provider-manager",
        AssignCollision::Replace,
    );
    assert_eq!(replaced.replaced, 1);
    assert_eq!(replaced.moved, 1);
    assert_eq!(
        store
            .read_plugin_storage("provider-manager", "pm_store", None)
            .expect("read replaced row")
            .expect("replaced row exists")
            .value,
        json!("imported")
    );
    assert!(store
        .read_plugin_storage(UNOWNED_OWNER, "pm_store", None)
        .expect("read unowned row")
        .is_none());
}

#[test]
fn discarding_a_colliding_value_removes_only_the_incoming_one() {
    use crate::persistent_store::commit::AssignCollision;
    let (_directory, mut store) = store_with_rows(&[
        (UNOWNED_OWNER, "pm_store", json!("imported")),
        ("provider-manager", "pm_store", json!("own")),
    ]);

    let outcome = assign(
        &mut store,
        &[(UNOWNED_OWNER.to_owned(), "pm_store".to_owned())],
        "provider-manager",
        AssignCollision::Discard,
    );
    assert_eq!(outcome.discarded, 1);
    assert_eq!(outcome.moved, 0);
    assert_eq!(
        store
            .read_plugin_storage("provider-manager", "pm_store", None)
            .expect("read own row")
            .expect("own row exists")
            .value,
        json!("own")
    );
    assert!(store
        .read_plugin_storage(UNOWNED_OWNER, "pm_store", None)
        .expect("read unowned row")
        .is_none());
}

/// An assignment records both owners' keys and orders under one stamp, so the
/// move is published and later writes for either owner still project.
#[test]
fn an_assignment_records_both_owners_units_and_later_owner_writes_commit() {
    use crate::persistent_store::commit::AssignCollision;
    use risunest_sync_wire::unit::{UnitKey, UnitValue};
    use std::collections::{BTreeMap, BTreeSet};
    fn units(store: &PersistentStore) -> BTreeMap<String, (String, String, String)> {
        store.connection.prepare("SELECT key,stamp,value,version FROM lww_units ORDER BY key").unwrap()
            .query_map([], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?, row.get(3)?)))).unwrap()
            .collect::<Result<_, _>>().unwrap()
    }
    fn key(parts: &[&str]) -> String {
        UnitKey::new(parts).unwrap().as_str().to_owned()
    }
    fn value(units: &BTreeMap<String, (String, String, String)>, parts: &[&str]) -> UnitValue {
        serde_json::from_str(&units[&key(parts)].1).unwrap()
    }
    fn changed(
        before: &BTreeMap<String, (String, String, String)>,
        after: &BTreeMap<String, (String, String, String)>,
    ) -> BTreeSet<String> {
        after.iter().filter(|(key, value)| before.get(*key) != Some(*value)).map(|(key, _)| key.clone()).collect()
    }
    let (_directory, mut store) = store_with_rows(&[
        ("plugin-a", "moved", json!(1)),
        ("plugin-a", "replaced", json!(2)),
        ("plugin-a", "discarded", json!(3)),
        ("plugin-a", "deferred", json!(4)),
        ("plugin-a", "kept", json!(5)),
        ("plugin-b", "replaced", json!("b-replaced")),
        ("plugin-b", "discarded", json!("b-discarded")),
        ("plugin-b", "deferred", json!("b-deferred")),
        ("plugin-b", "own", json!("b-own")),
    ]);
    let source = |name: &str| ("plugin-a".to_owned(), name.to_owned());

    let before = units(&store);
    let outcome = assign(&mut store, &[source("moved"), source("replaced")], "plugin-b", AssignCollision::Replace);
    assert_eq!((outcome.moved, outcome.replaced), (2, 1));
    let after = units(&store);
    let moved_keys = changed(&before, &after);
    assert_eq!(moved_keys, [
        key(&["plugin", "plugin-a", "moved"]),
        key(&["plugin", "plugin-a", "replaced"]),
        key(&["plugin", "plugin-b", "moved"]),
        key(&["plugin", "plugin-b", "replaced"]),
        key(&["order", "plugin-storage", "plugin-a"]),
        key(&["order", "plugin-storage", "plugin-b"]),
    ].into_iter().collect::<BTreeSet<_>>());
    assert_eq!(moved_keys.iter().map(|key| after[key].0.clone()).collect::<BTreeSet<_>>().len(), 1);
    assert_eq!(value(&after, &["plugin", "plugin-a", "moved"]), UnitValue::Deleted);
    assert_eq!(value(&after, &["plugin", "plugin-b", "moved"]), crate::persistent_store::lww::inline(&json!(1)).unwrap());
    assert_eq!(value(&after, &["plugin", "plugin-b", "replaced"]), crate::persistent_store::lww::inline(&json!(2)).unwrap());
    for key in &moved_keys {
        let pending: (String, String, String) = store.connection.query_row(
            "SELECT stamp,value,version FROM lww_outbox WHERE key=?1", [key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(&pending, &after[key]);
    }

    let before = units(&store);
    let outcome = assign(&mut store, &[source("discarded")], "plugin-b", AssignCollision::Discard);
    assert_eq!(outcome.discarded, 1);
    let after = units(&store);
    assert_eq!(changed(&before, &after), [
        key(&["plugin", "plugin-a", "discarded"]),
        key(&["order", "plugin-storage", "plugin-a"]),
    ].into_iter().collect::<BTreeSet<_>>());
    assert_eq!(value(&after, &["plugin", "plugin-b", "discarded"]), crate::persistent_store::lww::inline(&json!("b-discarded")).unwrap());

    let before = units(&store);
    let outcome = assign(&mut store, &[source("deferred")], "plugin-b", AssignCollision::Defer);
    assert_eq!(outcome.deferred, 1);
    assert_eq!(units(&store), before);

    for (owner, name, value) in [("plugin-a", "kept", json!(6)), ("plugin-b", "own", json!("b-own-2")), ("plugin-b", "moved", json!(7))] {
        store.commit(&WorkingSetCommit {
            expected_revision: store.revision().unwrap(),
            plugin_storage: Some(vec![PluginStorageMutation::Set { owner: owner.into(), key: name.into(), value: value.clone() }]),
            ..Default::default()
        }).unwrap_or_else(|error| panic!("write {owner}/{name} after assignment: {error:?}"));
        assert_eq!(store.read_plugin_storage(owner, name, None).unwrap().unwrap().value, value);
    }
}

/// The import stage hands values to plugins before the replacement is applied,
/// and its checkbox decides whether the rest may be taken automatically later.
#[test]
fn the_import_stage_assigns_staged_values_and_can_close_the_window_for_the_rest() {
    use crate::persistent_store::commit::StagedPluginAssignment;
    let directory = tempfile::tempdir().expect("create import stage directory");
    let mut store = PersistentStore::open(directory.path()).expect("open persistent store");
    let staging = store.replace_begin().expect("begin replacement");
    store
        .replace_put_root(
            &staging.staging_id,
            &json!({
                "plugins": [
                    { "name": "provider-manager" },
                    { "name": "yumi-translator" }
                ],
                "pluginCustomStorage": {
                    "pm_store": { "apiKey": "imported" },
                    "pm_keys": "imported keys",
                    "yt_glossary": ["term"]
                }
            }),
        )
        .expect("stage imported plugin storage");

    let preview = store
        .staged_plugin_preview(&staging.staging_id)
        .expect("read the staged values");
    assert_eq!(
        preview
            .values
            .iter()
            .map(|value| (value.key.as_str(), value.value_type.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("pm_store", "json"),
            ("pm_keys", "string"),
            ("yt_glossary", "json"),
        ]
    );
    assert!(preview.values.iter().all(|value| value.byte_size > 0));
    assert_eq!(
        preview.plugin_names,
        vec!["provider-manager".to_owned(), "yumi-translator".to_owned()]
    );

    store
        .assign_staged_plugin_values(
            &staging.staging_id,
            &[StagedPluginAssignment {
                owner: "provider-manager".to_owned(),
                keys: vec!["pm_store".to_owned(), "pm_keys".to_owned()],
            }],
            false,
        )
        .expect("assign the chosen values");
    store
        .replace_commit(&staging.staging_id, Some(0))
        .expect("activate the import");

    assert_eq!(
        store
            .read_plugin_storage("provider-manager", "pm_store", None)
            .expect("read assigned row")
            .expect("assigned row exists")
            .value,
        json!({ "apiKey": "imported" })
    );
    assert!(store
        .read_plugin_storage(UNOWNED_OWNER, "yt_glossary", None)
        .expect("read the value left alone")
        .is_some());
    // Automatic assignment was refused, so nothing is offered afterwards.
    assert!(store
        .begin_plugin_claim_session("yumi-translator", EMPTY_SCRIPT_HASH, "run-one")
        .expect("open a claim session")
        .is_none());
}

#[test]
fn leaving_automatic_assignment_on_offers_the_rest_to_the_first_plugin_that_asks() {
    use crate::persistent_store::commit::StagedPluginAssignment;
    let directory = tempfile::tempdir().expect("create import stage directory");
    let mut store = PersistentStore::open(directory.path()).expect("open persistent store");
    let staging = store.replace_begin().expect("begin replacement");
    store
        .replace_put_root(
            &staging.staging_id,
            &json!({ "pluginCustomStorage": { "pm_store": "imported", "yt_glossary": "left" },
                "plugins": [{ "name": "yumi-translator", "script": "", "enabled": true, "version": "3.0" }] }),
        )
        .expect("stage imported plugin storage");
    store
        .assign_staged_plugin_values(
            &staging.staging_id,
            &[StagedPluginAssignment {
                owner: "provider-manager".to_owned(),
                keys: vec!["pm_store".to_owned()],
            }],
            true,
        )
        .expect("assign the chosen values");
    store
        .replace_commit(&staging.staging_id, Some(0))
        .expect("activate the import");

    let session = store
        .begin_plugin_claim_session("yumi-translator", EMPTY_SCRIPT_HASH, "run-one")
        .expect("open a claim session")
        .expect("a waiting import opens a window");
    assert_eq!(
        claim(
            &mut store,
            &session,
            "yumi-translator",
            EMPTY_SCRIPT_HASH,
            "run-one",
            "yt_glossary",
        ),
        Some(json!("left"))
    );
    // The key a person already assigned is never on offer.
    assert!(claim(
        &mut store,
        &session,
        "yumi-translator",
        EMPTY_SCRIPT_HASH,
        "run-one",
        "pm_store",
    )
    .is_none());
}

#[test]
fn import_claims_exclude_later_owners_changed_scripts_and_closed_or_reopened_runtimes() {
    let (directory, store) = imported_store(json!({"waiting": "synthetic"}), json!({}));
    let hash = EMPTY_SCRIPT_HASH;
    assert!(store.begin_plugin_claim_session("later-plugin", hash, "later").unwrap().is_none());
    assert!(store.begin_plugin_claim_session("plugin-a", "changed-script-hash", "changed").unwrap().is_none());
    assert!(store.begin_plugin_claim_session("plugin-a", hash, "initial").unwrap().is_some());
    assert!(store.begin_plugin_claim_session("plugin-a", hash, "reload").unwrap().is_none());
    store.close_plugin_claim_eligibility().unwrap();
    assert!(store.begin_plugin_claim_session("provider-manager", hash, "after-close").unwrap().is_none());
    drop(store);
    let store = PersistentStore::open(directory.path()).unwrap();
    assert!(store.begin_plugin_claim_session("provider-manager", hash, "after-reopen").unwrap().is_none());
    assert!(store.read_plugin_storage(UNOWNED_OWNER, "waiting", None).unwrap().is_some());
}

#[test]
fn opening_the_store_discards_unused_import_claim_eligibility() {
    let (directory, store) = imported_store(json!({"waiting": "synthetic"}), json!({}));
    drop(store);
    let store = PersistentStore::open(directory.path()).unwrap();
    assert!(store.begin_plugin_claim_session("plugin-a",
        EMPTY_SCRIPT_HASH, "restart").unwrap().is_none());
}

#[test]
fn plugin_value_pages_bound_bytes_isolate_owners_and_keep_the_lease_revision() {
    use crate::persistent_store::PluginStorageValueQuery;
    let (_directory, mut store) = store_with_rows(&[
        ("owner-a", "zeta", json!("x".repeat(600_000))),
        ("owner-b", "zeta", json!("private")),
        ("owner-a", "alpha", json!("y".repeat(600_000))),
        ("owner-a", "large", json!("z".repeat(1_100_000))),
        ("owner-a", "last", json!(false)),
    ]);
    let revision = store.revision().unwrap();
    let lease = store.acquire_revision(revision).unwrap().lease;
    store.commit(&WorkingSetCommit {
        plugin_storage: Some(vec![PluginStorageMutation::Set {
            owner: "owner-a".to_owned(), key: "zeta".to_owned(), value: json!("new"),
        }]), ..empty_working_set_commit(revision)
    }).unwrap();
    let mut query = PluginStorageValueQuery { owner: Some("owner-a".to_owned()), after_key: None, limit: Some(2) };
    let mut keys = Vec::new();
    let mut sizes = Vec::new();
    loop {
        let page = store.read_plugin_storage_page(&query, Some(&lease)).unwrap();
        assert_eq!(page.revision, revision);
        assert_eq!(page.items.len(), 1);
        for item in page.items {
            assert_eq!(item.owner, "owner-a");
            sizes.push(serde_json::to_vec(&item.value).unwrap().len());
            keys.push(item.key);
        }
        query.after_key = page.next_cursor;
        if query.after_key.is_none() { break; }
    }
    assert_eq!(keys, vec!["zeta", "alpha", "large", "last"]);
    assert_eq!(sizes, vec![600_002, 600_002, 1_100_002, 5]);
    query.after_key = None;
    assert_eq!(store.read_plugin_storage_page(&query, None).unwrap().items[0].value, json!("new"));
    store.release_revision(&lease).unwrap();
    assert!(matches!(store.read_plugin_storage_page(&query, Some(&lease)), Err(StoreError::SnapshotReleased)));
    query.limit = Some(0);
    assert!(store.read_plugin_storage_page(&query, None).is_err());
}

#[test]
fn plugin_value_pages_return_two_thousand_values_in_eight_calls() {
    use crate::persistent_store::PluginStorageValueQuery;
    let keys: Vec<String> = (0..2000).map(|index| format!("key-{}", 2000 - index)).collect();
    let rows: Vec<(&str, &str, Value)> = keys.iter().enumerate()
        .map(|(index, key)| ("owner-a", key.as_str(), json!({ "index": index }))).collect();
    let (_directory, store) = store_with_rows(&rows);
    let mut query = PluginStorageValueQuery { owner: Some("owner-a".to_owned()), after_key: None, limit: None };
    let mut items = Vec::new();
    let mut calls = 0;
    loop {
        let page = store.read_plugin_storage_page(&query, None).unwrap();
        calls += 1;
        items.extend(page.items);
        query.after_key = page.next_cursor;
        if query.after_key.is_none() { break; }
    }
    assert_eq!(calls, 8);
    assert_eq!(items.len(), 2000);
    for (index, item) in items.iter().enumerate() {
        assert_eq!(item.owner, "owner-a");
        assert_eq!(item.key, keys[index]);
        assert_eq!(item.value, json!({ "index": index }));
    }
}
