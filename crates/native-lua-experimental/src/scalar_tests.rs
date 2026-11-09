use super::*;

fn finish(runtime: &mut Runtime) -> Step {
    for _ in 0..100_000 {
        let step = runtime.step();
        if step != Step::Suspended { return step; }
    }
    panic!("scalar test did not finish within scheduler bound");
}

fn source(runtime: &mut Runtime, source: &str) {
    runtime.start(source, Duration::from_secs(5), Cancellation::default()).unwrap();
    assert_eq!(finish(runtime), Step::Complete(Json::Null));
}

fn invoke(runtime: &mut Runtime, name: &str) -> Step {
    assert!(runtime.start_scalar_call(name, &[], Duration::from_secs(5), Cancellation::default()).unwrap());
    finish(runtime)
}

#[test]
fn host_return_arity_is_distinct_from_first_only_handler_completion() {
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    runtime.register_scalar("nothing", |_, _| Ok(ScalarReturn::NoValues)).unwrap();
    runtime.register_scalar("nilvalue", |_, _| Ok(ScalarReturn::One(Scalar::Nil))).unwrap();
    source(&mut runtime, "function run() assert(select('#',nothing())==0); assert(select('#',nilvalue())==1); return false,nil,99 end; function empty() end; function explicit() return nil end");
    assert_eq!(invoke(&mut runtime, "run"), Step::ScalarComplete(Scalar::Boolean(false)));
    assert_eq!(invoke(&mut runtime, "empty"), Step::ScalarComplete(Scalar::Nil));
    assert_eq!(invoke(&mut runtime, "explicit"), Step::ScalarComplete(Scalar::Nil));
}

#[test]
fn extra_handler_results_do_not_enter_scalar_conversion() {
    let mut runtime = Runtime::new(Limits { values: ValueLimits { nodes: 4, ..ValueLimits::default() }, ..Limits::default() }).unwrap();
    source(&mut runtime, "function run() local t={} for i=1,1000 do t[i]={} end; return 7, table.unpack(t) end");
    assert_eq!(invoke(&mut runtime, "run"), Step::ScalarComplete(Scalar::Number(7.0)));
}

#[test]
fn number_bridge_rounds_lua_integers_to_js_and_wraps_outbound_wasm_i64() {
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    runtime.register_scalar("echo", |_, args| Ok(ScalarReturn::One(args[0].clone()))).unwrap();
    runtime.register_scalar("wide", |_, args| Ok(ScalarReturn::One(Scalar::Number(match &args[0] {
        Scalar::String(key) if key == "edge" => 9_223_372_036_854_775_808.0,
        Scalar::String(key) if key == "mod" => 18_446_744_073_709_551_616.0,
        Scalar::String(key) if key == "tail" => 18_446_744_073_709_555_712.0,
        _ => -18_446_744_073_709_555_712.0,
    })))).unwrap();
    source(&mut runtime, r#"function run()
        assert(echo(9007199254740993)==9007199254740992)
        assert(math.type(echo(3))=='integer' and math.type(echo(1.5))=='float')
        assert(echo(0/0)~=echo(0/0) and echo(math.huge)==math.huge)
        assert(math.type(echo(-0.0))=='integer' and echo(-0.0)==0)
        assert(wide('edge')==math.mininteger and wide('mod')==0)
        assert(wide('tail')==4096 and wide('negative')==-4096)
        return 9007199254740993
    end"#);
    assert_eq!(invoke(&mut runtime, "run"), Step::ScalarComplete(Scalar::Number(9_007_199_254_740_992.0)));
}

#[test]
fn scalar_strings_reject_binary_and_nul_without_lossy_conversion() {
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    runtime.register_scalar("echo", |_, args| Ok(ScalarReturn::One(args[0].clone()))).unwrap();
    source(&mut runtime, r#"function run()
        assert(echo('한글 🦊')=='한글 🦊')
        assert(not pcall(echo,string.char(255)))
        assert(not pcall(echo,'a'..string.char(0)..'b'))
        assert(not pcall(echo,{}))
        return true
    end"#);
    assert_eq!(invoke(&mut runtime, "run"), Step::ScalarComplete(Scalar::Boolean(true)));
}

#[test]
fn argument_and_result_limits_guard_actual_callback_effects() {
    let state = Rc::new(RefCell::new(String::new()));
    let mut runtime = Runtime::new(Limits { values: ValueLimits { nodes: 2, utf8_bytes: 24, ..ValueLimits::default() }, ..Limits::default() }).unwrap();
    let target = state.clone();
    runtime.register_scalar("put", move |_, args| { *target.borrow_mut() = format!("{:?}", args[0]); Ok(ScalarReturn::NoValues) }).unwrap();
    runtime.register_scalar("large", |_, _| Ok(ScalarReturn::One(Scalar::String("x".repeat(25))))).unwrap();
    source(&mut runtime, "function run() put('kept'); assert(not pcall(put,string.rep('x',25))); assert(not pcall(put,1,2,3)); assert(not pcall(large)); return true end");
    assert_eq!(invoke(&mut runtime, "run"), Step::ScalarComplete(Scalar::Boolean(true)));
    assert_eq!(&*state.borrow(), "String(\"kept\")");
}

#[test]
fn named_lookup_missing_errors_and_globals_are_retained() {
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    source(&mut runtime, "n=0; disabled=false; bad=3; function fail() n=n+1; error('ordinary') end; function read() return n end");
    for name in ["missing", "disabled"] {
        assert!(!runtime.start_scalar_call(name, &[], Duration::from_secs(5), Cancellation::default()).unwrap());
    }
    assert!(runtime.start_scalar_call("bad", &[], Duration::from_secs(5), Cancellation::default()).is_err());
    assert!(matches!(invoke(&mut runtime, "fail"), Step::Failed(error) if error.contains("ordinary")));
    assert_eq!(invoke(&mut runtime, "read"), Step::ScalarComplete(Scalar::Number(1.0)));
}

#[test]
fn disabled_intrinsic_and_scheduler_yield_do_not_create_a_host_request() {
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    runtime.disable_host_await().unwrap();
    source(&mut runtime, "function forbidden() hostAwait('sleep',1) end; function run() coroutine.yield(); return true end");
    assert!(matches!(invoke(&mut runtime, "forbidden"), Step::Failed(_)));
    assert!(runtime.start_scalar_call("run", &[], Duration::from_secs(5), Cancellation::default()).unwrap());
    assert_eq!(runtime.step(), Step::Suspended);
    assert_eq!(finish(&mut runtime), Step::ScalarComplete(Scalar::Boolean(true)));
}

#[test]
fn emergency_cancel_retires_scalar_entry_and_source_limits_still_apply() {
    let mut runtime = Runtime::new(Limits { source_bytes: 128, ..Limits::default() }).unwrap();
    assert!(runtime.start(&"x".repeat(129), Duration::from_secs(5), Cancellation::default()).is_err());
    source(&mut runtime, "function run() coroutine.yield(); while true do end end");
    let emergency = Cancellation::default();
    runtime.start_scalar_call("run", &[], Duration::from_secs(5), emergency.clone()).unwrap();
    assert_eq!(runtime.step(), Step::Suspended);
    emergency.cancel();
    assert_eq!(runtime.step(), Step::Terminal(Terminal::Cancelled));
    assert_eq!(runtime.terminal_reason(), Some(Terminal::Cancelled));
    assert!(runtime.start_scalar_call("run", &[], Duration::from_secs(5), Cancellation::default()).is_err());
}
