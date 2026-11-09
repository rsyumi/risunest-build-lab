use super::*;
use crate::native_script::access::{AccessPool, Error, Exposure, FinishMode, InvocationPermit, Limits,
    OwnerPermit, ScriptStoreAccess, SegmentStatus};
use crate::persistent_store::{lww, CheckpointMode};
use serde_json::json;
use std::{panic::{catch_unwind, AssertUnwindSafe}, path::Path, process::Command,
    sync::{atomic::{AtomicU64, Ordering}, mpsc}, thread, time::Duration};

fn replace_fixture(store: &mut PersistentStore) {
    let mut database: Value = serde_json::from_str(include_str!("../../fixtures/persistent-fixture.json")).unwrap();
    database["templateDefaultVariables"] = json!("shared=template\ntemplate=available");
    database["characters"][0]["defaultVariables"] = json!("shared=character\nshared=second\nnullish=fallback\n spaced = exact \nextra=first=ignored\nempty=");
    database["characters"][0]["chats"][0]["scriptstate"] = json!({"$nullish": null, "$raw": [1, false], "$same": "stored"});
    database["characters"][0]["chats"][0]["futureMetadata"] = json!({"keep": [1, 2]});
    database["characters"][0]["chats"][0]["message"][0]["futureMessage"] = json!({"keep": true});
    let characters = database.as_object_mut().unwrap().remove("characters").unwrap();
    let presets = database.as_object_mut().unwrap().remove("botPresets").unwrap();
    let stage = store.replace_begin().unwrap();
    store.replace_put_root(&stage.staging_id, &database).unwrap();
    store.replace_put_presets(&stage.staging_id, presets.as_array().unwrap()).unwrap();
    store.replace_add_characters(&stage.staging_id, characters.as_array().unwrap()).unwrap();
    store.replace_commit(&stage.staging_id, Some(store.revision().unwrap())).unwrap();
}

fn fixture() -> (tempfile::TempDir, PersistentStore) {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    replace_fixture(&mut store);
    (directory, store)
}

fn limits() -> Limits {
    Limits { active_scopes: 8, aggregate_bytes: 4_000_000, scope_bytes: 1_000_000,
        variable_bytes: 100_000, variable_count: 128, staged_bytes: 100_000, staged_rows: 128,
        read_page_rows: 2, read_page_bytes: 100_000, result_bytes: 200_000,
        waits: 8, receipts: 16, receipt_bytes: 100_000, commit_attempts: 3,
        lifetime: Duration::from_secs(3600) }
}

fn target(store: &PersistentStore) -> Target {
    Target { character_id: "char-b".into(), conversation_id: "conv-beta".into(),
        library_identity: store.library_identity().unwrap(), binding_authority: store.lww_binding_authority().unwrap() }
}

fn open(store: &PersistentStore, pool: &AccessPool, exposure: Exposure) -> (ScriptStoreAccess, OwnerPermit, InvocationPermit) {
    let owner = OwnerPermit::new();
    let access = ScriptStoreAccess::open(store, target(store), store.revision().unwrap(), owner.clone(), exposure, pool).unwrap();
    let invocation = access.begin_invocation(false).unwrap();
    (access, owner, invocation)
}

fn conversation(store: &PersistentStore) -> Value {
    store.read_conversation("char-b", "conv-beta", None).unwrap().unwrap().value
}

fn set_field(store: &mut PersistentStore, components: &[&str], value: Value) {
    store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(),
        unit_mutations: Some(vec![lww::UnitMutation::Set {
            key: risunest_sync_wire::unit::UnitKey::new(components).unwrap(), value,
        }]), ..Default::default() }).unwrap();
}

fn edit_message(store: &mut PersistentStore, index: usize, data: &str) {
    let mut message = conversation(store)["message"][index].clone();
    message["data"] = json!(data);
    store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(),
        conversations: Some(vec![ConversationMutation::ReplaceRange {
            character_id: "char-b".into(), conversation_id: "conv-beta".into(), start: index as i64,
            delete_count: 1, messages: vec![message], conversation: None, configured_index: None,
        }]), ..Default::default() }).unwrap();
}

fn assert_error<T>(result: std::result::Result<T, Error>, expected: Error) {
    match result { Err(error) => assert_eq!(error, expected), Ok(_) => panic!("expected {expected:?}") }
}

fn wait_for_empty_pool(pool: &AccessPool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while pool.used_bytes() != 0 {
        assert!(std::time::Instant::now() < deadline, "scope buffers were not released");
        thread::yield_now();
    }
}

fn finish(access: &ScriptStoreAccess, store: &mut PersistentStore, mode: FinishMode) -> crate::native_script::access::SegmentOutcome {
    access.commit(store, &access.next_segment().unwrap(), mode).unwrap()
}

#[test]
fn variables_are_live_then_overlaid_and_default_values_do_not_suppress_writes() {
    let (_directory, mut store) = fixture();
    let pool = AccessPool::new(limits()).unwrap();
    let (access, _, invocation) = open(&store, &pool, Exposure::Full);
    for (key, expected) in [("shared", "character"), ("template", "available"), ("nullish", "fallback"),
        ("missing", "null"), (" spaced ", " exact "), ("extra", "first"), ("empty", "null")] {
        assert_eq!(access.read_variable(&store, &invocation, key).unwrap().value(), &json!(expected));
    }
    assert_eq!(access.read_variable(&store, &invocation, "raw").unwrap().value(), &json!([1, false]));
    assert!(!access.set_variable(&store, &invocation, "same".into(), "stored".into()).unwrap());
    assert!(access.set_variable(&store, &invocation, "shared".into(), "character".into()).unwrap());
    assert!(access.set_variable(&store, &invocation, "second".into(), "one".into()).unwrap());
    assert!(access.set_variable(&store, &invocation, "shared".into(), "last".into()).unwrap());
    assert!(!access.set_variable(&store, &invocation, "shared".into(), "last".into()).unwrap());
    assert_eq!(access.read_variable(&store, &invocation, "shared").unwrap().value(), &json!("last"));
    assert!(conversation(&store)["scriptstate"].get("$shared").is_none());
    set_field(&mut store, &["conversation", "char-b", "conv-beta", "scriptstate"], json!({"$external": 7, "$same": "new"}));
    assert_eq!(access.read_variable(&store, &invocation, "same").unwrap().value(), &json!("new"));
    let result = finish(&access, &mut store, FinishMode::Variables);
    assert_eq!(result.changed_keys, ["shared", "second"]);
    assert!(matches!(result.status, SegmentStatus::Committed(_)));
    assert_eq!(conversation(&store)["scriptstate"], json!({"$external": 7, "$same": "new", "$shared": "last", "$second": "one"}));
    let revision = store.revision().unwrap();
    assert_eq!(finish(&access, &mut store, FinishMode::Variables).status, SegmentStatus::NoChange);
    assert_eq!(store.revision().unwrap(), revision);
}

#[test]
fn explicit_variable_flush_keeps_messages_staged_and_waits_do_not_flush() {
    let (_directory, mut store) = fixture();
    let pool = AccessPool::new(limits()).unwrap();
    let (access, _, invocation) = open(&store, &pool, Exposure::Full);
    let before = conversation(&store);
    access.set_message_data(&store, &invocation, 0.0, "staged".into()).unwrap();
    access.set_message_role(&store, &invocation, 0.0, "system").unwrap();
    access.set_variable(&store, &invocation, "score".into(), "1".into()).unwrap();
    let wait = access.begin_host_wait(&invocation).unwrap();
    let mut nested = store.open_native_job_store().unwrap();
    set_field(&mut nested, &["conversation", "char-b", "conv-beta", "futureMetadata"], json!({"newer": true}));
    assert_eq!(conversation(&store)["message"], before["message"]);
    assert!(conversation(&store)["scriptstate"].get("$score").is_none());
    assert_eq!(access.accept_host_result(&wait).unwrap(), invocation);
    let flushed = finish(&access, &mut store, FinishMode::Variables);
    assert_eq!(flushed.changed_keys, ["score"]);
    assert!(!flushed.messages_changed);
    assert_eq!(conversation(&store)["message"], before["message"]);
    assert_eq!(access.read_message(&store, &invocation, 0.0).unwrap().value().as_ref().unwrap()["data"], "staged");
    assert_eq!(store.lease_diagnostics().active_count, 1);
    let completed = finish(&access, &mut store, FinishMode::Messages);
    assert!(completed.messages_changed);
    let after = conversation(&store);
    assert_eq!(after["message"][0]["data"], "staged");
    assert_eq!(after["message"][0]["role"], "char");
    assert_eq!(after["message"][0]["futureMessage"], before["message"][0]["futureMessage"]);
    assert_eq!(after["message"][0]["chatId"], before["message"][0]["chatId"]);
    assert_eq!(after["futureMetadata"], json!({"newer": true}));
    assert_eq!(after["scriptstate"]["$score"], "1");
    assert_eq!(store.lease_diagnostics().active_count, 0);
}

#[test]
fn live_reads_change_but_caught_stale_first_edit_keeps_variable_prefix() {
    let (_directory, mut store) = fixture();
    let pool = AccessPool::new(limits()).unwrap();
    let (access, _, invocation) = open(&store, &pool, Exposure::Full);
    let initial = access.read_message(&store, &invocation, 0.0).unwrap().value().clone();
    let mut peer = store.open_native_job_store().unwrap();
    edit_message(&mut peer, 0, "external");
    assert_ne!(access.read_message(&store, &invocation, 0.0).unwrap().value(), &initial);
    access.set_variable(&store, &invocation, "prefix".into(), "kept".into()).unwrap();
    assert_error(access.set_message_data(&store, &invocation, 0.0, "stale".into()), Error::StaleMessages);
    let result = finish(&access, &mut store, FinishMode::Messages);
    assert_eq!(result.error, Some(Error::StaleMessages));
    assert!(matches!(result.status, SegmentStatus::Committed(_)));
    assert_eq!(result.changed_keys, ["prefix"]);
    assert_eq!(conversation(&store)["message"][0]["data"], "external");
    assert_eq!(conversation(&store)["scriptstate"]["$prefix"], "kept");
    assert_eq!(store.lease_diagnostics().active_count, 0);
}

#[test]
fn staged_snapshot_rejects_aba_message_changes_and_preserves_per_key_variables() {
    for restore in [false, true] {
        let (_directory, mut store) = fixture();
        let pool = AccessPool::new(limits()).unwrap();
        let (access, _, invocation) = open(&store, &pool, Exposure::Full);
        let original = conversation(&store)["message"][0]["data"].as_str().unwrap().to_owned();
        access.set_message_data(&store, &invocation, 0.0, "owned".into()).unwrap();
        access.set_variable(&store, &invocation, "ours".into(), "yes".into()).unwrap();
        let mut peer = store.open_native_job_store().unwrap();
        edit_message(&mut peer, 0, "other");
        if restore { edit_message(&mut peer, 0, &original); }
        set_field(&mut peer, &["conversation", "char-b", "conv-beta", "scriptstate"], json!({"$other": "survives"}));
        assert_eq!(access.read_message(&store, &invocation, 0.0).unwrap().value().as_ref().unwrap()["data"], "owned");
        let before = conversation(&store)["message"].clone();
        let result = finish(&access, &mut store, FinishMode::Messages);
        assert_eq!(result.error, Some(Error::StaleMessages));
        assert!(matches!(result.status, SegmentStatus::Committed(_)));
        assert!(!result.messages_changed);
        assert_eq!(conversation(&store)["message"], before);
        assert_eq!(conversation(&store)["scriptstate"], json!({"$other": "survives", "$ours": "yes"}));
    }
}

#[test]
fn owner_finish_policy_decides_whether_handler_error_keeps_messages() {
    for mode in [FinishMode::Messages, FinishMode::WithoutMessages] {
        let (_directory, mut store) = fixture();
        let pool = AccessPool::new(limits()).unwrap();
        let (access, _, invocation) = open(&store, &pool, Exposure::Full);
        let before = conversation(&store)["message"].clone();
        access.set_message_data(&store, &invocation, 0.0, "before handler error".into()).unwrap();
        access.set_variable(&store, &invocation, "prefix".into(), "present".into()).unwrap();
        access.end_invocation(&invocation).unwrap();
        let result = finish(&access, &mut store, mode);
        assert!(result.error.is_none());
        assert_eq!(conversation(&store)["scriptstate"]["$prefix"], "present");
        if mode == FinishMode::Messages { assert_eq!(conversation(&store)["message"][0]["data"], "before handler error"); }
        else { assert_eq!(conversation(&store)["message"], before); }
    }
}

#[test]
fn window_reads_use_absolute_positive_and_tail_negative_indexes_without_truncating_history() {
    let (_directory, mut store) = fixture();
    let pool = AccessPool::new(limits()).unwrap();
    let (access, _, invocation) = open(&store, &pool, Exposure::Window { start: 1, length: 2 });
    assert_eq!(access.history_length(&store, &invocation).unwrap(), 3);
    assert!(access.read_message(&store, &invocation, 0.0).unwrap().value().is_none());
    assert!(access.read_message(&store, &invocation, f64::NAN).unwrap().value().is_none());
    assert_eq!(access.read_message(&store, &invocation, -1.8).unwrap().value().as_ref().unwrap(), &conversation(&store)["message"][2]);
    access.set_message_data(&store, &invocation, 1.8, "window".into()).unwrap();
    access.set_message_data(&store, &invocation, 0.0, "hidden".into()).unwrap();
    let rows = access.read_messages(&store, &invocation, 1, 2).unwrap();
    assert_eq!(rows.value()[0]["data"], "window");
    assert_eq!(rows.value().len(), 2);
    assert_error(access.read_messages(&store, &invocation, 0, 3), Error::Limit("history-range"));
    finish(&access, &mut store, FinishMode::Messages);
    assert_ne!(conversation(&store)["message"][0]["data"], "hidden");
}

#[test]
fn waits_are_invocation_bound_and_ordinary_abort_preserves_later_effects() {
    let (_directory, mut store) = fixture();
    let pool = AccessPool::new(limits()).unwrap();
    let (access, _, invocation) = open(&store, &pool, Exposure::Full);
    access.set_variable(&store, &invocation, "before".into(), "yes".into()).unwrap();
    let first = access.begin_host_wait(&invocation).unwrap();
    let second = access.begin_host_wait(&invocation).unwrap();
    access.abort_invocation(&invocation).unwrap();
    assert_eq!(access.accept_host_result(&second).unwrap(), invocation);
    access.set_variable(&store, &invocation, "after".into(), "yes".into()).unwrap();
    assert_error(access.accept_host_result(&second), Error::WaitInvalid);
    assert!(access.end_invocation(&invocation).unwrap());
    assert_error(access.begin_invocation(true), Error::InvocationInvalid);
    let later = access.begin_invocation(false).unwrap();
    assert_error(access.accept_host_result(&first), Error::InvocationInvalid);
    assert_error(access.set_variable(&store, &invocation, "old".into(), "no".into()), Error::InvocationInvalid);
    assert_eq!(access.read_variable(&store, &later, "after").unwrap().value(), &json!("yes"));
    finish(&access, &mut store, FinishMode::Variables);
    assert_eq!(conversation(&store)["scriptstate"]["$before"], "yes");
    assert_eq!(conversation(&store)["scriptstate"]["$after"], "yes");
    let late = access.begin_host_wait(&later).unwrap();
    access.release();
    assert_error(access.accept_host_result(&late), Error::Closed);
}

#[test]
fn revoked_owner_prevents_reservation_but_empty_if_changed_can_finish() {
    let (_directory, mut store) = fixture();
    let pool = AccessPool::new(limits()).unwrap();
    let (access, owner, invocation) = open(&store, &pool, Exposure::Full);
    access.set_variable(&store, &invocation, "never".into(), "published".into()).unwrap();
    let before = store.revision().unwrap();
    let issued_before = store.lww_clock_state().unwrap().issued;
    let intents_before = store.device_store().unwrap().connection().query_row("SELECT count(*) FROM lww_intents", [], |row| row.get::<_, i64>(0)).unwrap();
    owner.revoke();
    let result = finish(&access, &mut store, FinishMode::Messages);
    assert_eq!(result.status, SegmentStatus::NotCommitted);
    assert_eq!(result.error, Some(Error::OwnerInvalid));
    assert_eq!(store.revision().unwrap(), before);
    assert_eq!(store.lww_clock_state().unwrap().issued, issued_before);
    assert!(conversation(&store)["scriptstate"].get("$never").is_none());
    assert_eq!(store.device_store().unwrap().connection().query_row("SELECT count(*) FROM lww_intents", [], |row| row.get::<_, i64>(0)).unwrap(), intents_before);
    assert_eq!(finish(&access, &mut store, FinishMode::IfChanged).status, SegmentStatus::NoChange);
    assert_eq!(finish(&access, &mut store, FinishMode::Messages).error, Some(Error::OwnerInvalid));
}

#[test]
fn replacement_never_redirects_old_effects_into_the_new_library() {
    let (_directory, mut store) = fixture();
    let pool = AccessPool::new(limits()).unwrap();
    let (access, _, invocation) = open(&store, &pool, Exposure::Full);
    access.set_variable(&store, &invocation, "old".into(), "effect".into()).unwrap();
    access.set_message_data(&store, &invocation, 0.0, "old message".into()).unwrap();
    replace_fixture(&mut store);
    let before = conversation(&store);
    let result = finish(&access, &mut store, FinishMode::Messages);
    assert_eq!(result.status, SegmentStatus::NotCommitted);
    assert_eq!(result.error, Some(Error::StaleTarget));
    assert_eq!(conversation(&store), before);
    assert_eq!(store.lease_diagnostics().active_count, 0);
}

#[test]
fn exact_segment_retries_do_not_consume_later_writes_and_pruned_ids_expire() {
    let (_directory, mut store) = fixture();
    let mut config = limits(); config.receipts = 1;
    let pool = AccessPool::new(config).unwrap();
    let (access, _, invocation) = open(&store, &pool, Exposure::Full);
    access.set_variable(&store, &invocation, "value".into(), "first".into()).unwrap();
    let id = access.next_segment().unwrap();
    let first = access.flush_variables(&mut store, &id).unwrap();
    access.set_variable(&store, &invocation, "value".into(), "next".into()).unwrap();
    assert_eq!(access.flush_variables(&mut store, &id).unwrap(), first);
    assert_eq!(conversation(&store)["scriptstate"]["$value"], "first");
    assert_error(access.commit(&mut store, &id, FinishMode::Messages), Error::SegmentIntegrity);
    finish(&access, &mut store, FinishMode::Variables);
    assert_error(access.flush_variables(&mut store, &id), Error::SegmentExpired);
    assert_eq!(conversation(&store)["scriptstate"]["$value"], "next");
}

fn interrupt_after_reservation(access: &ScriptStoreAccess, store: &mut PersistentStore, settle: bool) -> crate::native_script::access::SegmentId {
    let mut observer = store.open_native_job_store().unwrap();
    lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("synthetic response loss"))));
    let revision = store.revision().unwrap();
    let id = access.next_segment().unwrap();
    let failure = catch_unwind(AssertUnwindSafe(|| access.flush_variables(store, &id))).unwrap_err();
    let message = failure.downcast_ref::<&str>().copied()
        .or_else(|| failure.downcast_ref::<String>().map(String::as_str));
    assert_eq!(message, Some("synthetic response loss"));
    let (request, digest): (String, String) = store.device_store().unwrap().connection()
        .query_row("SELECT request_id,digest FROM lww_intents WHERE complete=0", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    let receipt = |store: &PersistentStore| store.connection.query_row(
        "SELECT digest,revision FROM lww_requests WHERE request_id=?1", [&request],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))).optional().unwrap();
    assert_eq!(store.revision().unwrap(), revision);
    assert_eq!(receipt(store), None);
    if settle {
        observer.lww_recover_intents().unwrap();
        assert_eq!(receipt(store), Some((digest, revision + 1)));
        assert_eq!(store.revision().unwrap(), revision + 1);
    }
    assert_error(access.next_segment(), Error::PendingCommit);
    id
}

#[test]
fn lost_response_recovers_exact_attempt_after_revocation_and_release() {
    for settle in [false, true] {
        let (_directory, mut store) = fixture();
        let pool = AccessPool::new(limits()).unwrap();
        let (access, owner, invocation) = open(&store, &pool, Exposure::Full);
        let revision = store.revision().unwrap();
        access.set_variable(&store, &invocation, "issued".into(), "once".into()).unwrap();
        let id = interrupt_after_reservation(&access, &mut store, settle);
        assert_eq!(store.revision().unwrap(), revision + if settle { 1 } else { 0 });
        if settle {
            assert_eq!(conversation(&store)["scriptstate"]["$issued"], "once");
        } else {
            assert!(conversation(&store)["scriptstate"].get("$issued").is_none());
        }
        assert_error(access.next_segment(), Error::PendingCommit);
        owner.revoke();
        access.release();
        let result = access.recover_segment(&mut store, &id).unwrap();
        assert_eq!(result.status, SegmentStatus::Committed(revision + 1));
        assert_eq!(result.changed_keys, ["issued"]);
        assert_eq!(access.recover_segment(&mut store, &id).unwrap(), result);
        assert_eq!(store.revision().unwrap(), revision + 1);
        assert_eq!(conversation(&store)["scriptstate"]["$issued"], "once");
        assert_error(access.begin_invocation(false), Error::Closed);
    }
}

#[test]
fn dropped_scope_transfers_unresolved_attempt_to_bounded_pool_recovery() {
    let (_directory, mut store) = fixture();
    let pool = AccessPool::new(limits()).unwrap();
    let (access, owner, invocation) = open(&store, &pool, Exposure::Full);
    access.set_variable(&store, &invocation, "orphan".into(), "retained".into()).unwrap();
    interrupt_after_reservation(&access, &mut store, false);
    owner.revoke();
    drop(access);
    assert!(pool.used_bytes() > 0);
    let recovered = pool.recover_pending(&mut store);
    assert_eq!(recovered.len(), 1);
    assert!(matches!(recovered[0].as_ref().unwrap().status, SegmentStatus::Committed(_)));
    assert_eq!(conversation(&store)["scriptstate"]["$orphan"], "retained");
    assert!(pool.recover_pending(&mut store).is_empty());
    wait_for_empty_pool(&pool);
}

#[test]
fn admitted_write_linearizes_before_revocation_without_holding_gate_across_wait() {
    let (_directory, store) = fixture();
    let mut writer = store.open_native_job_store().unwrap();
    let pool = AccessPool::new(limits()).unwrap();
    let (access, owner, invocation) = open(&writer, &pool, Exposure::Full);
    access.set_variable(&writer, &invocation, "admitted".into(), "yes".into()).unwrap();
    let (reserved_tx, reserved_rx) = mpsc::channel();
    let (proceed_tx, proceed_rx) = mpsc::channel();
    let write = thread::spawn(move || {
        lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            reserved_tx.send(()).unwrap();
            proceed_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        })));
        let result = finish(&access, &mut writer, FinishMode::Variables);
        (access, writer, result)
    });
    reserved_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let revoke = thread::spawn(move || { started_tx.send(()).unwrap(); owner.revoke(); });
    started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    proceed_tx.send(()).unwrap();
    let (access, mut writer, result) = write.join().unwrap();
    revoke.join().unwrap();
    assert!(matches!(result.status, SegmentStatus::Committed(_)));
    assert_eq!(conversation(&writer)["scriptstate"]["$admitted"], "yes");
    assert_eq!(finish(&access, &mut writer, FinishMode::Messages).error, Some(Error::OwnerInvalid));
}

#[test]
fn precommit_revision_conflict_replans_effects_without_replaying_evaluation() {
    let (_directory, mut store) = fixture();
    let pool = AccessPool::new(limits()).unwrap();
    let (access, _, invocation) = open(&store, &pool, Exposure::Full);
    access.set_variable(&store, &invocation, "evaluated".into(), "once".into()).unwrap();
    let mut concurrent = store.open_native_job_store().unwrap();
    lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
        let revision = concurrent.revision().unwrap();
        crate::persistent_store::commit::incremental_commit(&mut concurrent.connection, revision, |_, _| Ok(()), |_, _, _| Ok(())).unwrap();
    })));
    let revision = store.revision().unwrap();
    let result = finish(&access, &mut store, FinishMode::Variables);
    assert_eq!(result.status, SegmentStatus::Committed(revision + 2));
    assert_eq!(result.changed_keys, ["evaluated"]);
    assert_eq!(conversation(&store)["scriptstate"]["$evaluated"], "once");
}

#[test]
fn stale_message_fallback_retries_a_competing_revision_and_keeps_primary_error() {
    let (_directory, mut store) = fixture();
    let pool = AccessPool::new(limits()).unwrap();
    let (access, _, invocation) = open(&store, &pool, Exposure::Full);
    access.set_message_data(&store, &invocation, 0.0, "stale edit".into()).unwrap();
    access.set_variable(&store, &invocation, "prefix".into(), "retained".into()).unwrap();
    edit_message(&mut store, 0, "newer message");
    let mut peer = store.open_native_job_store().unwrap();
    BEFORE_SCRIPT_SNAPSHOT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
        BEFORE_SCRIPT_SNAPSHOT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            set_field(&mut peer, &["conversation", "char-b", "conv-beta", "scriptstate"], json!({"$concurrent": "kept"}));
        })));
    })));
    let result = finish(&access, &mut store, FinishMode::Messages);
    assert_eq!(result.error, Some(Error::StaleMessages));
    assert!(matches!(result.status, SegmentStatus::Committed(_)));
    assert_eq!(result.changed_keys, ["prefix"]);
    assert_eq!(conversation(&store)["message"][0]["data"], "newer message");
    assert_eq!(conversation(&store)["scriptstate"], json!({"$prefix": "retained", "$concurrent": "kept"}));
}

#[test]
fn foreign_store_cannot_clear_pending_attempt_in_scope_or_pool_recovery() {
    for orphan in [false, true] {
        let (_directory_a, mut a) = fixture();
        let (_directory_b, mut b) = fixture();
        let pool = AccessPool::new(limits()).unwrap();
        let (access, owner, invocation) = open(&a, &pool, Exposure::Full);
        access.set_variable(&a, &invocation, "original".into(), "only".into()).unwrap();
        let id = interrupt_after_reservation(&access, &mut a, false);
        let b_before = conversation(&b);
        let revision_a = a.revision().unwrap();
        owner.revoke();
        if orphan {
            drop(access);
            let wrong = pool.recover_pending(&mut b);
            assert_eq!(wrong[0].as_ref().unwrap().status, SegmentStatus::Unresolved(RecoveryIssue::StoreMismatch));
            let recovered = pool.recover_pending(&mut a);
            assert_eq!(recovered[0].as_ref().unwrap().status, SegmentStatus::Committed(revision_a + 1));
            assert!(pool.recover_pending(&mut a).is_empty());
        } else {
            assert_eq!(access.recover_segment(&mut b, &id).unwrap().status, SegmentStatus::Unresolved(RecoveryIssue::StoreMismatch));
            assert_error(access.next_segment(), Error::PendingCommit);
            let recovered = access.recover_segment(&mut a, &id).unwrap();
            assert_eq!(recovered.status, SegmentStatus::Committed(revision_a + 1));
            assert_eq!(access.recover_segment(&mut a, &id).unwrap(), recovered);
        }
        assert_eq!(conversation(&b), b_before);
        assert_eq!(conversation(&a)["scriptstate"]["$original"], "only");
        assert_eq!(a.revision().unwrap(), revision_a + 1);
    }
}

#[test]
fn original_store_receipt_still_resolves_after_library_generation_replacement() {
    let (_directory, mut store) = fixture();
    let pool = AccessPool::new(limits()).unwrap();
    let (access, owner, invocation) = open(&store, &pool, Exposure::Full);
    access.set_variable(&store, &invocation, "original".into(), "old generation".into()).unwrap();
    let revision = store.revision().unwrap();
    let id = interrupt_after_reservation(&access, &mut store, true);
    owner.revoke();
    replace_fixture(&mut store);
    let current = conversation(&store);
    let current_revision = store.revision().unwrap();
    assert_eq!(access.recover_segment(&mut store, &id).unwrap().status, SegmentStatus::Committed(revision + 1));
    assert_eq!(conversation(&store), current);
    assert_eq!(store.revision().unwrap(), current_revision);
}

#[test]
fn tight_completion_budget_returns_known_outcome_and_expires_uncached_segment() {
    let (_directory, mut store) = fixture();
    let probe = AccessPool::new(limits()).unwrap();
    let (access, _, _) = open(&store, &probe, Exposure::Full);
    let baseline = probe.used_bytes();
    drop(access);
    for aggregate in [false, true] {
        let mut config = limits();
        if aggregate { config.aggregate_bytes = baseline + 160; } else { config.scope_bytes = baseline + 160; }
        let pool = AccessPool::new(config).unwrap();
        let (access, _, _) = open(&store, &pool, Exposure::Full);
        let id = access.next_segment().unwrap();
        let revision = store.revision().unwrap();
        assert_eq!(access.flush_variables(&mut store, &id).unwrap().status, SegmentStatus::NoChange);
        assert_eq!(store.revision().unwrap(), revision);
        assert!(pool.used_bytes() <= baseline + 160);
        assert_error(access.flush_variables(&mut store, &id), Error::SegmentExpired);
        drop(access);
        wait_for_empty_pool(&pool);
    }
}

#[test]
fn failed_and_quarantined_recovery_keep_frozen_attempt_and_block_new_effects() {
    let (_directory, mut store) = fixture();
    let pool = AccessPool::new(limits()).unwrap();
    let (access, owner, invocation) = open(&store, &pool, Exposure::Full);
    access.set_variable(&store, &invocation, "frozen".into(), "value".into()).unwrap();
    let id = interrupt_after_reservation(&access, &mut store, false);
    let (request, digest): (String, String) = store.device_store().unwrap().connection()
        .query_row("SELECT request_id,digest FROM lww_intents WHERE complete=0", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    store.device_store().unwrap().connection().execute("UPDATE lww_intents SET digest='corrupt' WHERE request_id=?1", [&request]).unwrap();
    assert_eq!(access.recover_segment(&mut store, &id).unwrap().status, SegmentStatus::Unresolved(RecoveryIssue::Failed));
    assert_error(access.set_variable(&store, &invocation, "later".into(), "blocked".into()), Error::PendingCommit);
    assert_error(access.next_segment(), Error::PendingCommit);
    access.recover_segment(&mut store, &id).unwrap();
    assert_eq!(access.recover_segment(&mut store, &id).unwrap().status, SegmentStatus::Unresolved(RecoveryIssue::Quarantined));
    assert!(conversation(&store)["scriptstate"].get("$frozen").is_none());
    owner.revoke(); access.release();
    store.device_store().unwrap().connection().execute("UPDATE lww_intents SET digest=?2 WHERE request_id=?1", rusqlite::params![request, digest]).unwrap();
    assert_eq!(access.recover_segment(&mut store, &id).unwrap().status, SegmentStatus::Unresolved(RecoveryIssue::Quarantined));
    store.device_store().unwrap().connection().execute("DELETE FROM lww_intent_failures WHERE request_id=?1", [&request]).unwrap();
    assert!(matches!(access.recover_segment(&mut store, &id).unwrap().status, SegmentStatus::Committed(_)));
    assert_eq!(conversation(&store)["scriptstate"]["$frozen"], "value");
}

#[test]
fn corrupt_receipt_and_changed_binding_cannot_acknowledge_or_reissue_attempt() {
    for receipt in [true, false] {
        let (_directory, mut store) = fixture();
        let pool = AccessPool::new(limits()).unwrap();
        let (access, owner, invocation) = open(&store, &pool, Exposure::Full);
        access.set_variable(&store, &invocation, "frozen".into(), "value".into()).unwrap();
        let id = interrupt_after_reservation(&access, &mut store, false);
        let (request, digest): (String, String) = store.device_store().unwrap().connection()
            .query_row("SELECT request_id,digest FROM lww_intents WHERE complete=0", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        let authority = store.lww_binding_authority().unwrap();
        if receipt {
            store.lww_recover_intents().unwrap();
            store.connection.execute("UPDATE lww_requests SET digest='corrupt' WHERE request_id=?1", [&request]).unwrap();
        } else { store.device_store().unwrap().connection().execute("UPDATE lww_clock SET binding_authority='999'", []).unwrap(); }
        let before = store.revision().unwrap();
        let result = access.recover_segment(&mut store, &id).unwrap();
        assert_eq!(result.status, SegmentStatus::Unresolved(if receipt { RecoveryIssue::Integrity } else { RecoveryIssue::Failed }));
        assert_eq!(store.revision().unwrap(), before);
        assert_error(access.next_segment(), Error::PendingCommit);
        owner.revoke(); access.release();
        if receipt {
            store.connection.execute("UPDATE lww_requests SET digest=?2 WHERE request_id=?1", rusqlite::params![request, digest]).unwrap();
        } else { store.device_store().unwrap().connection().execute("UPDATE lww_clock SET binding_authority=?1", [authority.0.to_string()]).unwrap(); }
        assert!(matches!(access.recover_segment(&mut store, &id).unwrap().status, SegmentStatus::Committed(_)));
        assert_eq!(conversation(&store)["scriptstate"]["$frozen"], "value");
    }
}

#[test]
fn proven_unreserved_attempt_needs_fresh_owner_admission() {
    let (_directory, mut store) = fixture();
    let pool = AccessPool::new(limits()).unwrap();
    let (access, owner, invocation) = open(&store, &pool, Exposure::Full);
    access.set_variable(&store, &invocation, "replan".into(), "forbidden".into()).unwrap();
    let id = interrupt_after_reservation(&access, &mut store, false);
    store.device_store().unwrap().connection().execute("DELETE FROM lww_intents WHERE complete=0", []).unwrap();
    owner.revoke();
    let before = store.revision().unwrap();
    let result = access.recover_segment(&mut store, &id).unwrap();
    assert_eq!(result.status, SegmentStatus::NotCommitted);
    assert_eq!(result.error, Some(Error::OwnerInvalid));
    assert_eq!(store.revision().unwrap(), before);
    assert!(conversation(&store)["scriptstate"].get("$replan").is_none());
}

fn restart_root(test: &str) -> Option<std::path::PathBuf> {
    (std::env::var("RISUNEST_SCRIPT_RESTART_TEST").ok().as_deref() == Some(test))
        .then(|| std::env::var_os("RISUNEST_SCRIPT_RESTART_ROOT").expect("synthetic child store path").into())
}

fn run_in_restarted_process(test: &str, root: &Path) {
    let module = module_path!().split_once("::").unwrap().1;
    let status = Command::new(std::env::current_exe().unwrap()).args(["--exact", &format!("{module}::{test}")])
        .env("RISUNEST_SCRIPT_RESTART_TEST", test).env("RISUNEST_SCRIPT_RESTART_ROOT", root)
        .status().expect("start isolated Rust test process");
    assert!(status.success());
}

#[test]
fn restart_recovers_existing_intent_without_script_or_service_replay() {
    const TEST: &str = "restart_recovers_existing_intent_without_script_or_service_replay";
    if let Some(root) = restart_root(TEST) {
        let store = PersistentStore::open(&root).unwrap();
        assert_eq!(conversation(&store)["scriptstate"]["$restart"], "persisted");
        return;
    }
    let (directory, mut store) = fixture();
    let pool = AccessPool::new(limits()).unwrap();
    let (access, _, invocation) = open(&store, &pool, Exposure::Full);
    access.set_variable(&store, &invocation, "restart".into(), "persisted".into()).unwrap();
    let revision = store.revision().unwrap();
    interrupt_after_reservation(&access, &mut store, false);
    access.release();
    drop(access); drop(pool); drop(store);
    run_in_restarted_process(TEST, directory.path());
    let store = PersistentStore::open(directory.path()).unwrap();
    assert_eq!(store.revision().unwrap(), revision + 1);
    assert_eq!(conversation(&store)["scriptstate"]["$restart"], "persisted");
}

#[test]
fn expiration_releases_staged_lease_during_dormant_wait() {
    let (_directory, store) = fixture();
    let clock = Arc::new(AtomicU64::new(0));
    let sampled = clock.clone();
    let mut config = limits(); config.lifetime = Duration::from_millis(5);
    let pool = AccessPool::with_clock(config, Arc::new(move || Duration::from_millis(sampled.load(Ordering::SeqCst)))).unwrap();
    let (access, _, invocation) = open(&store, &pool, Exposure::Full);
    access.set_message_data(&store, &invocation, 0.0, "pending".into()).unwrap();
    let wait = access.begin_host_wait(&invocation).unwrap();
    assert_eq!(store.lease_diagnostics().active_count, 1);
    clock.store(5, Ordering::SeqCst);
    pool.expire();
    assert_eq!(store.lease_diagnostics().active_count, 0);
    assert_error(access.accept_host_result(&wait), Error::Expired);
    assert_error(access.read_message(&store, &invocation, 0.0), Error::Expired);
    drop(access);
    wait_for_empty_pool(&pool);
}

#[test]
fn limits_bound_variables_staging_waits_pages_results_and_scope_admission() {
    let (_directory, mut store) = fixture();
    let mut config = limits(); config.variable_count = 1; config.staged_rows = 1; config.waits = 1; config.active_scopes = 1;
    let pool = AccessPool::new(config).unwrap();
    let (access, _, invocation) = open(&store, &pool, Exposure::Full);
    access.set_variable(&store, &invocation, "one".into(), "ok".into()).unwrap();
    assert_error(access.set_variable(&store, &invocation, "two".into(), "no".into()), Error::Limit("variables"));
    assert_error(access.set_variable(&store, &invocation, "one".into(), "x".repeat(100_001)), Error::Limit("variables"));
    access.set_message_data(&store, &invocation, 0.0, "one".into()).unwrap();
    assert_error(access.set_message_data(&store, &invocation, 1.0, "two".into()), Error::Limit("staged-messages"));
    assert_error(access.set_message_data(&store, &invocation, 0.0, "x".repeat(100_001)), Error::Limit("staged-messages"));
    access.begin_host_wait(&invocation).unwrap();
    assert_error(access.begin_host_wait(&invocation), Error::Limit("waits"));
    assert_error(ScriptStoreAccess::open(&store, target(&store), store.revision().unwrap(), OwnerPermit::new(), Exposure::Full, &pool), Error::Limit("active-scopes"));
    assert_eq!(access.read_variable(&store, &invocation, "one").unwrap().value(), &json!("ok"));
    for page_limit in [true, false] {
        let mut config = limits();
        if page_limit { config.read_page_bytes = 1; } else { config.result_bytes = 1; }
        let pool = AccessPool::new(config).unwrap();
        let (bounded, _, invocation) = open(&store, &pool, Exposure::Full);
        assert_error(bounded.read_messages(&store, &invocation, 0, 3), Error::Limit(if page_limit { "read-page-bytes" } else { "result-bytes" }));
    }
    assert_eq!(finish(&access, &mut store, FinishMode::Variables).changed_keys, ["one"]);
    assert_eq!(conversation(&store)["scriptstate"]["$one"], "ok");
}

#[test]
fn retained_results_share_scope_and_aggregate_byte_budgets() {
    let (_directory, store) = fixture();
    for aggregate in [false, true] {
        let probe = AccessPool::new(limits()).unwrap();
        let (access, _, invocation) = open(&store, &probe, Exposure::Full);
        let result = access.read_messages(&store, &invocation, 0, 3).unwrap();
        let exact = probe.used_bytes();
        drop(result); drop(access);
        let mut config = limits();
        if aggregate { config.aggregate_bytes = exact; } else { config.scope_bytes = exact; }
        let pool = AccessPool::new(config).unwrap();
        let (access, _, invocation) = open(&store, &pool, Exposure::Full);
        let retained = access.read_messages(&store, &invocation, 0, 3).unwrap();
        assert_error(access.read_messages(&store, &invocation, 0, 3), Error::Limit(if aggregate { "aggregate-bytes" } else { "scope-bytes" }));
        drop(retained);
        assert_eq!(access.read_messages(&store, &invocation, 0, 3).unwrap().value().len(), 3);
        drop(access);
        wait_for_empty_pool(&pool);
    }
}

#[test]
fn same_view_history_selection_preserves_window_overlay_and_live_refresh() {
    use crate::native_script::access::{HistoryRows, HistorySelection, NumberArgument};
    let (_directory, mut store) = fixture(); let pool = AccessPool::new(limits()).unwrap();
    let (access, _, invocation) = open(&store, &pool, Exposure::Window { start: 1, length: 2 });
    let list = |result: &crate::native_script::access::ReadResult<HistoryRows>| match result.value() {
        HistoryRows::Many(rows) => rows.iter().map(|row| row["data"].clone()).collect::<Vec<_>>(), _ => panic!("expected list"),
    };
    assert!(matches!(access.read_history(&store, &invocation, HistorySelection::One(NumberArgument::Value(0.0))).unwrap().value(), HistoryRows::One(None)));
    let first = access.read_history(&store, &invocation, HistorySelection::FullExposed).unwrap();
    assert_eq!(list(&first), conversation(&store)["message"].as_array().unwrap()[1..3].iter().map(|row| row["data"].clone()).collect::<Vec<_>>());
    drop(first);
    edit_message(&mut store, 2, "live-tail");
    let recent = access.read_history(&store, &invocation, HistorySelection::Recent(NumberArgument::Value(1.0))).unwrap();
    assert_eq!(list(&recent), [json!("live-tail")]); drop(recent);
    assert_error(access.set_message_data(&store, &invocation, 1.0, "stale".into()), Error::StaleMessages);
    drop(access);
    let (access, _, invocation) = open(&store, &pool, Exposure::Window { start: 1, length: 2 });
    access.set_message_data(&store, &invocation, 2.0, "staged-tail".into()).unwrap();
    assert_eq!(list(&access.read_history(&store, &invocation, HistorySelection::Recent(NumberArgument::Value(1.0))).unwrap()), [json!("staged-tail")]);
    assert_eq!(conversation(&store)["message"][2]["data"], "live-tail");
    assert_eq!(finish(&access, &mut store, FinishMode::Messages).error, None);
    assert_eq!(conversation(&store)["message"][2]["data"], "staged-tail");
}

#[test]
fn deferred_history_coercion_records_fence_and_owner_failure_takes_precedence() {
    use crate::native_script::access::{HistorySelection, NumberArgument};
    let (_directory, mut store) = fixture(); let pool = AccessPool::new(limits()).unwrap();
    let (access, owner, invocation) = open(&store, &pool, Exposure::Full);
    access.set_variable(&store, &invocation, "prefix".into(), "kept".into()).unwrap();
    assert_error(access.read_history(&store, &invocation, HistorySelection::Recent(NumberArgument::Invalid)), Error::HistoryCoercion);
    edit_message(&mut store, 0, "external");
    assert_error(access.set_message_data(&store, &invocation, 0.0, "lost".into()), Error::StaleMessages);
    assert_eq!(finish(&access, &mut store, FinishMode::Messages).error, Some(Error::StaleMessages));
    assert_eq!(conversation(&store)["scriptstate"]["$prefix"], "kept");
    assert_eq!(conversation(&store)["message"][0]["data"], "external");
    owner.revoke();
    assert_error(access.read_history(&store, &invocation, HistorySelection::Recent(NumberArgument::Invalid)), Error::OwnerInvalid);
}

#[test]
fn history_rows_and_incremental_encoded_text_remain_separately_charged_until_drop() {
    use crate::native_script::access::HistorySelection;
    let (_directory, store) = fixture(); let pool = AccessPool::new(limits()).unwrap();
    let (access, _, invocation) = open(&store, &pool, Exposure::Full);
    let rows = access.read_history(&store, &invocation, HistorySelection::FullExposed).unwrap();
    let row_bytes = pool.used_bytes();
    let mut text = rows.text_writer(4).unwrap(); text.push("ab").unwrap();
    assert!(pool.used_bytes() > row_bytes);
    assert_error(text.push("cde"), Error::Limit("encoded-history-bytes"));
    assert_eq!(text.as_ref(), "ab");
    text.push("cd").unwrap(); assert_eq!(text.as_ref(), "abcd");
    drop(rows); assert_eq!(text.as_ref(), "abcd");
    store.checkpoint(CheckpointMode::Truncate).unwrap();
    drop(text); drop(access); wait_for_empty_pool(&pool);
}

#[test]
fn full_history_result_limit_returns_no_prefix_and_preserves_pending_variables() {
    use crate::native_script::access::HistorySelection;
    let (_directory, mut store) = fixture(); let mut settings = limits(); settings.result_bytes = 64;
    let pool = AccessPool::new(settings).unwrap(); let (access, _, invocation) = open(&store, &pool, Exposure::Full);
    let before = conversation(&store)["message"].clone();
    access.set_variable(&store, &invocation, "prefix".into(), "kept".into()).unwrap();
    assert_error(access.read_history(&store, &invocation, HistorySelection::FullExposed), Error::Limit("result-bytes"));
    store.checkpoint(CheckpointMode::Truncate).unwrap();
    assert_eq!(finish(&access, &mut store, FinishMode::Messages).error, None);
    assert_eq!(conversation(&store)["scriptstate"]["$prefix"], "kept");
    assert_eq!(conversation(&store)["message"], before);
}
