use super::access::{AccessPool, Error, Exposure, FinishMode, Limits as StoreLimits, OwnerPermit, ScriptStoreAccess};
use super::lua_scalar_adapter::{HandlerResult, Outcome, Permissions, Progress, ScalarAdapter};
use crate::persistent_store::{script_access::Target, CheckpointMode, ConversationMutation, PersistentStore, StoreError, WorkingSetCommit};
use risunest_native_lua_experimental::{Cancellation, Limits, Scalar};
use serde_json::{json, Value};
use std::{cell::RefCell, rc::Rc, time::{Duration, Instant}};

fn corpus() -> Value { serde_json::from_str(include_str!("../../../tests/fixtures/lua/native-values-cases.json")).unwrap() }
fn settings() -> StoreLimits {
    StoreLimits { active_scopes: 4, aggregate_bytes: 4_000_000, scope_bytes: 1_000_000,
        variable_bytes: 100_000, variable_count: 128, staged_bytes: 100_000, staged_rows: 128,
        read_page_rows: 1, read_page_bytes: 100_000, result_bytes: 200_000,
        waits: 8, receipts: 16, receipt_bytes: 100_000, commit_attempts: 3, lifetime: Duration::from_secs(3600) }
}
fn fixture(rows: Value) -> (tempfile::TempDir, Rc<RefCell<PersistentStore>>) {
    let directory = tempfile::tempdir().unwrap(); let mut store = PersistentStore::open(directory.path()).unwrap();
    let mut database: Value = serde_json::from_str(include_str!("../../fixtures/persistent-fixture.json")).unwrap();
    database["characters"][0]["chats"][0]["message"] = rows;
    database["characters"][0]["chats"][0]["scriptstate"] = json!({});
    let characters = database.as_object_mut().unwrap().remove("characters").unwrap();
    let presets = database.as_object_mut().unwrap().remove("botPresets").unwrap();
    let stage = store.replace_begin().unwrap();
    store.replace_put_root(&stage.staging_id, &database).unwrap();
    store.replace_put_presets(&stage.staging_id, presets.as_array().unwrap()).unwrap();
    store.replace_add_characters(&stage.staging_id, characters.as_array().unwrap()).unwrap();
    store.replace_commit(&stage.staging_id, Some(store.revision().unwrap())).unwrap();
    (directory, Rc::new(RefCell::new(store)))
}
fn scope(store: &Rc<RefCell<PersistentStore>>, pool: &AccessPool, exposure: Exposure) -> Rc<ScriptStoreAccess> {
    let store = store.borrow();
    let target = Target { character_id: "char-b".into(), conversation_id: "conv-beta".into(),
        library_identity: store.library_identity().unwrap(), binding_authority: store.lww_binding_authority().unwrap() };
    Rc::new(ScriptStoreAccess::open(&store, target, store.revision().unwrap(), OwnerPermit::new(), exposure, pool).unwrap())
}
fn limits() -> Limits { Limits { instructions_per_slice: 100_000, ..Limits::default() } }
fn adapter(source: &str) -> ScalarAdapter { ScalarAdapter::initialize_values(source, limits(), Duration::from_secs(5)).unwrap() }
fn begin(adapter: &mut ScalarAdapter, access: &Rc<ScriptStoreAccess>, store: &Rc<RefCell<PersistentStore>>) -> Progress {
    adapter.begin("run", access.clone(), store.clone(), Permissions::Safe, false, Cancellation::default()).unwrap()
}
fn finish(adapter: &mut ScalarAdapter, mut progress: Progress) -> Outcome {
    for _ in 0..100_000 { match progress { Progress::Finished(outcome) => return outcome, Progress::Suspended => progress = adapter.step() } }
    panic!("values test exceeded scheduler bound");
}
fn call(adapter: &mut ScalarAdapter, access: &Rc<ScriptStoreAccess>, store: &Rc<RefCell<PersistentStore>>) -> Outcome {
    let progress = begin(adapter, access, store); finish(adapter, progress)
}
fn conversation(store: &Rc<RefCell<PersistentStore>>) -> Value {
    store.borrow().read_conversation("char-b", "conv-beta", None).unwrap().unwrap().value
}
fn edit(store: &Rc<RefCell<PersistentStore>>, data: &str) {
    let mut store = store.borrow_mut(); let expected_revision = store.revision().unwrap();
    store.commit(&WorkingSetCommit { expected_revision, conversations: Some(vec![ConversationMutation::ReplaceRange {
        character_id: "char-b".into(), conversation_id: "conv-beta".into(), start: 0, delete_count: 1,
        messages: vec![json!({"role":"user","data":data})], conversation: None, configured_index: None,
    }]), ..Default::default() }).unwrap();
}
fn commit(access: &ScriptStoreAccess, store: &Rc<RefCell<PersistentStore>>) -> super::access::SegmentOutcome {
    access.commit(&mut store.borrow_mut(), &access.next_segment().unwrap(), FinishMode::Messages).unwrap()
}

#[test]
fn shared_product_history_and_state_cases_observe_real_stored_values() {
    let corpus = corpus();
    for case in corpus["cases"].as_array().unwrap() {
        let (_directory, store) = fixture(corpus["rows"].clone()); let pool = AccessPool::new(settings()).unwrap();
        let access = scope(&store, &pool, Exposure::Full); let before = conversation(&store)["message"].clone(); let mut adapter = adapter(case["source"].as_str().unwrap());
        let result = call(&mut adapter, &access, &store);
        assert_eq!(result.result, HandlerResult::Returned(match &case["expected"] { Value::String(value) => Scalar::String(value.clone()), Value::Bool(value) => Scalar::Boolean(*value), _ => panic!("unexpected corpus result") }), "{}", case["name"]);
        assert_eq!(result.cleanup_error, None);
        assert_eq!(commit(&access, &store).error, None);
        let stored = conversation(&store);
        for (key, value) in case["variables"].as_object().into_iter().flatten() { assert_eq!(&stored["scriptstate"][format!("${key}")], value, "{}", case["name"]); }
        for key in case["absentVariables"].as_array().into_iter().flatten() { assert!(stored["scriptstate"].get(format!("${}", key.as_str().unwrap())).is_none()); }
        assert_eq!(stored["message"], before);
    }
}

#[test]
fn shared_recent_count_matrix_and_exposed_absolute_selection() {
    let corpus = corpus(); let (_directory, store) = fixture(corpus["rows"].clone()); let pool = AccessPool::new(settings()).unwrap();
    for case in corpus["recent"].as_array().unwrap() {
        let access = scope(&store, &pool, Exposure::Full);
        let argument = case["argument"].as_str().unwrap();
        let call_text = if argument.is_empty() { "getRecentChats(id)".into() } else { format!("getRecentChats(id,{argument})") };
        let mut adapter = adapter(&format!("function run(id) return #{call_text} end"));
        assert_eq!(call(&mut adapter, &access, &store).result, HandlerResult::Returned(Scalar::Number(case["expected"].as_f64().unwrap())), "{}", case["name"]);
    }
    let access = scope(&store, &pool, Exposure::Window { start: 1, length: 2 });
    let mut adapter = adapter("function run(id) assert(getChat(id,0)==nil); assert(getChat(id,-1).role==false); assert(#getRecentChats(id,math.huge)==2); assert(#getFullChat(id)==2); return getChat(id,' 1 ').data.z end");
    assert_eq!(call(&mut adapter, &access, &store).result, HandlerResult::Returned(Scalar::Number(1.0)));
}

#[test]
fn coercion_error_records_read_fence_and_keeps_earlier_state_write() {
    let (_directory, store) = fixture(corpus()["rows"].clone()); let pool = AccessPool::new(settings()).unwrap();
    let access = scope(&store, &pool, Exposure::Full);
    let mut adapter = adapter("function run(id) setState(id,'before',true); assert(not pcall(getRecentChats,id,{toString=false})); coroutine.yield(); assert(not pcall(setChat,id,0,'lost')); return getChat(id,0).data end");
    begin(&mut adapter, &access, &store); assert_eq!(adapter.step(), Progress::Suspended);
    edit(&store, "external");
    assert_eq!(finish(&mut adapter, Progress::Suspended).result, HandlerResult::Returned(Scalar::String("external".into())));
    assert_eq!(commit(&access, &store).error, Some(Error::StaleMessages));
    assert_eq!(conversation(&store)["scriptstate"]["$__before"], "true");
    assert_eq!(conversation(&store)["message"][0]["data"], "external");
}

#[test]
fn staged_selection_keeps_lease_and_reads_overlay_until_owner_finishes() {
    let (_directory, store) = fixture(corpus()["rows"].clone()); let pool = AccessPool::new(settings()).unwrap();
    let access = scope(&store, &pool, Exposure::Full);
    let mut adapter = adapter("function run(id) setChat(id,0,'staged'); assert(getChat(id,0).data=='staged'); assert(getFullChat(id)[1].data=='staged'); coroutine.yield(); return getRecentChats(id,4)[1].data end");
    begin(&mut adapter, &access, &store); assert_eq!(adapter.step(), Progress::Suspended);
    match store.borrow().checkpoint(CheckpointMode::Truncate) {
        Err(StoreError::Store { message }) => assert_eq!(message, "truncate checkpoint cannot run while an active read lease pins the WAL"),
        result => panic!("expected staged read lease: {result:?}"),
    }
    assert_eq!(conversation(&store)["message"][0]["data"], "first");
    assert_eq!(finish(&mut adapter, Progress::Suspended).result, HandlerResult::Returned(Scalar::String("staged".into())));
    assert_eq!(commit(&access, &store).error, None);
    assert_eq!(conversation(&store)["message"][0]["data"], "staged");
    store.borrow().checkpoint(CheckpointMode::Truncate).unwrap();
}

#[test]
fn conversion_reentry_and_full_argument_clone_errors_preserve_real_prefix() {
    let (_directory, store) = fixture(corpus()["rows"].clone()); let pool = AccessPool::new(settings()).unwrap();
    let access = scope(&store, &pool, Exposure::Full);
    let mut adapter = adapter(r#"function run(id)
        local n=0
        local key=setmetatable({}, {__tostring=function() n=n+1; setState(id,'passes',n); return 'x' end})
        assert(#getRecentChats(id,{[key]=1})==4 and n==2)
        assert(not pcall(getFullChatMain,function() end))
        assert(not pcall(getChatMain,id,0,function() end))
        assert(#getRecentChats(id,{__proto__=function() end})==4)
        return getState(id,'passes')
    end"#);
    assert_eq!(call(&mut adapter, &access, &store).result, HandlerResult::Returned(Scalar::Number(2.0)));
    assert_eq!(commit(&access, &store).error, None);
    assert_eq!(conversation(&store)["scriptstate"]["$__passes"], "2");
}

#[test]
fn escaped_output_limit_fails_without_truncation_or_losing_pending_state() {
    let (_directory, store) = fixture(json!([{"role":"user","data":"\n".repeat(600)}])); let pool = AccessPool::new(settings()).unwrap();
    let access = scope(&store, &pool, Exposure::Full); let mut limits = limits(); limits.values.utf8_bytes = 1024;
    let mut adapter = ScalarAdapter::initialize_values("function run(id) setState(id,'before',true); local ok,err=pcall(getFullChat,id); assert(not ok and tostring(err):find('encoded-history-bytes',1,true)); return getState(id,'before') end", limits, Duration::from_secs(5)).unwrap();
    assert_eq!(call(&mut adapter, &access, &store).result, HandlerResult::Returned(Scalar::Boolean(true)));
    assert_eq!(commit(&access, &store).error, None);
    assert_eq!(conversation(&store)["scriptstate"]["$__before"], "true");
    drop(adapter); drop(access);
    let deadline = Instant::now() + Duration::from_secs(5);
    while pool.used_bytes() != 0 { assert!(Instant::now() < deadline); std::thread::yield_now(); }
}

#[test]
fn fixed_module_profile_preserves_source_allowance_and_excludes_unimplemented_capabilities() {
    let source = "function run(id) assert(package==nil and io==nil and os==nil and debug==nil and hostAwait==nil); local old=error; error=function() end; assert(not pcall(require,'other')); error=old; assert(require('json')==json); return true end";
    let mut tiny = limits(); tiny.memory_bytes = 1024; assert!(ScalarAdapter::initialize_values(source, tiny, Duration::from_secs(5)).is_err());
    let mut limits = limits(); limits.source_bytes = source.len();
    let mut adapter = ScalarAdapter::initialize_values(source, limits, Duration::from_secs(5)).unwrap();
    assert!(ScalarAdapter::initialize_values(&(source.to_owned()+" "), limits, Duration::from_secs(5)).is_err());
    let (_directory, store) = fixture(corpus()["rows"].clone()); let pool = AccessPool::new(settings()).unwrap(); let access = scope(&store, &pool, Exposure::Full);
    assert_eq!(call(&mut adapter, &access, &store).result, HandlerResult::Returned(Scalar::Boolean(true)));
}

#[test]
fn values_profile_retains_globals_but_rejects_previous_invocation_write_key() {
    let (_directory, store) = fixture(corpus()["rows"].clone()); let pool = AccessPool::new(settings()).unwrap();
    let access = scope(&store, &pool, Exposure::Full);
    let mut adapter = adapter("count=0; function run(id) count=count+1; if old then setState(old,'stale',true) end; old=id; setState(id,'count',count); return getState(id,'count') end");
    assert_eq!(call(&mut adapter, &access, &store).result, HandlerResult::Returned(Scalar::Number(1.0)));
    assert_eq!(call(&mut adapter, &access, &store).result, HandlerResult::Returned(Scalar::Number(2.0)));
    assert_eq!(commit(&access, &store).error, None);
    assert_eq!(conversation(&store)["scriptstate"]["$__count"], "2");
    assert!(conversation(&store)["scriptstate"].get("$__stale").is_none());
    adapter.dispose();
    let permit = access.begin_invocation(false).unwrap(); access.end_invocation(&permit).unwrap();
}

#[test]
fn json_shape_budgets_include_array_root_keys_default_time_and_field_depth() {
    for (nodes, depth, full_succeeds, error) in [(5, 128, false, "json-nodes"), (6, 128, true, ""),
        (64, 1, false, "json-depth"), (64, 2, true, "")] {
        let (_directory, store) = fixture(json!([{"data":"x"}])); let pool = AccessPool::new(settings()).unwrap();
        let access = scope(&store, &pool, Exposure::Full); let mut limits = limits();
        limits.values.nodes = nodes; limits.values.depth = depth;
        let check = if full_succeeds { "assert(ok and value=='[{\"data\":\"x\",\"time\":0}]')".to_owned() }
            else { format!("assert(not ok and tostring(value):find('{error}',1,true))") };
        let source = format!("function run(id) setState(id,'before',true); assert(getChatMain(id,0)=='{{\"data\":\"x\",\"time\":0}}'); local ok,value=pcall(getFullChatMain,id); {check}; return getState(id,'before') end");
        let mut adapter = ScalarAdapter::initialize_values(&source, limits, Duration::from_secs(5)).unwrap();
        assert_eq!(call(&mut adapter, &access, &store).result, HandlerResult::Returned(Scalar::Boolean(true)), "nodes={nodes}, depth={depth}");
        assert_eq!(commit(&access, &store).error, None);
        assert_eq!(conversation(&store)["scriptstate"]["$__before"], "true");
        assert_eq!(conversation(&store)["message"], json!([{"data":"x"}]));
    }
}
