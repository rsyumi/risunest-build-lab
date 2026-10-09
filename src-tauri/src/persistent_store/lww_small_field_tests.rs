use super::*;
use crate::persistent_store::{hash_work, RootMutation};
use serde_json::json;

fn fixture() -> (tempfile::TempDir, PersistentStore) {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    store.commit(&WorkingSetCommit {
        expected_revision: 0,
        root: Some(json!({"language":"en", "opaque":{"keep":true},
            "plugins":[{"name":"synthetic-plugin","script":"x".repeat(512 * 1024)}],
            "modules":[{"id":"old-module","name":"Old"}],
            "personas":[{"id":"old-persona","name":"Old"}],
            "explicitGlobalChatVariables":{"synthetic":"before"}})),
        add_character: Some(json!({"type":"character","chaId":"synthetic-character","name":"Before",
            "desc":"y".repeat(512 * 1024),"creatorNotes":"delete me","lastInteraction":1,
            "statics":{"messages":2,"tokens":1},"chats":[]})),
        ..Default::default()
    }).unwrap();
    (directory, store)
}

fn set(parts: &[&str], value: Value) -> UnitMutation {
    UnitMutation::Set { key: UnitKey::new(parts).unwrap(), value }
}

fn shared(store: &PersistentStore, parts: &[&str]) -> Option<Value> {
    let (_, value) = read_unit(&store.connection, &UnitKey::new(parts).unwrap()).unwrap()?;
    json_value_resolved(&store.connection, &value).unwrap()
}

fn assert_no_large_values_hashed(work: &hash_work::HashWork) {
    assert!(work.incomplete.is_empty());
    assert_eq!(work.domains.get("native_large_unit_identity").map_or(0, |work| work.bytes), 0);
    assert!(work.domains["native_intent"].bytes < 2_048);
}

#[test]
fn root_patch_keeps_the_intent_and_capture_independent_of_unrelated_payloads() {
    let (_directory, mut store) = fixture();
    let before = store.read_root(None).unwrap().value;
    hash_work::reset_hash_work();
    hash_work::reset_commit_intent_inputs();
    store.commit(&WorkingSetCommit {
        expected_revision: 1,
        root_mutations: Some(vec![
            RootMutation::Set { key: "language".into(), value: json!("ko") },
            RootMutation::Delete { key: "opaque".into() },
        ]),
        ..Default::default()
    }).unwrap();
    assert_no_large_values_hashed(&hash_work::take_hash_work());
    let intents = hash_work::take_commit_intent_inputs();
    assert_eq!(intents.len(), 1);
    let intent: Value = serde_json::from_slice(&intents[0]).unwrap();
    assert!(intent["commit"].get("root").is_none());
    assert_eq!(intent["commit"]["rootMutations"].as_array().unwrap().len(), 2);
    let after = store.read_root(None).unwrap();
    assert_eq!(after.revision, 2);
    assert_eq!(after.value["language"], "ko");
    assert!(after.value.get("opaque").is_none());
    assert_eq!(after.value["plugins"], before["plugins"]);
    assert_eq!(after.value["modules"], before["modules"]);
    assert_eq!(shared(&store, &["root", "language"]), Some(json!("ko")));
}

#[test]
fn patched_collections_capture_deletions_and_keep_unrelated_records_out() {
    let (_directory, mut store) = fixture();
    hash_work::reset_hash_work();
    store.commit(&WorkingSetCommit {
        expected_revision: 1,
        root_mutations: Some(vec![
            RootMutation::Set { key: "modules".into(), value: json!([{"id":"new-module","name":"New"}]) },
            RootMutation::Delete { key: "personas".into() },
            RootMutation::Set { key: "explicitGlobalChatVariables".into(), value: json!({"next":"after"}) },
        ]),
        ..Default::default()
    }).unwrap();
    assert_no_large_values_hashed(&hash_work::take_hash_work());
    assert_eq!(shared(&store, &["exists", "modules", "old-module"]), None);
    assert_eq!(shared(&store, &["exists", "modules", "new-module"]), Some(json!(true)));
    assert_eq!(shared(&store, &["order", "modules"]), Some(json!(["new-module"])));
    assert_eq!(shared(&store, &["exists", "persona", "old-persona"]), None);
    assert_eq!(shared(&store, &["variable", "synthetic"]), None);
    assert_eq!(shared(&store, &["variable", "next"]), Some(json!("after")));
}

#[test]
fn character_field_batch_preserves_unedited_body_and_filters_local_statics() {
    let (_directory, mut store) = fixture();
    hash_work::reset_hash_work();
    store.commit(&WorkingSetCommit {
        expected_revision: 1,
        unit_mutations: Some(vec![
            set(&["character", "synthetic-character", "name"], json!("After")),
            UnitMutation::Delete { key: UnitKey::new(&["character", "synthetic-character", "creatorNotes"]).unwrap() },
            set(&["character", "synthetic-character", "statics"], json!({"messages":99,"tokens":7})),
            set(&["character", "synthetic-character", "lastInteraction"], json!(2)),
        ]),
        ..Default::default()
    }).unwrap();
    assert_no_large_values_hashed(&hash_work::take_hash_work());
    let detail = store.read_character("synthetic-character", None).unwrap().unwrap().value;
    assert_eq!(detail["name"], "After");
    assert_eq!(detail["desc"].as_str().unwrap().len(), 512 * 1024);
    assert!(detail.get("creatorNotes").is_none());
    assert_eq!(detail["lastInteraction"], 2);
    assert_eq!(detail["statics"]["messages"], 99);
    assert_eq!(shared(&store, &["character", "synthetic-character", "name"]), Some(json!("After")));
    assert_eq!(shared(&store, &["character", "synthetic-character", "creatorNotes"]), None);
    assert_eq!(shared(&store, &["character", "synthetic-character", "statics"]), Some(json!({"tokens":7})));
}

#[test]
fn compact_root_intent_recovers_before_and_after_library_commit_exactly_once() {
    for applied in [false, true] {
        let (directory, mut store) = fixture();
        let header = Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: "synthetic-recovery".into() };
        let input = WorkingSetCommit {
            expected_revision: 1,
            root_mutations: Some(vec![RootMutation::Set { key: "language".into(), value: json!("ko") }]),
            unit_mutations: Some(vec![set(&["character", "synthetic-character", "lastInteraction"], json!(2))]),
            ..Default::default()
        };
        let intent = Intent::Commit { commit: input.clone(), aliases: vec![] };
        let (stamp, digest) = store.reserve_intent(&header, &intent).unwrap();
        if applied {
            commit::commit_lww(&mut store.connection, &input, &[], &header, &stamp, &digest).unwrap();
        }
        drop(store);
        let mut store = PersistentStore::open(directory.path()).unwrap();
        assert_eq!(store.revision().unwrap(), 2);
        assert_eq!(store.read_root(None).unwrap().value["language"], "ko");
        assert_eq!(store.read_character("synthetic-character", None).unwrap().unwrap().value["lastInteraction"], 2);
        store.lww_recover_intents().unwrap();
        assert_eq!(store.revision().unwrap(), 2);
        let pending: i64 = store.device_store().unwrap().connection().query_row("SELECT count(*) FROM lww_intents", [], |row| row.get(0)).unwrap();
        assert_eq!(pending, 0);
    }
}

#[test]
fn root_patch_and_later_unit_edit_keep_order_and_reject_stale_revisions() {
    let (_directory, mut store) = fixture();
    let input = WorkingSetCommit {
        expected_revision: 1,
        root_mutations: Some(vec![RootMutation::Set { key: "language".into(), value: json!("ko") }]),
        unit_mutations: Some(vec![set(&["root", "language"], json!("ja"))]),
        ..Default::default()
    };
    store.commit(&input).unwrap();
    assert_eq!(store.read_root(None).unwrap().value["language"], "ja");
    assert_eq!(shared(&store, &["root", "language"]), Some(json!("ja")));
    assert!(matches!(store.commit(&input), Err(StoreError::RevisionConflict { expected: 1, actual: 2 })));
    assert_eq!(store.read_root(None).unwrap().value["language"], "ja");
}

#[test]
fn scoped_capture_queues_exactly_the_units_a_small_save_changed() {
    let (_directory, mut store) = fixture();
    let delete = |parts: &[&str]| UnitMutation::Delete { key: UnitKey::new(parts).unwrap() };
    store.commit(&WorkingSetCommit {
        expected_revision: 1,
        request_id: Some("synthetic-scoped-save".into()),
        root_mutations: Some(vec![
            RootMutation::Set { key: "language".into(), value: json!("ko") },
            RootMutation::Delete { key: "opaque".into() },
            RootMutation::Set { key: "modules".into(), value: json!([{"id":"new-module","name":"New"}]) },
            RootMutation::Set { key: "characterOrder".into(), value: json!(["synthetic-character"]) },
        ]),
        unit_mutations: Some(vec![
            set(&["character", "synthetic-character", "name"], json!("After")),
            delete(&["character", "synthetic-character", "creatorNotes"]),
            delete(&["character", "synthetic-character", "notes"]),
            set(&["character", "synthetic-character", "statics"], json!({"messages":5,"tokens":1})),
            set(&["character", "synthetic-character", "lastInteraction"], json!(2)),
        ]),
        ..Default::default()
    }).unwrap();
    let queued = |table: &str| -> Vec<String> {
        let mut statement = store.connection
            .prepare(&format!("SELECT key FROM {table} WHERE version=?1 ORDER BY key")).unwrap();
        statement.query_map(["synthetic-scoped-save"], |row| row.get(0)).unwrap()
            .collect::<Result<_, _>>().unwrap()
    };
    let mut expected: Vec<String> = [
        &["root", "language"][..],
        &["exists", "modules", "new-module"],
        &["record", "modules", "new-module"],
        // Retiring the record covers its fields.
        &["exists", "modules", "old-module"],
        &["order", "modules"],
        &["order", "characters"],
        &["character", "synthetic-character", "name"],
        &["character", "synthetic-character", "creatorNotes"],
    ].iter().map(|parts| UnitKey::new(parts).unwrap().as_str().to_owned()).collect();
    expected.sort();
    assert_eq!(queued("lww_units"), expected);
    assert_eq!(queued("lww_outbox"), expected);
    assert_eq!(shared(&store, &["exists", "modules", "old-module"]), None);
    assert_eq!(shared(&store, &["character", "synthetic-character", "statics"]), Some(json!({"tokens":1})));
    let detail = store.read_character("synthetic-character", None).unwrap().unwrap().value;
    assert_eq!(detail["statics"]["messages"], 5);
    assert_eq!(detail["lastInteraction"], 2);
    assert!(detail.get("notes").is_none());
    let root = store.read_root(None).unwrap().value;
    assert!(root.get("opaque").is_none());
    assert_eq!(root["personas"][0]["id"], "old-persona");
}
