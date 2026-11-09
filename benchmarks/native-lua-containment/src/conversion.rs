use super::{complete, finish, start};
use mlua::{Lua, LuaSerdeExt, Value};
use risunest_native_lua_experimental::{Limits, Runtime, Step, ValueLimits};
use serde_json::{json, Value as Json};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

fn runtime(values: ValueLimits) -> Runtime {
    Runtime::new(Limits { values, ..Limits::default() }).unwrap()
}

fn fail(runtime: &mut Runtime, source: &str, expected: &str) {
    start(runtime, source, Duration::from_secs(1));
    match finish(runtime) {
        Step::Failed(error) => assert!(error.contains(expected), "{error}"),
        result => panic!("expected bounded conversion failure, got {result:?}"),
    }
    complete(runtime, "return true", json!(true));
}

fn aliases() {
    for target in ["return t", "return sink(t)", "return hostAwait('s',t)"] {
        let mut runtime = runtime(ValueLimits { nodes: 31, ..ValueLimits::default() });
        let effects = Rc::new(Cell::new(0));
        let observed = effects.clone();
        runtime.register_sync("sink", move |_, _| { observed.set(observed.get()+1); Ok(json!(true)) }).unwrap();
        fail(&mut runtime, &format!("local t={{1}}; for i=1,4 do t={{t,t}} end; {target}"), "JSON value limit");
        assert_eq!(effects.get(), 0);
        runtime.dispose();
    }
    for source in ["return sink({1,2,3},{4,5,6})", "return hostAwait('s',{1,2,3},{4,5,6})"] {
        let mut runtime = runtime(ValueLimits { nodes: 7, ..ValueLimits::default() });
        runtime.register_sync("sink", |_, _| panic!("over-budget arguments reached host")).unwrap();
        fail(&mut runtime, source, "JSON value limit");
        runtime.dispose();
    }
}

fn limits() {
    let mut runtime = runtime(ValueLimits { depth: 3, nodes: 20, utf8_bytes: 5 });
    for source in ["return {['é']='🙂'}", "return {['ééé']=true}", "return hostAwait('ééé')"] {
        fail(&mut runtime, source, "JSON UTF-8 byte limit");
    }
    runtime.register_sync("sink", |_, _| panic!("over-budget arguments reached host")).unwrap();
    fail(&mut runtime, "return sink('é','🙂')", "JSON UTF-8 byte limit");
    fail(&mut runtime, "return hostAwait('s','é','éé')", "JSON UTF-8 byte limit");
    fail(&mut runtime, "return {{{{true}}}}", "JSON depth limit");
    complete(&mut runtime, "return {{{true}}}", json!([[[true]]]));
    complete(&mut runtime, "return {['é']='abc'}", json!({"é":"abc"}));
    runtime.dispose();
    let mut runtime = self::runtime(ValueLimits { nodes: 6, ..ValueLimits::default() });
    fail(&mut runtime, "return {a=true,b=true,c=true}", "JSON value limit");
    complete(&mut runtime, "return {a=true,b=true}", json!({"a":true,"b":true}));
    runtime.dispose();
}

fn parity() {
    let seed = json!({"null":null,"array":[null,true,42,"é"],"emptyArray":[],"emptyMap":{},"number":-42});
    let oracle = Lua::new();
    let oracle_seed = seed.clone();
    oracle.globals().set("seed", oracle.create_function(move |lua, ()| lua.to_value(&oracle_seed)).unwrap()).unwrap();
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    runtime.register_sync("seed", move |_, _| Ok(seed.clone())).unwrap();
    for source in [
        "return nil", "return true", "return 42", "return -1.25", "return 'é🙂'",
        "return {}", "return {1,2,3}", "return {a=1,b='x'}", "return {1,nil,3}",
        "return {1, ignored=function() end}", "return 0/0", "return math.huge",
        "return seed()", "return seed().emptyArray", "return seed().emptyMap", "return seed().null",
        "return function() end", "return coroutine.create(function() end)", "return string.char(255)",
        "return {[false]=true}", "local t={}; t.self=t; return t",
        "return setmetatable({x=42},{__pairs=function() error('must not run') end})",
    ] {
        let expected = oracle.load(source).eval::<Value>().and_then(|value| oracle.from_value::<Json>(value));
        start(&mut runtime, source, Duration::from_secs(1));
        match (expected, finish(&mut runtime)) {
            (Ok(expected), Step::Complete(actual)) => assert_eq!(actual, expected, "{source}"),
            (Err(_), Step::Failed(_)) => {},
            (expected, actual) => panic!("serde parity mismatch for {source}: {expected:?} vs {actual:?}"),
        }
    }
    runtime.dispose();
}

fn reverse() {
    for (limits, value, expected) in [
        (ValueLimits { nodes: 6, ..ValueLimits::default() }, json!({"a":1,"b":2,"c":3}), "JSON value limit"),
        (ValueLimits { depth: 2, ..ValueLimits::default() }, json!([[[1]]]), "JSON depth limit"),
        (ValueLimits { utf8_bytes: 5, ..ValueLimits::default() }, json!({"é":"🙂"}), "JSON UTF-8 byte limit"),
    ] {
        let mut runtime = runtime(limits);
        let response = value.clone();
        runtime.register_sync("value", move |_, _| Ok(response.clone())).unwrap();
        fail(&mut runtime, "return value()", expected);
        start(&mut runtime, "return hostAwait('s')", Duration::from_secs(1));
        let Step::Awaiting(request) = finish(&mut runtime) else { panic!("expected request"); };
        let error = runtime.reply(request.id, Ok(value)).unwrap_err();
        assert!(error.to_string().contains(expected));
        runtime.reply(request.id, Ok(json!(42))).unwrap();
        assert_eq!(finish(&mut runtime), Step::Complete(json!(42)));
        runtime.dispose();
    }
    let mut runtime = runtime(ValueLimits { utf8_bytes: 5, ..ValueLimits::default() });
    runtime.register_sync("bad", |_, _| Err("ééé".into())).unwrap();
    fail(&mut runtime, "return bad()", "JSON UTF-8 byte limit");
    start(&mut runtime, "return hostAwait('s')", Duration::from_secs(1));
    let Step::Awaiting(request) = finish(&mut runtime) else { panic!("expected request"); };
    assert!(runtime.reply(request.id, Err("ééé".into())).unwrap_err().to_string().contains("JSON UTF-8 byte limit"));
    runtime.reply(request.id, Ok(json!(null))).unwrap();
    assert_eq!(finish(&mut runtime), Step::Complete(Json::Null));
    runtime.dispose();
}

fn result_allocation(count: usize) -> super::allocations::Stats {
    let lua = Lua::new();
    lua.gc_stop();
    let function = lua.load(format!(r#"
        local values={{}}; for i=1,256 do values[i]=42 end
        local warm={{table.unpack(values)}}
        coroutine.yield(true)
        return table.unpack(warm,1,{count})
    "#)).into_function().unwrap();
    let thread = lua.create_thread(function).unwrap();
    assert_eq!(thread.resume_first(()).unwrap(), Value::Boolean(true));
    unsafe {
        lua.exec_raw::<()>(thread.clone(), |state| {
            let child = mlua::ffi::lua_tothread(state, -1);
            assert!(!child.is_null());
            let top = mlua::ffi::lua_gettop(child);
            assert_ne!(mlua::ffi::lua_checkstack(child, 4096), 0);
            assert_eq!(mlua::ffi::lua_gettop(child), top);
        }).unwrap();
    }
    // Child stack and result table are prepared. Only this thread's allocations
    // during the final resume are counted; no timers or worker threads are used.
    let before = lua.used_memory();
    let (result, stats) = super::allocations::measure(|| thread.resume_first(()));
    assert_eq!(result.unwrap(), Value::Integer(42));
    eprintln!("first-result count={count} LuaBytes={before}->{} allocations={stats:?}", lua.used_memory());
    stats
}

thread_local! {
    static ARGUMENT_CONVERSIONS: Cell<usize> = const { Cell::new(0) };
}

struct ObservedArguments(usize);
impl mlua::FromLuaMulti for ObservedArguments {
    fn from_lua_multi(values: mlua::MultiValue, _: &Lua) -> mlua::Result<Self> {
        ARGUMENT_CONVERSIONS.with(|count| count.set(count.get()+1));
        Ok(Self(values.len()))
    }
}

fn fanout() {
    let lua = Lua::new();
    let effects = Rc::new(Cell::new(0));
    let observed = effects.clone();
    ARGUMENT_CONVERSIONS.with(|count| count.set(0));
    let callback = lua.create_function_with_argument_limit(4, move |_, values: ObservedArguments| {
        observed.set(observed.get()+1);
        Ok(values.0)
    }).unwrap();
    assert_eq!(callback.call::<usize>((1,2,3,4)).unwrap(), 4);
    assert!(callback.call::<usize>((1,2,3,4,5)).unwrap_err().to_string().contains("Lua argument count limit"));
    assert_eq!(effects.get(), 1);
    ARGUMENT_CONVERSIONS.with(|count| assert_eq!(count.get(), 1, "rejected arguments entered FromLuaMulti"));
    let yields = Rc::new(Cell::new(0));
    let seen = yields.clone();
    lua.set_thread_event_callback(mlua::thread::ThreadTriggers::ON_YIELD, move |_, event| {
        assert!(matches!(event, mlua::thread::ThreadEvent::Yield(_)));
        seen.set(seen.get()+1);
        Ok(())
    });
    let thread = lua.create_thread(lua.load("coroutine.yield(nil,42); local value=coroutine.yield(1,2); return value").into_function().unwrap()).unwrap();
    assert_eq!(thread.resume_first(()).unwrap(), Value::Nil);
    assert_eq!(thread.resume_first(()).unwrap(), Value::Integer(1));
    assert_eq!(thread.resume_first(9).unwrap(), Value::Integer(9));
    assert_eq!(yields.get(), 2);
    lua.remove_thread_event_callback();
    let mut runtime = runtime(ValueLimits { nodes: 4, ..ValueLimits::default() });
    runtime.register_sync("sink", |_, _| panic!("fanout reached host")).unwrap();
    fail(&mut runtime, "return sink(table.unpack({1,2,3,4,5}))", "Lua argument count limit");
    fail(&mut runtime, "return hostAwait('s',table.unpack({1,2,3,4}))", "Lua argument count limit");
    complete(&mut runtime, "return table.unpack({42,2,3,4,5,6})", json!(42));
    complete(&mut runtime, "return", Json::Null);
    for source in ["coroutine.yield(); return nil", "coroutine.yield(table.unpack({1,2,3,4,5,6})); return nil"] {
        start(&mut runtime, source, Duration::from_secs(1));
        assert_eq!(runtime.step(), Step::Suspended);
        assert_eq!(finish(&mut runtime), Step::Complete(Json::Null));
    }
    start(&mut runtime, "error(string.rep('é',4096),0)", Duration::from_secs(1));
    let Step::Failed(error) = finish(&mut runtime) else { panic!("expected diagnostic"); };
    assert!(error.len() <= 4096 && error.ends_with("..."));
    complete(&mut runtime, "local ok,e=pcall(function() error(string.rep('é',4096),0) end); return #e", json!(8192));
    runtime.dispose();
    let one = result_allocation(1);
    let many = result_allocation(256);
    assert!(many.largest <= one.largest + 512, "first-result allocation grew: {one:?} vs {many:?}");
    assert!(many.total <= one.total + 512, "first-result allocations grew: {one:?} vs {many:?}");
}

pub fn probe(name: &str) {
    match name {
        "conversion-alias" => aliases(),
        "conversion-limits" => limits(),
        "conversion-parity" => parity(),
        "conversion-reverse" => reverse(),
        "conversion-fanout" => fanout(),
        _ => unreachable!(),
    }
}
