use super::*;
use crate::persistent_store::{device_store, lww, RootMutation};
use crate::native_generation::service::{self, PreparedGeneration};
use std::{panic::{catch_unwind, AssertUnwindSafe}, path::Path, process::Command, sync::{mpsc, Barrier}, thread};

fn fixture() -> (tempfile::TempDir, PersistentStore) {
    fixture_with_conversations(1)
}

fn fixture_with_conversations(count: usize) -> (tempfile::TempDir, PersistentStore) {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let mut database: Value = serde_json::from_str(include_str!("../../fixtures/persistent-fixture.json")).unwrap();
    database["characters"][0]["chats"][0]["futureMetadata"] = json!({"keep": [1, 2]});
    database["characters"][0]["chats"][0]["message"][0]["futureMessage"] = json!({"keep": true});
    let prototype = database["characters"][0]["chats"][0].clone();
    for index in 1..count {
        let mut conversation = prototype.clone();
        conversation["id"] = json!(format!("conv-native-{index}"));
        database["characters"][0]["chats"].as_array_mut().unwrap().push(conversation);
    }
    let characters = database.as_object_mut().unwrap().remove("characters").unwrap();
    let presets = database.as_object_mut().unwrap().remove("botPresets").unwrap();
    let stage = store.replace_begin().unwrap();
    store.replace_put_root(&stage.staging_id, &database).unwrap();
    store.replace_put_presets(&stage.staging_id, presets.as_array().unwrap()).unwrap();
    store.replace_add_characters(&stage.staging_id, characters.as_array().unwrap()).unwrap();
    store.replace_commit(&stage.staging_id, Some(0)).unwrap();
    (directory, store)
}

fn target(store: &PersistentStore) -> Target {
    Target { character_id: "char-b".into(), conversation_id: "conv-beta".into(),
        expected_revision: store.revision().unwrap(), library_identity: store.library_identity().unwrap(),
        binding_authority: store.lww_binding_authority().unwrap() }
}

fn start(store: &mut PersistentStore) -> (Owner, StoredHistory) {
    let ticket = store.generation_reserve(target(store)).unwrap();
    match store.generation_start(&ticket).unwrap() {
        Start::Started { owner, history, status } => {
            assert_eq!(status.phase, Phase::Running);
            (owner, history)
        }
        Start::Existing(_) => panic!("first start did not obtain ownership"),
    }
}

fn checkpoint(sequence: u64, text: &str) -> Checkpoint {
    Checkpoint { sequence, text: text.into() }
}

fn conversation(store: &PersistentStore) -> Value {
    store.read_conversation("char-b", "conv-beta", None).unwrap().unwrap().value
}

fn edit_message(store: &mut PersistentStore, data: &str) {
    let mut message = conversation(store)["message"][0].clone();
    message["data"] = json!(data);
    store.commit(&WorkingSetCommit {
        expected_revision: store.revision().unwrap(),
        conversations: Some(vec![ConversationMutation::ReplaceRange {
            character_id: "char-b".into(), conversation_id: "conv-beta".into(), start: 0, delete_count: 1,
            messages: vec![message], conversation: None, configured_index: None,
        }]), ..Default::default()
    }).unwrap();
}

fn assert_error<T: std::fmt::Debug>(result: StoreResult<T>, code: &str) {
    assert!(matches!(result, Err(StoreError::Validation { ref message }) if message == &format!("native-generation:{code}")), "{result:?}");
}

fn protection_target() -> NativeGenerationTarget<'static> {
    NativeGenerationTarget::Conversation { character_id: "char-b", conversation_id: "conv-beta" }
}

fn journal_body(store: &PersistentStore) -> Option<String> {
    store.device_store().unwrap().connection().query_row(
        "SELECT value FROM device_settings WHERE key=?1", [JOURNAL_KEY], |row| row.get(0)).optional().unwrap()
}

fn with_protection_guards<T>(store: &PersistentStore, read: impl FnOnce(&Connection, &Connection) -> T) -> T {
    let device = rusqlite::Transaction::new_unchecked(store.device_store().unwrap().connection(), rusqlite::TransactionBehavior::Immediate).unwrap();
    let library = rusqlite::Transaction::new_unchecked(&store.connection, rusqlite::TransactionBehavior::Immediate).unwrap();
    read(&device, &library)
}

fn protected(store: &PersistentStore, target: NativeGenerationTarget<'_>) -> StoreResult<bool> {
    let before = journal_body(store);
    let revision = store.revision().unwrap();
    let changes = |connection: &Connection| connection.query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0)).unwrap();
    let device_changes = changes(store.device_store().unwrap().connection());
    let library_changes = changes(&store.connection);
    let result = with_protection_guards(store, |device, library| has_protected_target(device, library, target));
    assert_eq!(journal_body(store), before);
    assert_eq!(store.revision().unwrap(), revision);
    assert_eq!(changes(store.device_store().unwrap().connection()), device_changes);
    assert_eq!(changes(&store.connection), library_changes);
    result
}

#[test]
fn protected_target_tracks_native_lifecycle_and_parent_scope() {
    let (_directory, mut store) = fixture_with_conversations(2);
    assert!(!protected(&store, protection_target()).unwrap());
    let ticket = store.generation_reserve(target(&store)).unwrap();
    assert!(!protected(&store, protection_target()).unwrap());
    let Start::Started { owner, .. } = store.generation_start(&ticket).unwrap() else { panic!("not started") };
    assert!(protected(&store, protection_target()).unwrap());
    assert!(protected(&store, NativeGenerationTarget::Character { character_id: "char-b" }).unwrap());
    assert!(!protected(&store, NativeGenerationTarget::Conversation { character_id: "char-b", conversation_id: "conv-native-1" }).unwrap());
    assert!(!protected(&store, NativeGenerationTarget::Character { character_id: "other" }).unwrap());
    assert!(!protected(&store, NativeGenerationTarget::Character { character_id: &"x".repeat(513) }).unwrap());
    store.generation_finish(&owner, false).unwrap();
    assert!(!protected(&store, protection_target()).unwrap());
    let (owner, _) = start(&mut store);
    store.generation_finish(&owner, true).unwrap();
    assert!(!protected(&store, protection_target()).unwrap());

    let (_directory, mut failed, intent) = effects_fixture(json!({"preset":{"maxContext":1}}));
    let ticket = service::reserve_ollama(&mut failed, &intent).unwrap();
    assert!(matches!(service::prepare_reserved_ollama(&mut failed, &ticket).unwrap(), PreparedGeneration::Failed { .. }));
    assert!(!protected(&failed, NativeGenerationTarget::Character { character_id: "character" }).unwrap());
}

#[test]
fn protected_target_survives_runtime_restart_until_explicit_recovery() {
    const TEST: &str = "protected_target_survives_runtime_restart_until_explicit_recovery";
    if let Some(root) = restart_root(TEST) {
        let mut store = PersistentStore::open(&root).unwrap();
        let journal = read_journal(store.device_store().unwrap().connection()).unwrap().unwrap();
        assert!(journal.records.values().all(|record| record.session.as_deref() != Some(runtime_session_id())));
        assert!(protected(&store, protection_target()).unwrap());
        assert_eq!(store.generation_reconcile_orphans().unwrap()[0].phase, Phase::Interrupted);
        assert!(!protected(&store, protection_target()).unwrap());
        return;
    }
    let (directory, mut store) = fixture();
    start(&mut store);
    drop(store);
    run_in_restarted_process(TEST, directory.path());
}

#[test]
fn protected_target_retains_pending_id_and_output_proof_across_phase_and_runtime() {
    for history_ids in [false, true] {
        for receipt_exists in [false, true] {
            let (_directory, mut store, owner) = if history_ids {
                let (directory, mut store, intent) = effects_fixture(json!({}));
                let owner = effects_owner(&mut store, &intent);
                if receipt_exists { lagging_id_journal(&mut store, &owner); }
                else { interrupt_ids_after_intent(&mut store, &owner); }
                (directory, store, owner)
            } else {
                let (directory, mut store) = fixture();
                let (owner, _) = start(&mut store);
                if receipt_exists { checkpoint_with_lagging_journal(&mut store, &owner); }
                else { interrupt_after_reservation(&mut store, &owner); }
                (directory, store, owner)
            };
            let query = if history_ids { NativeGenerationTarget::Character { character_id: "character" } } else { protection_target() };
            let record = store.generation_record(&owner.ticket).unwrap();
            let attempt = record.attempt.as_ref().unwrap();
            let actual_receipt: bool = store.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM lww_requests WHERE request_id=?1)", [&attempt.request_id], |row| row.get(0)).unwrap();
            assert_eq!(actual_receipt, receipt_exists);
            assert!(protected(&store, query).unwrap());
            let mut journal = read_journal(store.device_store().unwrap().connection()).unwrap().unwrap();
            let pending = journal.records.get_mut(&owner.ticket.sequence.0).unwrap();
            pending.session = Some(uuid::Uuid::new_v4().to_string());
            // A terminal label cannot discard an unresolved persisted segment.
            pending.phase = Phase::Interrupted;
            write_journal(store.device_store().unwrap().connection(), &journal).unwrap();
            assert!(protected(&store, query).unwrap());
            assert_eq!(store.generation_record(&owner.ticket).unwrap().attempt, record.attempt);
        }
    }
}

#[test]
fn protected_target_uses_guarded_library_and_binding_identity() {
    for replacement in [false, true] {
        let (_directory, mut store) = fixture();
        let (owner, _) = start(&mut store);
        checkpoint_with_lagging_journal(&mut store, &owner);
        let before = journal_body(&store);
        assert!(protected(&store, protection_target()).unwrap());
        if replacement {
            let mut database: Value = serde_json::from_str(include_str!("../../fixtures/persistent-fixture.json")).unwrap();
            let characters = database.as_object_mut().unwrap().remove("characters").unwrap();
            let presets = database.as_object_mut().unwrap().remove("botPresets").unwrap();
            let stage = store.replace_begin().unwrap();
            store.replace_put_root(&stage.staging_id, &database).unwrap();
            store.replace_put_presets(&stage.staging_id, presets.as_array().unwrap()).unwrap();
            store.replace_add_characters(&stage.staging_id, characters.as_array().unwrap()).unwrap();
            store.replace_commit(&stage.staging_id, Some(store.revision().unwrap())).unwrap();
            assert!(store.read_conversation("char-b", "conv-beta", None).unwrap().is_some());
        } else {
            let selection = super::super::sync_selection::read(&store.connection).unwrap();
            store.lww_switch_target(&lww::Header {
                binding_authority: store.lww_binding_authority().unwrap(), request_id: uuid::Uuid::new_v4().to_string(),
            }, &super::super::sync_selection::BindingSelectionChange {
                expected_epoch: selection.epoch, new_epoch: uuid::Uuid::new_v4().to_string(),
                target: super::super::sync_selection::SyncTarget::None,
                library_id: None, target_id: None, inspection_id: None, initial_publication: false,
            }).unwrap();
        }
        assert!(!protected(&store, protection_target()).unwrap());
        assert_eq!(journal_body(&store), before);
    }
}

#[test]
fn protected_target_refuses_missing_identity_or_device_access_even_without_journal() {
    for corruption in ["empty-library", "malformed-library", "authority", "device-table"] {
        let (_directory, store) = fixture();
        match corruption {
            "empty-library" => { store.connection.execute("UPDATE meta SET value='\"\"' WHERE key='activeGeneration'", []).unwrap(); }
            "malformed-library" => { store.connection.execute("UPDATE meta SET value='false' WHERE key='activeGeneration'", []).unwrap(); }
            "authority" => { store.device_store().unwrap().connection().execute("UPDATE lww_clock SET binding_authority='invalid'", []).unwrap(); }
            _ => { store.device_store().unwrap().connection().execute("DROP TABLE device_settings", []).unwrap(); }
        }
        assert!(with_protection_guards(&store, |device, library| has_protected_target(device, library, protection_target())).is_err());
    }
    let (_directory, store) = fixture();
    let unavailable = Connection::open_in_memory().unwrap();
    let device = rusqlite::Transaction::new_unchecked(&unavailable, rusqlite::TransactionBehavior::Immediate).unwrap();
    let library = rusqlite::Transaction::new_unchecked(&store.connection, rusqlite::TransactionBehavior::Immediate).unwrap();
    assert!(has_protected_target(&device, &library, protection_target()).is_err());
}

#[test]
fn protected_target_requires_both_existing_write_transactions() {
    let (_directory, store) = fixture();
    assert_error(has_protected_target(store.device_store().unwrap().connection(), &store.connection, protection_target()), "protection-write-guard");
    use rusqlite::TransactionBehavior::{Deferred, Immediate};
    for (device_behavior, library_behavior) in [(Deferred, Deferred), (Immediate, Deferred), (Deferred, Immediate), (Immediate, Immediate)] {
        let device = rusqlite::Transaction::new_unchecked(store.device_store().unwrap().connection(), device_behavior).unwrap();
        let library = rusqlite::Transaction::new_unchecked(&store.connection, library_behavior).unwrap();
        device.query_row("SELECT COUNT(*) FROM device_settings", [], |row| row.get::<_, i64>(0)).unwrap();
        library.query_row("SELECT COUNT(*) FROM meta", [], |row| row.get::<_, i64>(0)).unwrap();
        let result = has_protected_target(&device, &library, protection_target());
        if matches!(device_behavior, Immediate) && matches!(library_behavior, Immediate) { assert!(!result.unwrap()); }
        else { assert_error(result, "protection-write-guard"); }
    }
    assert_error(protected(&store, NativeGenerationTarget::Character { character_id: "" }), "invalid-target");
    assert_error(protected(&store, NativeGenerationTarget::Conversation { character_id: "char-b", conversation_id: "" }), "invalid-target");
}

#[test]
fn protected_target_rejects_malformed_present_journal_without_erasing_proof() {
    let (_directory, mut store) = fixture();
    let (owner, _) = start(&mut store);
    interrupt_after_reservation(&mut store, &owner);
    let original = journal_body(&store).unwrap();
    let original_value: Value = serde_json::from_str(&original).unwrap();
    let key = owner.ticket.sequence.0.to_string();
    let mut cases = vec!["{".to_owned()];
    for corruption in ["owner", "session", "attempt-missing", "digest", "request", "output-hash", "output-sequence", "library", "revision", "epoch", "sequence", "high-water"] {
        let mut value = original_value.clone();
        let record = &mut value["journal"]["records"][&key];
        match corruption {
            "owner" => record["owner"] = Value::Null,
            "session" => record["session"] = json!("missing-native-session"),
            "attempt-missing" => { record.as_object_mut().unwrap().remove("attempt"); record["phase"] = json!("completed"); }
            "digest" => record["attempt"]["intent_digest"] = json!("invalid"),
            "request" => record["attempt"]["request_id"] = json!("invalid"),
            "output-hash" => record["attempt"]["kind"]["Output"]["output_hash"] = json!("invalid"),
            "output-sequence" => record["attempt"]["kind"]["Output"]["sequence"] = json!(0),
            "library" => record["target"]["libraryIdentity"] = json!(""),
            "revision" => record["target"]["expectedRevision"] = json!(-1),
            "epoch" => value["journal"]["epoch"] = json!("invalid"),
            "sequence" => {
                let row = value["journal"]["records"].as_object_mut().unwrap().remove(&key).unwrap();
                value["journal"]["records"]["0"] = row;
            }
            _ => value["journal"]["high_water"] = json!(0),
        }
        cases.push(serde_json::to_string(&value).unwrap());
    }
    let record = serde_json::to_string(&original_value["journal"]["records"][&key]).unwrap();
    for second_key in [key.clone(), format!("0{key}")] {
        cases.push(format!("{{\"version\":2,\"journal\":{{\"epoch\":{},\"high_water\":{},\"records\":{{\"{}\":{},\"{}\":{}}}}}}}",
            original_value["journal"]["epoch"], owner.ticket.sequence.0, key, record, second_key, record));
    }
    for body in cases {
        store.device_store().unwrap().connection().execute("UPDATE device_settings SET value=?2 WHERE key=?1", params![JOURNAL_KEY, body]).unwrap();
        assert!(protected(&store, NativeGenerationTarget::Character { character_id: "unrelated" }).is_err());
        assert_eq!(journal_body(&store).as_deref(), Some(body.as_str()));
    }
}

#[test]
fn protected_target_rejects_incomplete_history_id_proof() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let owner = effects_owner(&mut store, &intent);
    interrupt_ids_after_intent(&mut store, &owner);
    let original: Value = serde_json::from_str(&journal_body(&store).unwrap()).unwrap();
    let key = owner.ticket.sequence.0.to_string();
    for corruption in ["preparation", "assignments", "empty", "index", "order", "uuid", "hash"] {
        let mut value = original.clone();
        let preparation = &mut value["journal"]["records"][&key]["preparation"];
        match corruption {
            "preparation" => *preparation = Value::Null,
            "assignments" => preparation["normalization"]["Pending"]["assignments"] = Value::Null,
            "empty" => preparation["normalization"]["Pending"]["assignments"] = json!([]),
            other => {
                let assignments = &mut preparation["normalization"]["Pending"]["assignments"];
                match other {
                    "index" => assignments[0]["index"] = json!(4096),
                    "order" => assignments[1]["index"] = assignments[0]["index"].clone(),
                    "uuid" => assignments[0]["id"] = json!("invalid"),
                    _ => assignments[0]["original_hash"] = json!("invalid"),
                }
            }
        }
        store.device_store().unwrap().connection().execute("UPDATE device_settings SET value=?2 WHERE key=?1", params![JOURNAL_KEY, serde_json::to_string(&value).unwrap()]).unwrap();
        assert_error(protected(&store, NativeGenerationTarget::Character { character_id: "character" }), "protection-attempt");
    }
}

#[test]
fn protected_target_enforces_exact_byte_and_record_budgets_without_pruning() {
    let (_directory, mut store) = fixture();
    let (owner, _) = start(&mut store);
    let mut extended: Value = serde_json::from_str(&journal_body(&store).unwrap()).unwrap();
    extended["future"] = json!("한글");
    store.device_store().unwrap().connection().execute("UPDATE device_settings SET value=?2 WHERE key=?1", params![JOURNAL_KEY, serde_json::to_string(&extended).unwrap()]).unwrap();
    let body = journal_body(&store).unwrap();
    with_protection_guards(&store, |device, library| {
        assert!(has_protected_target_limited(device, library, protection_target(), body.len()).unwrap());
        assert_error(has_protected_target_limited(device, library, protection_target(), body.len() - 1), "protection-journal-limit");
    });
    assert_eq!(journal_body(&store).unwrap(), body);
    let record = store.generation_record(&owner.ticket).unwrap();
    for (phase, limit) in [(Phase::Reserved, MAX_RESERVED), (Phase::Running, MAX_ACTIVE), (Phase::Completed, MAX_TERMINAL)] {
        let mut journal = Journal::new();
        for sequence in 1..=limit as u64 {
            let mut row = record.clone(); row.phase = phase.clone();
            journal.records.insert(sequence, row);
        }
        journal.high_water = limit as u64;
        write_journal(store.device_store().unwrap().connection(), &journal).unwrap();
        assert_eq!(protected(&store, protection_target()).unwrap(), phase == Phase::Running);
        journal.high_water += 1;
        let mut row = record.clone(); row.phase = phase;
        journal.records.insert(journal.high_water, row);
        write_journal(store.device_store().unwrap().connection(), &journal).unwrap();
        assert_error(protected(&store, protection_target()), "protection-journal-limit");
    }
    let mut journal = Journal::new();
    for (phase, count) in [(Phase::Reserved, MAX_RESERVED), (Phase::Running, MAX_ACTIVE), (Phase::Completed, MAX_TERMINAL)] {
        for _ in 0..count {
            journal.high_water += 1;
            let mut row = record.clone(); row.phase = phase.clone();
            journal.records.insert(journal.high_water, row);
        }
    }
    write_journal(store.device_store().unwrap().connection(), &journal).unwrap();
    assert!(protected(&store, protection_target()).unwrap());
    journal.high_water += 1;
    journal.records.insert(journal.high_water, record);
    write_journal(store.device_store().unwrap().connection(), &journal).unwrap();
    assert!(protected(&store, protection_target()).is_err());
}

#[test]
fn protected_target_enforces_frozen_assignment_budget() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let owner = effects_owner(&mut store, &intent);
    interrupt_ids_after_intent(&mut store, &owner);
    let mut journal = read_journal(store.device_store().unwrap().connection()).unwrap().unwrap();
    let record = journal.records.get_mut(&owner.ticket.sequence.0).unwrap();
    record.initial_count = MAX_HISTORY_MESSAGES + 1;
    let assignments: Vec<_> = (0..MAX_HISTORY_MESSAGES as usize).map(|index| IdAssignment {
        index, original_hash: "0".repeat(64), id: uuid::Uuid::from_u128(index as u128 + 1).to_string(),
    }).collect();
    record.preparation.as_mut().unwrap().normalization = Normalization::Pending { assignments: Some(assignments) };
    write_journal(store.device_store().unwrap().connection(), &journal).unwrap();
    assert!(protected(&store, NativeGenerationTarget::Character { character_id: "character" }).unwrap());
    let Normalization::Pending { assignments: Some(assignments) } = &mut journal.records.get_mut(&owner.ticket.sequence.0).unwrap().preparation.as_mut().unwrap().normalization else { unreachable!() };
    assignments.push(IdAssignment { index: MAX_HISTORY_MESSAGES as usize, original_hash: "0".repeat(64), id: uuid::Uuid::new_v4().to_string() });
    write_journal(store.device_store().unwrap().connection(), &journal).unwrap();
    assert_error(protected(&store, NativeGenerationTarget::Character { character_id: "character" }), "protection-attempt");
}

#[test]
fn receive_write_guards_exclude_native_start_and_attempt_admission() {
    let (_directory, mut store) = fixture_with_conversations(2);
    let ticket = store.generation_reserve(target(&store)).unwrap();
    let mut other = target(&store); other.conversation_id = "conv-native-1".into();
    let other_ticket = store.generation_reserve(other).unwrap();
    let Start::Started { owner, .. } = store.generation_start(&other_ticket).unwrap() else { panic!("not started") };
    let mut worker = store.open_native_job_store().unwrap();
    worker.device_store().unwrap().connection().busy_timeout(Duration::ZERO).unwrap();
    let before = journal_body(&store);
    let revision = store.revision().unwrap();
    {
        let device = rusqlite::Transaction::new_unchecked(store.device_store().unwrap().connection(), rusqlite::TransactionBehavior::Immediate).unwrap();
        let library = rusqlite::Transaction::new_unchecked(&store.connection, rusqlite::TransactionBehavior::Immediate).unwrap();
        assert!(!has_protected_target(&device, &library, protection_target()).unwrap());
        let busy = worker.device_store().unwrap().connection().execute_batch("BEGIN IMMEDIATE").unwrap_err();
        assert_eq!(busy.sqlite_error_code(), Some(rusqlite::ErrorCode::DatabaseBusy));
        assert_eq!(worker.generation_start(&ticket).unwrap_err().to_string(), busy.to_string());
        assert_eq!(worker.generation_checkpoint(&owner, &checkpoint(1, "after receive")).unwrap_err().to_string(), busy.to_string());
        assert_eq!(journal_body(&store), before);
        assert_eq!(store.revision().unwrap(), revision);
        assert!(!has_protected_target(&device, &library, protection_target()).unwrap());
        library.commit().unwrap();
        assert!(worker.device_store().unwrap().connection().execute_batch("BEGIN IMMEDIATE").is_err());
        device.commit().unwrap();
    }
    assert!(matches!(worker.generation_start(&ticket).unwrap(), Start::Started { .. }));
    worker.generation_checkpoint(&owner, &checkpoint(1, "after receive")).unwrap();
    assert!(protected(&store, protection_target()).unwrap());
    assert_eq!(worker.generation_status(&other_ticket).unwrap().checkpoint.0, 1);
}

#[test]
fn native_commit_barrier_excludes_receive_admission_until_segment_release() {
    let (_directory, mut store) = fixture();
    let (owner, _) = start(&mut store);
    let mut writer = store.open_native_job_store().unwrap();
    let receive = store.open_native_job_store().unwrap();
    receive.device_store().unwrap().connection().busy_timeout(Duration::ZERO).unwrap();
    let revision = store.revision().unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer_owner = owner.clone();
    let writing = thread::spawn(move || {
        lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            entered_tx.send(()).unwrap(); release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        })));
        writer.generation_checkpoint(&writer_owner, &checkpoint(1, "barrier-protected output"))
    });
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let pending = journal_body(&receive);
    let admission = receive.device_store().unwrap().connection().execute_batch("BEGIN IMMEDIATE");
    let unchanged = journal_body(&receive) == pending && receive.revision().unwrap() == revision;
    release_tx.send(()).unwrap();
    let result = writing.join().unwrap().unwrap();
    assert_eq!(admission.unwrap_err().sqlite_error_code(), Some(rusqlite::ErrorCode::DatabaseBusy));
    assert!(unchanged);
    assert_eq!(result.checkpoint.0, 1);
    assert!(protected(&receive, protection_target()).unwrap());
    assert_eq!(conversation(&receive)["message"][3]["data"], "barrier-protected output");
    store.generation_finish(&owner, false).unwrap();
    assert!(!protected(&receive, protection_target()).unwrap());
}

fn restart_root(test: &str) -> Option<std::path::PathBuf> {
    (std::env::var("RISUNEST_NATIVE_GENERATION_RESTART_TEST").ok().as_deref() == Some(test))
        .then(|| std::env::var_os("RISUNEST_NATIVE_GENERATION_RESTART_ROOT").expect("child store path").into())
}

fn run_in_restarted_process(test: &str, root: &Path) {
    let module = module_path!().split_once("::").unwrap().1;
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &format!("{module}::{test}"), "--nocapture"])
        .env("RISUNEST_NATIVE_GENERATION_RESTART_TEST", test)
        .env("RISUNEST_NATIVE_GENERATION_RESTART_ROOT", root)
        .status().expect("start isolated Rust test process");
    assert!(status.success(), "restarted process failed: {status}");
}

fn effects_fixture(patch: Value) -> (tempfile::TempDir, PersistentStore, OllamaIntent) {
    let mut case = patch;
    case["messages"] = super::preparation_tests::cases()["effectMessages"].clone();
    super::preparation_tests::stored(&super::preparation_tests::database(&case))
}

fn effects_conversation(store: &PersistentStore) -> Value {
    store.read_conversation("character", "conversation", None).unwrap().unwrap().value
}

fn effects_owner(store: &mut PersistentStore, intent: &OllamaIntent) -> Owner {
    let ticket = service::reserve_ollama(store, intent).unwrap();
    match store.generation_start_preparation(&ticket).unwrap() {
        Start::Started { owner, .. } => owner,
        other => panic!("{other:?}"),
    }
}

fn projection_lease(store: &mut PersistentStore) -> GenerationLease {
    let revision = store.revision().unwrap();
    GenerationLease { token: store.acquire_revision(revision).unwrap().lease, revision }
}

fn projection_reserved(patch: Value) -> (tempfile::TempDir, PersistentStore, Ticket, GenerationLease, NormalizationWitness) {
    let (directory, mut store, intent) = effects_fixture(patch);
    let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
    let original = projection_lease(&mut store);
    let witness = service::capture_normalization_witness(&store, &ticket, &original, &(0..8).collect::<Vec<_>>()).unwrap();
    (directory, store, ticket, original, witness)
}

fn projected(store: &PersistentStore, ticket: &Ticket, witness: &NormalizationWitness, original: &GenerationLease, current: &GenerationLease) -> NormalizationProjectionResult {
    let before = journal_body(store);
    let revision = store.revision().unwrap();
    let device_revision = store.device_store().unwrap().revision().unwrap();
    let readers = store.revision_leases.len();
    let result = service::project_normalization(store, ticket, witness, original, current).unwrap();
    assert_eq!(journal_body(store), before);
    assert_eq!(store.revision().unwrap(), revision);
    assert_eq!(store.device_store().unwrap().revision().unwrap(), device_revision);
    assert_eq!(store.revision_leases.len(), readers);
    result
}

fn mapped(result: NormalizationProjectionResult) -> NormalizationProjection {
    assert!(matches!(result.evidence, NormalizationEvidence::Verified(_)), "{result:?}");
    match result.mapping { NormalizationMapping::Mapped(value) => value, other => panic!("{other:?}") }
}

fn edit_effect_history(store: &mut PersistentStore, start: i64, delete_count: i64, messages: Vec<Value>) {
    store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), conversations: Some(vec![ConversationMutation::ReplaceRange {
        character_id: "character".into(), conversation_id: "conversation".into(), start, delete_count, messages,
        conversation: None, configured_index: None,
    }]), ..Default::default() }).unwrap();
}

#[test]
fn normalization_projection_reconstructs_compacted_assignments_without_writes_or_history_payloads() {
    let (_directory, mut store, ticket, original, witness) = projection_reserved(json!({}));
    let before = effects_conversation(&store);
    assert!(matches!(service::prepare_reserved_ollama(&mut store, &ticket).unwrap(), PreparedGeneration::Ready { .. }));
    assert!(matches!(store.generation_record(&ticket).unwrap().preparation.unwrap().normalization, Normalization::Settled { receipt: Some(_) }));
    let current = projection_lease(&mut store);
    let result = projected(&store, &ticket, &witness, &original, &current);
    assert_eq!(projected(&store, &ticket, &witness, &original, &current), result);
    let value = mapped(result);
    let after = effects_conversation(&store);
    let ids = assert_nullish_effect(&before, &after);
    assert_eq!(value.rows.iter().map(|row| row.assigned_id.clone()).collect::<Vec<_>>(), ids);
    assert_eq!(value.rows.iter().map(|row| row.original_index).collect::<Vec<_>>(), vec![0, 1, 4, 6]);
    assert_eq!(value.rows.iter().map(|row| row.original_id_state.clone()).collect::<Vec<_>>(), vec![OriginalIdState::Missing, OriginalIdState::Null, OriginalIdState::Null, OriginalIdState::Missing]);
    for row in &value.rows {
        assert_eq!(row.current_index, row.original_index);
        assert_eq!(row.original_row_hash, row_hash(&before["message"][row.original_index]).unwrap());
    }
    assert_eq!((value.baseline_revision, value.receipt_revision, value.projected_revision), (original.revision, current.revision, current.revision));
    assert_eq!((value.original_count, value.current_count, value.prefix_count), (8, 8, 8));
    let subset = service::capture_normalization_witness(&store, &ticket, &original, &[4]).unwrap();
    assert_eq!(mapped(projected(&store, &ticket, &subset, &original, &current)).rows, vec![value.rows[2].clone()]);
    store.release_revision(&current.token).unwrap();
    store.release_revision(&original.token).unwrap();
    assert!(store.revision_leases.is_empty());
}

#[test]
fn normalization_projection_keeps_receipt_revision_distinct_from_metadata_and_output_progress() {
    let mut database = super::preparation_tests::database(&json!({"messages":super::preparation_tests::cases()["effectMessages"]}));
    let mut other = database["characters"][0]["chats"][0].clone(); other["id"] = json!("other");
    database["characters"][0]["chats"].as_array_mut().unwrap().push(other);
    let (_directory, mut store, intent) = super::preparation_tests::stored(&database);
    let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
    let original = projection_lease(&mut store);
    let witness = service::capture_normalization_witness(&store, &ticket, &original, &(0..8).collect::<Vec<_>>()).unwrap();
    let PreparedGeneration::Ready { owner, .. } = service::prepare_reserved_ollama(&mut store, &ticket).unwrap() else { panic!("not ready") };
    let receipt_revision = store.revision().unwrap();
    store.commit(&WorkingSetCommit { expected_revision: receipt_revision,
        unit_mutations: Some(vec![lww::UnitMutation::Set { key: UnitKey::new(&["root", "language"]).unwrap(), value: json!("ko") },
            lww::UnitMutation::Set { key: UnitKey::new(&["conversation", "character", "conversation", "note"]).unwrap(), value: json!("new metadata") },
            lww::UnitMutation::Set { key: UnitKey::new(&["conversation", "character", "conversation", "scriptstate"]).unwrap(), value: json!({"newer":"variable"}) }]),
        conversations: Some(vec![ConversationMutation::ReplaceRange { character_id: "character".into(), conversation_id: "other".into(),
            start: 0, delete_count: 1, messages: vec![json!({"role":"user","data":"Other conversation edit.","chatId":"other-edit"})],
            conversation: None, configured_index: None }]),
        ..Default::default() }).unwrap();
    let metadata = projection_lease(&mut store);
    let value = mapped(projected(&store, &ticket, &witness, &original, &metadata));
    assert_eq!(value.receipt_revision, receipt_revision);
    assert!(value.projected_revision > receipt_revision);
    let status = store.generation_checkpoint(&owner, &checkpoint(1, "owned output")).unwrap();
    let output = projection_lease(&mut store);
    let value = mapped(projected(&store, &ticket, &witness, &original, &output));
    assert_eq!(value.receipt_revision, receipt_revision);
    assert_eq!(Some(value.projected_revision), status.committed_revision);
    assert_eq!((value.current_count, value.prefix_count), (9, 8));
    assert!(value.rows.iter().all(|row| row.current_index < 8));
    store.generation_checkpoint(&owner, &checkpoint(2, "owned output")).unwrap();
    assert_eq!(mapped(projected(&store, &ticket, &witness, &original, &output)), value);
    assert_eq!(effects_conversation(&store)["note"], "new metadata");
    assert_eq!(effects_conversation(&store)["scriptstate"], json!({"newer":"variable"}));
    assert_eq!(store.read_conversation("character", "other", None).unwrap().unwrap().value["message"][0]["data"], "Other conversation edit.");
}

#[test]
fn normalization_projection_rejects_foreign_duplicate_origin_and_same_value_aba() {
    for change in ["duplicate-insert", "append", "aba", "delete", "reorder", "owner-delete"] {
        let duplicate = json!({"role":"char","data":"Same disabled row.","chatId":"duplicate","disabled":true});
        let database = super::preparation_tests::database(&json!({"messages":[
            {"role":"user","data":"Question."}, duplicate, duplicate,
        ]}));
        let (_directory, mut store, intent) = super::preparation_tests::stored(&database);
        let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
        let original = projection_lease(&mut store);
        let witness = service::capture_normalization_witness(&store, &ticket, &original, &[0, 1, 2]).unwrap();
        service::prepare_reserved_ollama(&mut store, &ticket).unwrap();
        let before = effects_conversation(&store);
        let normalized = before["message"].as_array().unwrap().clone();
        let current = projection_lease(&mut store);
        assert_eq!(mapped(projected(&store, &ticket, &witness, &original, &current)).prefix_count, 3);
        match change {
            "duplicate-insert" => edit_effect_history(&mut store, 2, 0, vec![duplicate.clone()]),
            "append" => edit_effect_history(&mut store, 3, 0, vec![duplicate.clone()]),
            "aba" => {
                let mut row = normalized[0].clone(); row["data"] = json!("changed");
                edit_effect_history(&mut store, 0, 1, vec![row]);
                edit_effect_history(&mut store, 0, 1, vec![normalized[0].clone()]);
            }
            "delete" => edit_effect_history(&mut store, 0, 1, Vec::new()),
            "owner-delete" => {
                store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(),
                    conversations: Some(vec![ConversationMutation::Delete { character_id: "character".into(), conversation_id: "conversation".into() }]), ..Default::default() }).unwrap();
                let recreate = store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(),
                    conversations: Some(vec![ConversationMutation::ReplaceRange { character_id: "character".into(), conversation_id: "conversation".into(),
                        start: 0, delete_count: 0, messages: normalized.clone(), conversation: Some(before), configured_index: None }]), ..Default::default() });
                assert!(matches!(recreate, Err(StoreError::Validation { message }) if message == "retired-record-id"));
            }
            _ => edit_effect_history(&mut store, 0, 3, vec![normalized[1].clone(), normalized[0].clone(), normalized[2].clone()]),
        }
        if change == "duplicate-insert" {
            assert_eq!(&effects_conversation(&store)["message"].as_array().unwrap()[..3], normalized);
        }
        let changed = projection_lease(&mut store);
        let result = projected(&store, &ticket, &witness, &original, &changed);
        assert!(matches!(result.evidence, NormalizationEvidence::Verified(_)), "{change}: {result:?}");
        assert_eq!(result.mapping, unavailable(if change == "owner-delete" { ProjectionUnavailable::HistoryChanged } else { ProjectionUnavailable::MessageFenceChanged }), "{change}");
    }
}

#[test]
fn normalization_projection_refuses_stale_or_foreign_leases_and_forged_resident_witnesses() {
    let (_directory, mut store, ticket, original, witness) = projection_reserved(json!({}));
    service::prepare_reserved_ollama(&mut store, &ticket).unwrap();
    let current = projection_lease(&mut store);
    let old = projected(&store, &ticket, &witness, &original, &original);
    assert!(matches!(old.evidence, NormalizationEvidence::Recorded(_)));
    assert_eq!(old.mapping, unavailable(ProjectionUnavailable::StaleLease));
    for field in ["hash", "index", "nullish", "count", "target", "identity"] {
        let mut bad = witness.clone();
        match field {
            "hash" => bad.rows[0].row_hash = "0".repeat(64),
            "index" => bad.rows[0].original_index = 1,
            "nullish" => bad.rows[0].id_state = OriginalIdState::Null,
            "count" => bad.original_count += 1,
            "target" => bad.target.conversation_id = "another".into(),
            _ => bad.identity.store_id = "another".into(),
        }
        let result = projected(&store, &ticket, &bad, &original, &current);
        assert!(matches!(result.mapping, NormalizationMapping::Unavailable(ProjectionUnavailable::InvalidWitness | ProjectionUnavailable::IdentityChanged)), "{field}: {result:?}");
    }
    let (_other_dir, mut other, _) = effects_fixture(json!({}));
    let foreign = projection_lease(&mut other);
    assert_eq!(projected(&store, &ticket, &witness, &foreign, &current).mapping, unavailable(ProjectionUnavailable::LeaseUnavailable));
    let wrong_revision = GenerationLease { revision: current.revision, ..original.clone() };
    assert_eq!(projected(&store, &ticket, &witness, &wrong_revision, &current).mapping, unavailable(ProjectionUnavailable::StaleLease));
    store.release_revision(&original.token).unwrap();
    let result = projected(&store, &ticket, &witness, &original, &current);
    assert!(matches!(result.evidence, NormalizationEvidence::Recorded(_)));
    assert_eq!(result.mapping, unavailable(ProjectionUnavailable::LeaseUnavailable));
}

#[test]
fn normalization_witness_requires_exact_reserved_baseline_revision() {
    let (_directory, mut store, mut intent) = effects_fixture(json!({}));
    let earlier = projection_lease(&mut store);
    store.commit(&WorkingSetCommit { expected_revision: earlier.revision,
        root_mutations: Some(vec![super::super::RootMutation::Set { key: "language".into(), value: json!("ko") }]), ..Default::default() }).unwrap();
    intent.target.expected_revision = store.revision().unwrap();
    let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
    assert_error(service::capture_normalization_witness(&store, &ticket, &earlier, &[0]), "projection-stale-lease");
    let original = projection_lease(&mut store);
    let witness = service::capture_normalization_witness(&store, &ticket, &original, &[0]).unwrap();
    service::prepare_reserved_ollama(&mut store, &ticket).unwrap();
    let current = projection_lease(&mut store);
    assert_eq!(projected(&store, &ticket, &witness, &earlier, &current).mapping, unavailable(ProjectionUnavailable::StaleLease));
    assert_eq!(mapped(projected(&store, &ticket, &witness, &original, &current)).rows.len(), 1);
}

#[test]
fn normalization_projection_enforces_exact_small_history_and_resident_bounds() {
    let (_directory, mut store, ticket, original, witness) = projection_reserved(json!({}));
    service::prepare_reserved_ollama(&mut store, &ticket).unwrap();
    let current = projection_lease(&mut store);
    let bytes: usize = effects_conversation(&store)["message"].as_array().unwrap().iter().map(|row| serde_json::to_vec(row).unwrap().len()).sum();
    let exact = ProjectionLimits { history_rows: 8, history_bytes: bytes, resident_rows: 8 };
    assert_eq!(mapped(store.generation_project_normalization_limited(&ticket, &witness, &original, &current, exact).unwrap()).rows.len(), 4);
    for limits in [ProjectionLimits { history_rows: 7, ..exact }, ProjectionLimits { history_bytes: bytes - 1, ..exact }, ProjectionLimits { resident_rows: 7, ..exact }] {
        assert_eq!(store.generation_project_normalization_limited(&ticket, &witness, &original, &current, limits).unwrap().mapping, unavailable(ProjectionUnavailable::Limit));
    }
    let before = journal_body(&store);
    assert_error(store.generation_capture_normalization_witness_limited(&ticket, &original, &[0, 1], ProjectionLimits { resident_rows: 1, ..exact }), "projection-limit");
    assert_error(service::capture_normalization_witness(&store, &ticket, &original, &[1, 1]), "projection-witness");
    assert_error(service::capture_normalization_witness(&store, &ticket, &original, &[8]), "projection-witness");
    assert_eq!(journal_body(&store), before);
}

#[test]
fn normalization_projection_requires_actual_receipt_and_exact_reconstructed_commit() {
    for corruption in ["missing", "digest", "revision", "reconstruction"] {
        let (_directory, mut store, ticket, original, witness) = projection_reserved(json!({}));
        service::prepare_reserved_ollama(&mut store, &ticket).unwrap();
        let previous = store.generation_record(&ticket).unwrap();
        let NormalizationEvidence::Recorded(receipt) = normalization_evidence(&previous).unwrap() else { panic!("no receipt") };
        match corruption {
            "missing" => { store.connection.execute("DELETE FROM lww_requests WHERE request_id=?1", [&receipt.request_id]).unwrap(); }
            "revision" => { store.connection.execute("UPDATE lww_requests SET revision=revision+1 WHERE request_id=?1", [&receipt.request_id]).unwrap(); }
            _ => {
                store.connection.execute("UPDATE lww_requests SET digest=?2 WHERE request_id=?1", params![receipt.request_id, "0".repeat(64)]).unwrap();
                if corruption == "reconstruction" {
                    let mut changed = previous.clone();
                    let Normalization::Settled { receipt: Some((_, digest, _)) } = &mut changed.preparation.as_mut().unwrap().normalization else { unreachable!() };
                    *digest = "0".repeat(64);
                    store.generation_replace_record(&ticket, &previous, &changed).unwrap();
                }
            }
        }
        let current = projection_lease(&mut store);
        let saved = effects_conversation(&store);
        let result = projected(&store, &ticket, &witness, &original, &current);
        assert_eq!(result.mapping, unavailable(ProjectionUnavailable::ReceiptIntegrity), "{corruption}: {result:?}");
        if corruption == "reconstruction" { assert!(matches!(result.evidence, NormalizationEvidence::Verified(_))); }
        else { assert!(matches!(result.evidence, NormalizationEvidence::Recorded(_))); }
        assert_eq!(effects_conversation(&store), saved);
    }
}

#[test]
fn normalization_service_resolves_real_lost_ack_without_preparing_or_reassigning() {
    let (_directory, mut store, ticket, original, witness) = projection_reserved(json!({}));
    let path = format!("$.journal.records.\"{}\".attempt", ticket.sequence.0);
    store.device_store().unwrap().connection().execute_batch(&format!(
        "CREATE TEMP TRIGGER projection_lost_ack BEFORE UPDATE ON device_settings
         WHEN OLD.key='native-generation-journal-v1'
          AND json_extract(OLD.value,'{path}.kind')='HistoryIds'
          AND json_type(NEW.value,'{path}')='null'
         BEGIN SELECT RAISE(ABORT,'synthetic projection lost acknowledgement'); END;"
    )).unwrap();
    assert!(service::prepare_reserved_ollama(&mut store, &ticket).is_err());
    let saved = effects_conversation(&store);
    let current = projection_lease(&mut store);
    assert_eq!(projected(&store, &ticket, &witness, &original, &current).mapping, unavailable(ProjectionUnavailable::Pending));
    store.device_store().unwrap().connection().execute_batch("DROP TRIGGER projection_lost_ack").unwrap();
    let resolved = service::resolve_normalization(&mut store, &ticket).unwrap();
    assert!(matches!(resolved.evidence, NormalizationEvidence::Verified(_)));
    assert!(!resolved.status.preparation_ready && !resolved.status.pending_commit);
    assert_eq!(resolved.status.checkpoint.0, 0);
    assert_eq!(mapped(projected(&store, &ticket, &witness, &original, &current)).rows.len(), 4);
    assert_eq!(service::resolve_normalization(&mut store, &ticket).unwrap(), resolved);
    assert_eq!(effects_conversation(&store), saved);
    assert_eq!(store.revision().unwrap(), current.revision);
}

#[test]
fn normalization_projection_distinguishes_no_effects_and_committed_failure_or_cancellation() {
    let (_directory, mut store, intent) = super::preparation_tests::stored(&super::preparation_tests::database(&json!({})));
    let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
    let original = projection_lease(&mut store);
    let witness = service::capture_normalization_witness(&store, &ticket, &original, &[0]).unwrap();
    assert_eq!(projected(&store, &ticket, &witness, &original, &original).mapping, unavailable(ProjectionUnavailable::Pending));
    service::prepare_reserved_ollama(&mut store, &ticket).unwrap();
    let result = projected(&store, &ticket, &witness, &original, &original);
    assert_eq!(result.evidence, NormalizationEvidence::NoIdEffects);
    assert_eq!(result.mapping, NormalizationMapping::NoIdEffects);
    // Only the interaction effect committed.
    assert_eq!(store.revision().unwrap(), original.revision + 1);
    for failed in [false, true] {
        let patch = if failed { json!({"preset":{"maxContext":1}}) } else { json!({}) };
        let (_directory, mut store, ticket, original, witness) = projection_reserved(patch);
        service::prepare_reserved_ollama(&mut store, &ticket).unwrap();
        if !failed { service::cancel_generation(&mut store, &ticket).unwrap(); }
        let current = projection_lease(&mut store);
        let result = projected(&store, &ticket, &witness, &original, &current);
        assert_eq!(result.status.as_ref().unwrap().phase, if failed { Phase::Failed } else { Phase::Cancelled });
        assert_eq!(mapped(result).rows.len(), 4);
    }
}

#[test]
fn normalization_resolution_distinguishes_terminal_nonissuance_from_active_or_unresolved_work() {
    for started in [false, true] {
        let (_directory, mut store, ticket, original, witness) = projection_reserved(json!({}));
        if started { assert!(matches!(store.generation_start_preparation(&ticket).unwrap(), Start::Started { .. })); }
        let before = effects_conversation(&store);
        let journal = journal_body(&store);
        let active = service::resolve_normalization(&mut store, &ticket).unwrap();
        assert_eq!(active.evidence, NormalizationEvidence::Pending);
        assert_eq!(active.status.phase, if started { Phase::Running } else { Phase::Reserved });
        assert!(!active.status.pending_commit);
        assert_eq!(journal_body(&store), journal);
        assert_eq!(projected(&store, &ticket, &witness, &original, &original).mapping, unavailable(ProjectionUnavailable::Pending));
        assert_eq!(service::cancel_generation(&mut store, &ticket).unwrap().phase, Phase::Cancelled);
        let journal = journal_body(&store);
        let terminal = service::resolve_normalization(&mut store, &ticket).unwrap();
        assert_eq!(terminal.evidence, NormalizationEvidence::NoIdEffects);
        assert_eq!(terminal.status.phase, Phase::Cancelled);
        assert!(!terminal.status.pending_commit);
        let projection = projected(&store, &ticket, &witness, &original, &original);
        assert_eq!(projection.evidence, NormalizationEvidence::NoIdEffects);
        assert_eq!(projection.mapping, NormalizationMapping::NoIdEffects);
        assert_eq!(service::resolve_normalization(&mut store, &ticket).unwrap(), terminal);
        assert_eq!(journal_body(&store), journal);
        assert_eq!(effects_conversation(&store), before);
        assert_eq!(store.revision().unwrap(), original.revision);
    }
    let (_directory, mut store, ticket, original, witness) = projection_reserved(json!({}));
    let Start::Started { owner, .. } = store.generation_start_preparation(&ticket).unwrap() else { panic!("not started") };
    let (attempt, _) = interrupt_ids_after_intent(&mut store, &owner);
    let issued = store.generation_record(&ticket).unwrap();
    assert_eq!(issued.attempt.as_ref(), Some(&attempt));
    let mut terminal = issued.clone(); terminal.phase = Phase::Cancelled;
    store.generation_replace_record(&ticket, &issued, &terminal).unwrap();
    let unresolved = projected(&store, &ticket, &witness, &original, &original);
    assert_eq!(unresolved.evidence, NormalizationEvidence::Pending);
    assert_eq!(unresolved.mapping, unavailable(ProjectionUnavailable::Pending));
    assert!(unresolved.status.unwrap().pending_commit);
    assert_eq!(store.generation_record(&ticket).unwrap().attempt, Some(attempt));
}

#[test]
fn normalization_resolution_restart_settles_unissued_preparation_without_id_effects() {
    const TEST: &str = "normalization_resolution_restart_settles_unissued_preparation_without_id_effects";
    if let Some(root) = restart_root(TEST) {
        let input: Value = serde_json::from_slice(&std::fs::read(root.join("unissued-restart.json")).unwrap()).unwrap();
        let ticket: Ticket = serde_json::from_value(input["ticket"].clone()).unwrap();
        let mut store = PersistentStore::open(&root).unwrap();
        let before = effects_conversation(&store);
        let revision = store.revision().unwrap();
        let resolved = service::resolve_normalization(&mut store, &ticket).unwrap();
        assert_eq!(resolved.status.phase, Phase::Interrupted);
        assert!(!resolved.status.pending_commit);
        assert_eq!(resolved.evidence, NormalizationEvidence::NoIdEffects);
        let journal = journal_body(&store);
        assert_eq!(service::resolve_normalization(&mut store, &ticket).unwrap(), resolved);
        assert_eq!(journal_body(&store), journal);
        assert_eq!(effects_conversation(&store), before);
        assert_eq!(store.revision().unwrap(), revision);
        assert_eq!(before["message"].as_array().unwrap().iter().filter(|row| row["chatId"].is_null()).count(), 4);
        assert_eq!(resolved.status.committed_revision, None);
        return;
    }
    let (directory, mut store, ticket, _original, _witness) = projection_reserved(json!({}));
    assert!(matches!(store.generation_start_preparation(&ticket).unwrap(), Start::Started { .. }));
    std::fs::write(directory.path().join("unissued-restart.json"), serde_json::to_vec(&json!({"ticket":ticket})).unwrap()).unwrap();
    drop(store);
    run_in_restarted_process(TEST, directory.path());
}

#[test]
fn normalization_projection_keeps_recorded_truth_after_library_or_binding_change() {
    for replacement in [false, true] {
        let (_directory, mut store, ticket, original, witness) = projection_reserved(json!({}));
        service::prepare_reserved_ollama(&mut store, &ticket).unwrap();
        let resolved = service::resolve_normalization(&mut store, &ticket).unwrap();
        let NormalizationEvidence::Verified(receipt) = resolved.evidence else { panic!("not verified") };
        if replacement {
            let stage = store.replace_begin().unwrap();
            store.replace_put_root(&stage.staging_id, &json!({"syntheticReplacement":true})).unwrap();
            store.replace_put_presets(&stage.staging_id, &[]).unwrap();
            store.replace_commit(&stage.staging_id, Some(store.revision().unwrap())).unwrap();
        } else {
            let selection = super::super::sync_selection::read(&store.connection).unwrap();
            store.lww_switch_target(&lww::Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: uuid::Uuid::new_v4().to_string() },
                &super::super::sync_selection::BindingSelectionChange { expected_epoch: selection.epoch, new_epoch: uuid::Uuid::new_v4().to_string(),
                    target: super::super::sync_selection::SyncTarget::None, library_id: None, target_id: None, inspection_id: None, initial_publication: false }).unwrap();
        }
        let current = projection_lease(&mut store);
        let result = projected(&store, &ticket, &witness, &original, &current);
        assert_eq!(result.evidence, NormalizationEvidence::Recorded(receipt.clone()));
        assert_eq!(result.mapping, unavailable(ProjectionUnavailable::IdentityChanged));
        let before = journal_body(&store);
        assert_eq!(service::resolve_normalization(&mut store, &ticket).unwrap().evidence, NormalizationEvidence::Recorded(receipt));
        assert_eq!(journal_body(&store), before);
    }
}

#[test]
fn normalization_projection_uses_one_pinned_snapshot_across_a_real_output_commit() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let mut writer = store.open_native_job_store().unwrap();
    let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
    let original = projection_lease(&mut store);
    let witness = service::capture_normalization_witness(&store, &ticket, &original, &[0, 4]).unwrap();
    let PreparedGeneration::Ready { owner, .. } = service::prepare_reserved_ollama(&mut store, &ticket).unwrap() else { panic!("not ready") };
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writing = thread::spawn(move || {
        lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            entered_tx.send(()).unwrap(); release_rx.recv().unwrap();
        })));
        writer.generation_checkpoint(&owner, &checkpoint(1, "owned output")).unwrap()
    });
    entered_rx.recv().unwrap();
    let pinned_before_output = projection_lease(&mut store);
    release_tx.send(()).unwrap();
    let committed = writing.join().unwrap();
    let old = projected(&store, &ticket, &witness, &original, &pinned_before_output);
    assert!(matches!(old.evidence, NormalizationEvidence::Verified(_)));
    assert!(matches!(old.mapping, NormalizationMapping::Unavailable(ProjectionUnavailable::MessageFenceChanged | ProjectionUnavailable::StaleLease)));
    let current = projection_lease(&mut store);
    let value = mapped(projected(&store, &ticket, &witness, &original, &current));
    assert_eq!(Some(value.projected_revision), committed.committed_revision);
    assert_eq!(value.current_count, 9);
    assert!(value.rows.iter().all(|row| row.current_index < 8));
}

#[test]
fn normalization_projection_waits_for_a_pending_output_receipt_without_losing_id_evidence() {
    let (_directory, mut store, ticket, original, witness) = projection_reserved(json!({}));
    let PreparedGeneration::Ready { owner, .. } = service::prepare_reserved_ollama(&mut store, &ticket).unwrap() else { panic!("not ready") };
    let committed = checkpoint_with_lagging_journal(&mut store, &owner);
    let current = projection_lease(&mut store);
    let pending = projected(&store, &ticket, &witness, &original, &current);
    assert_eq!(pending.mapping, unavailable(ProjectionUnavailable::Pending));
    assert!(matches!(pending.evidence, NormalizationEvidence::Recorded(_)));
    let resolved = service::resolve_normalization(&mut store, &ticket).unwrap();
    assert_eq!(resolved.status, committed);
    let value = mapped(projected(&store, &ticket, &witness, &original, &current));
    assert!(value.receipt_revision < value.projected_revision);
    assert_eq!(value.rows.len(), 4);
}

#[test]
fn normalization_projection_restart_keeps_receipt_but_expires_original_lease() {
    const TEST: &str = "normalization_projection_restart_keeps_receipt_but_expires_original_lease";
    if let Some(root) = restart_root(TEST) {
        let input: Value = serde_json::from_slice(&std::fs::read(root.join("projection-restart.json")).unwrap()).unwrap();
        let ticket: Ticket = serde_json::from_value(input["ticket"].clone()).unwrap();
        let mut store = PersistentStore::open(&root).unwrap();
        let record = store.generation_record(&ticket).unwrap();
        let original = GenerationLease { token: input["token"].as_str().unwrap().into(), revision: input["revision"].as_i64().unwrap() };
        let witness = NormalizationWitness { ticket: ticket.clone(), target: record.target.clone(), identity: projection_identity(&store.connection).unwrap(),
            original_revision: original.revision, original_count: record.initial_count as usize, rows: Vec::new() };
        let resolved = service::resolve_normalization(&mut store, &ticket).unwrap();
        assert_eq!(resolved.status.phase, Phase::Interrupted);
        assert!(matches!(resolved.evidence, NormalizationEvidence::Verified(_)));
        let current = projection_lease(&mut store);
        let result = projected(&store, &ticket, &witness, &original, &current);
        assert_eq!(result.mapping, unavailable(ProjectionUnavailable::LeaseUnavailable));
        assert!(matches!(result.evidence, NormalizationEvidence::Recorded(_)));
        // The interaction effect and the ID commit.
        assert_eq!(current.revision, original.revision + 2);
        assert_eq!(effects_conversation(&store)["message"].as_array().unwrap().len(), 8);
        return;
    }
    let (directory, mut store, ticket, original, _witness) = projection_reserved(json!({}));
    service::prepare_reserved_ollama(&mut store, &ticket).unwrap();
    std::fs::write(directory.path().join("projection-restart.json"), serde_json::to_vec(&json!({"ticket":ticket,"token":original.token,"revision":original.revision})).unwrap()).unwrap();
    drop(store);
    run_in_restarted_process(TEST, directory.path());
}

#[test]
fn normalization_projection_ticket_pruning_does_not_reexecute_or_erase_known_commit() {
    let (_directory, mut store, ticket, original, witness) = projection_reserved(json!({}));
    service::prepare_reserved_ollama(&mut store, &ticket).unwrap();
    service::cancel_generation(&mut store, &ticket).unwrap();
    let known = service::resolve_normalization(&mut store, &ticket).unwrap();
    let current = projection_lease(&mut store);
    let before = effects_conversation(&store);
    for _ in 0..MAX_TERMINAL {
        let mut target = witness.target.clone(); target.expected_revision = current.revision;
        let later = store.generation_reserve(target).unwrap();
        service::cancel_generation(&mut store, &later).unwrap();
    }
    assert_eq!(projected(&store, &ticket, &witness, &original, &current).mapping, unavailable(ProjectionUnavailable::ExpiredTicket));
    assert!(matches!(known.evidence, NormalizationEvidence::Verified(_)));
    assert_eq!(effects_conversation(&store), before);
    assert_eq!(store.revision().unwrap(), current.revision);
}

fn assert_nullish_effect(before: &Value, after: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    let before = before["message"].as_array().unwrap();
    let after = after["message"].as_array().unwrap();
    assert_eq!(before.len(), after.len());
    for (old, new) in before.iter().zip(after) {
        let mut expected = old.clone();
        if old["chatId"].is_null() {
            let id = new["chatId"].as_str().unwrap();
            assert_eq!(uuid::Uuid::parse_str(id).unwrap().to_string(), id);
            ids.push(id.to_owned());
            expected["chatId"] = json!(id);
        }
        assert_eq!(&expected, new);
    }
    assert_eq!(ids.iter().collect::<std::collections::BTreeSet<_>>().len(), ids.len());
    ids
}

#[test]
fn native_service_persists_only_nullish_ids_then_prepares_and_checkpoints() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let before = effects_conversation(&store);
    let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
    let reserved = store.generation_status(&ticket).unwrap();
    let PreparedGeneration::Ready { owner, prepared, read_revision, status } = service::prepare_reserved_ollama(&mut store, &ticket).unwrap() else { panic!("not ready") };
    assert!(status.preparation_ready);
    assert_eq!(status.checkpoint.0, 0);
    assert_eq!(status.message_id, reserved.message_id);
    // The interaction effect commits first, then the ID commit.
    assert_eq!(status.committed_revision, Some(intent.target.expected_revision + 2));
    assert_eq!(read_revision, intent.target.expected_revision + 2);
    let normalized = effects_conversation(&store);
    assert_eq!(assert_nullish_effect(&before, &normalized).len(), 4);
    assert_eq!(normalized["futureConversation"], before["futureConversation"]);
    assert_eq!(normalized["scriptstate"], before["scriptstate"]);
    assert!(prepared.messages.iter().any(|message| message.content == "Selected missing ID."));
    assert!(matches!(service::prepare_reserved_ollama(&mut store, &ticket).unwrap(), PreparedGeneration::Existing(value) if value == status));
    assert_eq!(store.generation_ensure_history_ids(&owner).unwrap(), status);
    assert_eq!(effects_conversation(&store), normalized);
    store.generation_checkpoint(&owner, &checkpoint(1, "native output")).unwrap();
    let output = effects_conversation(&store);
    assert_eq!(&output["message"].as_array().unwrap()[..8], normalized["message"].as_array().unwrap());
    assert_eq!(output["message"][8]["chatId"], reserved.message_id);
    assert_eq!(store.generation_finish(&owner, false).unwrap().phase, Phase::Completed);
}

#[test]
fn supported_preparation_failure_keeps_ids_and_records_failed_without_output() {
    let (_directory, mut store, intent) = effects_fixture(json!({"preset":{"maxContext":1}}));
    let before = effects_conversation(&store);
    let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
    let PreparedGeneration::Failed { error, status } = service::prepare_reserved_ollama(&mut store, &ticket).unwrap() else { panic!("not failed") };
    assert_eq!(error, PreparationError::ContextLimit);
    assert_eq!(status.phase, Phase::Failed);
    assert_eq!(status.preparation_error, Some(error));
    assert!(!status.preparation_ready);
    assert_eq!(status.checkpoint.0, 0);
    let saved = effects_conversation(&store);
    assert_eq!(assert_nullish_effect(&before, &saved).len(), 4);
    assert_eq!(store.generation_recover(&ticket).unwrap(), status);
    assert_eq!(service::cancel_generation(&mut store, &ticket).unwrap(), status);
    let evidence = store.generation_effects_evidence(&ticket).unwrap().unwrap();
    assert_eq!(evidence.receipts.len(), 1);
    assert_eq!(evidence.affected_unit_keys, send_effect_unit_keys(&intent.target).unwrap().for_effect(SendEffect::Interaction).iter().map(|key| key.as_str().to_owned()).collect::<Vec<_>>());
    let mut retry = intent;
    retry.target.expected_revision = store.revision().unwrap();
    let retry = service::reserve_ollama(&mut store, &retry).unwrap();
    assert!(matches!(service::prepare_reserved_ollama(&mut store, &retry).unwrap(), PreparedGeneration::Failed { .. }));
    assert_eq!(effects_conversation(&store), saved);
}

#[test]
fn no_op_normalization_does_not_write_library_and_generic_methods_cannot_bypass_ready() {
    let (_directory, mut store, intent) = super::preparation_tests::stored(&super::preparation_tests::database(&json!({})));
    let owner = effects_owner(&mut store, &intent);
    let before = effects_conversation(&store);
    for settled in [false, true] {
        if settled { store.generation_ensure_history_ids(&owner).unwrap(); }
        assert_error(store.generation_checkpoint(&owner, &checkpoint(1, "must not write")), "preparation-pending");
        assert_error(store.generation_finish(&owner, false), "preparation-pending");
        // No ID commit: the interaction effect is the only write.
        assert_eq!(store.revision().unwrap(), intent.target.expected_revision + i64::from(settled));
        assert_eq!(effects_conversation(&store), before);
    }
    let snapshot = store.generation_owned_preparation_snapshot(&owner).unwrap().unwrap();
    crate::native_generation::prepare::prepare_ollama(&snapshot).unwrap();
    assert!(store.generation_mark_prepared(&owner).unwrap().preparation_ready);
    assert_eq!(store.generation_finish(&owner, false).unwrap().phase, Phase::Completed);
}

#[test]
fn preflight_checks_late_literals_and_selected_ids_before_reservation_or_effects() {
    for patch in [json!({"preset":{"maxContext":1},"character":{"desc":"{{screenwidth}}"}}),
        json!({"character":{"firstMessage":"{{metadata::version}}"},"messages":[{"role":"user","data":"literal"}]}),
        json!({"messages":[{"role":"user","data":"literal","chatId":""}]}),
        json!({"messages":[{"role":"user","data":"a","chatId":"same"},{"role":"user","data":"b","chatId":"same"}]})] {
        let (_directory, mut store, intent) = super::preparation_tests::stored(&super::preparation_tests::database(&patch));
        let before = effects_conversation(&store);
        assert!(matches!(service::reserve_ollama(&mut store, &intent), Err(GenerationError::Preparation(_))));
        assert_eq!(store.revision().unwrap(), intent.target.expected_revision);
        assert_eq!(effects_conversation(&store), before);
        assert!(read_journal(store.device_store().unwrap().connection()).unwrap().is_none());
    }
}

#[test]
fn projected_uuid_bytes_admit_exact_bound_and_reject_one_over_before_reserving() {
    for id in [None, Some(Value::Null)] {
        let mut row = json!({"role":"user","data":"é🦊"});
        if let Some(id) = id { row["chatId"] = id; }
        let (_directory, mut store, intent) = super::preparation_tests::stored(&super::preparation_tests::database(&json!({"messages":[row]})));
        let before = effects_conversation(&store);
        let mut expected = before["message"][0].clone();
        expected["chatId"] = json!("00000000-0000-4000-8000-000000000000");
        let bound = serde_json::to_vec(&expected).unwrap().len();
        assert!(matches!(store.generation_reserve_ollama_limited(&intent, bound - 1), Err(GenerationError::Store(StoreError::Validation { message })) if message == "native-generation:history-limit"));
        assert!(read_journal(store.device_store().unwrap().connection()).unwrap().is_none());
        assert_eq!(effects_conversation(&store), before);
        let ticket = store.generation_reserve_ollama_limited(&intent, bound).unwrap();
        assert!(matches!(service::prepare_reserved_ollama(&mut store, &ticket).unwrap(), PreparedGeneration::Ready { .. }));
        let saved = effects_conversation(&store);
        assert_eq!(serde_json::to_vec(&saved["message"][0]).unwrap().len(), bound);
        assert_eq!(assert_nullish_effect(&before, &saved).len(), 1);
    }
}

#[test]
fn ticket_cancellation_before_start_and_during_token_preparation_is_reachable() {
    for during in [false, true] {
        let (_directory, mut store, intent) = effects_fixture(json!({}));
        let before = effects_conversation(&store);
        let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
        if !during {
            assert_eq!(service::cancel_generation(&mut store, &ticket).unwrap().phase, Phase::Cancelled);
            assert!(matches!(service::prepare_reserved_ollama(&mut store, &ticket).unwrap(), PreparedGeneration::Existing(Status { phase: Phase::Cancelled, .. })));
            assert_eq!(effects_conversation(&store), before);
            assert_eq!(store.generation_effects_evidence(&ticket).unwrap(), Some(EffectsEvidence { affected_unit_keys: vec![], receipts: vec![] }));
            continue;
        }
        let mut preparing = store.open_native_job_store().unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let preparing_ticket = ticket.clone();
        let job = thread::spawn(move || {
            service::BEFORE_PREPARATION.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                entered_tx.send(()).unwrap(); release_rx.recv().unwrap();
            })));
            service::prepare_reserved_ollama(&mut preparing, &preparing_ticket).unwrap()
        });
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let duplicate = service::prepare_reserved_ollama(&mut store, &ticket).unwrap();
        assert!(matches!(duplicate, PreparedGeneration::Existing(Status { phase: Phase::Running, preparation_ready: false, .. })));
        // This returned cancellation proves gate acquisition before releasing preparation.
        let cancelled = service::cancel_generation(&mut store, &ticket).unwrap();
        assert_eq!(cancelled.phase, Phase::Cancelled);
        release_tx.send(()).unwrap();
        assert!(matches!(job.join().unwrap(), PreparedGeneration::Existing(status) if status == cancelled));
        assert_eq!(assert_nullish_effect(&before, &effects_conversation(&store)).len(), 4);
        assert_eq!(cancelled.checkpoint.0, 0);
        let evidence = store.generation_effects_evidence(&ticket).unwrap().unwrap();
        assert_eq!(evidence.receipts.len(), 1);
        assert_eq!(evidence.affected_unit_keys, send_effect_unit_keys(&intent.target).unwrap().for_effect(SendEffect::Interaction).iter().map(|key| key.as_str().to_owned()).collect::<Vec<_>>());
    }
}

#[test]
fn ticket_cancellation_waits_for_admitted_id_commit_and_retains_its_receipt() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let before = effects_conversation(&store);
    let owner = effects_owner(&mut store, &intent);
    let mut writer = store.open_native_job_store().unwrap();
    let mut cancellation = store.open_native_job_store().unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer_owner = owner.clone();
    let writing = thread::spawn(move || {
        after_interaction(Box::new(move || { entered_tx.send(()).unwrap(); release_rx.recv().unwrap(); }));
        writer.generation_ensure_history_ids(&writer_owner).unwrap()
    });
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let (waiting_tx, waiting_rx) = mpsc::channel();
    let ticket = owner.ticket.clone();
    let cancelling = thread::spawn(move || {
        FINISH_WAITING_FOR_SEGMENT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || waiting_tx.send(()).unwrap())));
        service::cancel_generation(&mut cancellation, &ticket).unwrap()
    });
    waiting_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    release_tx.send(()).unwrap();
    let committed = writing.join().unwrap();
    let cancelled = cancelling.join().unwrap();
    assert_eq!(cancelled.phase, Phase::Cancelled);
    assert_eq!(cancelled.committed_revision, committed.committed_revision);
    assert!(!cancelled.pending_commit);
    assert_eq!(assert_nullish_effect(&before, &effects_conversation(&store)).len(), 4);
    assert_eq!(store.generation_ensure_history_ids(&owner).unwrap(), cancelled);
}

fn frozen_assignments(store: &PersistentStore, ticket: &Ticket) -> Vec<IdAssignment> {
    match store.generation_record(ticket).unwrap().preparation.unwrap().normalization {
        Normalization::Pending { assignments: Some(assignments) } => assignments,
        other => panic!("expected frozen plan: {other:?}"),
    }
}

fn assert_stored_assignments(store: &PersistentStore, assignments: &[IdAssignment]) {
    let saved = effects_conversation(store);
    for assignment in assignments { assert_eq!(saved["message"][assignment.index]["chatId"], assignment.id); }
}

// Preparation commits the interaction effect before the ID commit, so a hook for the ID commit is armed by that commit.
fn after_interaction(hook: Box<dyn FnOnce()>) {
    lww::BEFORE_HELD_COMMIT.with(|slot| *slot.borrow_mut() = Some(Box::new(move || {
        lww::BEFORE_HELD_COMMIT.with(|slot| *slot.borrow_mut() = Some(hook));
    })));
}

fn interrupt_ids_after_intent(store: &mut PersistentStore, owner: &Owner) -> (Attempt, Vec<IdAssignment>) {
    after_interaction(Box::new(|| panic!("synthetic ID segment interruption")));
    assert!(catch_unwind(AssertUnwindSafe(|| store.generation_ensure_history_ids(owner))).is_err());
    let attempt = store.generation_record(&owner.ticket).unwrap().attempt.unwrap();
    assert!(matches!(attempt.kind, AttemptKind::HistoryIds));
    let digest: String = store.device_store().unwrap().connection().query_row(
        "SELECT digest FROM lww_intents WHERE request_id=?1", [&attempt.request_id], |row| row.get(0)).unwrap();
    assert_eq!(digest, attempt.intent_digest);
    (attempt, frozen_assignments(store, &owner.ticket))
}

#[test]
fn history_id_conflict_retains_assignments_and_preserves_later_variables() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let before = effects_conversation(&store);
    let owner = effects_owner(&mut store, &intent);
    let mut concurrent = store.open_native_job_store().unwrap();
    after_interaction(Box::new(move || {
        let revision = concurrent.revision().unwrap();
        crate::persistent_store::commit::incremental_commit(&mut concurrent.connection, revision, |_, _| Ok(()), |_, _, _| Ok(())).unwrap();
    }));
    assert!(matches!(store.generation_ensure_history_ids(&owner), Err(GenerationError::Store(StoreError::RevisionConflict { .. }))));
    let assignments = frozen_assignments(&store, &owner.ticket);
    assert_eq!(effects_conversation(&store), before);
    assert!(!store.generation_status(&owner.ticket).unwrap().pending_commit);
    let variables = json!({"newer":"value","unknown":[1,2]});
    store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(),
        unit_mutations: Some(vec![lww::UnitMutation::Set {
            key: UnitKey::new(&["conversation", "character", "conversation", "scriptstate"]).unwrap(), value: variables.clone(),
        }]), ..Default::default() }).unwrap();
    let normalized = store.generation_ensure_history_ids(&owner).unwrap();
    assert_eq!(normalized.phase, Phase::Running);
    assert_stored_assignments(&store, &assignments);
    assert_eq!(effects_conversation(&store)["scriptstate"], variables);
    let snapshot = store.generation_owned_preparation_snapshot(&owner).unwrap().unwrap();
    assert_eq!(snapshot.history.conversation["scriptstate"], variables);
    assert_eq!(snapshot.history.revision, normalized.committed_revision.unwrap());
}

#[test]
fn service_ticket_retry_resumes_a_definite_id_conflict_without_exposing_owner() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let before = effects_conversation(&store);
    let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
    let mut concurrent = store.open_native_job_store().unwrap();
    after_interaction(Box::new(move || {
        let revision = concurrent.revision().unwrap();
        crate::persistent_store::commit::incremental_commit(&mut concurrent.connection, revision, |_, _| Ok(()), |_, _, _| Ok(())).unwrap();
    }));
    assert!(matches!(service::prepare_reserved_ollama(&mut store, &ticket), Err(StoreError::RevisionConflict { .. })));
    let assignments = frozen_assignments(&store, &ticket);
    let owner_before = store.generation_record(&ticket).unwrap().owner;
    assert_eq!(effects_conversation(&store), before);
    let PreparedGeneration::Ready { status, .. } = service::prepare_reserved_ollama(&mut store, &ticket).unwrap() else { panic!("retry did not resume") };
    assert_stored_assignments(&store, &assignments);
    assert_eq!(store.generation_record(&ticket).unwrap().owner, owner_before);
    assert!(status.preparation_ready);
    let revision = store.revision().unwrap();
    assert!(matches!(service::prepare_reserved_ollama(&mut store, &ticket).unwrap(), PreparedGeneration::Existing(value) if value == status));
    assert_eq!(store.revision().unwrap(), revision);
    assert_eq!(assert_nullish_effect(&before, &effects_conversation(&store)).len(), 4);
}

#[test]
fn service_ticket_retry_resolves_lost_id_acknowledgement_before_preparing_once() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
    let path = format!("$.journal.records.\"{}\".attempt", ticket.sequence.0);
    // Fail the real journal write after the library receipt exists, not the preparation implementation.
    store.device_store().unwrap().connection().execute_batch(&format!(
        "CREATE TEMP TRIGGER synthetic_id_ack BEFORE UPDATE ON device_settings
         WHEN OLD.key='native-generation-journal-v1'
          AND json_extract(OLD.value,'{path}.kind')='HistoryIds'
          AND json_type(NEW.value,'{path}')='null'
         BEGIN SELECT RAISE(ABORT,'synthetic lost ID acknowledgement'); END;"
    )).unwrap();
    assert!(service::prepare_reserved_ollama(&mut store, &ticket).is_err());
    let pending = store.generation_status(&ticket).unwrap();
    assert!(pending.pending_commit);
    assert!(!pending.preparation_ready);
    let saved = effects_conversation(&store);
    let issued = store.generation_record(&ticket).unwrap();
    let attempt = issued.attempt.as_ref().unwrap();
    let receipt: (String, i64) = store.connection.query_row(
        "SELECT digest,revision FROM lww_requests WHERE request_id=?1", [&attempt.request_id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(receipt, (attempt.intent_digest.clone(), attempt.expected_revision + 1));
    store.device_store().unwrap().connection().execute_batch("DROP TRIGGER synthetic_id_ack;").unwrap();
    let mut retry = store.open_native_job_store().unwrap();
    let PreparedGeneration::Ready { status, .. } = service::prepare_reserved_ollama(&mut retry, &ticket).unwrap() else { panic!("lost acknowledgement was not recovered") };
    assert_eq!(status.committed_revision, Some(receipt.1));
    assert!(!status.pending_commit);
    assert!(status.preparation_ready);
    assert_eq!(retry.generation_record(&ticket).unwrap().owner, issued.owner);
    assert_eq!(effects_conversation(&retry), saved);
    assert_eq!(retry.revision().unwrap(), receipt.1);
    assert!(matches!(service::prepare_reserved_ollama(&mut retry, &ticket).unwrap(), PreparedGeneration::Existing(value) if value == status));
}

#[test]
fn history_id_unknown_outcome_replays_exact_frozen_ids_after_real_restart() {
    const TEST: &str = "history_id_unknown_outcome_replays_exact_frozen_ids_after_real_restart";
    if let Some(root) = restart_root(TEST) {
        let mut store = PersistentStore::open(&root).unwrap();
        let statuses = store.generation_reconcile_orphans().unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].phase, Phase::Interrupted);
        assert_eq!(statuses[0].checkpoint.0, 0);
        assert!(!statuses[0].preparation_ready);
        assert!(!statuses[0].pending_commit);
        assert!(matches!(service::prepare_reserved_ollama(&mut store, &statuses[0].ticket).unwrap(), PreparedGeneration::Existing(_)));
        return;
    }
    let (directory, mut store, intent) = effects_fixture(json!({}));
    let before = effects_conversation(&store);
    let owner = effects_owner(&mut store, &intent);
    let (attempt, assignments) = interrupt_ids_after_intent(&mut store, &owner);
    assert_eq!(effects_conversation(&store), before);
    drop(store);
    run_in_restarted_process(TEST, directory.path());
    let mut reopened = PersistentStore::open(directory.path()).unwrap();
    assert_stored_assignments(&reopened, &assignments);
    assert_eq!(assert_nullish_effect(&before, &effects_conversation(&reopened)).len(), 4);
    let receipt: (String, i64) = reopened.connection.query_row("SELECT digest,revision FROM lww_requests WHERE request_id=?1", [&attempt.request_id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(receipt, (attempt.intent_digest, attempt.expected_revision + 1));
    let status = reopened.generation_recover(&owner.ticket).unwrap();
    assert_eq!(status.phase, Phase::Interrupted);
    assert_eq!(status.committed_revision, Some(receipt.1));
    assert_eq!(status.checkpoint.0, 0);
}

#[test]
fn history_id_journal_before_intent_retries_the_same_ids_in_the_same_runtime() {
    let (directory, mut store, intent) = effects_fixture(json!({}));
    let before = effects_conversation(&store);
    let owner = effects_owner(&mut store, &intent);
    let (attempt, assignments) = interrupt_ids_after_intent(&mut store, &owner);
    // Recreate the durable cut after journal freeze and before reserve_intent.
    store.device_store().unwrap().connection().execute("DELETE FROM lww_intents WHERE request_id=?1", [&attempt.request_id]).unwrap();
    drop(store);
    let mut reopened = PersistentStore::open(directory.path()).unwrap();
    let recovered = reopened.generation_recover(&owner.ticket).unwrap();
    assert_eq!(recovered.phase, Phase::Running);
    assert!(!recovered.pending_commit);
    assert_eq!(effects_conversation(&reopened), before);
    assert_eq!(frozen_assignments(&reopened, &owner.ticket), assignments);
    reopened.generation_ensure_history_ids(&owner).unwrap();
    assert_stored_assignments(&reopened, &assignments);
}

fn lagging_id_journal(store: &mut PersistentStore, owner: &Owner) -> (Attempt, Vec<IdAssignment>, Status) {
    let captured = Arc::new(Mutex::new(None));
    let during_commit = Arc::clone(&captured);
    let observer = store.open_native_job_store().unwrap();
    let ticket = owner.ticket.clone();
    after_interaction(Box::new(move || {
        *during_commit.lock().unwrap() = Some(observer.generation_record(&ticket).unwrap());
    }));
    let status = store.generation_ensure_history_ids(owner).unwrap();
    let issued = captured.lock().unwrap().take().unwrap();
    let settled = store.generation_record(&owner.ticket).unwrap();
    store.generation_replace_record(&owner.ticket, &settled, &issued).unwrap();
    (issued.attempt.unwrap(), frozen_assignments(store, &owner.ticket), status)
}

#[test]
fn same_runtime_id_receipt_recovery_never_reassigns_or_acknowledges_corrupt_provenance() {
    for corruption in ["none", "digest", "missing"] {
        let (_directory, mut store, intent) = effects_fixture(json!({}));
        let owner = effects_owner(&mut store, &intent);
        let (attempt, assignments, committed) = lagging_id_journal(&mut store, &owner);
        let before = effects_conversation(&store);
        match corruption {
            "digest" => { store.connection.execute("UPDATE lww_requests SET digest=?2 WHERE request_id=?1", params![attempt.request_id, "0".repeat(64)]).unwrap(); }
            "missing" => { store.connection.execute("DELETE FROM lww_requests WHERE request_id=?1", [&attempt.request_id]).unwrap(); }
            _ => {},
        }
        if corruption == "none" {
            assert_eq!(store.generation_recover(&owner.ticket).unwrap(), committed);
            assert_eq!(store.generation_ensure_history_ids(&owner).unwrap(), committed);
            assert_eq!(store.revision().unwrap(), committed.committed_revision.unwrap());
        } else {
            assert_error(store.generation_recover(&owner.ticket), if corruption == "digest" { "receipt-integrity" } else { "receipt-missing" });
            assert!(store.generation_ensure_history_ids(&owner).is_err());
            assert!(store.generation_status(&owner.ticket).unwrap().pending_commit);
        }
        assert_stored_assignments(&store, &assignments);
        assert_eq!(effects_conversation(&store), before);
    }
}

#[test]
fn pending_history_id_proof_settles_stale_after_binding_or_library_replacement_without_adoption() {
    for replacement in [false, true] {
        let (_directory, mut store, intent) = effects_fixture(json!({}));
        let owner = effects_owner(&mut store, &intent);
        let (attempt, _, _) = lagging_id_journal(&mut store, &owner);
        if replacement {
            let stage = store.replace_begin().unwrap();
            store.replace_put_root(&stage.staging_id, &json!({"replacement":true})).unwrap();
            store.replace_put_presets(&stage.staging_id, &[]).unwrap();
            store.replace_commit(&stage.staging_id, Some(store.revision().unwrap())).unwrap();
        } else {
            let selection = super::super::sync_selection::read(&store.connection).unwrap();
            store.lww_switch_target(&lww::Header {
                binding_authority: store.lww_binding_authority().unwrap(), request_id: uuid::Uuid::new_v4().to_string(),
            }, &super::super::sync_selection::BindingSelectionChange {
                expected_epoch: selection.epoch, new_epoch: uuid::Uuid::new_v4().to_string(),
                target: super::super::sync_selection::SyncTarget::None,
                library_id: None, target_id: None, inspection_id: None, initial_publication: false,
            }).unwrap();
        }
        let root = store.read_root(None).unwrap().value;
        let conversation = store.read_conversation("character", "conversation", None).unwrap().map(|value| value.value);
        let revision = store.revision().unwrap();
        // Receipts outlive the switch, so the attempt resolves exactly once and the record ends Stale.
        let recovered = store.generation_recover(&owner.ticket).unwrap();
        assert_eq!(recovered.phase, Phase::Stale);
        assert!(!recovered.pending_commit);
        assert!(!recovered.preparation_ready);
        let record = store.generation_record(&owner.ticket).unwrap();
        assert!(record.attempt.is_none());
        assert!(matches!(record.preparation.unwrap().normalization, Normalization::Settled { receipt: Some((id, _, _)) } if id == attempt.request_id));
        assert!(matches!(service::prepare_reserved_ollama(&mut store, &owner.ticket).unwrap(), PreparedGeneration::Existing(value) if value == recovered));
        assert_eq!(service::cancel_generation(&mut store, &owner.ticket).unwrap(), recovered);
        assert_eq!(store.revision().unwrap(), revision);
        assert_eq!(store.read_root(None).unwrap().value, root);
        assert_eq!(store.read_conversation("character", "conversation", None).unwrap().map(|value| value.value), conversation);
    }
}

#[test]
fn foreign_history_aba_and_deletion_fence_id_effects_without_overwriting() {
    for action in ["edit", "aba", "delete"] {
        let (_directory, mut store, intent) = effects_fixture(json!({}));
        let owner = effects_owner(&mut store, &intent);
        let original = effects_conversation(&store)["message"][0].clone();
        if action == "delete" {
            store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(),
                conversations: Some(vec![ConversationMutation::Delete { character_id: "character".into(), conversation_id: "conversation".into() }]), ..Default::default() }).unwrap();
        } else {
            let mut changed = original.clone(); changed["data"] = json!("foreign edit");
            for row in std::iter::once(changed).chain((action == "aba").then_some(original)) {
                store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(),
                    conversations: Some(vec![ConversationMutation::ReplaceRange { character_id: "character".into(), conversation_id: "conversation".into(),
                        start: 0, delete_count: 1, messages: vec![row], conversation: None, configured_index: None }]), ..Default::default() }).unwrap();
            }
        }
        let before = store.read_conversation("character", "conversation", None).unwrap().map(|value| value.value);
        let revision = store.revision().unwrap();
        assert_eq!(store.generation_ensure_history_ids(&owner).unwrap().phase, Phase::Stale);
        assert_eq!(store.revision().unwrap(), revision);
        assert_eq!(store.read_conversation("character", "conversation", None).unwrap().map(|value| value.value), before);
    }
}

#[test]
fn owned_preparation_reads_current_configuration_and_preserves_other_conversations() {
    let mut data = super::preparation_tests::database(&json!({"messages":super::preparation_tests::cases()["effectMessages"]}));
    let mut other = data["characters"][0]["chats"][0].clone();
    other["id"] = json!("other-conversation");
    data["characters"][0]["chats"].as_array_mut().unwrap().push(other);
    let (_directory, mut store, intent) = super::preparation_tests::stored(&data);
    let owner = effects_owner(&mut store, &intent);
    store.generation_ensure_history_ids(&owner).unwrap();
    let normalized = effects_conversation(&store);
    let mut preset = store.read_preset("preset-a", None).unwrap().unwrap().value;
    preset["mainPrompt"] = json!("Current main.");
    store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(),
        root_mutations: Some(vec![super::super::RootMutation::Set { key: "futureRoot".into(), value: json!({"newer":true}) }]),
        replace_presets: Some(vec![preset]),
        conversations: Some(vec![ConversationMutation::ReplaceRange { character_id: "character".into(), conversation_id: "other-conversation".into(),
            start: 0, delete_count: 1, messages: vec![json!({"role":"user","data":"Other edit.","chatId":"other-message"})],
            conversation: None, configured_index: None }]), ..Default::default() }).unwrap();
    let other = store.read_conversation("character", "other-conversation", None).unwrap().unwrap().value;
    let snapshot = store.generation_owned_preparation_snapshot(&owner).unwrap().unwrap();
    assert_eq!(snapshot.root["futureRoot"], json!({"newer":true}));
    assert_eq!(snapshot.preset["mainPrompt"], "Current main.");
    assert_eq!(snapshot.history.revision, store.revision().unwrap());
    assert_eq!(snapshot.history.messages, normalized["message"].as_array().unwrap().clone());
    let prepared = crate::native_generation::prepare::prepare_ollama(&snapshot).unwrap();
    assert_eq!(prepared.messages[0].content, "Current main.");
    store.generation_mark_prepared(&owner).unwrap();
    store.generation_checkpoint(&owner, &checkpoint(1, "response")).unwrap();
    assert_eq!(store.read_conversation("character", "other-conversation", None).unwrap().unwrap().value, other);
    assert_eq!(store.read_root(None).unwrap().value["futureRoot"], json!({"newer":true}));
}

#[test]
fn pending_history_id_attempt_is_retained_and_settlement_discards_frozen_rows() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let owner = effects_owner(&mut store, &intent);
    let (_, assignments) = interrupt_ids_after_intent(&mut store, &owner);
    let target = Target { expected_revision: store.revision().unwrap(), ..intent.target.clone() };
    for _ in 0..MAX_RESERVED + 3 { store.generation_reserve(target.clone()).unwrap(); }
    assert!(store.generation_status(&owner.ticket).unwrap().pending_commit);
    assert_eq!(frozen_assignments(&store, &owner.ticket), assignments);
    let status = store.generation_recover(&owner.ticket).unwrap();
    assert!(!status.pending_commit);
    let record = store.generation_record(&owner.ticket).unwrap();
    assert!(matches!(record.preparation.unwrap().normalization, Normalization::Settled { receipt: Some(_) }));
    assert_stored_assignments(&store, &assignments);
    let journal = read_journal(store.device_store().unwrap().connection()).unwrap().unwrap();
    assert_eq!(journal.records.len(), MAX_RESERVED + 1);
}

#[test]
fn native_snapshot_and_checkpoints_preserve_history_metadata_and_newer_variables() {
    let (_directory, mut store) = fixture();
    let before = conversation(&store);
    let (owner, history) = start(&mut store);
    assert_eq!(history.revision, store.revision().unwrap());
    assert_eq!(history.character["chaId"], "char-b");
    assert_eq!(history.messages, before["message"].as_array().unwrap().clone());
    assert_eq!(history.conversation["futureMetadata"], before["futureMetadata"]);
    let first = store.generation_checkpoint(&owner, &checkpoint(1, "first")).unwrap();
    let variables = json!({"same": "newer", "other": 17});
    store.commit(&WorkingSetCommit {
        expected_revision: store.revision().unwrap(),
        unit_mutations: Some(vec![lww::UnitMutation::Set {
            key: UnitKey::new(&["conversation", "char-b", "conv-beta", "scriptstate"]).unwrap(), value: variables.clone(),
        }]), ..Default::default()
    }).unwrap();
    let second = store.generation_checkpoint(&owner, &checkpoint(2, "first second 🦊")).unwrap();
    assert_eq!(second.phase, Phase::Running);
    assert!(second.committed_revision > first.committed_revision);
    let after = conversation(&store);
    assert_eq!(&after["message"].as_array().unwrap()[..3], before["message"].as_array().unwrap());
    assert_eq!(after["futureMetadata"], before["futureMetadata"]);
    assert_eq!(after["scriptstate"], variables);
    assert_eq!(after["message"][3]["chatId"], second.message_id);
    assert_eq!(after["message"][3]["data"], "first second 🦊");
    assert_eq!(store.generation_finish(&owner, false).unwrap().phase, Phase::Completed);
}

#[test]
fn repeat_checkpoint_is_idempotent_and_changed_retry_is_rejected() {
    let (_directory, mut store) = fixture();
    let (owner, _) = start(&mut store);
    let first = store.generation_checkpoint(&owner, &checkpoint(1, "one")).unwrap();
    assert_eq!(store.generation_checkpoint(&owner, &checkpoint(1, "one")).unwrap(), first);
    assert_eq!(store.revision().unwrap(), first.committed_revision.unwrap());
    assert_error(store.generation_checkpoint(&owner, &checkpoint(1, "different")), "checkpoint-integrity");
    let unchanged = store.generation_checkpoint(&owner, &checkpoint(2, "one")).unwrap();
    assert_eq!(unchanged.committed_revision, first.committed_revision);
    assert_eq!(unchanged.checkpoint.0, 2);
    assert_eq!(conversation(&store)["message"].as_array().unwrap().len(), 4);
    assert_error(store.generation_checkpoint(&owner, &checkpoint(4, "gap")), "checkpoint-sequence");
}

#[test]
fn paused_callback_cannot_overwrite_edit_or_aba_edit() {
    for restore in [false, true] {
        let (_directory, mut store) = fixture();
        let original = conversation(&store)["message"][0]["data"].as_str().unwrap().to_owned();
        let (owner, _) = start(&mut store);
        edit_message(&mut store, "concurrent");
        if restore { edit_message(&mut store, &original); }
        let before = conversation(&store);
        let result = store.generation_checkpoint(&owner, &checkpoint(1, "late callback")).unwrap();
        assert_eq!(result.phase, Phase::Stale);
        assert_eq!(conversation(&store), before);
    }
}

#[test]
fn history_count_and_serialized_utf8_bytes_include_exact_limits_and_reject_one_over() {
    let (_directory, mut store) = fixture();
    edit_message(&mut store, "é🦊\n\"");
    let messages = conversation(&store)["message"].as_array().unwrap().clone();
    let count = messages.len() as i64;
    let bytes = messages.iter().map(|message| serde_json::to_vec(message).unwrap().len()).sum::<usize>();
    let request = target(&store);
    assert_error(store.generation_snapshot_limited(&request, true, count - 1, bytes), "history-limit");
    assert_error(store.generation_snapshot_limited(&request, true, count, bytes - 1), "history-limit");
    for (max_messages, max_bytes) in [(count, bytes), (count + 1, bytes), (count, bytes + 1)] {
        let (history, _) = store.generation_snapshot_limited(&request, true, max_messages, max_bytes).unwrap();
        assert_eq!(history.messages, messages);
    }
}

#[test]
fn deleted_conversation_id_cannot_be_recreated_or_adopted_by_old_owner() {
    let (_directory, mut store) = fixture();
    let before = conversation(&store);
    let (owner, _) = start(&mut store);
    store.commit(&WorkingSetCommit {
        expected_revision: store.revision().unwrap(),
        conversations: Some(vec![ConversationMutation::Delete {
            character_id: "char-b".into(), conversation_id: "conv-beta".into(),
        }]), ..Default::default()
    }).unwrap();
    let recreate = store.commit(&WorkingSetCommit {
        expected_revision: store.revision().unwrap(),
        conversations: Some(vec![ConversationMutation::ReplaceRange {
            character_id: "char-b".into(), conversation_id: "conv-beta".into(), start: 0, delete_count: 0,
            messages: before["message"].as_array().unwrap().clone(), conversation: Some(before), configured_index: None,
        }]), ..Default::default()
    });
    assert!(matches!(recreate, Err(StoreError::Validation { message }) if message == "retired-record-id"));
    let revision = store.revision().unwrap();
    assert_eq!(store.generation_checkpoint(&owner, &checkpoint(1, "late callback")).unwrap().phase, Phase::Stale);
    assert_eq!(store.revision().unwrap(), revision);
    assert!(store.read_conversation("char-b", "conv-beta", None).unwrap().is_none());
}

#[test]
fn another_conversation_edit_does_not_cancel_or_get_overwritten_by_generation() {
    let (_directory, mut store) = fixture_with_conversations(2);
    let before = conversation(&store);
    let (owner, _) = start(&mut store);
    let mut other = before["message"][0].clone();
    other["data"] = json!("different conversation edit");
    store.commit(&WorkingSetCommit {
        expected_revision: store.revision().unwrap(),
        conversations: Some(vec![ConversationMutation::ReplaceRange {
            character_id: "char-b".into(), conversation_id: "conv-native-1".into(), start: 0, delete_count: 1,
            messages: vec![other.clone()], conversation: None, configured_index: None,
        }]), ..Default::default()
    }).unwrap();
    assert_eq!(store.generation_checkpoint(&owner, &checkpoint(1, "owned output")).unwrap().phase, Phase::Running);
    assert_eq!(&conversation(&store)["message"].as_array().unwrap()[..3], before["message"].as_array().unwrap());
    assert_eq!(conversation(&store)["message"][3]["data"], "owned output");
    let other_after = store.read_conversation("char-b", "conv-native-1", None).unwrap().unwrap().value;
    assert_eq!(other_after["message"][0], other);
    assert_eq!(store.generation_finish(&owner, false).unwrap().phase, Phase::Completed);
}

#[test]
fn cancellation_and_wrong_owner_reject_late_callbacks_without_new_writes() {
    let (_directory, mut store) = fixture();
    let (owner, _) = start(&mut store);
    let mut wrong = owner.clone();
    wrong.token = uuid::Uuid::new_v4().to_string();
    assert_error(store.generation_checkpoint(&wrong, &checkpoint(1, "wrong")), "stale-owner");
    let cancelled = store.generation_finish(&owner, true).unwrap();
    assert_eq!(cancelled.phase, Phase::Cancelled);
    assert_error(store.generation_checkpoint(&owner, &checkpoint(1, "late")), "stale-owner");
    assert_eq!(conversation(&store)["message"].as_array().unwrap().len(), 3);
}

#[test]
fn competing_connections_allocate_distinct_tickets_and_only_start_once() {
    let (_directory, mut store) = fixture();
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let mut worker = store.open_native_job_store().unwrap();
        let target = target(&store);
        let barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || { barrier.wait(); worker.generation_reserve(target).unwrap() }));
    }
    barrier.wait();
    let tickets: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
    assert_ne!(tickets[0], tickets[1]);
    assert!(serde_json::to_value(&tickets[0]).unwrap()["sequence"].is_string());
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let mut worker = store.open_native_job_store().unwrap();
        let (ticket, barrier) = (tickets[0].clone(), Arc::clone(&barrier));
        workers.push(thread::spawn(move || { barrier.wait(); worker.generation_start(&ticket).unwrap() }));
    }
    barrier.wait();
    let results: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|result| matches!(result, Start::Started { .. })).count(), 1);
    assert_error(store.generation_start(&tickets[1]), "target-active");
}

#[test]
fn bounded_reservations_and_terminal_records_never_restart_expired_tickets() {
    let (_directory, mut store) = fixture();
    let expired = store.generation_reserve(target(&store)).unwrap();
    for _ in 0..MAX_RESERVED { store.generation_reserve(target(&store)).unwrap(); }
    assert_error(store.generation_start(&expired), "expired-ticket");
    let (active, _) = start(&mut store);
    for _ in 0..MAX_RESERVED + 1 { store.generation_reserve(target(&store)).unwrap(); }
    assert_eq!(store.generation_status(&active.ticket).unwrap().phase, Phase::Running);
    store.generation_finish(&active, false).unwrap();
    for _ in 0..MAX_TERMINAL {
        let (owner, _) = start(&mut store);
        store.generation_finish(&owner, true).unwrap();
    }
    assert_error(store.generation_start(&active.ticket), "expired-ticket");
    let journal = read_journal(store.device_store().unwrap().connection()).unwrap().unwrap();
    assert!(journal.records.len() <= MAX_RESERVED + MAX_TERMINAL + MAX_ACTIVE);
    let old_epoch = Ticket { epoch: uuid::Uuid::new_v4().to_string(), sequence: journal.high_water.into() };
    assert_error(store.generation_status(&old_epoch), "expired-ticket");
}

#[test]
fn reservation_overflow_is_rejected_without_reusing_a_ticket() {
    let (_directory, mut store) = fixture();
    let ticket = store.generation_reserve(target(&store)).unwrap();
    let mut journal = read_journal(store.device_store().unwrap().connection()).unwrap().unwrap();
    journal.high_water = u64::MAX;
    write_journal(store.device_store().unwrap().connection(), &journal).unwrap();
    assert_error(store.generation_reserve(target(&store)), "ticket-overflow");
    assert_eq!(store.generation_status(&ticket).unwrap().phase, Phase::Reserved);
}

#[test]
fn restart_reclaims_all_slots_without_caller_tickets_and_reuses_target() {
    const TEST: &str = "restart_reclaims_all_slots_without_caller_tickets_and_reuses_target";
    if let Some(root) = restart_root(TEST) {
        let mut store = PersistentStore::open(&root).unwrap();
        let before = read_journal(store.device_store().unwrap().connection()).unwrap().unwrap();
        assert_eq!(before.records.values().filter(|record| record.phase == Phase::Running).count(), MAX_ACTIVE);
        assert!(before.records.values().filter(|record| record.phase == Phase::Running)
            .all(|record| record.session.as_deref() != Some(runtime_session_id())));
        let (owner, _) = start(&mut store);
        let after = read_journal(store.device_store().unwrap().connection()).unwrap().unwrap();
        assert_eq!(after.records.values().filter(|record| record.phase == Phase::Interrupted).count(), MAX_ACTIVE);
        assert_eq!(after.records.values().filter(|record| record.phase == Phase::Running).count(), 1);
        assert_eq!(owner.session, runtime_session_id());
        store.generation_finish(&owner, true).unwrap();
        return;
    }
    let (directory, mut store) = fixture_with_conversations(MAX_ACTIVE + 1);
    let mut owners = Vec::new();
    for index in 0..MAX_ACTIVE {
        let mut request = target(&store);
        if index > 0 { request.conversation_id = format!("conv-native-{index}"); }
        let ticket = store.generation_reserve(request).unwrap();
        let Start::Started { owner, .. } = store.generation_start(&ticket).unwrap() else { panic!("new operation not started") };
        owners.push(owner);
    }
    let mut extra = target(&store);
    extra.conversation_id = format!("conv-native-{MAX_ACTIVE}");
    let extra = store.generation_reserve(extra).unwrap();
    assert_error(store.generation_start(&extra), "active-limit");
    let mut peer = store.open_native_job_store().unwrap();
    assert!(peer.generation_reconcile_orphans().unwrap().is_empty());
    for owner in &owners {
        assert_eq!(owner.session, runtime_session_id());
        assert_eq!(peer.generation_recover(&owner.ticket).unwrap().phase, Phase::Running);
    }
    drop(peer);
    drop(store);
    run_in_restarted_process(TEST, directory.path());
    let mut reopened = PersistentStore::open(directory.path()).unwrap();
    for owner in owners {
        assert_eq!(reopened.generation_status(&owner.ticket).unwrap().phase, Phase::Interrupted);
        assert!(matches!(reopened.generation_start(&owner.ticket).unwrap(), Start::Existing(Status { phase: Phase::Interrupted, .. })));
        assert_error(reopened.generation_checkpoint(&owner, &checkpoint(1, "old callback")), "stale-owner");
    }
}

fn interrupt_after_reservation(store: &mut PersistentStore, owner: &Owner) -> Attempt {
    lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("synthetic process interruption"))));
    assert!(catch_unwind(AssertUnwindSafe(|| store.generation_checkpoint(owner, &checkpoint(1, "recovered native text")))).is_err());
    let attempt = store.generation_record(&owner.ticket).unwrap().attempt.unwrap();
    let digest: String = store.device_store().unwrap().connection().query_row(
        "SELECT digest FROM lww_intents WHERE request_id=?1", [&attempt.request_id], |row| row.get(0)).unwrap();
    assert_eq!(attempt.intent_digest, digest);
    attempt
}

#[test]
fn frozen_intent_replays_at_store_open_and_receipt_recovers_exactly_one_output() {
    const TEST: &str = "frozen_intent_replays_at_store_open_and_receipt_recovers_exactly_one_output";
    if let Some(root) = restart_root(TEST) {
        let mut store = PersistentStore::open(&root).unwrap();
        let recovered = store.generation_reconcile_orphans().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].phase, Phase::Interrupted);
        assert_eq!(recovered[0].checkpoint.0, 1);
        assert!(!recovered[0].pending_commit);
        return;
    }
    let (directory, mut store) = fixture();
    let (owner, _) = start(&mut store);
    let attempt = interrupt_after_reservation(&mut store, &owner);
    drop(store);
    run_in_restarted_process(TEST, directory.path());
    let mut reopened = PersistentStore::open(directory.path()).unwrap();
    let receipt: (String, i64) = reopened.connection.query_row(
        "SELECT digest,revision FROM lww_requests WHERE request_id=?1", [&attempt.request_id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(receipt.0, attempt.intent_digest);
    let result = reopened.generation_recover(&owner.ticket).unwrap();
    assert_eq!(result.phase, Phase::Interrupted);
    assert_eq!(result.committed_revision, Some(receipt.1));
    assert!(!result.pending_commit);
    assert_eq!(conversation(&reopened)["message"][3]["data"], "recovered native text");
    assert_eq!(conversation(&reopened)["message"].as_array().unwrap().len(), 4);
    assert_eq!(reopened.generation_recover(&owner.ticket).unwrap(), result);
}

#[test]
fn same_runtime_resolution_before_lww_reservation_preserves_owner_without_output() {
    let (directory, mut store) = fixture();
    let (owner, _) = start(&mut store);
    let attempt = interrupt_after_reservation(&mut store, &owner);
    // This is the durable state before reserve_intent begins its device transaction.
    store.device_store().unwrap().connection().execute("DELETE FROM lww_intents WHERE request_id=?1", [&attempt.request_id]).unwrap();
    drop(store);
    let mut reopened = PersistentStore::open(directory.path()).unwrap();
    let result = reopened.generation_recover(&owner.ticket).unwrap();
    assert_eq!(result.phase, Phase::Running);
    assert_eq!(result.committed_revision, None);
    assert_eq!(conversation(&reopened)["message"].as_array().unwrap().len(), 3);
}

fn checkpoint_with_lagging_journal(store: &mut PersistentStore, owner: &Owner) -> Status {
    let captured = Arc::new(Mutex::new(None));
    let captured_at_commit = Arc::clone(&captured);
    let observer = store.open_native_job_store().unwrap();
    let ticket = owner.ticket.clone();
    lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
        *captured_at_commit.lock().unwrap() = Some(observer.generation_record(&ticket).unwrap());
    })));
    let committed = store.generation_checkpoint(owner, &checkpoint(1, "committed")).unwrap();
    let issued = captured.lock().unwrap().take().unwrap();
    let settled = store.generation_record(&owner.ticket).unwrap();
    store.generation_replace_record(&owner.ticket, &settled, &issued).unwrap();
    committed
}

#[test]
fn same_runtime_recovery_settles_a_lost_acknowledgment_before_reporting_running() {
    let (_directory, mut store) = fixture();
    let (owner, _) = start(&mut store);
    let committed = checkpoint_with_lagging_journal(&mut store, &owner);
    assert!(store.generation_status(&owner.ticket).unwrap().pending_commit);
    let mut peer = store.open_native_job_store().unwrap();
    let recovered = peer.generation_recover(&owner.ticket).unwrap();
    assert_eq!(recovered.phase, Phase::Running);
    assert_eq!(recovered.committed_revision, committed.committed_revision);
    assert_eq!(recovered.checkpoint.0, 1);
    assert!(!recovered.pending_commit);
    assert_eq!(peer.generation_recover(&owner.ticket).unwrap(), recovered);
    assert_eq!(peer.revision().unwrap(), committed.committed_revision.unwrap());
    assert_eq!(peer.generation_checkpoint(&owner, &checkpoint(2, "continued")).unwrap().phase, Phase::Running);
    assert_eq!(conversation(&peer)["message"].as_array().unwrap().len(), 4);
    assert_eq!(conversation(&peer)["message"][3]["data"], "continued");
}

#[test]
fn a_committed_receipt_settles_a_lagging_journal_without_adopting_a_later_writer() {
    let (directory, mut store) = fixture();
    let (owner, _) = start(&mut store);
    let committed = checkpoint_with_lagging_journal(&mut store, &owner);
    edit_message(&mut store, "later writer");
    drop(store);
    let mut reopened = PersistentStore::open(directory.path()).unwrap();
    let recovered = reopened.generation_recover(&owner.ticket).unwrap();
    assert_eq!(recovered.committed_revision, committed.committed_revision);
    assert_eq!(recovered.phase, Phase::Stale);
    assert_eq!(conversation(&reopened)["message"][0]["data"], "later writer");
    assert_eq!(conversation(&reopened)["message"][3]["data"], "committed");
}

#[test]
fn corrupted_receipt_never_acknowledges_or_reissues_the_checkpoint() {
    let (_directory, mut store) = fixture();
    let (owner, _) = start(&mut store);
    let attempt = interrupt_after_reservation(&mut store, &owner);
    store.lww_recover_intents().unwrap();
    store.connection.execute("UPDATE lww_requests SET digest=?2 WHERE request_id=?1", params![attempt.request_id, "0".repeat(64)]).unwrap();
    assert_error(store.generation_recover(&owner.ticket), "receipt-integrity");
    assert!(store.generation_status(&owner.ticket).unwrap().pending_commit);
    assert_eq!(conversation(&store)["message"].as_array().unwrap().len(), 4);
}

// Each held commit takes one library transaction that finishes after the snapshot and before the segment's CAS.
fn conflict_held_commits(mut concurrent: PersistentStore, count: usize) {
    if count == 0 { return; }
    lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
        let revision = concurrent.revision().unwrap();
        crate::persistent_store::commit::incremental_commit(&mut concurrent.connection, revision, |_, _| Ok(()), |_, _, _| Ok(())).unwrap();
        conflict_held_commits(concurrent, count - 1);
    })));
}

#[test]
fn definite_revision_conflict_retries_with_a_new_frozen_request_within_a_bound() {
    for conflicts in [MAX_REVISION_RETRIES, MAX_REVISION_RETRIES + 1] {
        let (_directory, mut store) = fixture();
        let (owner, _) = start(&mut store);
        conflict_held_commits(store.open_native_job_store().unwrap(), conflicts);
        let result = store.generation_checkpoint(&owner, &checkpoint(1, "retry"));
        lww::BEFORE_HELD_COMMIT.with(|hook| hook.borrow_mut().take());
        let status = if conflicts > MAX_REVISION_RETRIES {
            assert!(matches!(result, Err(StoreError::RevisionConflict { .. })));
            let pending = store.generation_status(&owner.ticket).unwrap();
            assert!(!pending.pending_commit);
            assert_eq!((pending.phase, pending.checkpoint.0, conversation(&store)["message"].as_array().unwrap().len()), (Phase::Running, 0, 3));
            store.generation_checkpoint(&owner, &checkpoint(1, "retry")).unwrap()
        } else { result.unwrap() };
        assert_eq!((status.phase, status.checkpoint.0, status.pending_commit), (Phase::Running, 1, false));
        assert_eq!(status.committed_revision, Some(store.revision().unwrap()));
        assert_eq!(conversation(&store)["message"].as_array().unwrap().len(), 4);
        assert_eq!(conversation(&store)["message"][3]["data"], "retry");
    }
}

#[test]
fn cancellation_acknowledges_the_in_flight_commit_before_stopping_later_segments() {
    let (_directory, store) = fixture();
    let mut generation = store.open_native_job_store().unwrap();
    let (owner, _) = start(&mut generation);
    let mut cancellation = store.open_native_job_store().unwrap();
    let (issued_tx, issued_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer_owner = owner.clone();
    let writing = thread::spawn(move || {
        lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            issued_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        })));
        generation.generation_checkpoint(&writer_owner, &checkpoint(1, "issued before cancellation")).unwrap()
    });
    issued_rx.recv().unwrap();
    let cancel_owner = owner.clone();
    let (waiting_tx, waiting_rx) = mpsc::channel();
    let cancelling = thread::spawn(move || {
        FINISH_WAITING_FOR_SEGMENT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || waiting_tx.send(()).unwrap())));
        cancellation.generation_finish(&cancel_owner, true).unwrap()
    });
    waiting_rx.recv_timeout(Duration::from_secs(10)).expect("cancellation did not encounter the issued segment");
    release_tx.send(()).unwrap();
    let committed = writing.join().unwrap();
    let cancelled = cancelling.join().unwrap();
    assert_eq!(cancelled.phase, Phase::Cancelled);
    assert_eq!(cancelled.committed_revision, committed.committed_revision);
    assert!(!cancelled.pending_commit);
    let mut observer = store.open_native_job_store().unwrap();
    assert_error(observer.generation_checkpoint(&owner, &checkpoint(2, "late")), "stale-owner");
    assert_eq!(conversation(&observer)["message"][3]["data"], "issued before cancellation");
}

#[test]
fn replacement_preserves_receipts_and_invalidates_generation_ownership() {
    let (_directory, mut store) = fixture();
    let (owner, _) = start(&mut store);
    let committed = store.generation_checkpoint(&owner, &checkpoint(1, "old library")).unwrap();
    let receipts: i64 = store.connection.query_row("SELECT count(*) FROM lww_requests", [], |row| row.get(0)).unwrap();
    let stage = store.replace_begin().unwrap();
    store.replace_put_root(&stage.staging_id, &json!({})).unwrap();
    store.replace_put_presets(&stage.staging_id, &[]).unwrap();
    store.replace_commit(&stage.staging_id, Some(store.revision().unwrap())).unwrap();
    let after: i64 = store.connection.query_row("SELECT count(*) FROM lww_requests", [], |row| row.get(0)).unwrap();
    assert!(after > receipts);
    assert!(store.revision().unwrap() > committed.committed_revision.unwrap());
    assert_eq!(store.generation_checkpoint(&owner, &checkpoint(2, "wrong library")).unwrap().phase, Phase::Stale);
}

#[test]
fn snapshot_restore_keeps_receipts_but_does_not_revive_output_ownership() {
    let (_directory, mut store) = fixture();
    let snapshot = store.snapshot_create("synthetic-native-generation").unwrap();
    let (owner, _) = start(&mut store);
    let committed = store.generation_checkpoint(&owner, &checkpoint(1, "after snapshot")).unwrap();
    let receipts: i64 = store.connection.query_row("SELECT count(*) FROM lww_requests", [], |row| row.get(0)).unwrap();
    let stage = store.snapshot_restore_stage(&snapshot.id, &uuid::Uuid::new_v4().to_string()).unwrap();
    let restored = store.snapshot_restore_activate(&stage.staging_id, store.revision().unwrap(), store.lww_binding_authority().unwrap()).unwrap();
    assert!(restored.revision > committed.committed_revision.unwrap());
    let after: i64 = store.connection.query_row("SELECT count(*) FROM lww_requests", [], |row| row.get(0)).unwrap();
    assert!(after > receipts);
    assert_eq!(store.generation_finish(&owner, false).unwrap().phase, Phase::Stale);
    assert_eq!(conversation(&store)["message"].as_array().unwrap().len(), 3);
}

#[test]
fn journal_uses_existing_schema_and_is_excluded_from_public_settings_backup_and_sync() {
    use risunest_external_storage_format::section::SectionKind;
    let (directory, mut store) = fixture();
    let before_units = store.lww_read_outbox(store.lww_binding_authority().unwrap(), 1000).unwrap().entries;
    let before_rows = store.device_store_mut().unwrap().read_local_setting_rows().unwrap();
    let ticket = store.generation_reserve(target(&store)).unwrap();
    assert!(store.device_store().unwrap().read_setting(JOURNAL_KEY).is_err());
    let after_rows = store.device_store_mut().unwrap().read_local_setting_rows().unwrap();
    assert_eq!(before_rows, after_rows);
    let scratch = tempfile::tempdir().unwrap();
    let sections = store.device_store_mut().unwrap().capture_backup_sections(&[SectionKind::LocalSettings], scratch.path()).unwrap();
    assert_eq!(sections[0].len(), before_rows.len() as u64);
    let after_units = store.lww_read_outbox(store.lww_binding_authority().unwrap(), 1000).unwrap().entries;
    assert_eq!(before_units, after_units);
    drop(store);
    let reopened = PersistentStore::open(directory.path()).unwrap();
    let legacy_device_reader = device_store::DeviceStore::open(&directory.path().join("persistent")).unwrap();
    assert!(legacy_device_reader.read_setting(JOURNAL_KEY).is_err());
    assert_eq!(reopened.generation_status(&ticket).unwrap().phase, Phase::Reserved);
}

#[test]
fn unsupported_or_stale_targets_and_oversized_output_fail_without_mutation() {
    let (_directory, mut store) = fixture();
    let before = store.revision().unwrap();
    let mut stale = target(&store);
    stale.expected_revision -= 1;
    assert!(matches!(store.generation_reserve(stale), Err(StoreError::RevisionConflict { .. })));
    let mut missing = target(&store);
    missing.conversation_id = "absent".into();
    assert_error(store.generation_reserve(missing), "missing-conversation");
    let mut binding = target(&store);
    binding.binding_authority = (binding.binding_authority.0 + 1).into();
    assert_error(store.generation_reserve(binding), "stale-target");
    let (owner, _) = start(&mut store);
    assert_error(store.generation_checkpoint(&owner, &checkpoint(0, "bad sequence")), "checkpoint-limit");
    assert_error(store.generation_checkpoint(&owner, &Checkpoint { sequence: 1, text: "x".repeat(MAX_OUTPUT_BYTES + 1) }), "checkpoint-limit");
    assert_eq!(store.revision().unwrap(), before);
}

// The top-level members every experimental reader before the envelope required.
#[derive(Deserialize)]
#[allow(dead_code)]
struct PriorJournal { epoch: String, high_water: u64, records: BTreeMap<String, serde::de::IgnoredAny> }

fn write_body(store: &PersistentStore, body: &str) {
    store.device_store().unwrap().connection().execute("UPDATE device_settings SET value=?2 WHERE key=?1", params![JOURNAL_KEY, body]).unwrap();
}

// Byte layouts of the unwrapped journal writers: 3c31b2fa8 (unchanged at 2c7bec3b8) and c0d0490dd (unchanged through 9592a63a3).
// Only scalar leaves are filled from the live store, so a later Record change cannot reshape these bodies.
const LEGACY_JOURNAL: &str = r#"{"epoch":$epoch,"high_water":$high_water,"records":{$records}}"#;
const LEGACY_RECORD_A: &str = r#"{"target":{"characterId":$target.characterId,"conversationId":$target.conversationId,"expectedRevision":$target.expectedRevision,"libraryIdentity":$target.libraryIdentity,"bindingAuthority":$target.bindingAuthority},"phase":$phase,"session":$session,"owner":$owner,"message_id":$message_id,"message_time":$message_time,"fence":{"character":$character,"conversation":$conversation,"messages":$messages,"count":$fence.count},"initial_count":$initial_count,"checkpoint":$checkpoint,"output_hash":$output_hash,"committed_revision":$committed_revision,"attempt":$attempt}"#;
const LEGACY_ATTEMPT_A: &str = r#"{"sequence":$attempt.kind.Output.sequence,"request_id":$attempt.request_id,"intent_digest":$attempt.intent_digest,"output_hash":$attempt.kind.Output.output_hash,"expected_revision":$attempt.expected_revision}"#;
const LEGACY_RECORD_B: &str = r#"{"target":{"characterId":$target.characterId,"conversationId":$target.conversationId,"expectedRevision":$target.expectedRevision,"libraryIdentity":$target.libraryIdentity,"bindingAuthority":$target.bindingAuthority},"phase":$phase,"session":$session,"owner":$owner,"message_id":$message_id,"message_time":$message_time,"fence":{"character":$character,"conversation":$conversation,"messages":$messages,"count":$fence.count},"initial_count":$initial_count,"checkpoint":$checkpoint,"output_hash":$output_hash,"committed_revision":$committed_revision,"attempt":$attempt,"preparation":$preparation}"#;
const LEGACY_ATTEMPT_B: &str = r#"{"request_id":$attempt.request_id,"intent_digest":$attempt.intent_digest,"expected_revision":$attempt.expected_revision,"kind":$kind}"#;
const LEGACY_OUTPUT_KIND_B: &str = r#"{"Output":{"sequence":$attempt.kind.Output.sequence,"output_hash":$attempt.kind.Output.output_hash}}"#;
const LEGACY_PREPARATION_B: &str = r#"{"preset_id":$preparation.preset_id,"normalization":$normalization,"ready":$preparation.ready,"error":$error}"#;

// Replaces each `$path` with the scalar found at that path of `value`; composite members are chosen by the caller.
fn literal_body(template: &str, value: &Value, parts: &[(&str, String)]) -> String {
    let mut text = template.to_owned();
    for (name, part) in parts { text = text.replace(&format!("${name}"), part); }
    let mut body = String::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find('$') {
        body.push_str(&rest[..start]);
        let path = &rest[start + 1..];
        let end = path.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.')).unwrap_or(path.len());
        let leaf = path[..end].split('.').try_fold(value, |node, key| key.parse::<usize>().ok().and_then(|index| node.get(index)).or_else(|| node.get(key)))
            .unwrap_or_else(|| panic!("missing legacy leaf {}", &path[..end]));
        assert!(!leaf.is_object() && !leaf.is_array(), "legacy leaf {} is not scalar", &path[..end]);
        body.push_str(&serde_json::to_string(leaf).unwrap());
        rest = &path[end..];
    }
    body.push_str(rest);
    body
}

fn legacy_record(record: &Value, earliest: bool) -> String {
    assert!(record["execution"].is_null(), "legacy writers never stored an execution");
    let unit = |name: &str| if record["fence"][name].is_null() { "null".to_owned() } else {
        literal_body(&format!(r#"{{"stamp":$fence.{name}.stamp,"version":$fence.{name}.version}}"#), record, &[])
    };
    let attempt = if record["attempt"].is_null() { "null".to_owned() } else if earliest {
        literal_body(LEGACY_ATTEMPT_A, record, &[])
    } else {
        let kind = if record["attempt"]["kind"] == "HistoryIds" { r#""HistoryIds""#.to_owned() } else { literal_body(LEGACY_OUTPUT_KIND_B, record, &[]) };
        literal_body(LEGACY_ATTEMPT_B, record, &[("kind", kind)])
    };
    let mut parts = vec![("character", unit("character")), ("conversation", unit("conversation")), ("messages", unit("messages")), ("attempt", attempt)];
    if earliest {
        assert!(record["preparation"].is_null(), "the earliest writer had no preparation");
        return literal_body(LEGACY_RECORD_A, record, &parts);
    }
    let preparation = &record["preparation"];
    parts.push(("preparation", if preparation.is_null() { "null".to_owned() } else {
        let normalization = if let Some(assignments) = preparation["normalization"]["Pending"]["assignments"].as_array() {
            let items: Vec<_> = (0..assignments.len()).map(|index| literal_body(&format!(
                r#"{{"index":$preparation.normalization.Pending.assignments.{index}.index,"original_hash":$preparation.normalization.Pending.assignments.{index}.original_hash,"id":$preparation.normalization.Pending.assignments.{index}.id}}"#),
                record, &[])).collect();
            format!(r#"{{"Pending":{{"assignments":[{}]}}}}"#, items.join(","))
        } else if preparation["normalization"]["Pending"].is_object() {
            r#"{"Pending":{"assignments":null}}"#.to_owned()
        } else if preparation["normalization"]["Settled"]["receipt"].is_null() {
            r#"{"Settled":{"receipt":null}}"#.to_owned()
        } else {
            literal_body(r#"{"Settled":{"receipt":[$preparation.normalization.Settled.receipt.0,$preparation.normalization.Settled.receipt.1,$preparation.normalization.Settled.receipt.2]}}"#, record, &[])
        };
        let error = match &preparation["error"] {
            Value::Null => "null".to_owned(),
            error if error.get("field").is_some() => literal_body(r#"{"kind":$preparation.error.kind,"field":$preparation.error.field}"#, record, &[]),
            _ => literal_body(r#"{"kind":$preparation.error.kind}"#, record, &[]),
        };
        literal_body(LEGACY_PREPARATION_B, record, &[("normalization", normalization), ("error", error)])
    }));
    literal_body(LEGACY_RECORD_B, record, &parts)
}

// The stored journal as the c0d0490dd writer wrote it, or as the earlier 3c31b2fa8 writer did.
fn legacy_body(store: &PersistentStore, earliest: bool) -> String {
    let envelope: Value = serde_json::from_str(&journal_body(store).unwrap()).unwrap();
    let journal = &envelope["journal"];
    let records: Vec<_> = journal["records"].as_object().unwrap().iter()
        .map(|(key, record)| format!("{}:{}", serde_json::to_string(key).unwrap(), legacy_record(record, earliest))).collect();
    literal_body(LEGACY_JOURNAL, journal, &[("records", records.join(","))])
}

fn receipt_count(store: &PersistentStore) -> i64 {
    store.connection.query_row("SELECT count(*) FROM lww_requests", [], |row| row.get(0)).unwrap()
}

#[test]
fn journal_envelope_converts_both_legacy_shapes_once_without_library_effects() {
    for earliest in [true, false] {
        let (_directory, mut store, tickets, query) = if earliest {
            let (directory, mut store) = fixture();
            let (completed, _) = start(&mut store);
            store.generation_checkpoint(&completed, &checkpoint(1, "complete")).unwrap();
            store.generation_finish(&completed, false).unwrap();
            let (running, _) = start(&mut store);
            interrupt_after_reservation(&mut store, &running);
            let reserved = store.generation_reserve(target(&store)).unwrap();
            (directory, store, vec![completed.ticket, running.ticket, reserved], protection_target())
        } else {
            let mut database = super::preparation_tests::database(&json!({"messages":super::preparation_tests::cases()["effectMessages"]}));
            let mut other = database["characters"][0]["chats"][0].clone();
            other["id"] = json!("other");
            database["characters"][0]["chats"].as_array_mut().unwrap().push(other);
            let (directory, mut store, intent) = super::preparation_tests::stored(&database);
            let settled = service::reserve_ollama(&mut store, &intent).unwrap();
            assert!(matches!(service::prepare_reserved_ollama(&mut store, &settled).unwrap(), PreparedGeneration::Ready { .. }));
            service::cancel_generation(&mut store, &settled).unwrap();
            let mut pending = intent.clone();
            pending.target.conversation_id = "other".into();
            pending.target.expected_revision = store.revision().unwrap();
            let pending = effects_owner(&mut store, &pending);
            interrupt_ids_after_intent(&mut store, &pending);
            let mut again = intent;
            again.target.expected_revision = store.revision().unwrap();
            let reserved = service::reserve_ollama(&mut store, &again).unwrap();
            (directory, store, vec![settled, pending.ticket, reserved], NativeGenerationTarget::Character { character_id: "character" })
        };
        let envelope: Value = serde_json::from_str(&journal_body(&store).unwrap()).unwrap();
        let statuses: Vec<_> = tickets.iter().map(|ticket| store.generation_status(ticket).unwrap()).collect();
        assert!(statuses[1].pending_commit);
        let (revision, receipts) = (store.revision().unwrap(), receipt_count(&store));
        let legacy = legacy_body(&store, earliest);
        write_body(&store, &legacy);
        assert!(serde_json::from_str::<PriorJournal>(&legacy).is_ok());
        // Readers decode a legacy body in place and never convert it.
        assert_eq!(tickets.iter().map(|ticket| store.generation_status(ticket).unwrap()).collect::<Vec<_>>(), statuses);
        assert!(protected(&store, query).unwrap());
        assert_eq!(store.generation_projection_record(&tickets[0]).unwrap(), store.generation_record(&tickets[0]).unwrap());
        assert_eq!(journal_body(&store).as_deref(), Some(legacy.as_str()));
        if earliest {
            store.device_store().unwrap().connection().execute_batch(
                "CREATE TEMP TRIGGER conversion_failure BEFORE UPDATE ON device_settings WHEN OLD.key='native-generation-journal-v1'
                 BEGIN SELECT RAISE(ABORT,'synthetic conversion failure'); END;").unwrap();
            assert!(store.generation_convert_journal().is_err());
            assert_eq!(journal_body(&store).as_deref(), Some(legacy.as_str()));
            store.device_store().unwrap().connection().execute_batch("DROP TRIGGER conversion_failure").unwrap();
        }
        store.generation_convert_journal().unwrap();
        let converted = journal_body(&store).unwrap();
        // A legacy body has no send effects, timing or recorded draws; conversion gives them the defaults that never rerun an
        // effect, and a reserved send draws afresh.
        let mut expected = envelope.clone();
        for record in expected["journal"]["records"].as_object_mut().unwrap().values_mut() {
            if let Some(preparation) = record.get_mut("preparation").filter(|value| value.is_object()) {
                preparation["effects"] = json!({"interaction":{"Settled":{"receipt":null}},"variables":{"Settled":{"receipt":null}},"memory":{"Settled":{"receipt":null}}});
                preparation["normalized_at"] = Value::Null;
                preparation["ready_at"] = Value::Null;
                preparation.as_object_mut().unwrap().remove("entropy");
            }
        }
        assert_eq!(serde_json::from_str::<Value>(&converted).unwrap(), expected);
        assert!(serde_json::from_str::<PriorJournal>(&converted).is_err());
        store.generation_convert_journal().unwrap();
        assert_eq!(journal_body(&store).unwrap(), converted);
        assert_eq!((store.revision().unwrap(), receipt_count(&store)), (revision, receipts));
        assert_eq!(tickets.iter().map(|ticket| store.generation_status(ticket).unwrap()).collect::<Vec<_>>(), statuses);
        // Ordinary recovery then resolves the old pending attempt exactly once.
        let recovered = store.generation_recover(&tickets[1]).unwrap();
        assert!(!recovered.pending_commit);
        assert_eq!(recovered.committed_revision, Some(revision + 1));
        assert_eq!(store.revision().unwrap(), revision + 1);
    }
}

// A reservation keeps its admission's draws and the WebView's host capture until the preparation settles; a record
// written before reservations kept them prepares with fresh draws and no capture. A capture journaled while the probe
// reported `from17` (Unicode 17 or newer) still reads, and its case mapping beyond ASCII declines.
#[test]
fn a_reservation_keeps_its_draws_until_preparation_and_a_record_without_them_prepares() {
    use crate::native_generation::cbs::{CaseUnicode, HostCapture};
    let now = i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()).unwrap();
    for kept in [true, false] {
        let (_directory, mut store, mut intent) = super::preparation_tests::stored(&super::preparation_tests::database(&json!({})));
        intent.host = Some(HostCapture::new(false, CaseUnicode::Unicode16, vec![(now - 86_400_000, 0)], now + 86_400_000));
        let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
        let key = ticket.sequence.0.to_string();
        let mut body: Value = serde_json::from_str(&journal_body(&store).unwrap()).unwrap();
        let preparation = body["journal"]["records"][&key]["preparation"].as_object_mut().unwrap();
        assert!(preparation["entropy"]["draws"].is_array(), "{preparation:?}");
        assert_eq!(preparation["host"]["caseUnicode"], "unicode16", "{preparation:?}");
        if !kept {
            preparation.remove("entropy");
            preparation.remove("host");
            write_body(&store, &serde_json::to_string(&body).unwrap());
        }
        assert!(matches!(service::prepare_reserved_ollama(&mut store, &ticket).unwrap(), PreparedGeneration::Ready { .. }));
        let settled: Value = serde_json::from_str(&journal_body(&store).unwrap()).unwrap();
        assert_eq!(settled["journal"]["records"][&key]["preparation"]["ready"], true);
        assert!(settled["journal"]["records"][&key]["preparation"].get("entropy").is_none(), "{settled}");
        assert!(settled["journal"]["records"][&key]["preparation"].get("host").is_none(), "{settled}");
    }
    // The CBS stage and the example messages each read the journaled tables; Unicode 16 maps both like Rust.
    for (patch, refusal) in [
        (json!({"preset":{"mainPrompt":"{{upper::\u{e9}}}"}}), "cbs.case-table"),
        (json!({"character":{"exampleMessage":"<START>\n\u{c9}mile: hi"}}), "example-case-table"),
    ] {
        let (_directory, mut store, mut intent) = super::preparation_tests::stored(&super::preparation_tests::database(&patch));
        intent.host = Some(HostCapture::new(false, CaseUnicode::Unicode16, vec![(now - 86_400_000, 0)], now + 86_400_000));
        let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
        let key = ticket.sequence.0.to_string();
        let mut body: Value = serde_json::from_str(&journal_body(&store).unwrap()).unwrap();
        body["journal"]["records"][&key]["preparation"]["host"]["caseUnicode"] = json!("from17");
        write_body(&store, &serde_json::to_string(&body).unwrap());
        let PreparedGeneration::Failed { error, .. } = service::prepare_reserved_ollama(&mut store, &ticket).unwrap() else { panic!("{refusal}") };
        assert_eq!(error, PreparationError::Unsupported(refusal.into()));
    }
}

#[test]
fn journal_decoder_refuses_unknown_mixed_duplicate_and_malformed_bodies_without_changes() {
    let (_directory, mut store) = fixture();
    let (owner, _) = start(&mut store);
    interrupt_after_reservation(&mut store, &owner);
    let reserved = store.generation_reserve(target(&store)).unwrap();
    let envelope = journal_body(&store).unwrap();
    let value: Value = serde_json::from_str(&envelope).unwrap();
    let journal = serde_json::to_string(&value["journal"]).unwrap();
    let legacy = legacy_body(&store, false);
    let key = owner.ticket.sequence.0.to_string();
    let record = serde_json::to_string(&value["journal"]["records"][&key]).unwrap();
    let with = |patch: &dyn Fn(&mut Value)| { let mut changed = value.clone(); patch(&mut changed); serde_json::to_string(&changed).unwrap() };
    let legacy_with = |patch: &dyn Fn(&mut Value)| {
        let mut changed: Value = serde_json::from_str(&legacy).unwrap(); patch(&mut changed); serde_json::to_string(&changed).unwrap()
    };
    let revision = store.revision().unwrap();
    // An unknown member beside a known version is read as that version and never rewritten by a read or conversion.
    let status = store.generation_status(&owner.ticket).unwrap();
    let extended = format!("{},\"future\":true}}", &envelope[..envelope.len() - 1]);
    write_body(&store, &extended);
    assert_eq!(read_journal(store.device_store().unwrap().connection()).unwrap().unwrap().records.len(), 2);
    assert_eq!(store.generation_status(&owner.ticket).unwrap(), status);
    assert!(protected(&store, protection_target()).unwrap());
    assert_eq!(store.generation_projection_record(&owner.ticket).unwrap(), store.generation_record(&owner.ticket).unwrap());
    store.generation_convert_journal().unwrap();
    assert_eq!(journal_body(&store).as_deref(), Some(extended.as_str()));
    assert_eq!(store.revision().unwrap(), revision);
    let cases: Vec<(String, &str)> = vec![
        (envelope.replacen("\"version\":3", "\"version\":4", 1), "journal-version"),
        (envelope.replacen("\"version\":3", "\"version\":\"3\"", 1), "journal-version"),
        (envelope.replacen("\"version\":3", "\"version\":3.0", 1), "journal-version"),
        (format!("{{\"journal\":{journal}}}"), "journal-shape"),
        ("{\"version\":2}".into(), "journal-shape"),
        ("{}".into(), "journal-shape"),
        (with(&|value| value["records"] = value["journal"]["records"].clone()), "journal-shape"),
        (legacy_with(&|value| value["version"] = json!(2)), "journal-shape"),
        (legacy_with(&|value| value["future"] = json!(true)), "journal-shape"),
        (format!("{{\"version\":2,\"version\":2,\"journal\":{journal}}}"), "journal-format"),
        (format!("{{\"version\":2,\"journal\":{journal},\"journal\":{journal}}}"), "journal-format"),
        (envelope.replacen(&format!("\"{key}\":"), &format!("\"0{key}\":{record},\"{key}\":"), 1), "journal-format"),
        (envelope.replacen("{\"target\":", "{\"phase\":\"reserved\",\"target\":", 1), "journal-format"),
        (with(&|value| { value["journal"]["records"][&key].as_object_mut().unwrap().remove("execution"); }), "journal-format"),
        (legacy_with(&|value| value["records"][&key]["future"] = json!(true)), "journal-format"),
        (legacy_with(&|value| { value["records"][&key]["attempt"]["sequence"] = json!(1); value["records"][&key]["attempt"]["output_hash"] = json!("0".repeat(64)); }), "journal-shape"),
        (legacy_with(&|value| { value["records"][&key]["attempt"].as_object_mut().unwrap().remove("kind"); value["records"][&key]["attempt"]["sequence"] = json!(1); }), "journal-shape"),
        (legacy_with(&|value| { value["records"][&key].as_object_mut().unwrap().remove("preparation"); }), "journal-shape"),
        ("[]".into(), "journal-format"),
        ("{".into(), "journal-format"),
    ];
    for (body, code) in cases {
        write_body(&store, &body);
        assert_error(read_journal(store.device_store().unwrap().connection()), code);
        assert_error(store.generation_status(&owner.ticket), code);
        assert_error(protected(&store, NativeGenerationTarget::Character { character_id: "unrelated" }), code);
        assert_error(store.generation_projection_record(&owner.ticket), code);
        assert_error(store.generation_convert_journal(), code);
        assert_error(store.generation_reserve(target(&store)), code);
        assert_error(store.generation_cancel(&reserved), code);
        assert_error(store.generation_recover(&owner.ticket), code);
        assert_eq!(journal_body(&store).as_deref(), Some(body.as_str()), "{code}");
        assert_eq!(store.revision().unwrap(), revision);
    }
}

#[test]
fn journal_conversion_checks_read_and_envelope_bounds_before_replacing_without_pruning() {
    let (_directory, mut store) = fixture();
    let (owner, _) = start(&mut store);
    interrupt_after_reservation(&mut store, &owner);
    for _ in 0..3 { store.generation_reserve(target(&store)).unwrap(); }
    let envelope = journal_body(&store).unwrap();
    let legacy = legacy_body(&store, true);
    assert!(legacy.len() < envelope.len());
    write_body(&store, &legacy);
    for limit in [legacy.len() - 1, envelope.len() - 1] {
        assert_error(store.generation_convert_journal_limited(limit), "protection-journal-limit");
        assert_eq!(journal_body(&store).as_deref(), Some(legacy.as_str()));
    }
    store.generation_convert_journal_limited(envelope.len()).unwrap();
    assert_eq!(journal_body(&store).unwrap(), envelope);
}

fn owned_fixture(conversations: usize) -> (tempfile::TempDir, PersistentStore, Vec<OllamaIntent>) {
    let mut database = super::preparation_tests::database(&json!({}));
    let prototype = database["characters"][0]["chats"][0].clone();
    for index in 1..conversations {
        let mut conversation = prototype.clone();
        conversation["id"] = json!(format!("conversation-{index}"));
        database["characters"][0]["chats"].as_array_mut().unwrap().push(conversation);
    }
    let (directory, store, intent) = super::preparation_tests::stored(&database);
    let intents = (0..conversations).map(|index| {
        let mut intent = intent.clone();
        if index > 0 { intent.target.conversation_id = format!("conversation-{index}"); }
        intent
    }).collect();
    (directory, store, intents)
}

// The owner path: ID normalization, owned preparation, the ready mark with a lifecycle, then the memory effect
// the dispatch claim waits for.
fn ready_owned(store: &mut PersistentStore, intent: &OllamaIntent) -> (Owner, String) {
    let mut intent = intent.clone();
    intent.target.expected_revision = store.revision().unwrap();
    let owner = effects_owner(store, &intent);
    store.generation_ensure_history_ids(&owner).unwrap();
    let snapshot = store.generation_owned_preparation_snapshot(&owner).unwrap().unwrap();
    crate::native_generation::prepare::prepare_ollama(&snapshot).unwrap();
    let lifecycle = uuid::Uuid::new_v4().to_string();
    let status = store.generation_mark_ready_owned(&owner, &lifecycle).unwrap();
    assert!(status.preparation_ready && status.phase == Phase::Running);
    assert_eq!(execution(store, &owner.ticket), Execution { lifecycle_id: lifecycle.clone(), dispatch_id: None, cancel: None, terminal: None, output_receipt: None });
    store.generation_send_variables(&owner, &lifecycle, &PreparedVariablePlan::default()).unwrap();
    store.generation_send_memory(&owner, &lifecycle, None).unwrap();
    (owner, lifecycle)
}

fn issued(store: &mut PersistentStore, intent: &OllamaIntent) -> (Owner, String) {
    let (owner, lifecycle) = ready_owned(store, intent);
    let DispatchClaim::Issued { dispatch_id, status } = store.generation_claim_dispatch(&owner, &lifecycle).unwrap() else { panic!("dispatch not issued") };
    assert_eq!(status.phase, Phase::Running);
    assert_eq!(execution(store, &owner.ticket).dispatch_id, Some(dispatch_id));
    (owner, lifecycle)
}

fn execution(store: &PersistentStore, ticket: &Ticket) -> Execution {
    store.generation_execution(ticket).unwrap().expect("execution member")
}

fn text_hash(text: &str) -> String {
    risunest_sync_wire::hash(text.as_bytes())
}

fn normal(sequence: u64, text: &str) -> TerminalDecision {
    TerminalDecision::Normal { sequence, output_hash: text_hash(text) }
}

fn failure() -> TerminalDecision {
    TerminalDecision::Failed { failure: FailureDescriptor { category: "http".into(), detail: None, stage: Some("response".into()), http_status: Some(503) } }
}

#[test]
fn dispatch_claim_is_one_compare_and_replace_per_lifecycle() {
    let (_directory, mut store, intents) = owned_fixture(1);
    let query = NativeGenerationTarget::Character { character_id: "character" };
    let (owner, lifecycle) = ready_owned(&mut store, &intents[0]);
    assert!(protected(&store, query).unwrap());
    let revision = store.revision().unwrap();
    assert_error(store.generation_claim_dispatch(&owner, &uuid::Uuid::new_v4().to_string()), "stale-lifecycle");
    let barrier = Arc::new(Barrier::new(3));
    let workers: Vec<_> = (0..2).map(|_| {
        let mut worker = store.open_native_job_store().unwrap();
        let (owner, lifecycle, barrier) = (owner.clone(), lifecycle.clone(), Arc::clone(&barrier));
        thread::spawn(move || { barrier.wait(); worker.generation_claim_dispatch(&owner, &lifecycle) })
    }).collect();
    barrier.wait();
    let results: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
    let issued: Vec<_> = results.iter().filter_map(|result| match result {
        Ok(DispatchClaim::Issued { dispatch_id, .. }) => Some(dispatch_id.clone()), _ => None,
    }).collect();
    assert_eq!(issued.len(), 1, "{results:?}");
    assert!(results.iter().any(|result| matches!(result, Err(StoreError::Validation { message }) if message == "native-generation:dispatch-claimed")));
    assert_eq!(execution(&store, &owner.ticket).dispatch_id.as_deref(), Some(issued[0].as_str()));
    let before = journal_body(&store);
    assert_error(store.generation_claim_dispatch(&owner, &lifecycle), "dispatch-claimed");
    assert_eq!(journal_body(&store), before);
    assert_eq!(store.revision().unwrap(), revision);
    assert!(protected(&store, query).unwrap());
}

#[test]
fn restart_settles_owned_records_by_dispatch_and_cancel_without_a_new_claim() {
    const TEST: &str = "restart_settles_owned_records_by_dispatch_and_cancel_without_a_new_claim";
    if let Some(root) = restart_root(TEST) {
        let input: Value = serde_json::from_slice(&std::fs::read(root.join("owned-restart.json")).unwrap()).unwrap();
        let owners: Vec<(Owner, String)> = input["owners"].as_array().unwrap().iter().map(|entry| (Owner {
            ticket: serde_json::from_value(entry["ticket"].clone()).unwrap(), session: entry["session"].as_str().unwrap().into(),
            token: entry["token"].as_str().unwrap().into(),
        }, entry["lifecycle"].as_str().unwrap().to_owned())).collect();
        let mut store = PersistentStore::open(&root).unwrap();
        let revision = store.revision().unwrap();
        let before: Vec<_> = owners.iter().map(|(owner, _)| execution(&store, &owner.ticket)).collect();
        let statuses = store.generation_reconcile_orphans().unwrap();
        assert_eq!(statuses.iter().map(|status| (status.ticket.clone(), status.phase.clone())).collect::<Vec<_>>(), vec![
            (owners[0].0.ticket.clone(), Phase::Cancelled), (owners[1].0.ticket.clone(), Phase::Interrupted),
            (owners[2].0.ticket.clone(), Phase::Interrupted)]);
        assert!(matches!(store.generation_claim_dispatch(&owners[0].0, &owners[0].1).unwrap(), DispatchClaim::Refused(Status { phase: Phase::Cancelled, .. })));
        for (owner, lifecycle) in &owners[1..] {
            assert_error(store.generation_claim_dispatch(owner, lifecycle), "dispatch-claimed");
            assert_error(store.generation_checkpoint(owner, &checkpoint(1, "after restart")), "stale-owner");
        }
        assert_eq!(owners.iter().map(|(owner, _)| execution(&store, &owner.ticket)).collect::<Vec<_>>(), before);
        assert_eq!(store.revision().unwrap(), revision);
        return;
    }
    let (directory, mut store, intents) = owned_fixture(3);
    let ready = ready_owned(&mut store, &intents[0]);
    let cancelled = issued(&mut store, &intents[1]);
    let running = issued(&mut store, &intents[2]);
    store.generation_cancel(&ready.0.ticket).unwrap();
    store.generation_cancel(&cancelled.0.ticket).unwrap();
    // A live runtime leaves dispatched owners to settle themselves.
    for (owner, _) in [&cancelled, &running] {
        assert_eq!(store.generation_recover(&owner.ticket).unwrap().phase, Phase::Running);
    }
    let owners: Vec<_> = [&ready, &cancelled, &running].iter().map(|(owner, lifecycle)| json!({
        "ticket": owner.ticket, "session": owner.session, "token": owner.token, "lifecycle": lifecycle,
    })).collect();
    std::fs::write(directory.path().join("owned-restart.json"), serde_json::to_vec(&json!({"owners": owners})).unwrap()).unwrap();
    drop(store);
    run_in_restarted_process(TEST, directory.path());
}

#[test]
fn execution_owned_records_refuse_legacy_mutators_in_every_owned_state() {
    for state in ["ready", "issued", "offered"] {
        let (_directory, mut store, intents) = owned_fixture(1);
        let (owner, lifecycle) = if state == "ready" { ready_owned(&mut store, &intents[0]) } else { issued(&mut store, &intents[0]) };
        if state == "offered" { interrupt_after_reservation(&mut store, &owner); }
        let before = journal_body(&store);
        let revision = store.revision().unwrap();
        assert_error(store.generation_finish(&owner, false), "execution-owned");
        assert_error(store.generation_finish(&owner, true), "execution-owned");
        assert_error(store.generation_mark_prepared(&owner), "execution-owned");
        assert_error(store.generation_fail_preparation(&owner, PreparationError::ContextLimit), "execution-owned");
        assert_error(store.generation_mark_ready_owned(&owner, &uuid::Uuid::new_v4().to_string()), "execution-owned");
        let replay = store.generation_mark_ready_owned(&owner, &lifecycle).unwrap();
        assert_eq!((replay.phase, replay.pending_commit), (Phase::Running, state == "offered"));
        assert_eq!(journal_body(&store), before, "{state}");
        assert_eq!(store.revision().unwrap(), revision);
    }
}

#[test]
fn owned_cancel_persists_a_decision_and_leaves_settlement_to_owner_or_recovery() {
    // Before dispatch the decision blocks the claim, and recovery settles Cancelled without a request.
    let (_directory, mut store, intents) = owned_fixture(1);
    let (owner, lifecycle) = ready_owned(&mut store, &intents[0]);
    let revision = store.revision().unwrap();
    let cancelled = store.generation_cancel(&owner.ticket).unwrap();
    assert_eq!(cancelled.phase, Phase::Running);
    assert_eq!(store.generation_cancel_for(&owner.ticket, CancelReason::Edit).unwrap(), cancelled);
    assert_eq!(execution(&store, &owner.ticket).cancel, Some(CancelReason::User));
    assert!(matches!(store.generation_claim_dispatch(&owner, &lifecycle).unwrap(), DispatchClaim::Refused(Status { phase: Phase::Running, .. })));
    assert_error(store.generation_checkpoint(&owner, &checkpoint(1, "never sent")), "dispatch-required");
    assert_eq!(execution(&store, &owner.ticket).dispatch_id, None);
    assert_eq!(store.generation_recover(&owner.ticket).unwrap().phase, Phase::Cancelled);
    assert_eq!(store.revision().unwrap(), revision);

    // After dispatch the decision waits for the owner, whose settlement is final.
    let (_directory, mut store, intents) = owned_fixture(1);
    let (owner, _) = issued(&mut store, &intents[0]);
    assert_eq!(store.generation_cancel_for(&owner.ticket, CancelReason::Plugin).unwrap().phase, Phase::Running);
    assert_eq!(store.generation_recover(&owner.ticket).unwrap().phase, Phase::Running);
    assert_eq!(execution(&store, &owner.ticket).cancel, Some(CancelReason::Plugin));
    let settled = store.generation_settle_owned(&owner, TerminalDecision::Cancelled { reason: CancelReason::Plugin }).unwrap();
    assert_eq!((settled.phase.clone(), settled.checkpoint.0), (Phase::Cancelled, 0));
    let body = journal_body(&store);
    assert_eq!(store.generation_cancel(&owner.ticket).unwrap(), settled);
    assert_eq!(journal_body(&store), body);
}

#[test]
fn journal_lagging_owned_output_resolves_once_from_its_receipt() {
    let (_directory, mut store, intents) = owned_fixture(1);
    let (owner, _) = issued(&mut store, &intents[0]);
    let rows = effects_conversation(&store)["message"].as_array().unwrap().len();
    let committed = checkpoint_with_lagging_journal(&mut store, &owner);
    let lagging = store.generation_record(&owner.ticket).unwrap();
    let attempt = lagging.attempt.clone().unwrap();
    assert_eq!(lagging.execution.unwrap().output_receipt, None);
    let receipts = receipt_count(&store);
    let expected = OutputReceipt { request_id: attempt.request_id, intent_digest: attempt.intent_digest,
        receipt_revision: committed.committed_revision.unwrap(), source_sequence: 1, output_hash: text_hash("committed") };
    assert_eq!(store.generation_resolve_output(&owner, 1, &text_hash("committed")).unwrap(), OutputResolution::Committed(expected.clone()));
    assert_eq!(execution(&store, &owner.ticket).output_receipt, Some(expected.clone()));
    let body = journal_body(&store);
    assert_eq!(store.generation_resolve_output(&owner, 1, &text_hash("committed")).unwrap(), OutputResolution::Committed(expected.clone()));
    assert_eq!(journal_body(&store), body);
    assert_eq!((store.revision().unwrap(), receipt_count(&store)), (committed.committed_revision.unwrap(), receipts));
    assert_eq!(effects_conversation(&store)["message"].as_array().unwrap().len(), rows + 1);
    // A retained tuple that disagrees with the actual receipt is an integrity failure, not evidence.
    store.connection.execute("UPDATE lww_requests SET digest=?2 WHERE request_id=?1", params![expected.request_id, "0".repeat(64)]).unwrap();
    assert_error(store.generation_resolve_output(&owner, 1, &text_hash("committed")), "receipt-integrity");
    assert_eq!(journal_body(&store), body);
    store.connection.execute("UPDATE lww_requests SET digest=?2 WHERE request_id=?1", params![expected.request_id, expected.intent_digest]).unwrap();
    assert_eq!(store.generation_settle_owned(&owner, normal(1, "committed")).unwrap().phase, Phase::Completed);

    // A normal decision admitted while its offer still lags is kept and completes once that offer resolves.
    let (_directory, mut store, intents) = owned_fixture(1);
    let (owner, _) = issued(&mut store, &intents[0]);
    checkpoint_with_lagging_journal(&mut store, &owner);
    assert!(store.generation_status(&owner.ticket).unwrap().pending_commit);
    assert_eq!(store.generation_settle_owned(&owner, normal(1, "committed")).unwrap().phase, Phase::Completed);
    assert_eq!(execution(&store, &owner.ticket).output_receipt.map(|receipt| receipt.source_sequence), Some(1));

    // A frozen output stays unresolved while its intent cannot replay, then is proven uncommitted once it never reached the library.
    let (_directory, mut store, intents) = owned_fixture(1);
    let (owner, _) = issued(&mut store, &intents[0]);
    let attempt = interrupt_after_reservation(&mut store, &owner);
    store.device_store().unwrap().connection().execute(
        "INSERT INTO lww_intent_failures(request_id,failures,error,quarantined) VALUES(?1,1,'synthetic',1)", [&attempt.request_id]).unwrap();
    let body = journal_body(&store);
    assert_eq!(store.generation_resolve_output(&owner, 1, &text_hash("recovered native text")).unwrap(), OutputResolution::Unresolved);
    assert_eq!(journal_body(&store), body);
    for table in ["lww_intent_failures", "lww_intents"] {
        store.device_store().unwrap().connection().execute(&format!("DELETE FROM {table} WHERE request_id=?1"), [&attempt.request_id]).unwrap();
    }
    // A decision admitted with a pending offer stays durable when that offer proves uncommitted, and recovery never completes it.
    assert_error(store.generation_settle_owned(&owner, normal(1, "recovered native text")), "output-evidence-unavailable");
    assert_eq!(execution(&store, &owner.ticket).terminal, Some(normal(1, "recovered native text")));
    assert!(!store.generation_status(&owner.ticket).unwrap().pending_commit);
    assert_eq!(store.generation_resolve_output(&owner, 1, &text_hash("recovered native text")).unwrap(), OutputResolution::Uncommitted);
    assert_eq!(execution(&store, &owner.ticket).output_receipt, None);
    assert_eq!(effects_conversation(&store)["message"].as_array().unwrap().len(), rows);
    assert_eq!(store.generation_recover(&owner.ticket).unwrap().phase, Phase::Running);
    let previous = store.generation_record(&owner.ticket).unwrap();
    let mut restarted = previous.clone();
    restarted.session = Some(uuid::Uuid::new_v4().to_string());
    store.generation_replace_record(&owner.ticket, &previous, &restarted).unwrap();
    assert_eq!(store.generation_recover(&owner.ticket).unwrap().phase, Phase::Interrupted);
}

#[test]
fn owned_cancel_and_output_linearize_at_segment_admission() {
    // An output segment admitted first commits, and the cancelled settlement retains it.
    let (_directory, mut store, intents) = owned_fixture(1);
    let (owner, _) = issued(&mut store, &intents[0]);
    let mut writer = store.open_native_job_store().unwrap();
    let mut cancellation = store.open_native_job_store().unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer_owner = owner.clone();
    let writing = thread::spawn(move || {
        lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            entered_tx.send(()).unwrap(); release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        })));
        writer.generation_checkpoint(&writer_owner, &checkpoint(1, "admitted before cancel")).unwrap()
    });
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let (waiting_tx, waiting_rx) = mpsc::channel();
    let ticket = owner.ticket.clone();
    let cancelling = thread::spawn(move || {
        FINISH_WAITING_FOR_SEGMENT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || waiting_tx.send(()).unwrap())));
        cancellation.generation_cancel(&ticket).unwrap()
    });
    waiting_rx.recv_timeout(Duration::from_secs(10)).expect("cancellation did not wait for the admitted segment");
    release_tx.send(()).unwrap();
    let committed = writing.join().unwrap();
    let cancelled = cancelling.join().unwrap();
    assert_eq!((committed.checkpoint.0, cancelled.phase.clone(), cancelled.committed_revision), (1, Phase::Running, committed.committed_revision));
    assert_eq!(execution(&store, &owner.ticket).cancel, Some(CancelReason::User));
    assert!(matches!(store.generation_resolve_output(&owner, 1, &text_hash("admitted before cancel")).unwrap(), OutputResolution::Committed(_)));
    let settled = store.generation_settle_owned(&owner, normal(1, "admitted before cancel")).unwrap();
    assert_eq!((settled.phase, settled.committed_revision), (Phase::Cancelled, committed.committed_revision));
    assert_eq!(execution(&store, &owner.ticket).terminal, Some(TerminalDecision::Cancelled { reason: CancelReason::User }));
    assert_eq!(effects_conversation(&store)["message"].as_array().unwrap().last().unwrap()["data"], "admitted before cancel");

    // A decision admitted first closes the later output segment before any write.
    let (_directory, mut store, intents) = owned_fixture(1);
    let (owner, _) = issued(&mut store, &intents[0]);
    store.generation_cancel(&owner.ticket).unwrap();
    let (before, revision, saved) = (journal_body(&store), store.revision().unwrap(), effects_conversation(&store));
    assert_error(store.generation_checkpoint(&owner, &checkpoint(1, "after cancel")), "execution-closed");
    assert_eq!((journal_body(&store), store.revision().unwrap()), (before, revision));
    assert_eq!(store.generation_resolve_output(&owner, 1, &text_hash("after cancel")).unwrap(), OutputResolution::Uncommitted);
    assert_eq!(store.generation_settle_owned(&owner, normal(1, "after cancel")).unwrap().phase, Phase::Cancelled);
    assert_eq!(effects_conversation(&store), saved);
}

#[test]
fn owned_completion_requires_the_exact_retained_terminal_output_receipt() {
    // Equal text keeps the original receipt; a stripped receipt is unavailable evidence, never completion.
    let (_directory, mut store, intents) = owned_fixture(1);
    let (owner, _) = issued(&mut store, &intents[0]);
    let first = store.generation_checkpoint(&owner, &checkpoint(1, "one")).unwrap();
    let receipt = execution(&store, &owner.ticket).output_receipt.unwrap();
    assert_eq!((receipt.source_sequence, Some(receipt.receipt_revision), receipt.output_hash.clone()), (1, first.committed_revision, text_hash("one")));
    let second = store.generation_checkpoint(&owner, &checkpoint(2, "one")).unwrap();
    assert_eq!((second.checkpoint.0, second.committed_revision), (2, first.committed_revision));
    assert_eq!(execution(&store, &owner.ticket).output_receipt, Some(receipt.clone()));
    assert_eq!(store.generation_resolve_output(&owner, 2, &text_hash("one")).unwrap(), OutputResolution::Committed(receipt));
    assert_error(store.generation_checkpoint(&owner, &checkpoint(2, "two")), "checkpoint-integrity");
    let previous = store.generation_record(&owner.ticket).unwrap();
    let mut stripped = previous.clone();
    stripped.execution.as_mut().unwrap().output_receipt = None;
    store.generation_replace_record(&owner.ticket, &previous, &stripped).unwrap();
    assert_error(store.generation_resolve_output(&owner, 2, &text_hash("one")), "output-evidence-unavailable");
    let body = journal_body(&store);
    assert_error(store.generation_settle_owned(&owner, normal(2, "one")), "output-evidence-unavailable");
    assert_eq!(journal_body(&store), body);
    assert_eq!(execution(&store, &owner.ticket).terminal, None);
    assert_eq!(store.generation_recover(&owner.ticket).unwrap().phase, Phase::Running);
    // The refused decision leaves failure settlement available, and it retains the committed output.
    let failed = store.generation_settle_owned(&owner, failure()).unwrap();
    assert_eq!((failed.phase, failed.checkpoint.0, failed.committed_revision), (Phase::Failed, 2, first.committed_revision));
    assert_eq!(execution(&store, &owner.ticket).terminal, Some(failure()));
    assert_eq!(effects_conversation(&store)["message"].as_array().unwrap().last().unwrap()["data"], "one");

    // A normal transport end without an accepted output cannot complete.
    let (_directory, mut store, intents) = owned_fixture(1);
    let (owner, _) = issued(&mut store, &intents[0]);
    let body = journal_body(&store);
    assert_error(store.generation_settle_owned(&owner, normal(1, "")), "output-evidence-unavailable");
    assert_eq!(journal_body(&store), body);
    assert_eq!(execution(&store, &owner.ticket).terminal, None);
    // The ticket's only commit is the interaction effect; these history rows already carry IDs.
    let effect = store.generation_status(&owner.ticket).unwrap().committed_revision;
    assert_eq!(effect, Some(store.revision().unwrap()));
    let failed = store.generation_settle_owned(&owner, failure()).unwrap();
    assert_eq!((failed.phase, failed.checkpoint.0, failed.committed_revision), (Phase::Failed, 0, effect));

    // An empty terminal text is a real output, and a later cancel does not rewrite completion.
    let (_directory, mut store, intents) = owned_fixture(1);
    let (owner, _) = issued(&mut store, &intents[0]);
    store.generation_checkpoint(&owner, &checkpoint(1, "")).unwrap();
    let completed = store.generation_settle_owned(&owner, normal(1, "")).unwrap();
    assert_eq!(completed.phase, Phase::Completed);
    assert_eq!(effects_conversation(&store)["message"].as_array().unwrap().last().unwrap()["data"], "");
    let body = journal_body(&store);
    assert_eq!(store.generation_cancel(&owner.ticket).unwrap(), completed);
    assert_eq!(store.generation_settle_owned(&owner, TerminalDecision::Cancelled { reason: CancelReason::User }).unwrap(), completed);
    assert_eq!(journal_body(&store), body);
}

#[test]
fn protected_target_validates_execution_members_within_existing_bounds() {
    let (_directory, mut store, intents) = owned_fixture(1);
    let query = NativeGenerationTarget::Character { character_id: "character" };
    let (owner, lifecycle) = ready_owned(&mut store, &intents[0]);
    store.generation_claim_dispatch(&owner, &lifecycle).unwrap();
    store.generation_checkpoint(&owner, &checkpoint(1, "protected output")).unwrap();
    store.generation_cancel(&owner.ticket).unwrap();
    assert!(protected(&store, query).unwrap());
    let original: Value = serde_json::from_str(&journal_body(&store).unwrap()).unwrap();
    let key = owner.ticket.sequence.0.to_string();
    for corruption in ["lifecycle", "dispatch", "undispatched", "request", "digest", "revision", "sequence", "ahead", "hash", "decision", "failure", "unprepared", "reserved"] {
        let mut value = original.clone();
        let record = &mut value["journal"]["records"][&key];
        let expected_revision = record["target"]["expectedRevision"].clone();
        let execution = &mut record["execution"];
        match corruption {
            "lifecycle" => execution["lifecycle_id"] = json!("invalid"),
            "dispatch" => execution["dispatch_id"] = json!("invalid"),
            "undispatched" => execution["dispatch_id"] = Value::Null,
            "request" => execution["output_receipt"]["request_id"] = json!("invalid"),
            "digest" => execution["output_receipt"]["intent_digest"] = json!("invalid"),
            "revision" => execution["output_receipt"]["receipt_revision"] = expected_revision,
            "sequence" => execution["output_receipt"]["source_sequence"] = json!(0),
            "ahead" => execution["output_receipt"]["source_sequence"] = json!(2),
            "hash" => execution["output_receipt"]["output_hash"] = json!("0".repeat(64)),
            "decision" => execution["terminal"] = json!({"Normal": {"sequence": 0, "output_hash": "0".repeat(64)}}),
            "failure" => execution["terminal"] = json!({"Failed": {"failure": {"category": "x".repeat(MAX_DESCRIPTOR_BYTES + 1),
                "detail": null, "stage": null, "http_status": null}}}),
            "unprepared" => record["preparation"]["ready"] = json!(false),
            _ => record["phase"] = json!("reserved"),
        }
        let body = serde_json::to_string(&value).unwrap();
        write_body(&store, &body);
        assert_error(protected(&store, NativeGenerationTarget::Character { character_id: "unrelated" }), "protection-execution");
        assert_eq!(journal_body(&store).as_deref(), Some(body.as_str()), "{corruption}");
    }
    write_body(&store, &serde_json::to_string(&original).unwrap());
    assert_eq!(store.generation_settle_owned(&owner, TerminalDecision::Cancelled { reason: CancelReason::User }).unwrap().phase, Phase::Cancelled);
    assert!(!protected(&store, query).unwrap());
    let mut record = store.generation_record(&owner.ticket).unwrap();
    record.phase = Phase::Running;
    record.execution.as_mut().unwrap().terminal = Some(TerminalDecision::Failed { failure: FailureDescriptor {
        category: "c".repeat(MAX_DESCRIPTOR_BYTES), detail: Some("d".repeat(MAX_DESCRIPTOR_BYTES)),
        stage: Some("s".repeat(MAX_DESCRIPTOR_BYTES)), http_status: Some(599) } });
    let mut journal = Journal::new();
    for _ in 0..MAX_ACTIVE {
        journal.high_water += 1;
        journal.records.insert(journal.high_water, record.clone());
    }
    write_journal(store.device_store().unwrap().connection(), &journal).unwrap();
    assert!(protected(&store, query).unwrap());
    journal.high_water += 1;
    journal.records.insert(journal.high_water, record);
    write_journal(store.device_store().unwrap().connection(), &journal).unwrap();
    assert_error(protected(&store, query), "protection-journal-limit");
}

#[test]
fn send_effect_unit_keys_are_the_exact_root_character_and_conversation_units() {
    let (_directory, _store, intent) = effects_fixture(json!({}));
    let keys = send_effect_unit_keys(&intent.target).unwrap();
    let keys: Vec<_> = [SendEffect::Interaction, SendEffect::Variables, SendEffect::Memory].into_iter()
        .flat_map(|effect| keys.for_effect(effect).into_iter().map(|key| key.as_str().to_owned())).collect();
    assert_eq!(keys, [r#"["root","statics"]"#, r#"["character","character","lastInteraction"]"#,
        r#"["conversation","character","conversation","scriptstate"]"#, r#"["conversation","character","conversation","note"]"#,
        r#"["conversation","character","conversation","lastMemory"]"#]);
}

fn statics(store: &PersistentStore) -> Value { store.read_root(None).unwrap().value["statics"].clone() }

fn last_interaction(store: &PersistentStore) -> Option<u64> {
    store.read_character("character", None).unwrap().unwrap().value["lastInteraction"].as_u64()
}

#[test]
fn interaction_effect_commits_once_before_ids_and_keeps_the_fence_and_unknown_fields() {
    let (_directory, mut store, intent) = effects_fixture(json!({"root":{"statics":{"messages":41,"imports":2,"futureStat":{"keep":true}}}}));
    let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
    let reserved = store.generation_record(&ticket).unwrap();
    let since = now_millis().unwrap();
    let PreparedGeneration::Ready { status, .. } = service::prepare_reserved_ollama(&mut store, &ticket).unwrap() else { panic!("not ready") };
    assert_eq!(statics(&store), json!({"messages":42,"imports":2,"futureStat":{"keep":true}}));
    let interaction = last_interaction(&store).unwrap();
    assert!(interaction >= since && interaction <= now_millis().unwrap());
    let record = store.generation_record(&ticket).unwrap();
    let effects = &record.preparation.as_ref().unwrap().effects;
    let EffectState::Settled { receipt: Some((request_id, digest, revision)) } = &effects.interaction else { panic!("no interaction receipt") };
    let receipt: (String, i64) = store.connection.query_row(
        "SELECT digest,revision FROM lww_requests WHERE request_id=?1", [request_id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(receipt, (digest.clone(), intent.target.expected_revision + 1));
    assert_eq!(*revision, receipt.1);
    assert_eq!(effects.memory, EffectState::Pending);
    // The ID commit follows the effect, which left the target's existence fences alone.
    assert_eq!(status.committed_revision, Some(receipt.1 + 1));
    assert_eq!((&record.fence.character, &record.fence.conversation), (&reserved.fence.character, &reserved.fence.conversation));
    assert!(matches!(service::prepare_reserved_ollama(&mut store, &ticket).unwrap(), PreparedGeneration::Existing(value) if value == status));
    assert_eq!((statics(&store)["messages"].clone(), last_interaction(&store)), (json!(42), Some(interaction)));
}

#[test]
fn interaction_conflict_retries_with_a_new_request_within_a_bound_and_counts_once() {
    for conflicts in [1, MAX_REVISION_RETRIES + 1] {
        let (_directory, mut store, intent) = effects_fixture(json!({}));
        let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
        conflict_held_commits(store.open_native_job_store().unwrap(), conflicts);
        let result = service::prepare_reserved_ollama(&mut store, &ticket);
        lww::BEFORE_HELD_COMMIT.with(|hook| hook.borrow_mut().take());
        if conflicts > MAX_REVISION_RETRIES {
            assert!(matches!(result, Err(StoreError::RevisionConflict { .. })));
            assert!(!store.generation_status(&ticket).unwrap().pending_commit);
            assert!(statics(&store).is_null());
            assert!(matches!(service::prepare_reserved_ollama(&mut store, &ticket).unwrap(), PreparedGeneration::Ready { .. }));
        } else {
            assert!(matches!(result.unwrap(), PreparedGeneration::Ready { .. }));
        }
        assert_eq!(statics(&store), json!({"messages":1,"imports":0}));
        // Each conflicting transaction took one revision; the effect and the ID commit took one each.
        assert_eq!(store.revision().unwrap(), intent.target.expected_revision + conflicts as i64 + 2);
    }
}

#[test]
fn rejected_effect_admission_leaves_no_unknown_attempt_and_retries_once() {
    let (directory, mut store, intent) = effects_fixture(json!({}));
    let owner = effects_owner(&mut store, &intent);
    let locker = PersistentStore::open(directory.path()).unwrap();
    let held = locker.acquire_intent_admission().unwrap();
    assert!(matches!(store.generation_ensure_history_ids(&owner), Err(GenerationError::Store(StoreError::CommitBusy))));
    assert!(!store.generation_status(&owner.ticket).unwrap().pending_commit);
    assert!(statics(&store).is_null());
    drop(held);
    store.generation_ensure_history_ids(&owner).unwrap();
    assert_eq!(statics(&store), json!({"messages":1,"imports":0}));
    assert_eq!(store.revision().unwrap(), intent.target.expected_revision + 2);
}

#[test]
fn preparation_commits_wait_within_their_bound_for_device_admission_another_connection_holds() {
    use std::time::{Duration, Instant};
    fn hold(directory: &Path) -> (mpsc::Sender<()>, thread::JoinHandle<()>) {
        let locker = PersistentStore::open(directory).unwrap();
        let (held, holding) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let holder = thread::spawn(move || {
            let _admission = locker.acquire_intent_admission().unwrap();
            held.send(()).unwrap();
            let _ = released.recv();
        });
        holding.recv().unwrap();
        (release, holder)
    }
    let wait = |deadline| lww::AdmissionWait { deadline, first_pause: Duration::from_millis(5), max_pause: Duration::from_millis(20) };
    let (directory, mut store, intent) = effects_fixture(json!({}));
    let ticket = service::reserve_ollama(&mut store, &intent).unwrap();

    store.set_admission_wait(wait(Duration::from_millis(150)));
    let (release, holder) = hold(directory.path());
    let started = Instant::now();
    assert!(matches!(service::prepare_reserved_ollama(&mut store, &ticket), Err(StoreError::CommitBusy)));
    assert!(started.elapsed() >= Duration::from_millis(150));
    release.send(()).unwrap();
    holder.join().unwrap();
    assert!(!store.generation_status(&ticket).unwrap().pending_commit);
    assert!(statics(&store).is_null());

    store.set_admission_wait(wait(Duration::from_secs(10)));
    let (release, holder) = hold(directory.path());
    let releasing = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        release.send(()).unwrap();
    });
    assert!(matches!(service::prepare_reserved_ollama(&mut store, &ticket).unwrap(), PreparedGeneration::Ready { .. }));
    releasing.join().unwrap();
    holder.join().unwrap();
    assert_eq!(statics(&store), json!({"messages":1,"imports":0}));
    assert_eq!(store.revision().unwrap(), intent.target.expected_revision + 2);
}

#[test]
fn lost_interaction_acknowledgement_resolves_its_receipt_once() {
    const TEST: &str = "lost_interaction_acknowledgement_resolves_its_receipt_once";
    if let Some(root) = restart_root(TEST) {
        let input: Value = serde_json::from_slice(&std::fs::read(root.join("effect-receipt-loss.json")).unwrap()).unwrap();
        let ticket: Ticket = serde_json::from_value(input["ticket"].clone()).unwrap();
        let mut store = PersistentStore::open(&root).unwrap();
        assert_error(store.generation_recover(&ticket), "receipt-missing");
        assert!(store.generation_status(&ticket).unwrap().pending_commit);
        assert_eq!(store.generation_effects_evidence(&ticket).unwrap(), None);
        assert_eq!(serde_json::to_value(store.generation_record(&ticket).unwrap().attempt).unwrap(), input["attempt"]);
        assert_eq!(store.revision().unwrap(), input["revision"].as_i64().unwrap());
        return;
    }
    for loss in ["none", "receipt", "overwritten"] {
        let (directory, mut store, intent) = effects_fixture(json!({}));
        let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
        let path = format!("$.journal.records.\"{}\".attempt", ticket.sequence.0);
        // Fail the journal write that clears the effect attempt after its library receipt exists.
        store.device_store().unwrap().connection().execute_batch(&format!(
            "CREATE TEMP TRIGGER synthetic_effect_ack BEFORE UPDATE ON device_settings
             WHEN OLD.key='native-generation-journal-v1'
              AND json_extract(OLD.value,'{path}.kind.Effect.effect')='Interaction'
              AND json_type(NEW.value,'{path}')='null'
             BEGIN SELECT RAISE(ABORT,'synthetic lost effect acknowledgement'); END;"
        )).unwrap();
        assert!(service::prepare_reserved_ollama(&mut store, &ticket).is_err());
        let attempt = store.generation_record(&ticket).unwrap().attempt.unwrap();
        assert!(matches!(attempt.kind, AttemptKind::Effect { effect: SendEffect::Interaction }));
        assert!(store.generation_status(&ticket).unwrap().pending_commit);
        assert_eq!(statics(&store), json!({"messages":1,"imports":0}));
        store.device_store().unwrap().connection().execute_batch("DROP TRIGGER synthetic_effect_ack;").unwrap();
        if loss == "overwritten" {
            let keys = send_effect_unit_keys(&intent.target).unwrap();
            store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), unit_mutations: Some(vec![
                UnitMutation::Set { key: keys.statics, value: json!({"messages":9,"imports":0}) },
                UnitMutation::Set { key: keys.interaction, value: json!(123) },
            ]), ..Default::default() }).unwrap();
        }
        let saved_statics = statics(&store);
        let revision = store.revision().unwrap();
        if loss != "none" {
            let receipt: (String, i64) = store.connection.query_row("SELECT digest,revision FROM lww_requests WHERE request_id=?1",
                [&attempt.request_id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
            store.connection.execute("DELETE FROM lww_requests WHERE request_id=?1", [&attempt.request_id]).unwrap();
            assert_error(store.generation_recover(&ticket), "receipt-missing");
            assert_eq!(store.generation_effects_evidence(&ticket).unwrap(), None);
            std::fs::write(directory.path().join("effect-receipt-loss.json"), serde_json::to_vec(&json!({
                "ticket":ticket, "attempt":attempt, "revision":revision })).unwrap()).unwrap();
            run_in_restarted_process(TEST, directory.path());
            // Restore only the actual receipt captured before the synthetic corruption.
            store.connection.execute("INSERT INTO lww_requests(request_id,digest,revision) VALUES(?1,?2,?3)",
                params![attempt.request_id, receipt.0, receipt.1]).unwrap();
        }
        let mut retry = store.open_native_job_store().unwrap();
        let PreparedGeneration::Ready { status, .. } = service::prepare_reserved_ollama(&mut retry, &ticket).unwrap() else { panic!("lost acknowledgement was not recovered") };
        assert_eq!(statics(&retry), saved_statics);
        assert_eq!((retry.revision().unwrap(), status.committed_revision), (revision + 1, Some(revision + 1)));
        let EffectState::Settled { receipt: Some((id, _, revision)) } = retry.generation_record(&ticket).unwrap().preparation.unwrap().effects.interaction else { panic!("not settled") };
        assert_eq!((id, revision), (attempt.request_id, attempt.expected_revision + 1));
    }
}

#[test]
fn protected_target_rejects_an_effect_attempt_outside_its_phase() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let owner = effects_owner(&mut store, &intent);
    lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("synthetic effect interruption"))));
    assert!(catch_unwind(AssertUnwindSafe(|| store.generation_ensure_history_ids(&owner))).is_err());
    assert!(matches!(store.generation_record(&owner.ticket).unwrap().attempt.unwrap().kind, AttemptKind::Effect { effect: SendEffect::Interaction }));
    let query = NativeGenerationTarget::Character { character_id: "character" };
    assert!(protected(&store, query).unwrap());
    let original: Value = serde_json::from_str(&journal_body(&store).unwrap()).unwrap();
    let key = owner.ticket.sequence.0.to_string();
    for corruption in ["ready", "settled", "normalized", "memory"] {
        let mut value = original.clone();
        let record = &mut value["journal"]["records"][&key];
        match corruption {
            "ready" => record["preparation"]["ready"] = json!(true),
            "settled" => record["preparation"]["effects"]["interaction"] = json!({"Settled":{"receipt":null}}),
            "normalized" => record["preparation"]["normalization"] = json!({"Settled":{"receipt":null}}),
            _ => record["attempt"]["kind"] = json!({"Effect":{"effect":"Memory"}}),
        }
        store.device_store().unwrap().connection().execute("UPDATE device_settings SET value=?2 WHERE key=?1", params![JOURNAL_KEY, serde_json::to_string(&value).unwrap()]).unwrap();
        assert_error(protected(&store, query), "protection-attempt");
    }
}

// Ready with a lifecycle, before the memory effect.
fn marked_ready(store: &mut PersistentStore, intent: &OllamaIntent) -> (Owner, String, Option<String>) {
    let owner = effects_owner(store, intent);
    store.generation_ensure_history_ids(&owner).unwrap();
    let snapshot = store.generation_owned_preparation_snapshot(&owner).unwrap().unwrap();
    let prepared = crate::native_generation::prepare::prepare_ollama(&snapshot).unwrap();
    let lifecycle = uuid::Uuid::new_v4().to_string();
    store.generation_mark_ready_owned(&owner, &lifecycle).unwrap();
    store.generation_send_variables(&owner, &lifecycle, &PreparedVariablePlan::default()).unwrap();
    (owner, lifecycle, prepared.last_memory)
}

#[test]
fn memory_effect_commits_after_ready_before_the_claim_and_only_once() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let (owner, lifecycle, memory) = marked_ready(&mut store, &intent);
    let memory = memory.expect("prepared memory marker");
    assert_error(store.generation_claim_dispatch(&owner, &lifecycle), "effects-pending");
    let (revision, fence) = (store.revision().unwrap(), store.generation_record(&owner.ticket).unwrap().fence);
    assert_error(store.generation_send_memory(&owner, &uuid::Uuid::new_v4().to_string(), Some(&memory)), "stale-lifecycle");
    let status = store.generation_send_memory(&owner, &lifecycle, Some(&memory)).unwrap();
    assert_eq!((store.revision().unwrap(), status.committed_revision), (revision + 1, Some(revision + 1)));
    assert_eq!(effects_conversation(&store)["lastMemory"], json!(memory));
    assert_eq!(store.generation_record(&owner.ticket).unwrap().fence, fence);
    // Settled, it never writes again.
    assert_eq!(store.generation_send_memory(&owner, &lifecycle, Some("Another marker.")).unwrap(), status);
    assert_eq!(store.revision().unwrap(), revision + 1);
    assert!(matches!(store.generation_claim_dispatch(&owner, &lifecycle).unwrap(), DispatchClaim::Issued { .. }));
    assert_error(store.generation_send_memory(&owner, &lifecycle, Some(&memory)), "dispatch-claimed");
    store.generation_settle_lost(&owner).unwrap();
    // A stored marker that already matches settles without a write; an absent marker removes the stored one.
    for next in [Some(memory.as_str()), None] {
        let mut intent = intent.clone();
        intent.target.expected_revision = store.revision().unwrap();
        let (owner, lifecycle, _) = marked_ready(&mut store, &intent);
        let revision = store.revision().unwrap();
        store.generation_send_memory(&owner, &lifecycle, next).unwrap();
        assert_eq!(store.revision().unwrap(), revision + i64::from(next.is_none()));
        assert_eq!(effects_conversation(&store).get("lastMemory").and_then(Value::as_str), next);
        assert!(matches!(store.generation_claim_dispatch(&owner, &lifecycle).unwrap(), DispatchClaim::Issued { .. }));
        store.generation_settle_lost(&owner).unwrap();
    }
}

#[test]
fn normalization_projection_maps_after_the_memory_effect_at_checkpoint_zero() {
    let (_directory, mut store, ticket, original, witness) = projection_reserved(json!({}));
    let Start::Started { owner, .. } = store.generation_start_preparation(&ticket).unwrap() else { panic!("not started") };
    store.generation_ensure_history_ids(&owner).unwrap();
    let snapshot = store.generation_owned_preparation_snapshot(&owner).unwrap().unwrap();
    crate::native_generation::prepare::prepare_ollama(&snapshot).unwrap();
    let lifecycle = uuid::Uuid::new_v4().to_string();
    store.generation_mark_ready_owned(&owner, &lifecycle).unwrap();
    store.generation_send_variables(&owner, &lifecycle, &PreparedVariablePlan::default()).unwrap();
    let normalized = projection_lease(&mut store);
    let before = mapped(projected(&store, &ticket, &witness, &original, &normalized));
    let status = store.generation_send_memory(&owner, &lifecycle, Some("Synthetic memory marker.")).unwrap();
    let current = projection_lease(&mut store);
    let after = mapped(projected(&store, &ticket, &witness, &original, &current));
    assert_eq!(status.committed_revision, Some(current.revision));
    assert_eq!((after.receipt_revision, after.projected_revision), (before.receipt_revision, current.revision));
    assert_eq!(after.rows, before.rows);
}

fn switch_binding(store: &mut PersistentStore) {
    let selection = super::super::sync_selection::read(&store.connection).unwrap();
    store.lww_switch_target(&lww::Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: uuid::Uuid::new_v4().to_string() },
        &super::super::sync_selection::BindingSelectionChange { expected_epoch: selection.epoch, new_epoch: uuid::Uuid::new_v4().to_string(),
            target: super::super::sync_selection::SyncTarget::None, library_id: None, target_id: None, inspection_id: None, initial_publication: false }).unwrap();
}

#[test]
fn retargeted_attempt_without_receipt_or_intent_ends_stale_without_writing() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let owner = effects_owner(&mut store, &intent);
    let (attempt, _) = interrupt_ids_after_intent(&mut store, &owner);
    // The durable cut after the journal freeze and before the intent: nothing can still apply.
    store.device_store().unwrap().connection().execute("DELETE FROM lww_intents WHERE request_id=?1", [&attempt.request_id]).unwrap();
    switch_binding(&mut store);
    let (revision, saved) = (store.revision().unwrap(), effects_conversation(&store));
    let recovered = store.generation_recover(&owner.ticket).unwrap();
    assert_eq!((recovered.phase, recovered.pending_commit), (Phase::Stale, false));
    let record = store.generation_record(&owner.ticket).unwrap();
    assert!(record.attempt.is_none());
    // Settlement discards the frozen rows, and no ID reached the library.
    assert_eq!(record.preparation.unwrap().normalization, Normalization::Pending { assignments: None });
    assert_eq!((store.revision().unwrap(), effects_conversation(&store)), (revision, saved.clone()));
    assert_eq!(saved["message"].as_array().unwrap().iter().filter(|row| row["chatId"].is_null()).count(), 4);
}

#[test]
fn retargeted_quarantined_attempt_resolves_only_proven_discardable_intents() {
    for uncertain in [false, true] {
        let (directory, mut store, intent) = effects_fixture(json!({}));
        let owner = effects_owner(&mut store, &intent);
        let (attempt, _) = interrupt_ids_after_intent(&mut store, &owner);
        store.device_store().unwrap().connection().execute(
            "INSERT INTO lww_intent_failures(request_id,failures,error,quarantined) VALUES(?1,3,'synthetic',1)", [&attempt.request_id]).unwrap();
        if uncertain {
            assert_eq!(store.connection.execute("UPDATE lww_units SET version=?1 WHERE key=?2",
                params![attempt.request_id, r#"["messages","character","conversation"]"#]).unwrap(), 1);
        }
        switch_binding(&mut store);
        let (revision, saved) = (store.revision().unwrap(), effects_conversation(&store));
        let recovered = store.generation_recover(&owner.ticket).unwrap();
        assert_eq!((recovered.phase.clone(), recovered.pending_commit), (Phase::Stale, uncertain));
        let retained = store.generation_record(&owner.ticket).unwrap();
        assert_eq!(retained.attempt.as_ref(), uncertain.then_some(&attempt));
        assert_eq!(store.generation_active().unwrap().iter().any(|active| active.ticket == owner.ticket), uncertain);
        // The retained attempt belongs to the old authority and never protects the retargeted one.
        assert!(!protected(&store, NativeGenerationTarget::Character { character_id: "character" }).unwrap());
        assert_eq!(store.generation_cancel(&owner.ticket).unwrap(), recovered);
        drop(store);
        let mut store = PersistentStore::open(directory.path()).unwrap();
        assert_eq!(store.generation_recover(&owner.ticket).unwrap(), recovered);
        assert_eq!(store.generation_record(&owner.ticket).unwrap().attempt, retained.attempt);
        assert_eq!((store.revision().unwrap(), effects_conversation(&store)), (revision, saved));
    }
}


#[test]
fn send_effect_evidence_requires_complete_original_receipts_and_preserves_target() {
    for memory in [None, Some("Synthetic marker.")] {
        let (_directory, mut store, intent) = effects_fixture(json!({}));
        let (owner, lifecycle, _) = marked_ready(&mut store, &intent);
        assert_eq!(store.generation_effects_evidence(&owner.ticket).unwrap(), None);
        store.generation_send_memory(&owner, &lifecycle, memory).unwrap();
        let evidence = store.generation_effects_evidence(&owner.ticket).unwrap().unwrap();
        let keys = send_effect_unit_keys(&intent.target).unwrap();
        let mut expected = keys.for_effect(SendEffect::Interaction);
        if memory.is_some() { expected.extend(keys.for_effect(SendEffect::Memory)); }
        assert_eq!(evidence.affected_unit_keys, expected.into_iter().map(|key| key.as_str().to_owned()).collect::<Vec<_>>());
        assert_eq!(evidence.receipts.len(), if memory.is_some() { 2 } else { 1 });
        let revision = store.revision().unwrap();
        store.generation_send_memory(&owner, &lifecycle, Some("Must never overwrite the settled marker.")).unwrap();
        assert_eq!(store.generation_effects_evidence(&owner.ticket).unwrap(), Some(evidence.clone()));
        assert_eq!(store.revision().unwrap(), revision);
        let body = journal_body(&store).unwrap();
        let mut corrupt: Value = serde_json::from_str(&body).unwrap();
        corrupt["journal"]["records"][owner.ticket.sequence.0.to_string()]["preparation"]["effects"]["interaction"] = json!("Pending");
        write_body(&store, &serde_json::to_string(&corrupt).unwrap());
        assert_error(store.generation_effects_evidence(&owner.ticket), "protection-effects");
        write_body(&store, &body);
        let receipt = &evidence.receipts[0];
        for corruption in ["digest", "revision", "missing"] {
            store.connection.execute_batch("CREATE TEMP TABLE saved_effect_receipts AS SELECT * FROM lww_requests").unwrap();
            match corruption {
                "digest" => { store.connection.execute("UPDATE lww_requests SET digest=?2 WHERE request_id=?1", params![receipt.request_id, "0".repeat(64)]).unwrap(); }
                "revision" => { store.connection.execute("UPDATE lww_requests SET revision=revision+1 WHERE request_id=?1", [&receipt.request_id]).unwrap(); }
                _ => { store.connection.execute("DELETE FROM lww_requests WHERE request_id=?1", [&receipt.request_id]).unwrap(); }
            }
            assert_eq!(store.generation_effects_evidence(&owner.ticket).unwrap(), None, "{corruption}");
            store.connection.execute_batch("DELETE FROM lww_requests; INSERT INTO lww_requests SELECT * FROM saved_effect_receipts; DROP TABLE saved_effect_receipts").unwrap();
        }
        store.generation_settle_owned(&owner, TerminalDecision::Cancelled { reason: CancelReason::User }).unwrap();
        assert_eq!(store.generation_target(&owner.ticket).unwrap(), intent.target);
        let mut unknown = owner.ticket.clone(); unknown.sequence = 0.into();
        assert_error(store.generation_target(&unknown), "unknown-ticket");
        let before = journal_body(&store);
        let stage = store.replace_begin().unwrap();
        store.replace_put_root(&stage.staging_id, &json!({"syntheticReplacement":true})).unwrap();
        store.replace_put_presets(&stage.staging_id, &[]).unwrap();
        store.replace_commit(&stage.staging_id, Some(store.revision().unwrap())).unwrap();
        assert_eq!(store.generation_target(&owner.ticket).unwrap(), intent.target);
        assert_eq!(store.generation_effects_evidence(&owner.ticket).unwrap(), None);
        assert_eq!(journal_body(&store), before);
    }
}

#[test]
fn output_metadata_measures_completion_until_the_store_ownership_check() {
    let (_directory, mut store, intents) = owned_fixture(1);
    let (owner, _) = issued(&mut store, &intents[0]);
    let start = std::time::Instant::now() - Duration::from_millis(1234);
    let info = OutputInfo { model: "Synthetic timing model".into(), input_tokens: 3, output_tokens: 7, max_context: 100,
        stage3: 17, stage4_started: Some(start) };
    store.generation_checkpoint_with(&owner, &checkpoint(1, "Synthetic timing output."), Some(&info)).unwrap();
    let end = u64::try_from(start.elapsed().as_millis()).unwrap();
    let row = effects_conversation(&store)["message"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(row["generationInfo"]["stageTiming"]["stage3"], 17);
    let elapsed = row["generationInfo"]["stageTiming"]["stage4"].as_u64().unwrap();
    assert!((1234..=end).contains(&elapsed));
}

#[test]
fn retargeted_quarantined_output_preserves_unresolved_evidence_across_library_replacement() {
    let (directory, mut store, intents) = owned_fixture(1);
    let (owner, _) = issued(&mut store, &intents[0]);
    let original = store.generation_target(&owner.ticket).unwrap();
    let attempt = interrupt_after_reservation(&mut store, &owner);
    store.device_store().unwrap().connection().execute(
        "INSERT INTO lww_intent_failures(request_id,failures,error,quarantined) VALUES(?1,3,'synthetic',1)", [&attempt.request_id]).unwrap();
    let stage = store.replace_begin().unwrap();
    store.replace_put_root(&stage.staging_id, &json!({"syntheticReplacement":true})).unwrap();
    store.replace_put_presets(&stage.staging_id, &[]).unwrap();
    store.replace_commit(&stage.staging_id, Some(store.revision().unwrap())).unwrap();
    let revision = store.revision().unwrap();
    let status = store.generation_recover(&owner.ticket).unwrap();
    assert_eq!((status.phase.clone(), status.pending_commit), (Phase::Stale, true));
    assert_eq!(store.generation_resolve_output(&owner, 1, &text_hash("recovered native text")).unwrap(), OutputResolution::Unresolved);
    assert_eq!(store.generation_cancel(&owner.ticket).unwrap(), status);
    assert_eq!(store.generation_effects_evidence(&owner.ticket).unwrap(), None);
    assert_eq!(store.generation_active().unwrap()[0].library_identity, original.library_identity);
    drop(store);
    let mut store = PersistentStore::open(directory.path()).unwrap();
    assert_eq!(store.generation_recover(&owner.ticket).unwrap(), status);
    assert_eq!(store.generation_record(&owner.ticket).unwrap().attempt, Some(attempt));
    assert_eq!(store.generation_target(&owner.ticket).unwrap(), original);
    assert_eq!(store.revision().unwrap(), revision);
}

#[test]
fn retargeted_quarantined_committed_ids_keep_the_exact_receipt() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let owner = effects_owner(&mut store, &intent);
    let (attempt, assignments) = interrupt_ids_after_intent(&mut store, &owner);
    store.device_store().unwrap().connection().execute("CREATE TEMP TABLE saved_generation_intent AS SELECT * FROM lww_intents WHERE request_id=?1", [&attempt.request_id]).unwrap();
    store.lww_recover_intents().unwrap();
    assert_stored_assignments(&store, &assignments);
    // Recreate the real durable cut after the library commit but before device-side completion.
    store.device_store().unwrap().connection().execute_batch("INSERT INTO lww_intents SELECT * FROM saved_generation_intent").unwrap();
    store.device_store().unwrap().connection().execute(
        "INSERT INTO lww_intent_failures(request_id,failures,error,quarantined) VALUES(?1,3,'synthetic',1)", [&attempt.request_id]).unwrap();
    switch_binding(&mut store);
    let revision = store.revision().unwrap();
    let recovered = store.generation_recover(&owner.ticket).unwrap();
    assert_eq!((recovered.phase, recovered.pending_commit), (Phase::Stale, false));
    let record = store.generation_record(&owner.ticket).unwrap();
    assert_eq!(record.preparation.unwrap().normalization, Normalization::Settled {
        receipt: Some((attempt.request_id, attempt.intent_digest, attempt.expected_revision + 1)) });
    assert_stored_assignments(&store, &assignments);
    assert_eq!(store.revision().unwrap(), revision);
}

fn variable_plan(store: &mut PersistentStore, intent: &OllamaIntent, names: &[&str]) -> (Owner, PreparedVariablePlan) {
    let owner = effects_owner(store, intent);
    store.generation_ensure_history_ids(&owner).unwrap();
    let snapshot = store.generation_owned_preparation_snapshot(&owner).unwrap().unwrap();
    (owner, variables_from_snapshot(&snapshot, names))
}

fn variables_from_snapshot(snapshot: &PreparationSnapshot, names: &[&str]) -> PreparedVariablePlan {
    let budget = risunest_ir::Budget::new(risunest_ir::Limits { max_owned_bytes: 1 << 20, max_json_bytes: 1 << 20,
        max_text_units: 1 << 18, max_values: 1 << 16, max_depth: 128, max_work: 1 << 20 },
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))).unwrap();
    let effects: Vec<_> = names.iter().map(|name| risunest_ir::stage::Effect::ChatVariable {
        name: risunest_ir::Text::from_str(name, &budget).unwrap(),
        value: risunest_ir::Value::Text(risunest_ir::Text::from_str("true", &budget).unwrap()),
    }).collect();
    let (plan, result) = crate::native_generation::preparation_effects::preparation_variables(snapshot, &effects);
    result.unwrap();
    plan
}

fn variables_ready(store: &mut PersistentStore, intent: &OllamaIntent, names: &[&str]) -> (Owner, String, PreparedVariablePlan) {
    let (owner, plan) = variable_plan(store, intent, names);
    let snapshot = store.generation_owned_preparation_snapshot(&owner).unwrap().unwrap();
    crate::native_generation::prepare::prepare_ollama(&snapshot).unwrap();
    let lifecycle = uuid::Uuid::new_v4().to_string();
    store.generation_mark_ready_owned(&owner, &lifecycle).unwrap();
    (owner, lifecycle, plan)
}

fn variable_map(store: &mut PersistentStore, value: Value) {
    store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), unit_mutations: Some(vec![UnitMutation::Set {
        key: UnitKey::new(&["conversation", "character", "conversation", "scriptstate"]).unwrap(), value,
    }]), ..Default::default() }).unwrap();
}

#[test]
fn variables_merge_the_fresh_map_once_and_preserve_arbitrary_unwritten_keys() {
    let (_directory, mut store, mut intent) = effects_fixture(json!({}));
    variable_map(&mut store, json!({"$__internal_ka_first":null,"non-variable":{"old":1},"$other":"old"}));
    intent.target.expected_revision = store.revision().unwrap();
    let (owner, lifecycle, plan) = variables_ready(&mut store, &intent, &["__internal_ka_first", "__internal_da_second", "__internal_ka_first"]);
    assert_eq!(plan.assignments.len(), 3);
    assert_eq!(plan.original["$__internal_ka_first"], Some(Value::Null));
    assert_eq!(plan.original["$__internal_da_second"], None);
    assert_error(store.generation_send_memory(&owner, &lifecycle, Some("not yet")), "effects-pending");
    assert_error(store.generation_claim_dispatch(&owner, &lifecycle), "effects-pending");
    assert_error(store.generation_send_variables(&owner, &uuid::Uuid::new_v4().to_string(), &plan), "stale-lifecycle");
    variable_map(&mut store, json!({"$__internal_ka_first":null,"non-variable":{"fresh":2},"$other":"new","foreign":[1,2]}));
    let (revision, count, fence) = (store.revision().unwrap(), receipt_count(&store), store.generation_record(&owner.ticket).unwrap().fence);
    let settled = store.generation_send_variables(&owner, &lifecycle, &plan).unwrap();
    assert_eq!(effects_conversation(&store)["scriptstate"], json!({"$__internal_ka_first":"true","$__internal_da_second":"true",
        "non-variable":{"fresh":2},"$other":"new","foreign":[1,2]}));
    assert_eq!((store.revision().unwrap(), receipt_count(&store)), (revision + 1, count + 1));
    let record = store.generation_record(&owner.ticket).unwrap();
    assert_eq!(record.fence, fence);
    let EffectState::Settled { receipt: Some(receipt) } = record.preparation.unwrap().effects.variables else { panic!("missing variable receipt") };
    let actual: (String, String, i64) = store.connection.query_row("SELECT request_id,digest,revision FROM lww_requests WHERE revision=?1", [revision + 1],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
    assert_eq!(receipt, actual);
    assert_eq!(settled.committed_revision, Some(receipt.2));
    // Settled effects never overwrite a later edit, including the written key.
    variable_map(&mut store, json!({"$__internal_ka_first":"foreign","keep":false}));
    let revision = store.revision().unwrap();
    assert_eq!(store.generation_send_variables(&owner, &lifecycle, &plan).unwrap(), settled);
    assert_eq!((store.revision().unwrap(), effects_conversation(&store)["scriptstate"].clone()), (revision, json!({"$__internal_ka_first":"foreign","keep":false})));
    store.generation_send_memory(&owner, &lifecycle, None).unwrap();
    assert!(matches!(store.generation_claim_dispatch(&owner, &lifecycle).unwrap(), DispatchClaim::Issued { .. }));
}

#[test]
fn variables_distinguish_missing_null_noop_and_stale_written_keys() {
    for initial in [None, Some(Value::Null), Some(json!({"$__internal_ka_a":"true","keep":7}))] {
        let (_directory, mut store, mut intent) = effects_fixture(json!({}));
        match initial.clone() {
            Some(value) => variable_map(&mut store, value),
            None => { store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), unit_mutations: Some(vec![UnitMutation::Delete {
                key: UnitKey::new(&["conversation", "character", "conversation", "scriptstate"]).unwrap() }]), ..Default::default() }).unwrap(); }
        }
        intent.target.expected_revision = store.revision().unwrap();
        let (owner, lifecycle, plan) = variables_ready(&mut store, &intent, &["__internal_ka_a"]);
        let (revision, count) = (store.revision().unwrap(), receipt_count(&store));
        store.generation_send_variables(&owner, &lifecycle, &plan).unwrap();
        let changed = i64::from(initial.as_ref().is_none_or(|value| !value.is_object()));
        assert_eq!((store.revision().unwrap(), receipt_count(&store)), (revision + changed, count + changed));
        let effects = store.generation_record(&owner.ticket).unwrap().preparation.unwrap().effects;
        assert!(matches!(effects.variables, EffectState::Settled { receipt } if receipt.is_some() == (changed == 1)));
        assert_eq!(effects_conversation(&store)["scriptstate"]["$__internal_ka_a"], "true");
    }
    for edited in [json!({"$__internal_ka_a":"true"}), json!({"$__internal_ka_a":null}), json!([]), json!("scalar")] {
        let (_directory, mut store, intent) = effects_fixture(json!({}));
        let (owner, lifecycle, plan) = variables_ready(&mut store, &intent, &["__internal_ka_a"]);
        variable_map(&mut store, edited.clone());
        let revision = store.revision().unwrap();
        assert_eq!(store.generation_send_variables(&owner, &lifecycle, &plan).unwrap().phase, Phase::Stale);
        assert_eq!((store.revision().unwrap(), effects_conversation(&store)["scriptstate"].clone()), (revision, edited));
    }
}

#[test]
fn cancellation_between_variable_and_memory_segments_preserves_only_admitted_effects() {
    for after_variables in [false, true] {
        let (_directory, mut store, intent) = effects_fixture(json!({}));
        let (owner, lifecycle, plan) = variables_ready(&mut store, &intent, &["__internal_da_a"]);
        let before = effects_conversation(&store);
        if after_variables { store.generation_send_variables(&owner, &lifecycle, &plan).unwrap(); }
        store.generation_cancel(&owner.ticket).unwrap();
        let revision = store.revision().unwrap();
        store.generation_send_variables(&owner, &lifecycle, &plan).unwrap();
        store.generation_send_memory(&owner, &lifecycle, Some("must not be written")).unwrap();
        assert!(matches!(store.generation_claim_dispatch(&owner, &lifecycle).unwrap(), DispatchClaim::Refused(_)));
        let after = effects_conversation(&store);
        assert_eq!((store.revision().unwrap(), &after["lastMemory"]), (revision, &before["lastMemory"]));
        assert_eq!(after["scriptstate"].get("$__internal_da_a").is_some(), after_variables);
        store.generation_settle_lost(&owner).unwrap();
        assert_eq!(store.generation_status(&owner.ticket).unwrap().phase, Phase::Cancelled);
    }
}

#[test]
fn variable_conflict_retry_uses_original_key_expectations_and_fresh_unrelated_values() {
    for same_key in [false, true] {
        let (_directory, mut store, intent) = effects_fixture(json!({}));
        let (owner, lifecycle, plan) = variables_ready(&mut store, &intent, &["__internal_ka_a"]);
        let mut concurrent = store.open_native_job_store().unwrap();
        let ticket = owner.ticket.clone();
        let first_attempt = Arc::new(Mutex::new(None));
        let captured = Arc::clone(&first_attempt);
        lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            *captured.lock().unwrap() = concurrent.generation_record(&ticket).unwrap().attempt;
            let mutation = UnitMutation::Set {
                key: UnitKey::new(&["conversation", "character", "conversation", "scriptstate"]).unwrap(),
                value: if same_key { json!({"$__internal_ka_a":"true","foreign":1}) } else { json!({"foreign":1}) },
            };
            let revision = concurrent.revision().unwrap();
            // A competing library transaction changes the map after the first view and frozen attempt.
            // Device admission is already held, so use the real projection mutation inside this transaction.
            super::super::commit::incremental_commit(&mut concurrent.connection, revision, |_, _| Ok(()),
                |tx, generation, _| lww::apply_mutations(tx, generation, std::slice::from_ref(&mutation))).unwrap();
        })));
        let (revision, receipts) = (store.revision().unwrap(), receipt_count(&store));
        let status = store.generation_send_variables(&owner, &lifecycle, &plan).unwrap();
        lww::BEFORE_HELD_COMMIT.with(|hook| hook.borrow_mut().take());
        let first_attempt = first_attempt.lock().unwrap().take().expect("the first variable attempt reached the competing transaction");
        assert_eq!(first_attempt.kind, AttemptKind::Effect { effect: SendEffect::Variables });
        assert_eq!(status.phase, if same_key { Phase::Stale } else { Phase::Running });
        assert_eq!(store.revision().unwrap(), revision + if same_key { 1 } else { 2 });
        assert_eq!(receipt_count(&store), receipts + i64::from(!same_key));
        assert_eq!(effects_conversation(&store)["scriptstate"], json!({"$__internal_ka_a":"true","foreign":1}));
        let variables = store.generation_record(&owner.ticket).unwrap().preparation.unwrap().effects.variables;
        if same_key { assert_eq!(variables, EffectState::Pending); }
        else {
            let EffectState::Settled { receipt: Some((request_id, _, settled_at)) } = variables else { panic!("missing retry receipt") };
            assert_ne!(request_id, first_attempt.request_id);
            assert_eq!(settled_at, revision + 2);
        }
        assert!(!status.pending_commit);
    }
}

fn lose_variable_ack(store: &PersistentStore, ticket: &Ticket) {
    let path = format!("$.journal.records.\"{}\".attempt", ticket.sequence.0);
    store.device_store().unwrap().connection().execute_batch(&format!(r#"
        CREATE TEMP TRIGGER synthetic_variable_ack BEFORE UPDATE ON device_settings
        WHEN OLD.key='native-generation-journal-v1'
            AND json_extract(OLD.value,'{path}.kind.Effect.effect')='Variables'
            AND json_type(NEW.value,'{path}')='null'
        BEGIN SELECT RAISE(ABORT,'synthetic lost variable acknowledgement'); END;
    "#)).unwrap();
}

#[test]
fn real_lore_failure_prefix_survives_lost_ack_without_repreparation() {
    use crate::native_generation::cbs::{CaseUnicode, HostCapture};
    let now = i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()).unwrap();
    for lost_ack in [false, true] {
        let mut case = json!({
            "character":{"loreSettings":{"recursiveScanning":false,"tokenBudget":1000,"scanDepth":10},"globalLore":[
                {"id":"flag","key":"","alwaysActive":true,"content":"@@keep_activate_after_match\nOwned lore.","insertorder":200},
                {"id":"failing","key":"","alwaysActive":true,"content":"@@keep_activate_after_match\n<|endoftext|>","insertorder":100}
            ]}
        });
        case["messages"] = super::preparation_tests::cases()["effectMessages"].clone();
        case["messages"][0]["name"] = json!("Synthetic foreign speaker");
        let (_directory, mut store, mut intent) = super::preparation_tests::stored(&super::preparation_tests::database(&case));
        intent.host = Some(HostCapture::new(false, CaseUnicode::Unicode16, vec![(now - 86_400_000, 0)], now + 86_400_000));
        let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
        let before = effects_conversation(&store);
        let error = PreparationError::Tokenizer("disallowed_special_token".into());
        if lost_ack { lose_variable_ack(&store, &ticket); }
        let prepared = service::prepare_reserved_ollama(&mut store, &ticket);
        if lost_ack {
            assert!(prepared.is_err());
        } else {
            assert!(matches!(prepared.unwrap(), PreparedGeneration::Failed { error: actual, status }
                if actual == error && status.phase == Phase::Failed));
        }
        let frozen = store.generation_record(&ticket).unwrap();
        assert_eq!(frozen.preparation.as_ref().unwrap().error, Some(error.clone()));
        assert_eq!((frozen.preparation.as_ref().unwrap().ready, frozen.execution), (false, None));
        assert_eq!(effects_conversation(&store)["scriptstate"]["$__internal_ka_flag"], "true");
        assert!(effects_conversation(&store)["scriptstate"].get("$__internal_ka_failing").is_none());
        assert_eq!(effects_conversation(&store).get("lastMemory"), before.get("lastMemory"));
        let attempt = frozen.attempt;
        if lost_ack {
            assert_eq!(attempt.as_ref().unwrap().kind, AttemptKind::Effect { effect: SendEffect::Variables });
            store.device_store().unwrap().connection().execute_batch("DROP TRIGGER synthetic_variable_ack").unwrap();
        } else {
            assert!(attempt.is_none());
        }
        variable_map(&mut store, json!({"$__internal_ka_flag":"foreign","unrelated":{"keep":1}}));
        let revision = store.revision().unwrap();
        service::BEFORE_PREPARATION.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("must not reevaluate failed lore"))));
        let mut retry = store.open_native_job_store().unwrap();
        assert!(matches!(service::prepare_reserved_ollama(&mut retry, &ticket).unwrap(), PreparedGeneration::Existing(status) if status.phase == Phase::Failed));
        service::BEFORE_PREPARATION.with(|hook| hook.borrow_mut().take());
        let status = retry.generation_status(&ticket).unwrap();
        assert_eq!((status.phase, status.preparation_ready, status.preparation_error), (Phase::Failed, false, Some(error)));
        assert_eq!(store.revision().unwrap(), revision);
        assert_eq!(effects_conversation(&store)["scriptstate"], json!({"$__internal_ka_flag":"foreign","unrelated":{"keep":1}}));
        assert_eq!(effects_conversation(&store).get("lastMemory"), before.get("lastMemory"));
        let evidence = retry.generation_effects_evidence(&ticket).unwrap().unwrap();
        assert_eq!(evidence.receipts.len(), 2);
        if let Some(attempt) = attempt {
            assert!(evidence.receipts.iter().any(|receipt| receipt.request_id == attempt.request_id && receipt.revision == attempt.expected_revision + 1));
        }
        let record = retry.generation_record(&ticket).unwrap();
        assert!(record.attempt.is_none() && record.execution.is_none());
    }
}

#[test]
fn failed_preparation_pins_original_error_with_prefix_and_recovers_without_recomputing() {
    for recovery in ["resume", "cancel", "recover", "snapshot"] {
        let (_directory, mut store, intent) = effects_fixture(json!({}));
        let (owner, plan) = variable_plan(&mut store, &intent, &["__internal_ka_a"]);
        let error = PreparationError::ContextLimit;
        lose_variable_ack(&store, &owner.ticket);
        let revision = store.revision().unwrap();
        assert!(store.generation_fail_preparation_with_variables(&owner, error.clone(), &plan).is_err());
        let frozen = store.generation_record(&owner.ticket).unwrap();
        assert_eq!((frozen.phase, frozen.preparation.as_ref().unwrap().ready, frozen.execution), (Phase::Running, false, None));
        assert_eq!(frozen.preparation.unwrap().error, Some(error.clone()));
        let attempt = frozen.attempt.unwrap();
        assert_eq!(attempt.kind, AttemptKind::Effect { effect: SendEffect::Variables });
        assert_eq!(store.revision().unwrap(), revision + 1);
        store.device_store().unwrap().connection().execute_batch("DROP TRIGGER synthetic_variable_ack").unwrap();
        variable_map(&mut store, json!({"$__internal_ka_a":"foreign","unrelated":42}));
        let revision = store.revision().unwrap();
        service::BEFORE_PREPARATION.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("must not recompute a pinned failure"))));
        let mut retry = store.open_native_job_store().unwrap();
        match recovery {
            "resume" => assert!(matches!(service::prepare_reserved_ollama(&mut retry, &owner.ticket).unwrap(), PreparedGeneration::Existing(status) if status.phase == Phase::Failed)),
            "cancel" => assert_eq!(retry.generation_cancel(&owner.ticket).unwrap().phase, Phase::Failed),
            "recover" => assert_eq!(retry.generation_recover(&owner.ticket).unwrap().phase, Phase::Failed),
            _ => assert!(retry.generation_owned_preparation_snapshot(&owner).unwrap().is_none()),
        }
        service::BEFORE_PREPARATION.with(|hook| hook.borrow_mut().take());
        let status = retry.generation_status(&owner.ticket).unwrap();
        assert_eq!((status.phase, status.preparation_ready, status.preparation_error), (Phase::Failed, false, Some(error)));
        assert_eq!(store.revision().unwrap(), revision);
        assert_eq!(effects_conversation(&store)["scriptstate"], json!({"$__internal_ka_a":"foreign","unrelated":42}));
        let evidence = retry.generation_effects_evidence(&owner.ticket).unwrap().unwrap();
        assert!(evidence.receipts.iter().any(|receipt| receipt.request_id == attempt.request_id && receipt.revision == revision - 1));
    }
}

#[test]
fn cancelled_preparation_and_definitely_unapplied_failure_do_not_replay_a_variable_plan() {
    for cancel_first in [false, true] {
        let (_directory, mut store, intent) = effects_fixture(json!({}));
        let (owner, plan) = variable_plan(&mut store, &intent, &["__internal_da_a"]);
        let before = effects_conversation(&store);
        let revision = store.revision().unwrap();
        if cancel_first {
            store.generation_cancel(&owner.ticket).unwrap();
            assert_eq!(store.generation_fail_preparation_with_variables(&owner, PreparationError::ContextLimit, &plan).unwrap().phase, Phase::Cancelled);
        } else {
            let blocker = store.open_native_job_store().unwrap();
            let admission = blocker.acquire_intent_admission().unwrap();
            assert!(matches!(store.generation_fail_preparation_with_variables(&owner, PreparationError::ContextLimit, &plan), Err(StoreError::CommitBusy)));
            drop(admission);
            let pinned = store.generation_record(&owner.ticket).unwrap();
            assert!(pinned.attempt.is_none());
            assert_eq!(pinned.preparation.unwrap().error, Some(PreparationError::ContextLimit));
            assert!(matches!(store.generation_resume_preparation(&owner.ticket).unwrap(), Start::Existing(status)
                if status.phase == Phase::Failed && status.preparation_error == Some(PreparationError::ContextLimit)));
        }
        assert_eq!((store.revision().unwrap(), effects_conversation(&store)), (revision, before));
    }
}

#[test]
fn variables_follow_captured_preset_identity_and_canonical_gui_selection() {
    for change in ["equivalent", "same-preset", "override", "switch", "reorder", "removed"] {
        let (_directory, mut store, mut intent) = effects_fixture(json!({}));
        let original = store.read_preset("preset-a", None).unwrap().unwrap().value;
        let mut other = original.clone(); other["id"] = json!("preset-b");
        store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), replace_presets: Some(vec![original.clone(), other.clone()]),
            root_mutations: Some(vec![RootMutation::Delete { key: "botPresetsId".into() }]), ..Default::default() }).unwrap();
        if change == "override" { intent.effective_preset_id = "preset-b".into(); }
        intent.target.expected_revision = store.revision().unwrap();
        let (owner, lifecycle, plan) = variables_ready(&mut store, &intent, &["__internal_ka_a"]);
        assert_eq!(plan.selected_preset_id.as_deref(), Some("preset-a"));
        let mut captured = original.clone(); captured["temperature"] = json!(23);
        let (presets, selection) = match change {
            "switch" => (vec![original, other], json!("preset-b")),
            "reorder" => (vec![other, original], json!(0)),
            "removed" => (vec![other], json!(0)),
            _ => (vec![captured, other], json!("preset-a")),
        };
        store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), replace_presets: Some(presets),
            root_mutations: Some(vec![RootMutation::Set { key: "botPresetsId".into(), value: selection }]), ..Default::default() }).unwrap();
        let revision = store.revision().unwrap();
        let stale = matches!(change, "switch" | "reorder" | "removed");
        let status = store.generation_send_variables(&owner, &lifecycle, &plan).unwrap();
        assert_eq!(status.phase, if stale { Phase::Stale } else { Phase::Running }, "{change}");
        assert_eq!(store.revision().unwrap(), revision + i64::from(!stale));
        assert_eq!(effects_conversation(&store)["scriptstate"].get("$__internal_ka_a").is_some(), !stale);
    }
}

#[test]
fn variable_attempt_windows_preserve_frozen_identity_and_protection() {
    for failure in [false, true] {
        for window in ["before-intent", "durable-intent", "receipt"] {
            let (_directory, mut store, intent) = effects_fixture(json!({}));
            let (owner, mut plan) = variable_plan(&mut store, &intent, &["__internal_ka_a"]);
            // The author note is part of the same frozen write: both values land, or neither.
            plan.note = Some(crate::native_generation::preparation_effects::PreparedNote { value: json!("Trigger note."),
                original: Some(json!("Synthetic author note.")) });
            let lifecycle = uuid::Uuid::new_v4().to_string();
            if !failure { store.generation_mark_ready_owned(&owner, &lifecycle).unwrap(); }
            let revision = store.revision().unwrap();
            if window == "receipt" { lose_variable_ack(&store, &owner.ticket); }
            else { lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("synthetic variable intent interruption")))); }
            let write = catch_unwind(AssertUnwindSafe(|| if failure {
                store.generation_fail_preparation_with_variables(&owner, PreparationError::ContextLimit, &plan)
            } else { store.generation_send_variables(&owner, &lifecycle, &plan) }));
            assert!(if window == "receipt" { write.unwrap().is_err() } else { write.is_err() });
            let frozen = store.generation_record(&owner.ticket).unwrap();
            let attempt = frozen.attempt.clone().unwrap();
            assert_eq!(frozen.preparation.as_ref().unwrap().error, failure.then_some(PreparationError::ContextLimit));
            assert_eq!(attempt.kind, AttemptKind::Effect { effect: SendEffect::Variables });
            if window == "before-intent" {
                // Model the journal-before-intent cut: no receipt exists, and recovery has no known-unapplied proof.
                store.device_store().unwrap().connection().execute("DELETE FROM lww_intents WHERE request_id=?1", [&attempt.request_id]).unwrap();
                for _ in 0..2 { assert_error(store.generation_recover(&owner.ticket), "receipt-missing"); }
                assert_eq!(store.generation_record(&owner.ticket).unwrap().attempt, Some(attempt));
                assert!(store.generation_status(&owner.ticket).unwrap().pending_commit);
                assert!(protected(&store, NativeGenerationTarget::Character { character_id:"character" }).unwrap());
                assert!(store.generation_active().unwrap().iter().any(|active| active.ticket == owner.ticket));
                assert_eq!(store.revision().unwrap(), revision);
                assert_eq!(effects_conversation(&store)["note"], "Synthetic author note.");
                continue;
            }
            if window == "receipt" {
                store.device_store().unwrap().connection().execute_batch("DROP TRIGGER synthetic_variable_ack").unwrap();
                variable_map(&mut store, json!({"$__internal_ka_a":"foreign","keep":1}));
            }
            let recovered = store.generation_recover(&owner.ticket).unwrap();
            assert_eq!(recovered.phase, if failure { Phase::Failed } else { Phase::Running });
            assert!(!recovered.pending_commit);
            let record = store.generation_record(&owner.ticket).unwrap();
            assert_eq!(record.preparation.as_ref().unwrap().error, failure.then_some(PreparationError::ContextLimit));
            assert_eq!(record.preparation.unwrap().effects.variables, EffectState::Settled {
                receipt: Some((attempt.request_id, attempt.intent_digest, revision + 1)) });
            assert_eq!(effects_conversation(&store)["scriptstate"]["$__internal_ka_a"], if window == "receipt" { "foreign" } else { "true" });
            assert_eq!(effects_conversation(&store)["note"], "Trigger note.");
            assert_eq!(store.revision().unwrap(), revision + if window == "receipt" { 2 } else { 1 });
        }
    }
}

#[test]
fn pinned_variable_failure_recovers_after_process_restart_with_its_original_receipt() {
    const TEST: &str = "pinned_variable_failure_recovers_after_process_restart_with_its_original_receipt";
    if let Some(root) = restart_root(TEST) {
        let saved: Value = serde_json::from_slice(&std::fs::read(root.join("variable-failure.json")).unwrap()).unwrap();
        let ticket: Ticket = serde_json::from_value(saved["ticket"].clone()).unwrap();
        let mut store = PersistentStore::open(&root).unwrap();
        service::BEFORE_PREPARATION.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("foreign recovery must not recompute"))));
        assert!(matches!(service::prepare_reserved_ollama(&mut store, &ticket).unwrap(), PreparedGeneration::Existing(status)
            if status.phase == Phase::Failed && status.preparation_error == Some(PreparationError::ContextLimit)));
        service::BEFORE_PREPARATION.with(|hook| hook.borrow_mut().take());
        let receipt = store.generation_record(&ticket).unwrap().preparation.unwrap().effects.variables;
        assert_eq!(serde_json::to_value(receipt).unwrap(), saved["receipt"]);
        assert_eq!(store.revision().unwrap(), saved["revision"].as_i64().unwrap());
        assert_eq!(effects_conversation(&store)["scriptstate"]["$__internal_ka_a"], "foreign");
        return;
    }
    let (directory, mut store, intent) = effects_fixture(json!({}));
    let (owner, plan) = variable_plan(&mut store, &intent, &["__internal_ka_a"]);
    lose_variable_ack(&store, &owner.ticket);
    assert!(store.generation_fail_preparation_with_variables(&owner, PreparationError::ContextLimit, &plan).is_err());
    let attempt = store.generation_record(&owner.ticket).unwrap().attempt.unwrap();
    store.device_store().unwrap().connection().execute_batch("DROP TRIGGER synthetic_variable_ack").unwrap();
    variable_map(&mut store, json!({"$__internal_ka_a":"foreign"}));
    let receipt = EffectState::Settled { receipt: Some((attempt.request_id, attempt.intent_digest, attempt.expected_revision + 1)) };
    std::fs::write(directory.path().join("variable-failure.json"), serde_json::to_vec(&json!({"ticket":owner.ticket,"receipt":receipt,"revision":store.revision().unwrap()})).unwrap()).unwrap();
    run_in_restarted_process(TEST, directory.path());
}

#[test]
fn failed_variable_conflicts_retry_only_the_retained_live_plan_and_keep_original_error() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let (owner, plan) = variable_plan(&mut store, &intent, &["__internal_ka_a"]);
    let revision = store.revision().unwrap();
    conflict_held_commits(store.open_native_job_store().unwrap(), 2);
    let status = store.generation_fail_preparation_with_variables(&owner, PreparationError::ContextLimit, &plan).unwrap();
    assert_eq!((status.phase, status.preparation_error, status.pending_commit), (Phase::Failed, Some(PreparationError::ContextLimit), false));
    assert_eq!(store.revision().unwrap(), revision + 3);
    assert_eq!(effects_conversation(&store)["scriptstate"]["$__internal_ka_a"], "true");
}

// Literal layouts from the v2 writer. Values are substituted only at scalar leaves, never by removing new fields.
const V2_RECORD_BYTES: &str = r#"{"target":{"characterId":$target.characterId,"conversationId":$target.conversationId,"expectedRevision":$target.expectedRevision,"libraryIdentity":$target.libraryIdentity,"bindingAuthority":$target.bindingAuthority},"phase":$phase,"session":$session,"owner":$owner,"message_id":$message_id,"message_time":$message_time,"fence":{"character":$character,"conversation":$conversation,"messages":$messages,"count":$fence.count},"initial_count":$initial_count,"checkpoint":$checkpoint,"output_hash":$output_hash,"committed_revision":$committed_revision,"attempt":$attempt,"preparation":$preparation,"execution":$execution}"#;
const V2_PREPARATION_BYTES: &str = r#"{"preset_id":$preparation.preset_id,"normalization":$normalization,"ready":$preparation.ready,"error":$error,"effects":{"interaction":$interaction,"memory":$memory},"normalized_at":$preparation.normalized_at,"ready_at":$preparation.ready_at$replay}"#;
const V2_EXECUTION_BYTES: &str = r#"{"lifecycle_id":$execution.lifecycle_id,"dispatch_id":$execution.dispatch_id,"cancel":$execution.cancel,"terminal":$terminal,"output_receipt":$receipt}"#;

fn old_receipt_state(record: &Value, path: &str) -> String {
    let value = path.split('.').fold(record, |value, name| &value[name]);
    if value == "Pending" { return r#""Pending""#.into(); }
    assert!(value.get("Settled").is_some());
    if value["Settled"]["receipt"].is_null() { r#"{"Settled":{"receipt":null}}"#.into() }
    else { literal_body(&format!(r#"{{"Settled":{{"receipt":[${path}.Settled.receipt.0,${path}.Settled.receipt.1,${path}.Settled.receipt.2]}}}}"#), record, &[]) }
}

fn v2_record_bytes(record: &Value) -> String {
    let unit = |name: &str| if record["fence"][name].is_null() { "null".into() } else {
        literal_body(&format!(r#"{{"stamp":$fence.{name}.stamp,"version":$fence.{name}.version}}"#), record, &[])
    };
    let attempt = if record["attempt"].is_null() { "null".into() } else {
        let kind = &record["attempt"]["kind"];
        let kind = if kind == "HistoryIds" { r#""HistoryIds""#.into() }
            else if kind.get("Output").is_some() { literal_body(LEGACY_OUTPUT_KIND_B, record, &[]) }
            else {
                assert!(matches!(kind["Effect"]["effect"].as_str(), Some("Interaction" | "Memory")));
                literal_body(r#"{"Effect":{"effect":$attempt.kind.Effect.effect}}"#, record, &[])
            };
        literal_body(LEGACY_ATTEMPT_B, record, &[("kind", kind)])
    };
    let preparation = if record["preparation"].is_null() { "null".into() } else {
        let p = &record["preparation"];
        let normalization = if p["normalization"].get("Settled").is_some() { old_receipt_state(record, "preparation.normalization") }
            else if let Some(assignments) = p["normalization"]["Pending"]["assignments"].as_array() {
                let rows: Vec<_> = (0..assignments.len()).map(|index| literal_body(&format!(r#"{{"index":$preparation.normalization.Pending.assignments.{index}.index,"original_hash":$preparation.normalization.Pending.assignments.{index}.original_hash,"id":$preparation.normalization.Pending.assignments.{index}.id}}"#), record, &[])).collect();
                format!(r#"{{"Pending":{{"assignments":[{}]}}}}"#, rows.join(","))
            } else { r#"{"Pending":{"assignments":null}}"#.into() };
        let error = if p["error"].is_null() { "null".into() }
            else if p["error"].get("field").is_some() { literal_body(r#"{"kind":$preparation.error.kind,"field":$preparation.error.field}"#, record, &[]) }
            else { literal_body(r#"{"kind":$preparation.error.kind}"#, record, &[]) };
        assert!(p["host"].is_null(), "these frozen synthetic fixtures do not carry host data");
        let replay = if p["entropy"].is_null() { String::new() } else {
            let draws = p["entropy"]["draws"].as_array().unwrap();
            let rows: Vec<_> = (0..draws.len()).map(|index| literal_body(&format!("$preparation.entropy.draws.{index}"), record, &[])).collect();
            literal_body(r#", "entropy":{"now":$preparation.entropy.now,"draws":[$draws]}"#, record, &[("draws", rows.join(","))])
        };
        literal_body(V2_PREPARATION_BYTES, record, &[("normalization", normalization), ("error", error),
            ("interaction", old_receipt_state(record, "preparation.effects.interaction")), ("memory", old_receipt_state(record, "preparation.effects.memory")), ("replay", replay)])
    };
    let execution = if record["execution"].is_null() { "null".into() } else {
        let e = &record["execution"];
        let terminal = if e["terminal"].is_null() { "null".into() }
            else if e["terminal"].get("Normal").is_some() { literal_body(r#"{"Normal":{"sequence":$execution.terminal.Normal.sequence,"output_hash":$execution.terminal.Normal.output_hash}}"#, record, &[]) }
            else if e["terminal"].get("Cancelled").is_some() { literal_body(r#"{"Cancelled":{"reason":$execution.terminal.Cancelled.reason}}"#, record, &[]) }
            else { literal_body(r#"{"Failed":{"failure":{"category":$execution.terminal.Failed.failure.category,"detail":$execution.terminal.Failed.failure.detail,"stage":$execution.terminal.Failed.failure.stage,"http_status":$execution.terminal.Failed.failure.http_status}}}"#, record, &[]) };
        let receipt = if e["output_receipt"].is_null() { "null".into() } else {
            literal_body(r#"{"request_id":$execution.output_receipt.request_id,"intent_digest":$execution.output_receipt.intent_digest,"receipt_revision":$execution.output_receipt.receipt_revision,"source_sequence":$execution.output_receipt.source_sequence,"output_hash":$execution.output_receipt.output_hash}"#, record, &[])
        };
        literal_body(V2_EXECUTION_BYTES, record, &[("terminal", terminal), ("receipt", receipt)])
    };
    literal_body(V2_RECORD_BYTES, record, &[("character", unit("character")), ("conversation", unit("conversation")), ("messages", unit("messages")),
        ("attempt", attempt), ("preparation", preparation), ("execution", execution)])
}

fn v2_body(store: &PersistentStore) -> String {
    let body: Value = serde_json::from_str(&journal_body(store).unwrap()).unwrap();
    let journal = &body["journal"];
    let records: Vec<_> = journal["records"].as_object().unwrap().iter().map(|(key, record)|
        format!("{}:{}", serde_json::to_string(key).unwrap(), v2_record_bytes(record))).collect();
    literal_body(r#"{"version":2,"journal":{"epoch":$epoch,"high_water":$high_water,"records":{$records}}}"#, journal, &[("records", records.join(","))])
}

// The old reader checks its exact version before decoding records. Its effect enum has only Interaction/Memory.
fn old_v2_read(body: &str) -> StoreResult<V2Journal> {
    let members: JournalMembers = serde_json::from_str(body).map_err(|_| invalid("journal-format"))?;
    let version = members.version.ok_or_else(|| invalid("journal-shape"))?;
    if serde_json::from_str::<u64>(version.get()).map_err(|_| invalid("journal-version"))? != 2 { return Err(invalid("journal-version")); }
    serde_json::from_str(members.journal.ok_or_else(|| invalid("journal-shape"))?.get()).map_err(|_| invalid("journal-format"))
}

#[test]
fn v2_journal_bytes_convert_without_replaying_effects_or_losing_receipts_and_attempts() {
    for state in ["reserved", "ready", "terminal", "interaction", "history", "memory", "output"] {
        let (_directory, mut store, mut intent) = effects_fixture(json!({}));
        let ticket = if state == "reserved" { service::reserve_ollama(&mut store, &intent).unwrap() } else {
            let owner = effects_owner(&mut store, &intent);
            if state == "interaction" {
                lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("old interaction cut"))));
                assert!(catch_unwind(AssertUnwindSafe(|| store.generation_ensure_history_ids(&owner))).is_err());
            } else if state == "history" { interrupt_ids_after_intent(&mut store, &owner); }
            else {
                store.generation_ensure_history_ids(&owner).unwrap();
                let lifecycle = uuid::Uuid::new_v4().to_string();
                store.generation_mark_ready_owned(&owner, &lifecycle).unwrap();
                store.generation_send_variables(&owner, &lifecycle, &PreparedVariablePlan::default()).unwrap();
                if state == "memory" {
                    lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("old memory cut"))));
                    assert!(catch_unwind(AssertUnwindSafe(|| store.generation_send_memory(&owner, &lifecycle, Some("old marker")))).is_err());
                } else if matches!(state, "output" | "terminal") {
                    store.generation_send_memory(&owner, &lifecycle, Some("old marker")).unwrap();
                    store.generation_claim_dispatch(&owner, &lifecycle).unwrap();
                    if state == "output" { interrupt_after_reservation(&mut store, &owner); }
                    else {
                        store.generation_checkpoint(&owner, &checkpoint(1, "old complete")).unwrap();
                        store.generation_settle_owned(&owner, normal(1, "old complete")).unwrap();
                    }
                }
            }
            owner.ticket
        };
        // Keep a second, differently staged record so conversion cannot silently lose journal members.
        let mut companion = intent.clone();
        companion.target.expected_revision = store.revision().unwrap();
        let companion = store.generation_reserve_ollama(&companion).unwrap();
        assert_ne!(companion, ticket);
        let before = store.generation_record(&ticket).unwrap();
        if state == "reserved" { assert_eq!(before.preparation.as_ref().unwrap().effects.variables, EffectState::Pending); }
        let original_status = store.generation_status(&ticket).unwrap();
        let (revision, receipts) = (store.revision().unwrap(), receipt_count(&store));
        let outbox = store.lww_read_outbox(store.lww_binding_authority().unwrap(), 1000).unwrap().entries;
        // The expected full journal is captured independently of the compatibility decoder.
        let mut expected_value: Value = serde_json::from_str(&journal_body(&store).unwrap()).unwrap();
        for record in expected_value["journal"]["records"].as_object_mut().unwrap().values_mut() {
            if !record["preparation"].is_null() {
                record["preparation"]["effects"]["variables"] = json!({"Settled":{"receipt":null}});
            }
        }
        let old = v2_body(&store);
        assert!(!old.contains("variables"));
        assert!(old_v2_read(&old).is_ok());
        write_body(&store, &old);
        let decoded = store.generation_record(&ticket).unwrap();
        assert_eq!(decoded.attempt, before.attempt);
        assert_eq!(decoded.execution, before.execution);
        assert_eq!(decoded.preparation.as_ref().unwrap().effects.variables, EffectState::Settled { receipt: None });
        assert_eq!(store.generation_status(&ticket).unwrap(), original_status);
        assert_eq!(protected(&store, NativeGenerationTarget::Character { character_id:"character" }).unwrap(), state != "reserved" && state != "terminal");
        assert_eq!(journal_body(&store).as_deref(), Some(old.as_str()));
        let decoded_journal = decode_journal(&old).unwrap().journal;
        assert_eq!(decoded_journal.records.len(), 2);
        assert_eq!(serde_json::to_value(&decoded_journal).unwrap(), expected_value["journal"]);
        let expected = encode_journal(&decoded_journal).unwrap();
        for limit in [old.len() - 1, expected.len() - 1] {
            assert_error(store.generation_convert_journal_limited(limit), "protection-journal-limit");
            assert_eq!(journal_body(&store).as_deref(), Some(old.as_str()));
        }
        store.device_store().unwrap().connection().execute_batch("CREATE TEMP TRIGGER v2_conversion_failure BEFORE UPDATE ON device_settings WHEN OLD.key='native-generation-journal-v1' BEGIN SELECT RAISE(ABORT,'synthetic v2 conversion'); END;").unwrap();
        assert!(store.generation_convert_journal().is_err());
        assert_eq!(journal_body(&store).as_deref(), Some(old.as_str()));
        store.device_store().unwrap().connection().execute_batch("DROP TRIGGER v2_conversion_failure").unwrap();
        store.generation_convert_journal().unwrap();
        assert_eq!(journal_body(&store).as_deref(), Some(expected.as_str()));
        assert_eq!(serde_json::from_str::<Value>(&journal_body(&store).unwrap()).unwrap(), expected_value);
        assert!(matches!(old_v2_read(&expected), Err(StoreError::Validation { message }) if message == "native-generation:journal-version"));
        store.generation_convert_journal().unwrap();
        assert_eq!(journal_body(&store).as_deref(), Some(expected.as_str()));
        assert_eq!((store.revision().unwrap(), receipt_count(&store)), (revision, receipts));
        assert_eq!(store.lww_read_outbox(store.lww_binding_authority().unwrap(), 1000).unwrap().entries, outbox);
        if state == "reserved" {
            // A converted reservation reaches Ready with Variables already settled and no new receipt.
            let Start::Started { owner, .. } = store.generation_resume_preparation(&ticket).unwrap() else { panic!("old reservation") };
            store.generation_ensure_history_ids(&owner).unwrap();
            let lifecycle = uuid::Uuid::new_v4().to_string();
            store.generation_mark_ready_owned(&owner, &lifecycle).unwrap();
            let snapshot = store.generation_preparation_snapshot(&{ intent.target.expected_revision = store.revision().unwrap(); intent }).unwrap();
            let plan = variables_from_snapshot(&snapshot, &["__internal_ka_retroactive"]);
            let revision = store.revision().unwrap();
            store.generation_send_variables(&owner, &lifecycle, &plan).unwrap();
            assert_eq!(store.revision().unwrap(), revision);
            assert!(effects_conversation(&store)["scriptstate"].get("$__internal_ka_retroactive").is_none());
        }
    }
}

#[test]
fn variable_failure_receipt_corruption_never_erases_attempt_or_releases_protection() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let (owner, plan) = variable_plan(&mut store, &intent, &["__internal_ka_a"]);
    lose_variable_ack(&store, &owner.ticket);
    assert!(store.generation_fail_preparation_with_variables(&owner, PreparationError::ContextLimit, &plan).is_err());
    store.device_store().unwrap().connection().execute_batch("DROP TRIGGER synthetic_variable_ack").unwrap();
    let frozen = store.generation_record(&owner.ticket).unwrap();
    let attempt = frozen.attempt.clone().unwrap();
    let (revision, value) = (store.revision().unwrap(), effects_conversation(&store)["scriptstate"].clone());
    for mode in ["digest", "revision", "missing"] {
        store.connection.execute_batch("CREATE TEMP TABLE saved_variable_receipts AS SELECT * FROM lww_requests").unwrap();
        match mode {
            "digest" => { store.connection.execute("UPDATE lww_requests SET digest=?2 WHERE request_id=?1", params![attempt.request_id, "0".repeat(64)]).unwrap(); }
            "revision" => { store.connection.execute("UPDATE lww_requests SET revision=revision+1 WHERE request_id=?1", [&attempt.request_id]).unwrap(); }
            _ => { store.connection.execute("DELETE FROM lww_requests WHERE request_id=?1", [&attempt.request_id]).unwrap(); }
        }
        assert!(store.generation_recover(&owner.ticket).is_err());
        assert_eq!(store.generation_record(&owner.ticket).unwrap(), frozen);
        assert!(protected(&store, NativeGenerationTarget::Character { character_id:"character" }).unwrap());
        assert_eq!(store.generation_effects_evidence(&owner.ticket).unwrap(), None);
        assert_eq!((store.revision().unwrap(), effects_conversation(&store)["scriptstate"].clone()), (revision, value.clone()));
        store.connection.execute_batch("DELETE FROM lww_requests; INSERT INTO lww_requests SELECT * FROM saved_variable_receipts; DROP TABLE saved_variable_receipts").unwrap();
    }
    assert_eq!(store.generation_recover(&owner.ticket).unwrap().phase, Phase::Failed);
    assert_eq!(store.revision().unwrap(), revision);
}

#[test]
fn v3_variable_attempt_validation_rejects_impossible_combinations_and_old_readers_refuse_it() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let (owner, plan) = variable_plan(&mut store, &intent, &["__internal_ka_a"]);
    lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("partial failed variables"))));
    assert!(catch_unwind(AssertUnwindSafe(|| store.generation_fail_preparation_with_variables(&owner, PreparationError::ContextLimit, &plan))).is_err());
    let body = journal_body(&store).unwrap();
    let original: Value = serde_json::from_str(&body).unwrap();
    let key = owner.ticket.sequence.0.to_string();
    assert!(protected(&store, NativeGenerationTarget::Character { character_id:"character" }).unwrap());
    for corruption in ["ready", "error", "execution", "normalization", "interaction", "variables", "memory"] {
        let mut broken = original.clone();
        let record = &mut broken["journal"]["records"][&key];
        match corruption {
            "ready" => record["preparation"]["ready"] = json!(true),
            "error" => record["preparation"]["error"] = Value::Null,
            "execution" => record["execution"] = json!({"lifecycle_id":uuid::Uuid::new_v4().to_string(),"dispatch_id":null,"cancel":null,"terminal":null,"output_receipt":null}),
            "normalization" => record["preparation"]["normalization"] = json!({"Pending":{"assignments":null}}),
            "interaction" => record["preparation"]["effects"]["interaction"] = json!("Pending"),
            field => record["preparation"]["effects"][field] = json!({"Settled":{"receipt":null}}),
        }
        write_body(&store, &serde_json::to_string(&broken).unwrap());
        assert_error(protected(&store, NativeGenerationTarget::Character { character_id:"character" }), "protection-attempt");
    }
    write_body(&store, &body);
    // Version rejection happens before record decoding in the actual old version gate.
    assert!(matches!(old_v2_read(&body), Err(StoreError::Validation { message }) if message == "native-generation:journal-version"));
    let fake_v2 = body.replacen("\"version\":3", "\"version\":2", 1);
    assert_error(decode_journal(&fake_v2).map(|_| ()), "journal-format");
    // The strict unwrapped reader also retains its original enum domain.
    let legacy_kind = literal_body(LEGACY_ATTEMPT_B, &original["journal"]["records"][&key],
        &[("kind", r#"{"Effect":{"effect":"Variables"}}"#.into())]);
    assert!(serde_json::from_str::<LegacyAttempt>(&legacy_kind).is_err());
    let mut journal = decode_journal(&body).unwrap().journal;
    journal.records.get_mut(&owner.ticket.sequence.0).unwrap().phase = Phase::Failed;
    let attempt = journal.records[&owner.ticket.sequence.0].attempt.clone();
    journal.prune();
    assert_eq!(journal.records[&owner.ticket.sequence.0].attempt, attempt);
    write_body(&store, &encode_journal(&journal).unwrap());
    assert!(protected(&store, NativeGenerationTarget::Character { character_id:"character" }).unwrap());
}

#[test]
fn dispatch_requires_variables_even_when_the_persisted_memory_state_is_settled() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let (owner, lifecycle, _) = variables_ready(&mut store, &intent, &["__internal_ka_a"]);
    // A receipt-free memory disposition cannot authorize bypassing a still-pending variable segment.
    let previous = store.generation_record(&owner.ticket).unwrap();
    let mut pending = previous.clone();
    pending.preparation.as_mut().unwrap().effects.memory = EffectState::Settled { receipt: None };
    store.generation_replace_record(&owner.ticket, &previous, &pending).unwrap();
    let (revision, body) = (store.revision().unwrap(), journal_body(&store));
    assert_error(store.generation_claim_dispatch(&owner, &lifecycle), "effects-pending");
    assert_eq!(execution(&store, &owner.ticket).dispatch_id, None);
    assert_eq!((store.revision().unwrap(), journal_body(&store)), (revision, body));
}

#[test]
fn converted_reserved_failure_checks_the_live_target_without_replaying_variables() {
    for edited in [false, true] {
        let (_directory, mut store, intent) = effects_fixture(json!({}));
        let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
        let old = v2_body(&store);
        write_body(&store, &old);
        store.generation_convert_journal().unwrap();
        let Start::Started { owner, .. } = store.generation_start_preparation(&ticket).unwrap() else { panic!("converted reservation did not start") };
        store.generation_ensure_history_ids(&owner).unwrap();
        let snapshot = store.generation_owned_preparation_snapshot(&owner).unwrap().unwrap();
        let plan = variables_from_snapshot(&snapshot, &["__internal_ka_never_retroactive"]);
        assert_eq!(store.generation_record(&ticket).unwrap().preparation.unwrap().effects.variables, EffectState::Settled { receipt: None });
        if edited {
            let mut message = effects_conversation(&store)["message"][0].clone();
            message["data"] = json!("Edited after the converted owner captured its plan.");
            edit_effect_history(&mut store, 0, 1, vec![message]);
        }
        let (revision, receipts, conversation) = (store.revision().unwrap(), receipt_count(&store), effects_conversation(&store));
        service::BEFORE_PREPARATION.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("converted failure must not recompute"))));
        let status = store.generation_fail_preparation_with_variables(&owner, PreparationError::ContextLimit, &plan).unwrap();
        assert_eq!(status.phase, if edited { Phase::Stale } else { Phase::Failed });
        assert_eq!(status.preparation_error, (!edited).then_some(PreparationError::ContextLimit));
        assert!(!status.pending_commit && !status.preparation_ready);
        assert!(matches!(service::prepare_reserved_ollama(&mut store, &ticket).unwrap(), PreparedGeneration::Existing(recovered) if recovered == status));
        service::BEFORE_PREPARATION.with(|hook| hook.borrow_mut().take());
        let record = store.generation_record(&ticket).unwrap();
        assert!(record.attempt.is_none() && record.execution.is_none());
        assert_eq!(record.preparation.unwrap().effects.variables, EffectState::Settled { receipt: None });
        assert_eq!((store.revision().unwrap(), receipt_count(&store), effects_conversation(&store)), (revision, receipts, conversation));
    }
}

fn plan_budget() -> risunest_ir::Budget {
    risunest_ir::Budget::new(risunest_ir::Limits { max_owned_bytes: 1 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 18,
        max_values: 1 << 16, max_depth: 128, max_work: 1 << 20 },
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))).unwrap()
}

// The plan admission's dry run accepts, from a preparation with settled history IDs.
fn trigger_plan(store: &mut PersistentStore, intent: &OllamaIntent, effects: &dyn Fn(&risunest_ir::Budget) -> Vec<risunest_ir::stage::Effect>)
    -> (Owner, PreparedVariablePlan) {
    let owner = effects_owner(store, intent);
    store.generation_ensure_history_ids(&owner).unwrap();
    let snapshot = store.generation_owned_preparation_snapshot(&owner).unwrap().unwrap();
    let budget = plan_budget();
    let (plan, result) = crate::native_generation::preparation_effects::preparation_plan(&snapshot, &effects(&budget));
    result.unwrap();
    (owner, plan)
}

fn text_value(value: &str, budget: &risunest_ir::Budget) -> risunest_ir::Value {
    risunest_ir::Value::Text(risunest_ir::Text::from_str(value, budget).unwrap())
}

fn chat_variable(name: &str, value: risunest_ir::Value, budget: &risunest_ir::Budget) -> risunest_ir::stage::Effect {
    risunest_ir::stage::Effect::ChatVariable { name: risunest_ir::Text::from_str(name, budget).unwrap(), value }
}

fn author_note(value: &str, budget: &risunest_ir::Budget) -> risunest_ir::stage::Effect {
    risunest_ir::stage::Effect::ChatField { field: risunest_ir::Text::from_str("note", budget).unwrap(), value: text_value(value, budget) }
}

fn note_map(store: &mut PersistentStore, value: Value) {
    store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), unit_mutations: Some(vec![UnitMutation::Set {
        key: UnitKey::new(&["conversation", "character", "conversation", "note"]).unwrap(), value,
    }]), ..Default::default() }).unwrap();
}

const NOTE_KEY: &str = r#"["conversation","character","conversation","note"]"#;

// Lore keep variables, then trigger writes: any name and scalar value in effect order, one scriptstate write in which
// the last write of a name wins, and no write over a key edited since the preparation read it.
#[test]
fn variables_store_any_name_and_scalar_value_in_effect_order() {
    use risunest_ir::Value as Ir;
    for concurrent in [None, Some("$mood"), Some("$count")] {
        let (_directory, mut store, mut intent) = effects_fixture(json!({}));
        variable_map(&mut store, json!({"$mood":"calm","$__internal_ka_lore":null,"unrelated":[1]}));
        intent.target.expected_revision = store.revision().unwrap();
        let (owner, plan) = trigger_plan(&mut store, &intent, &|budget| vec![
            chat_variable("__internal_ka_lore", text_value("true", budget), budget), chat_variable("mood", text_value("bright", budget), budget),
            chat_variable("__internal_ka_lore", text_value("trigger", budget), budget), chat_variable("count", Ir::Number(3.0), budget),
            chat_variable("ratio", Ir::Number(0.1), budget), chat_variable("zero", Ir::Number(-0.0), budget),
            chat_variable("large", Ir::Number(1e21), budget), chat_variable("flag", Ir::Bool(false), budget),
            chat_variable("empty", Ir::Null, budget), chat_variable("\u{c774}\u{b984} {}", text_value("\u{1f98a}", budget), budget)]);
        assert_eq!(plan.assignments.len(), 10);
        assert_eq!(plan.original.len(), 9);
        assert_eq!(plan.original["$__internal_ka_lore"], Some(Value::Null));
        assert_eq!(plan.original["$mood"], Some(json!("calm")));
        assert_eq!(plan.original["$count"], None);
        let lifecycle = uuid::Uuid::new_v4().to_string();
        store.generation_mark_ready_owned(&owner, &lifecycle).unwrap();
        if let Some(key) = concurrent {
            let mut state = effects_conversation(&store)["scriptstate"].clone();
            state[key] = json!("concurrent");
            variable_map(&mut store, state.clone());
            let revision = store.revision().unwrap();
            assert_eq!(store.generation_send_variables(&owner, &lifecycle, &plan).unwrap().phase, Phase::Stale, "{key}");
            assert_eq!((store.revision().unwrap(), effects_conversation(&store)["scriptstate"].clone()), (revision, state));
            continue;
        }
        let revision = store.revision().unwrap();
        store.generation_send_variables(&owner, &lifecycle, &plan).unwrap();
        assert_eq!(store.revision().unwrap(), revision + 1);
        let state = effects_conversation(&store)["scriptstate"].clone();
        assert_eq!(state, json!({"$mood":"bright","$__internal_ka_lore":"trigger","unrelated":[1],"$count":3,"$ratio":0.1,"$zero":0,
            "$large":serde_json::from_str::<Value>("1e+21").unwrap(),"$flag":false,"$empty":null,"$\u{c774}\u{b984} {}":"\u{1f98a}"}));
        // `JSON.stringify` writes a whole number without a fraction.
        assert_eq!(serde_json::to_string(&state["$count"]).unwrap(), "3");
        assert_eq!(serde_json::to_string(&state["$zero"]).unwrap(), "0");
        assert_eq!(effects_conversation(&store)["note"], "Synthetic author note.");
        store.generation_send_memory(&owner, &lifecycle, None).unwrap();
        assert!(matches!(store.generation_claim_dispatch(&owner, &lifecycle).unwrap(), DispatchClaim::Issued { .. }));
    }
}

// The trigger's chat variables and author note are one effect: one revision and receipt, or no write at all when a value
// the preparation read has changed since.
#[test]
fn the_author_note_commits_with_the_variables_in_one_revision() {
    for case in ["both", "note-only", "unchanged", "edited-note", "edited-variable"] {
        let (_directory, mut store, mut intent) = effects_fixture(json!({}));
        variable_map(&mut store, json!({"$mood":"calm","keep":1}));
        intent.target.expected_revision = store.revision().unwrap();
        let (mood, note) = if case == "unchanged" { ("calm", "Synthetic author note.") } else { ("bright", "Trigger note.") };
        let (owner, plan) = trigger_plan(&mut store, &intent, &|budget| {
            let mut effects = vec![author_note("First trigger note.", budget), author_note(note, budget)];
            if case != "note-only" { effects.insert(0, chat_variable("mood", text_value(mood, budget), budget)); }
            effects
        });
        assert_eq!(plan.note.as_ref().map(|planned| (&planned.value, &planned.original)), Some((&json!(note), &Some(json!("Synthetic author note.")))));
        let lifecycle = uuid::Uuid::new_v4().to_string();
        store.generation_mark_ready_owned(&owner, &lifecycle).unwrap();
        match case {
            "edited-note" => note_map(&mut store, json!("Edited during preparation.")),
            "edited-variable" => variable_map(&mut store, json!({"$mood":"edited","keep":1})),
            _ => {}
        }
        let (revision, receipts, before) = (store.revision().unwrap(), receipt_count(&store), effects_conversation(&store));
        assert_error(store.generation_send_memory(&owner, &lifecycle, None), "effects-pending");
        let status = store.generation_send_variables(&owner, &lifecycle, &plan).unwrap();
        if case.starts_with("edited") {
            assert_eq!((status.phase, store.revision().unwrap(), effects_conversation(&store)), (Phase::Stale, revision, before), "{case}");
            continue;
        }
        let written = i64::from(case != "unchanged");
        assert_eq!((status.phase.clone(), store.revision().unwrap(), receipt_count(&store)), (Phase::Running, revision + written, receipts + written), "{case}");
        let conversation = effects_conversation(&store);
        assert_eq!(conversation["note"], note);
        assert_eq!(conversation["scriptstate"], json!({"$mood": if case == "both" { "bright" } else { "calm" }, "keep":1}));
        let effects = store.generation_record(&owner.ticket).unwrap().preparation.unwrap().effects;
        assert!(matches!(&effects.variables, EffectState::Settled { receipt } if receipt.is_some() == (written == 1)));
        // Settled, neither value is written again, even over a later edit.
        note_map(&mut store, json!("Edited after the send wrote."));
        let revision = store.revision().unwrap();
        assert_eq!(store.generation_send_variables(&owner, &lifecycle, &plan).unwrap(), status);
        assert_eq!((store.revision().unwrap(), effects_conversation(&store)["note"].clone()), (revision, json!("Edited after the send wrote.")));
        store.generation_send_memory(&owner, &lifecycle, None).unwrap();
        let evidence = store.generation_effects_evidence(&owner.ticket).unwrap().unwrap();
        assert_eq!(evidence.affected_unit_keys.iter().any(|key| key == NOTE_KEY), written == 1);
        assert!(matches!(store.generation_claim_dispatch(&owner, &lifecycle).unwrap(), DispatchClaim::Issued { .. }));
    }
}

// TS keeps a trigger's variable and note writes when the send then fails, as at the context limit.
#[test]
fn a_failed_preparation_keeps_the_trigger_variables_and_note() {
    for note_only in [false, true] {
        let (_directory, mut store, intent) = effects_fixture(json!({}));
        let (owner, plan) = trigger_plan(&mut store, &intent, &|budget| {
            let mut effects = vec![author_note("Trigger note.", budget)];
            if !note_only { effects.insert(0, chat_variable("mood", text_value("dark", budget), budget)); }
            effects
        });
        let (revision, receipts) = (store.revision().unwrap(), receipt_count(&store));
        let status = store.generation_fail_preparation_with_variables(&owner, PreparationError::ContextLimit, &plan).unwrap();
        assert_eq!((status.phase, status.preparation_error, status.pending_commit), (Phase::Failed, Some(PreparationError::ContextLimit), false));
        assert_eq!((store.revision().unwrap(), receipt_count(&store)), (revision + 1, receipts + 1), "{note_only}");
        let conversation = effects_conversation(&store);
        assert_eq!(conversation["note"], "Trigger note.");
        assert_eq!(conversation["scriptstate"].get("$mood").is_some(), !note_only);
        let record = store.generation_record(&owner.ticket).unwrap();
        assert!(record.attempt.is_none() && record.execution.is_none());
        let evidence = store.generation_effects_evidence(&owner.ticket).unwrap().unwrap();
        assert!(evidence.affected_unit_keys.iter().any(|key| key == NOTE_KEY));
    }
}

const MESSAGES_KEY: &str = r#"["messages","character","conversation"]"#;

// A start trigger's message edits: the rows from index 2, the first of them given new data, and one appended row.
fn message_edits(budget: &risunest_ir::Budget, appended: &str) -> Vec<risunest_ir::stage::Effect> {
    use risunest_ir::stage::{Effect, MessageEdit};
    vec![chat_variable("mood", text_value("bright", budget), budget), author_note("Trigger note.", budget),
        Effect::Message(MessageEdit::Cut { start: 2.0, end: None }),
        Effect::Message(MessageEdit::SetData { index: 0.0, data: text_value("Edited by the trigger.", budget) }),
        Effect::Message(MessageEdit::Append { role: risunest_ir::chat::MessageRole::Char, data: text_value("Appended by the trigger.", budget),
            chat_id: risunest_ir::Text::from_str(appended, budget).unwrap() })]
}

fn edited_rows(rows: &[Value], appended: &str) -> Vec<Value> {
    let mut rows = rows[2..].to_vec();
    rows[0]["data"] = json!("Edited by the trigger.");
    rows.push(json!({"role":"char","data":"Appended by the trigger.","chatId":appended}));
    rows
}

fn stored_rows(store: &PersistentStore) -> Vec<Value> { effects_conversation(store)["message"].as_array().unwrap().clone() }

// TS commits a start trigger's message edits with its variables and note (`ScriptConversationAccess.commit`): one revision
// and one receipt, after which the record fences the edited history and the output row follows it.
#[test]
fn message_edits_commit_with_the_variables_in_one_revision_and_the_output_follows_them() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let appended = uuid::Uuid::new_v4().to_string();
    let (owner, plan) = trigger_plan(&mut store, &intent, &|budget| message_edits(budget, &appended));
    let read = stored_rows(&store);
    let expected = edited_rows(&read, &appended);
    assert_eq!((read.len(), plan.messages.as_ref().map(crate::native_generation::preparation_effects::PreparedMessages::count)), (8, Some(7)));
    let lifecycle = uuid::Uuid::new_v4().to_string();
    store.generation_mark_ready_owned(&owner, &lifecycle).unwrap();
    assert!(!journal_body(&store).unwrap().contains("edited_count"));
    let (revision, receipts) = (store.revision().unwrap(), receipt_count(&store));
    assert_eq!(store.generation_send_variables(&owner, &lifecycle, &plan).unwrap().phase, Phase::Running);
    assert_eq!((store.revision().unwrap(), receipt_count(&store)), (revision + 1, receipts + 1));
    let conversation = effects_conversation(&store);
    assert_eq!((&conversation["message"], &conversation["scriptstate"]["$mood"], &conversation["note"]),
        (&json!(expected), &json!("bright"), &json!("Trigger note.")));
    let record = store.generation_record(&owner.ticket).unwrap();
    let preparation = record.preparation.as_ref().unwrap();
    let EffectState::Settled { receipt: Some((request_id, _, written)) } = &preparation.effects.variables else { panic!("{preparation:?}") };
    assert_eq!((*written, record.fence.count, record.initial_count, preparation.edited_count), (revision + 1, 7, 8, Some(7)));
    assert_eq!(record.fence.messages.as_ref().map(|fence| &fence.version), Some(request_id));
    assert!(journal_body(&store).unwrap().contains("edited_count"));
    store.generation_send_memory(&owner, &lifecycle, None).unwrap();
    let evidence = store.generation_effects_evidence(&owner.ticket).unwrap().unwrap();
    assert!(evidence.affected_unit_keys.iter().any(|key| key == MESSAGES_KEY));
    assert!(matches!(store.generation_claim_dispatch(&owner, &lifecycle).unwrap(), DispatchClaim::Issued { .. }));
    store.generation_checkpoint(&owner, &checkpoint(1, "Out")).unwrap();
    store.generation_checkpoint(&owner, &checkpoint(2, "Output.")).unwrap();
    assert_eq!(store.generation_settle_owned(&owner, normal(2, "Output.")).unwrap().phase, Phase::Completed);
    let rows = stored_rows(&store);
    assert_eq!((&rows[..7], rows.len()), (&expected[..], 8));
    assert_eq!((&rows[7]["data"], &rows[7]["chatId"]), (&json!("Output."), &json!(record.message_id)));
}

// A history edit after the preparation read the rows, even one that restores the same content, fences the edits: the
// send ends Stale and writes neither the rows nor the variables.
#[test]
fn a_history_edit_since_the_preparation_makes_the_message_edits_stale() {
    for foreign in ["edit", "aba"] {
        let (_directory, mut store, intent) = effects_fixture(json!({}));
        let appended = uuid::Uuid::new_v4().to_string();
        let (owner, plan) = trigger_plan(&mut store, &intent, &|budget| message_edits(budget, &appended));
        let lifecycle = uuid::Uuid::new_v4().to_string();
        store.generation_mark_ready_owned(&owner, &lifecycle).unwrap();
        let read = stored_rows(&store);
        let mut row = read[7].clone();
        row["data"] = json!("Edited elsewhere.");
        edit_effect_history(&mut store, 7, 1, vec![row]);
        if foreign == "aba" { edit_effect_history(&mut store, 7, 1, vec![read[7].clone()]); }
        let (revision, receipts, before) = (store.revision().unwrap(), receipt_count(&store), effects_conversation(&store));
        assert_eq!(store.generation_send_variables(&owner, &lifecycle, &plan).unwrap().phase, Phase::Stale, "{foreign}");
        assert_eq!((store.revision().unwrap(), receipt_count(&store), effects_conversation(&store)), (revision, receipts, before), "{foreign}");
        assert_eq!(store.generation_record(&owner.ticket).unwrap().preparation.and_then(|preparation| preparation.edited_count), None);
    }
}

// The rows and variables land in one revision before the journal acknowledges them; the receipt then resolves the
// frozen attempt with its edited count, without a second write.
#[test]
fn a_lost_acknowledgement_of_the_message_edits_resolves_from_their_receipt_without_rewriting() {
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let appended = uuid::Uuid::new_v4().to_string();
    let (owner, plan) = trigger_plan(&mut store, &intent, &|budget| message_edits(budget, &appended));
    let expected = edited_rows(&stored_rows(&store), &appended);
    let lifecycle = uuid::Uuid::new_v4().to_string();
    store.generation_mark_ready_owned(&owner, &lifecycle).unwrap();
    let revision = store.revision().unwrap();
    lose_variable_ack(&store, &owner.ticket);
    assert!(store.generation_send_variables(&owner, &lifecycle, &plan).is_err());
    let attempt = store.generation_record(&owner.ticket).unwrap().attempt.unwrap();
    assert_eq!((&attempt.kind, attempt.edited_count), (&AttemptKind::Effect { effect: SendEffect::Variables }, Some(7)));
    assert_eq!((store.revision().unwrap(), stored_rows(&store), effects_conversation(&store)["scriptstate"]["$mood"].clone()),
        (revision + 1, expected.clone(), json!("bright")));
    store.device_store().unwrap().connection().execute_batch("DROP TRIGGER synthetic_variable_ack").unwrap();
    let mut retry = store.open_native_job_store().unwrap();
    assert_eq!(retry.generation_send_variables(&owner, &lifecycle, &plan).unwrap().phase, Phase::Running);
    assert_eq!((store.revision().unwrap(), stored_rows(&store)), (revision + 1, expected.clone()));
    let record = retry.generation_record(&owner.ticket).unwrap();
    assert_eq!((record.attempt.is_none(), record.fence.count, record.preparation.as_ref().unwrap().edited_count), (true, 7, Some(7)));
    assert_eq!(record.preparation.unwrap().effects.variables, EffectState::Settled {
        receipt: Some((attempt.request_id, attempt.intent_digest, revision + 1)) });
    retry.generation_send_memory(&owner, &lifecycle, None).unwrap();
    assert!(matches!(retry.generation_claim_dispatch(&owner, &lifecycle).unwrap(), DispatchClaim::Issued { .. }));
    retry.generation_checkpoint(&owner, &checkpoint(1, "Output.")).unwrap();
    assert_eq!(retry.generation_settle_owned(&owner, normal(1, "Output.")).unwrap().phase, Phase::Completed);
    let rows = stored_rows(&store);
    assert_eq!((&rows[..7], &rows[7]["data"]), (&expected[..], &json!("Output.")));
}

// TS keeps a trigger's message edits with its variables when the send then fails at the context limit, whether or not
// the history needed new IDs first.
#[test]
fn a_failed_preparation_keeps_the_trigger_message_edits() {
    for normalized in [true, false] {
        let (_directory, mut store, intent) = if normalized { effects_fixture(json!({})) }
            else { super::preparation_tests::stored(&super::preparation_tests::database(&json!({}))) };
        let appended = uuid::Uuid::new_v4().to_string();
        let (owner, plan) = trigger_plan(&mut store, &intent, &|budget| message_edits(budget, &appended));
        let read = stored_rows(&store);
        let (revision, receipts) = (store.revision().unwrap(), receipt_count(&store));
        let status = store.generation_fail_preparation_with_variables(&owner, PreparationError::ContextLimit, &plan).unwrap();
        assert_eq!((status.phase, status.preparation_error), (Phase::Failed, Some(PreparationError::ContextLimit)), "{normalized}");
        assert_eq!((store.revision().unwrap(), receipt_count(&store)), (revision + 1, receipts + 1), "{normalized}");
        assert_eq!(stored_rows(&store), edited_rows(&read, &appended), "{normalized}");
        assert_eq!(effects_conversation(&store)["note"], "Trigger note.");
        let record = store.generation_record(&owner.ticket).unwrap();
        assert_eq!(record.preparation.unwrap().edited_count, Some(i64::try_from(read.len()).unwrap() - 1), "{normalized}");
        let evidence = store.generation_effects_evidence(&owner.ticket).unwrap().unwrap();
        assert!(evidence.affected_unit_keys.iter().any(|key| key == MESSAGES_KEY), "{normalized}");
    }
}

// Message edits without a variable or note write are the same single write, with no unit mutation.
#[test]
fn message_edits_alone_commit_in_one_revision() {
    use risunest_ir::stage::{Effect, MessageEdit};
    let (_directory, mut store, intent) = effects_fixture(json!({}));
    let (owner, plan) = trigger_plan(&mut store, &intent, &|_| vec![Effect::Message(MessageEdit::Cut { start: 2.0, end: None })]);
    assert!(plan.assignments.is_empty() && plan.note.is_none() && plan.messages.is_some());
    let (read, before) = (stored_rows(&store), effects_conversation(&store));
    let lifecycle = uuid::Uuid::new_v4().to_string();
    store.generation_mark_ready_owned(&owner, &lifecycle).unwrap();
    let (revision, receipts) = (store.revision().unwrap(), receipt_count(&store));
    assert_eq!(store.generation_send_variables(&owner, &lifecycle, &plan).unwrap().phase, Phase::Running);
    assert_eq!((store.revision().unwrap(), receipt_count(&store)), (revision + 1, receipts + 1));
    let after = effects_conversation(&store);
    assert_eq!((&after["message"], &after["scriptstate"], &after["note"]), (&json!(read[2..]), &before["scriptstate"], &before["note"]));
    assert_eq!(store.generation_record(&owner.ticket).unwrap().preparation.unwrap().edited_count, Some(6));
    store.generation_send_memory(&owner, &lifecycle, None).unwrap();
    assert!(matches!(store.generation_claim_dispatch(&owner, &lifecycle).unwrap(), DispatchClaim::Issued { .. }));
}
