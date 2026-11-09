use super::access::{AccessPool, Error, Exposure, FinishMode, Limits as StoreLimits, OwnerPermit,
    ScriptStoreAccess, SegmentStatus};
use super::lua_scalar_adapter::{HandlerResult, Outcome, Permissions, Progress, ScalarAdapter};
use crate::persistent_store::{lww, script_access::Target, CheckpointMode, ConversationMutation, PersistentStore, StoreError, WorkingSetCommit};
use risunest_native_lua_experimental::{Cancellation, Limits, Scalar, Terminal, ValueLimits};
use serde_json::{json, Value};
use std::{cell::RefCell, panic::{catch_unwind, AssertUnwindSafe}, rc::Rc,
    sync::{Arc, atomic::{AtomicU64, Ordering}}, time::{Duration, Instant}};

fn replace(store: &mut PersistentStore) {
    let mut database: Value = serde_json::from_str(include_str!("../../fixtures/persistent-fixture.json")).unwrap();
    database["templateDefaultVariables"] = json!("default=template\nextra=available");
    database["characters"][0]["defaultVariables"] = json!("default=character\ndefault=second");
    database["characters"][0]["chats"][0]["scriptstate"] = json!({"$same":"stored", "$raw":[1,false]});
    database["characters"][0]["chats"][0]["future"] = json!({"kept":true});
    database["characters"][0]["chats"][0]["message"][0]["future"] = json!({"kept":true});
    let characters = database.as_object_mut().unwrap().remove("characters").unwrap();
    let presets = database.as_object_mut().unwrap().remove("botPresets").unwrap();
    let stage = store.replace_begin().unwrap();
    store.replace_put_root(&stage.staging_id, &database).unwrap();
    store.replace_put_presets(&stage.staging_id, presets.as_array().unwrap()).unwrap();
    store.replace_add_characters(&stage.staging_id, characters.as_array().unwrap()).unwrap();
    store.replace_commit(&stage.staging_id, Some(store.revision().unwrap())).unwrap();
}

fn fixture() -> (tempfile::TempDir, Rc<RefCell<PersistentStore>>) {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    replace(&mut store);
    (directory, Rc::new(RefCell::new(store)))
}

fn store_limits() -> StoreLimits {
    StoreLimits { active_scopes: 8, aggregate_bytes: 4_000_000, scope_bytes: 1_000_000,
        variable_bytes: 100_000, variable_count: 128, staged_bytes: 100_000, staged_rows: 128,
        read_page_rows: 2, read_page_bytes: 100_000, result_bytes: 200_000,
        waits: 8, receipts: 16, receipt_bytes: 100_000, commit_attempts: 3, lifetime: Duration::from_secs(3600) }
}

fn scope(store: &Rc<RefCell<PersistentStore>>, pool: &AccessPool, exposure: Exposure) -> (Rc<ScriptStoreAccess>, OwnerPermit) {
    let store = store.borrow();
    let target = Target { character_id: "char-b".into(), conversation_id: "conv-beta".into(),
        library_identity: store.library_identity().unwrap(), binding_authority: store.lww_binding_authority().unwrap() };
    let owner = OwnerPermit::new();
    (Rc::new(ScriptStoreAccess::open(&store, target, store.revision().unwrap(), owner.clone(), exposure, pool).unwrap()), owner)
}

fn limits() -> Limits { Limits { instructions_per_slice: 100_000, ..Limits::default() } }
fn adapter(source: &str) -> ScalarAdapter { ScalarAdapter::initialize(source, limits(), Duration::from_secs(5)).unwrap() }
fn conversation(store: &Rc<RefCell<PersistentStore>>) -> Value {
    store.borrow().read_conversation("char-b", "conv-beta", None).unwrap().unwrap().value
}

fn begin(adapter: &mut ScalarAdapter, access: &Rc<ScriptStoreAccess>, store: &Rc<RefCell<PersistentStore>>, name: &str) -> Progress {
    adapter.begin(name, access.clone(), store.clone(), Permissions::Safe, false, Cancellation::default()).unwrap()
}

fn run(adapter: &mut ScalarAdapter, mut progress: Progress) -> Outcome {
    for _ in 0..10_000 {
        match progress { Progress::Finished(outcome) => return outcome, Progress::Suspended => progress = adapter.step() }
    }
    panic!("Lua adapter exceeded scheduler bound");
}

fn call(adapter: &mut ScalarAdapter, access: &Rc<ScriptStoreAccess>, store: &Rc<RefCell<PersistentStore>>, name: &str) -> Outcome {
    let progress = begin(adapter, access, store, name);
    run(adapter, progress)
}

fn returned(outcome: Outcome, value: Scalar) {
    assert_eq!(outcome, Outcome { result: HandlerResult::Returned(value), aborted: false, cleanup_error: None });
}

fn commit(access: &ScriptStoreAccess, store: &Rc<RefCell<PersistentStore>>, mode: FinishMode) -> super::access::SegmentOutcome {
    access.commit(&mut store.borrow_mut(), &access.next_segment().unwrap(), mode).unwrap()
}

fn edit(store: &Rc<RefCell<PersistentStore>>, index: usize, data: &str) {
    let mut message = conversation(store)["message"][index].clone();
    message["data"] = json!(data);
    let mut store = store.borrow_mut();
    let expected_revision = store.revision().unwrap();
    store.commit(&WorkingSetCommit { expected_revision, conversations: Some(vec![ConversationMutation::ReplaceRange {
        character_id: "char-b".into(), conversation_id: "conv-beta".into(), start: index as i64,
        delete_count: 1, messages: vec![message], conversation: None, configured_index: None,
    }]), ..Default::default() }).unwrap();
}

#[test]
fn real_lua_variables_preserve_defaults_pending_values_and_return_arity() {
    let (_directory, store) = fixture();
    let pool = AccessPool::new(store_limits()).unwrap();
    let (access, _) = scope(&store, &pool, Exposure::Full);
    let mut adapter = adapter(r#"function run(id)
        assert(getChatVar(id,'missing')=='null' and getChatVar(id,'default')=='character')
        assert(getChatVar(id,'extra')=='available')
        assert(select('#',setChatVar(id,'empty',''))==0)
        assert(getChatVar(id,'empty')=='')
        assert(select('#',setChatVarChanged(id,'same','stored'))==0)
        assert(setChatVarChanged(id,'default','character')==true)
        assert(select('#',setChatVarChanged(id,'default','character'))==0)
        setChatVar(id,'unicode','한글 🦊'); assert(getChatVar(id,'unicode')=='한글 🦊')
        setChatVar(id,'score','first'); setChatVar(id,'score','last')
        return getChatVar(id,'score')
    end"#);
    returned(call(&mut adapter, &access, &store, "run"), Scalar::String("last".into()));
    assert!(conversation(&store)["scriptstate"].get("$score").is_none());
    assert!(matches!(commit(&access, &store, FinishMode::Messages).status, SegmentStatus::Committed(_)));
    let state = &conversation(&store)["scriptstate"];
    assert_eq!(state["$score"], "last"); assert_eq!(state["$empty"], "");
    assert_eq!(state["$default"], "character"); assert_eq!(state["$unicode"], "한글 🦊");
}

#[test]
fn real_lua_history_indexes_window_count_and_metadata_match_scalar_contract() {
    let (_directory, store) = fixture();
    let pool = AccessPool::new(store_limits()).unwrap();
    let (access, _) = scope(&store, &pool, Exposure::Window { start: 1, length: 2 });
    let mut adapter = adapter(r#"function run(id)
        assert(getChatLength(id)==3 and math.type(getChatLength(id))=='integer')
        assert(getChatData(id,0)=='' and getChatData(id,1.9)=='message-001')
        assert(getChatData(id,-1)=='message-002' and getChatRole(id,-2)=='char')
        assert(getChatData(id,0/0)=='' and getChatData(id,math.huge)=='')
        assert(getChatData(id,-math.huge)=='' and getChatData(id,9007199254740993)=='')
        setChat(id,-1,'new'); setChatRole(id,1,'anything')
        assert(getChatData(id,-1)=='new'); return true
    end"#);
    returned(call(&mut adapter, &access, &store, "run"), Scalar::Boolean(true));
    commit(&access, &store, FinishMode::Messages);
    let chat = conversation(&store);
    assert_eq!(chat["message"][2]["data"], "new"); assert_eq!(chat["message"][1]["role"], "char");
    assert_eq!(chat["future"], json!({"kept":true}));
    assert_eq!(chat["message"][0]["future"], json!({"kept":true}));
    assert_eq!(chat["message"][2]["chatId"], "msg-002");
}

#[test]
fn full_history_nan_negative_zero_and_nil_message_data_are_preserved() {
    let (_directory, store) = fixture();
    let pool = AccessPool::new(store_limits()).unwrap(); let (access, _) = scope(&store, &pool, Exposure::Full);
    let mut adapter = adapter("function run(id) assert(getChatData(id,0/0)=='message-000'); assert(getChatData(id,-0.0)=='message-000'); setChat(id,0,nil); setChatRole(id,0,'user'); return getChatData(id,0) end");
    returned(call(&mut adapter, &access, &store, "run"), Scalar::String(String::new()));
    commit(&access, &store, FinishMode::Messages);
    assert_eq!(conversation(&store)["message"][0]["data"], "");
    assert_eq!(conversation(&store)["message"][0]["future"], json!({"kept":true}));
}

#[test]
fn gsub_sort_and_metamethod_callbacks_execute_synchronously_against_the_store() {
    let (_directory, store) = fixture(); let pool = AccessPool::new(store_limits()).unwrap();
    let (access, _) = scope(&store, &pool, Exposure::Full);
    let mut adapter = adapter(r#"function run(id)
        setChatVar(id,'suffix','!')
        local result=string.gsub('a','a',function() return getChatData(id,0)..getChatVar(id,'suffix') end)
        local rows={2,0,1}; table.sort(rows,function(a,b) return getChatData(id,a)<getChatData(id,b) end)
        local proxy=setmetatable({}, {__index=function(_,key) setChatVar(id,'meta',key); return getChatRole(id,1) end})
        assert(proxy.seen=='char' and rows[1]==0 and result=='message-000!')
        return getChatVar(id,'meta')
    end"#);
    returned(call(&mut adapter, &access, &store, "run"), Scalar::String("seen".into()));
    assert!(conversation(&store)["scriptstate"].get("$meta").is_none());
    commit(&access, &store, FinishMode::Messages);
    assert_eq!(conversation(&store)["scriptstate"]["$meta"], "seen");
}

#[test]
fn explicit_owner_flush_does_not_publish_staged_messages_or_follow_scheduler_yields() {
    let (_directory, store) = fixture(); let pool = AccessPool::new(store_limits()).unwrap();
    let (access, _) = scope(&store, &pool, Exposure::Full);
    let mut adapter = adapter("function run(id) setChatVar(id,'score','first'); setChat(id,0,'staged'); coroutine.yield(); setChatVar(id,'score','second'); return getChatData(id,0) end");
    begin(&mut adapter, &access, &store, "run"); assert_eq!(adapter.step(), Progress::Suspended);
    assert!(conversation(&store)["scriptstate"].get("$score").is_none());
    assert_eq!(conversation(&store)["message"][0]["data"], "message-000");
    let outcome = commit(&access, &store, FinishMode::Variables);
    assert!(!outcome.messages_changed); assert_eq!(conversation(&store)["scriptstate"]["$score"], "first");
    assert_eq!(conversation(&store)["message"][0]["data"], "message-000");
    returned(run(&mut adapter, Progress::Suspended), Scalar::String("staged".into()));
    commit(&access, &store, FinishMode::Messages);
    assert_eq!(conversation(&store)["scriptstate"]["$score"], "second");
    assert_eq!(conversation(&store)["message"][0]["data"], "staged");
}

#[test]
fn live_read_sees_external_change_and_caught_stale_edit_keeps_variable_prefix() {
    let (_directory, store) = fixture(); let pool = AccessPool::new(store_limits()).unwrap();
    let (access, _) = scope(&store, &pool, Exposure::Full);
    let mut adapter = adapter("function run(id) assert(getChatData(id,0)=='message-000'); setChatVar(id,'prefix','kept'); coroutine.yield(); assert(getChatData(id,0)=='external'); assert(not pcall(setChat,id,0,'lost')); return true end");
    begin(&mut adapter, &access, &store, "run"); assert_eq!(adapter.step(), Progress::Suspended);
    edit(&store, 0, "external");
    returned(run(&mut adapter, Progress::Suspended), Scalar::Boolean(true));
    let outcome = commit(&access, &store, FinishMode::Messages);
    assert_eq!(outcome.error, Some(Error::StaleMessages));
    assert_eq!(conversation(&store)["message"][0]["data"], "external");
    assert_eq!(conversation(&store)["scriptstate"]["$prefix"], "kept");
}

#[test]
fn staged_reads_and_error_finishing_preserve_owner_policy() {
    for mode in [FinishMode::Messages, FinishMode::WithoutMessages] {
        let (_directory, store) = fixture(); let pool = AccessPool::new(store_limits()).unwrap();
        let (access, _) = scope(&store, &pool, Exposure::Full);
        let mut adapter = adapter("function run(id) setChatVar(id,'prefix','kept'); setChat(id,0,'staged'); coroutine.yield(); assert(getChatData(id,0)=='staged'); error('ordinary') end");
        begin(&mut adapter, &access, &store, "run"); assert_eq!(adapter.step(), Progress::Suspended);
        assert!(matches!(run(&mut adapter, Progress::Suspended).result, HandlerResult::Failed(error) if error.contains("ordinary")));
        commit(&access, &store, mode);
        assert_eq!(conversation(&store)["scriptstate"]["$prefix"], "kept");
        assert_eq!(conversation(&store)["message"][0]["data"], if mode == FinishMode::Messages { "staged" } else { "message-000" });
    }
}

#[test]
fn ordinary_abort_preserves_later_effects_globals_and_next_invocation() {
    let (_directory, store) = fixture(); let pool = AccessPool::new(store_limits()).unwrap();
    let (access, _) = scope(&store, &pool, Exposure::Full);
    let mut adapter = adapter("count=0; function run(id) count=count+1; setChatVar(id,'before','yes'); coroutine.yield(); setChatVar(id,'after','yes'); return count end; function read() return count end");
    assert!(adapter.begin("run", access.clone(), store.clone(), Permissions::Safe, true, Cancellation::default()).is_err());
    returned(call(&mut adapter, &access, &store, "read"), Scalar::Number(0.0));
    begin(&mut adapter, &access, &store, "run"); assert_eq!(adapter.step(), Progress::Suspended);
    adapter.abort_caller().unwrap();
    let outcome = run(&mut adapter, Progress::Suspended);
    assert_eq!(outcome.result, HandlerResult::Returned(Scalar::Number(1.0))); assert!(outcome.aborted);
    assert_eq!(outcome.cleanup_error, None);
    returned(call(&mut adapter, &access, &store, "read"), Scalar::Number(1.0));
    commit(&access, &store, FinishMode::Messages);
    assert_eq!(conversation(&store)["scriptstate"]["$before"], "yes");
    assert_eq!(conversation(&store)["scriptstate"]["$after"], "yes");
}

#[test]
fn emergency_retirement_clears_invocation_without_rolling_back_published_prefix() {
    let (_directory, store) = fixture(); let pool = AccessPool::new(store_limits()).unwrap();
    let (access, _) = scope(&store, &pool, Exposure::Full);
    let mut adapter = adapter("function run(id) setChatVar(id,'prefix','kept'); coroutine.yield(); setChatVar(id,'after','lost') end");
    let emergency = Cancellation::default();
    adapter.begin("run", access.clone(), store.clone(), Permissions::Safe, false, emergency.clone()).unwrap();
    assert_eq!(adapter.step(), Progress::Suspended); commit(&access, &store, FinishMode::Variables);
    emergency.cancel();
    assert_eq!(adapter.step(), Progress::Finished(Outcome { result: HandlerResult::Terminal(Terminal::Cancelled), aborted: false, cleanup_error: None }));
    assert!(adapter.begin("run", access.clone(), store.clone(), Permissions::Safe, false, Cancellation::default()).is_err());
    let permit = access.begin_invocation(false).unwrap(); access.end_invocation(&permit).unwrap();
    assert_eq!(conversation(&store)["scriptstate"]["$prefix"], "kept");
    assert!(conversation(&store)["scriptstate"].get("$after").is_none());
}

#[test]
fn stale_access_ids_and_display_permissions_cannot_publish_message_writes() {
    let (_directory, store) = fixture(); let pool = AccessPool::new(store_limits()).unwrap();
    let (access, _) = scope(&store, &pool, Exposure::Full);
    let mut adapter = adapter("function remember(id) old=id; setChatVar(id,'savedid',id) end; function run(id) assert(select('#',setChatVar(old,'old','lost'))==0); setChat(id,0,'lost'); setChatRole(id,0,'char'); setChatVar(id,'display','kept'); return getChatData(id,0) end");
    returned(call(&mut adapter, &access, &store, "remember"), Scalar::Nil);
    let start = adapter.begin("run", access.clone(), store.clone(), Permissions::EditDisplay, false, Cancellation::default()).unwrap();
    returned(run(&mut adapter, start), Scalar::String("message-000".into()));
    commit(&access, &store, FinishMode::Messages);
    assert_eq!(conversation(&store)["scriptstate"]["$display"], "kept");
    assert!(conversation(&store)["scriptstate"].get("$old").is_none());
    assert_eq!(conversation(&store)["message"][0]["data"], "message-000");
    drop(adapter);
    let mut other = self::adapter("function run(id) local old=getChatVar(id,'savedid'); assert(old~=id); setChatVar(old,'foreign','lost'); return true end");
    returned(call(&mut other, &access, &store, "run"), Scalar::Boolean(true));
    commit(&access, &store, FinishMode::Messages);
    assert!(conversation(&store)["scriptstate"].get("$foreign").is_none());
}

#[test]
fn every_ordinary_exit_releases_permit_and_unsupported_host_does_not_replay() {
    let (_directory, store) = fixture(); let pool = AccessPool::new(store_limits()).unwrap();
    let (access, _) = scope(&store, &pool, Exposure::Full);
    let mut adapter = adapter("n=0; bad=7; function fail(id) n=n+1; setChatVar(id,'prefix','kept'); error('fail') end; function forbidden(id) n=n+1; setChatVar(id,'prefix','kept'); hostAwait('sleep',1); setChatVar(id,'after','lost') end; function object() return {} end; function read() return n end");
    assert_eq!(call(&mut adapter, &access, &store, "missing").result, HandlerResult::Missing);
    for name in ["bad", "fail", "forbidden", "object", "\0"] {
        let outcome = call(&mut adapter, &access, &store, name);
        assert!(matches!(outcome.result, HandlerResult::Failed(_))); assert_eq!(outcome.cleanup_error, None);
        let permit = access.begin_invocation(false).unwrap(); access.end_invocation(&permit).unwrap();
    }
    returned(call(&mut adapter, &access, &store, "read"), Scalar::Number(2.0));
    commit(&access, &store, FinishMode::Messages);
    assert_eq!(conversation(&store)["scriptstate"]["$prefix"], "kept");
    assert!(conversation(&store)["scriptstate"].get("$after").is_none());
}

#[test]
fn initialization_host_access_fails_and_source_is_not_rerun_per_handler() {
    assert!(ScalarAdapter::initialize("getChatVar('id','key')", limits(), Duration::from_secs(5)).is_err());
    let (_directory, store) = fixture(); let pool = AccessPool::new(store_limits()).unwrap();
    let (access, _) = scope(&store, &pool, Exposure::Full);
    for suffix in ["", "return function() end", "return {nested=function() end}"] {
        let mut adapter = adapter(&format!("count=(count or 0)+1; function run() count=count+1; return count end; {suffix}"));
        returned(call(&mut adapter, &access, &store, "run"), Scalar::Number(2.0));
        returned(call(&mut adapter, &access, &store, "run"), Scalar::Number(3.0));
    }
}

#[test]
fn invocation_exhaustion_rejects_before_creating_a_permit_or_replaying_effects() {
    let (_directory, store) = fixture(); let pool = AccessPool::new(store_limits()).unwrap();
    let (access, _) = scope(&store, &pool, Exposure::Full);
    let mut adapter = ScalarAdapter::with_invocation_limit(
        "n=0; function run(id) n=n+1; setChatVar(id,'count',tostring(n)); return n end",
        limits(), Duration::from_secs(5), 2).unwrap();
    returned(call(&mut adapter, &access, &store, "run"), Scalar::Number(1.0));
    returned(call(&mut adapter, &access, &store, "run"), Scalar::Number(2.0));
    assert!(matches!(adapter.begin("run", access.clone(), store.clone(), Permissions::Safe, false, Cancellation::default()),
        Err(error) if error == "Lua invocation sequence exhausted"));
    let permit = access.begin_invocation(false).unwrap(); access.end_invocation(&permit).unwrap();
    commit(&access, &store, FinishMode::Messages);
    assert_eq!(conversation(&store)["scriptstate"]["$count"], "2");
}

#[test]
fn unsupported_scalar_values_and_tiny_limits_preserve_prior_store_effects() {
    let (_directory, store) = fixture(); let pool = AccessPool::new(store_limits()).unwrap();
    let (access, _) = scope(&store, &pool, Exposure::Full);
    let settings = Limits { values: ValueLimits { utf8_bytes: 32, nodes: 4, ..ValueLimits::default() }, ..limits() };
    let mut adapter = ScalarAdapter::initialize(r#"function run(id)
        setChatVar(id,'prefix','kept')
        assert(not pcall(setChatVar,id,'nil',nil))
        assert(not pcall(setChatVar,id,'missing'))
        assert(not pcall(setChatVar,id,'bad',string.char(255)))
        assert(not pcall(setChatVar,id,'nul',string.char(0)))
        assert(not pcall(setChatVar,id,'large',string.rep('x',33)))
        assert(not pcall(getChatVar,id,'raw'))
        return true
    end"#, settings, Duration::from_secs(5)).unwrap();
    returned(call(&mut adapter, &access, &store, "run"), Scalar::Boolean(true));
    commit(&access, &store, FinishMode::Messages);
    assert_eq!(conversation(&store)["scriptstate"], json!({"$same":"stored", "$raw":[1,false], "$prefix":"kept"}));
}

#[test]
fn owner_revocation_and_library_replacement_do_not_retarget_lua_effects() {
    for replacement in [false, true] {
        let (_directory, store) = fixture(); let pool = AccessPool::new(store_limits()).unwrap();
        let (access, owner) = scope(&store, &pool, Exposure::Full);
        let mut adapter = adapter("function run(id) setChatVar(id,'prefix','pending'); coroutine.yield(); pcall(getChatData,id,0); setChatVar(id,'after','pending') end");
        begin(&mut adapter, &access, &store, "run"); assert_eq!(adapter.step(), Progress::Suspended);
        if replacement { replace(&mut store.borrow_mut()); } else { owner.revoke(); }
        let _ = run(&mut adapter, Progress::Suspended);
        let result = commit(&access, &store, FinishMode::Messages);
        assert_eq!(result.status, SegmentStatus::NotCommitted);
        assert!(conversation(&store)["scriptstate"].get("$prefix").is_none());
        assert!(conversation(&store)["scriptstate"].get("$after").is_none());
    }
}

#[test]
fn dropped_adapter_ends_permit_and_finalizer_cannot_borrow_old_binding() {
    let (_directory, store) = fixture(); let pool = AccessPool::new(store_limits()).unwrap();
    let (access, _) = scope(&store, &pool, Exposure::Full);
    let mut adapter = adapter("function run(id) retained=setmetatable({}, {__gc=function() setChatVar(id,'finalizer','lost') end}); setChatVar(id,'prefix','kept'); setChat(id,0,'staged'); coroutine.yield() end");
    begin(&mut adapter, &access, &store, "run"); assert_eq!(adapter.step(), Progress::Suspended);
    drop(adapter);
    let permit = access.begin_invocation(false).unwrap(); access.end_invocation(&permit).unwrap();
    commit(&access, &store, FinishMode::WithoutMessages);
    assert_eq!(conversation(&store)["scriptstate"]["$prefix"], "kept");
    assert!(conversation(&store)["scriptstate"].get("$finalizer").is_none());
    store.borrow().checkpoint(CheckpointMode::Truncate).unwrap();
}

#[test]
fn expired_scope_releases_real_lease_while_lua_is_suspended() {
    let (_directory, store) = fixture(); let clock = Arc::new(AtomicU64::new(0)); let sample = clock.clone();
    let mut settings = store_limits(); settings.lifetime = Duration::from_millis(5);
    let pool = AccessPool::with_clock(settings, Arc::new(move || Duration::from_millis(sample.load(Ordering::SeqCst)))).unwrap();
    let (access, _) = scope(&store, &pool, Exposure::Full);
    let mut adapter = adapter("function run(id) setChat(id,0,'staged'); coroutine.yield(); return getChatData(id,0) end");
    begin(&mut adapter, &access, &store, "run"); assert_eq!(adapter.step(), Progress::Suspended);
    match store.borrow().checkpoint(CheckpointMode::Truncate) {
        Err(StoreError::Store { message }) => assert_eq!(message, "truncate checkpoint cannot run while an active read lease pins the WAL"),
        result => panic!("expected pinned WAL checkpoint rejection: {result:?}"),
    }
    adapter.abort_caller().unwrap();
    clock.store(5, Ordering::SeqCst); pool.expire();
    store.borrow().checkpoint(CheckpointMode::Truncate).unwrap();
    let outcome = run(&mut adapter, Progress::Suspended);
    assert!(matches!(outcome.result, HandlerResult::Failed(_)));
    assert!(outcome.aborted);
    assert_eq!(outcome.cleanup_error, Some(Error::InvocationInvalid));
    assert_eq!(conversation(&store)["message"][0]["data"], "message-000");
    drop(adapter); drop(access);
    let deadline = Instant::now() + Duration::from_secs(5);
    while pool.used_bytes() != 0 { assert!(Instant::now() < deadline); std::thread::yield_now(); }
}

#[test]
fn lost_flush_response_recovers_original_store_without_replaying_lua() {
    let (_directory, store) = fixture(); let (_foreign_directory, foreign) = fixture();
    let pool = AccessPool::new(store_limits()).unwrap(); let (access, owner) = scope(&store, &pool, Exposure::Full);
    let mut adapter = adapter("count=0; function run(id) count=count+1; setChatVar(id,'issued',tostring(count)); coroutine.yield(); return count end");
    begin(&mut adapter, &access, &store, "run"); assert_eq!(adapter.step(), Progress::Suspended);
    let revision = store.borrow().revision().unwrap();
    lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("synthetic response loss"))));
    let segment = access.next_segment().unwrap();
    assert!(catch_unwind(AssertUnwindSafe(|| access.flush_variables(&mut store.borrow_mut(), &segment))).is_err());
    owner.revoke();
    assert_eq!(access.recover_segment(&mut foreign.borrow_mut(), &segment).unwrap().status,
        SegmentStatus::Unresolved(crate::persistent_store::script_access::RecoveryIssue::StoreMismatch));
    let outcome = access.recover_segment(&mut store.borrow_mut(), &segment).unwrap();
    assert_eq!(outcome.status, SegmentStatus::Committed(revision + 1));
    assert_eq!(access.recover_segment(&mut store.borrow_mut(), &segment).unwrap(), outcome);
    returned(run(&mut adapter, Progress::Suspended), Scalar::Number(1.0));
    assert_eq!(conversation(&store)["scriptstate"]["$issued"], "1");
    assert!(conversation(&foreign)["scriptstate"].get("$issued").is_none());
    assert_eq!(store.borrow().revision().unwrap(), revision + 1);
}
