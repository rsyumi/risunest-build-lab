use super::*;

fn finish(runtime: &mut Runtime) -> Step {
    for _ in 0..1_000_000 {
        let step = runtime.step();
        if step != Step::Suspended { return step; }
    }
    panic!("Wasmoon profile test did not finish within scheduler bound");
}

#[test]
fn a_protected_decline_ends_the_invocation_at_once() {
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    runtime.install_wasmoon_profile(&[]).unwrap();
    runtime.register_unsupported("hash", true, |_| true).unwrap();
    runtime.start_ignoring_result("function callListenMain() pcall(hash, 'x') while true do end end",
        Duration::from_secs(5), Cancellation::default()).unwrap();
    assert_eq!(finish(&mut runtime), Step::IgnoredComplete);
    let empty: [u16; 0] = [];
    let started = Instant::now();
    runtime.start_listener([&empty; 4], Duration::from_secs(5), Cancellation::default()).unwrap();
    assert_eq!(finish(&mut runtime), Step::Terminal(Terminal::Cancelled));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(runtime.declined().as_deref(), Some("lua-host:hash"));
}

#[test]
fn an_allocation_failure_is_declined_wherever_lua_could_catch_it() {
    let empty: [u16; 0] = [];
    let runtime = || {
        let mut runtime = Runtime::new(Limits { memory_bytes: 4 << 20, ..Limits::default() }).unwrap();
        runtime.install_wasmoon_profile(&[]).unwrap();
        runtime
    };
    let listeners = [
        "pcall(string.rep, 'x', 1 << 26)",
        "xpcall(string.rep, function(reason) return reason end, 'x', 1 << 26)",
        "coroutine.resume(coroutine.create(function() return string.rep('x', 1 << 26) end))",
        "load(function() return string.rep('x', 1 << 26) end)",
        // Each invalid byte becomes three bytes of U+FFFD in `js_text`, a Rust callback that reports the failure as an
        // error value.
        "local text = string.rep(string.char(255), 1 << 20) pcall(Error.create, text)",
    ];
    let mut outcomes = listeners.iter().map(|listener| {
        let mut runtime = runtime();
        runtime.start_ignoring_result(&format!("function callListenMain() {listener} return 'continued' end"),
            Duration::from_secs(5), Cancellation::default()).unwrap();
        assert_eq!(finish(&mut runtime), Step::IgnoredComplete);
        runtime.start_listener([&empty; 4], Duration::from_secs(5), Cancellation::default()).unwrap();
        let ended = matches!(finish(&mut runtime), Step::Terminal(Terminal::Cancelled));
        (*listener, runtime.declined(), ended)
    }).collect::<Vec<_>>();
    // Uncaught at the top level.
    let top = "string.rep('x', 1 << 26)";
    let mut runtime = runtime();
    runtime.start_ignoring_result(top, Duration::from_secs(5), Cancellation::default()).unwrap();
    let failed = matches!(finish(&mut runtime), Step::Failed(_));
    outcomes.push((top, runtime.declined(), failed));
    let expected = listeners.iter().chain([&top]).map(|source| (*source, Some("lua-memory".to_owned()), true)).collect::<Vec<_>>();
    assert_eq!(outcomes, expected);
}

#[test]
fn the_catching_functions_name_their_argument_errors_as_the_standard_ones_do() {
    let source = "local messages = {}
        for _, call in ipairs({ function() return pcall() end, function() return xpcall() end,
            function() return coroutine.resume() end, function() return load() end }) do
            local ok, message = pcall(call)
            -- The position names the prelude here.
            messages[#messages + 1] = tostring(ok) .. ' ' .. message:gsub('^.-:%d+: ', '')
        end
        return table.concat(messages, '|')";
    let run = |wasmoon: bool| {
        let mut runtime = Runtime::new(Limits::default()).unwrap();
        if wasmoon { runtime.install_wasmoon_profile(&[]).unwrap(); }
        runtime.start(source, Duration::from_secs(5), Cancellation::default()).unwrap();
        finish(&mut runtime)
    };
    let standard = run(false);
    assert!(matches!(&standard, Step::Complete(Json::String(text)) if text.matches("false bad argument").count() == 4), "{standard:?}");
    assert_eq!(run(true), standard);
}

fn units(text: &str) -> Vec<u16> { text.encode_utf16().collect() }

// `res = await getFunction(name)(...)`: the first result, nil for a missing global or a table, and an error for a value
// that is not a function.
#[test]
fn a_handler_receives_its_arguments_and_completes_with_its_first_result() {
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    runtime.install_wasmoon_profile(&[]).unwrap();
    runtime.start_ignoring_result("function run(id, data) return id .. ':' .. data, 'ignored' end function table_result() return {} end value = 1",
        Duration::from_secs(5), Cancellation::default()).unwrap();
    assert_eq!(finish(&mut runtime), Step::IgnoredComplete);
    let mut handler = |name: &str, arguments: &[&str]| {
        let arguments = arguments.iter().map(|argument| units(argument)).collect::<Vec<_>>();
        let arguments = arguments.iter().map(Vec::as_slice).collect::<Vec<_>>();
        runtime.start_handler(&units(name), &arguments, Duration::from_secs(5), Cancellation::default()).unwrap();
        finish(&mut runtime)
    };
    assert_eq!(handler("run", &["key", "data"]), Step::ScalarComplete(Scalar::String("key:data".into())));
    assert_eq!(handler("missing", &["key"]), Step::ScalarComplete(Scalar::Nil));
    assert_eq!(handler("table_result", &["key"]), Step::ScalarComplete(Scalar::Nil));
    assert!(matches!(handler("value", &["key"]), Step::Failed(_)));
    assert_eq!(handler("run", &["again", ""]), Step::ScalarComplete(Scalar::String("again:".into())));
}

#[test]
fn card_code_has_no_bytecode_dump_and_a_collect_only_collector() {
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    runtime.start("assert(string.dump == nil)
        for _, option in ipairs({ 'stop', 'restart', 'count', 'step', 'isrunning', 'incremental', 'generational', 'setpause' }) do
            assert(not pcall(collectgarbage, option), option)
        end
        assert(collectgarbage() == 0 and collectgarbage('collect') == 0)", Duration::from_secs(5), Cancellation::default()).unwrap();
    assert_eq!(finish(&mut runtime), Step::Complete(Json::Null));
}

// The panic retires the VM before it resumes into Lua, so no catching function lets card code continue.
#[test]
fn a_host_panic_retires_the_vm_even_when_card_code_catches_it() {
    for catch in ["pcall(boom)", "xpcall(boom, function() end)", "coroutine.resume(coroutine.create(boom))",
        "coroutine.wrap(function() pcall(boom) end)()"] {
        let mut runtime = Runtime::new(Limits::default()).unwrap();
        runtime.register_scalar("boom", |_, _| -> Result<ScalarReturn, String> { panic!("injected Lua host panic") }).unwrap();
        runtime.start(&format!("{catch} return 'continued'"), Duration::from_secs(5), Cancellation::default()).unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| finish(&mut runtime)));
        assert!(runtime.panicked(), "{catch}");
        assert!(!matches!(outcome, Ok(Step::Complete(_))), "{catch}: card code continued");
        assert!(runtime.start("return 1", Duration::from_secs(5), Cancellation::default()).is_err(), "{catch}");
    }
}
