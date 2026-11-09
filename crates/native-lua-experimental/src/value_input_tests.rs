use super::*;

fn finish(runtime: &mut Runtime) -> Step {
    for _ in 0..100_000 { let result = runtime.step(); if result != Step::Suspended { return result; } }
    panic!("values test exceeded scheduler bound");
}
fn source(runtime: &mut Runtime, source: &str) {
    runtime.start_ignoring_result(source, Duration::from_secs(5), Cancellation::default()).unwrap();
    assert_eq!(finish(runtime), Step::IgnoredComplete);
}
fn invoke(runtime: &mut Runtime) -> Step {
    assert!(runtime.start_values_call("run", &[], Duration::from_secs(5), Cancellation::default()).unwrap());
    finish(runtime)
}
fn reader(runtime: &mut Runtime) {
    runtime.register_history_reader("read", HistoryReader::Recent, || Ok("active".into()), |selection| {
        match selection { PreparedHistory::Recent(NumberArgument::Value(value)) => Ok(value.to_string()),
            PreparedHistory::Recent(NumberArgument::Invalid) => Ok("coercion-error".into()), _ => unreachable!() }
    }).unwrap();
}

#[test]
fn recent_coercion_preserves_truthiness_arrays_cycles_and_radix_rounding() {
    let mut runtime = Runtime::new(Limits::default()).unwrap(); reader(&mut runtime);
    source(&mut runtime, r#"function run()
        assert(read()== '0' and read(nil,nil)=='0' and read(nil,0/0)=='0')
        assert(read(nil,'bad')=='NaN' and read(nil,{})=='NaN')
        assert(read(nil,true)=='1' and read(nil,false)=='0' and read(nil,'  +2e0  ')=='2')
        assert(read(nil,{'0x2'})=='2' and read(nil,{{'0b10'}})=='2')
        assert(read(nil,'-0x2')=='NaN' and read(nil,'0o2')=='2')
        assert(read(nil,'0x20000000000001')=='9007199254740992')
        assert(read(nil,'0x20000000000003')=='9007199254740996')
        local cycle={}; cycle[1]=cycle; assert(read(nil,cycle)=='0')
        assert(read(nil,{toString=false})=='coercion-error')
        assert(read(nil,{valueOf=false})=='NaN')
        return true
    end"#);
    assert_eq!(invoke(&mut runtime), Step::ScalarComplete(Scalar::Boolean(true)));
}

#[test]
fn clone_checks_final_own_payload_and_the_entire_argument_list() {
    let mut runtime = Runtime::new(Limits::default()).unwrap(); reader(&mut runtime);
    source(&mut runtime, r#"function run()
        assert(not pcall(read,function() end,2))
        assert(not pcall(read,nil,2,function() end))
        assert(not pcall(read,nil,{valueOf=function() end}))
        assert(read(nil,{__proto__=function() end})=='NaN')
        assert(read(nil,{__proto__={toString=false}})=='NaN')
        local calls=0
        local key=setmetatable({}, {__tostring=function() calls=calls+1; return 'x' end})
        assert(read(nil,{[key]=2})=='NaN' and calls==2)
        return true
    end"#);
    assert_eq!(invoke(&mut runtime), Step::ScalarComplete(Scalar::Boolean(true)));
}

#[test]
fn reentrant_key_conversion_preserves_prefix_and_rechecks_invocation_fence() {
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    let writes = Rc::new(RefCell::new(Vec::new()));
    let seen = writes.clone();
    runtime.register_values_scalar("write", move |_, values| { seen.borrow_mut().extend(values); Ok(ScalarReturn::NoValues) }).unwrap();
    let fence = Rc::new(Cell::new(1)); let changed = fence.clone();
    runtime.register_values_scalar("revoke", move |_, _| { changed.set(2); Ok(ScalarReturn::NoValues) }).unwrap();
    runtime.register_history_reader("read", HistoryReader::Recent, move || Ok(fence.get().to_string()), |_| Ok("unexpected host read".to_string())).unwrap();
    source(&mut runtime, r#"function run()
        local key=setmetatable({}, {__tostring=function() write('prefix'); revoke(); return 'x' end})
        local ok,err=pcall(read,nil,{[key]=2}); assert(not ok and tostring(err):find('Lua invocation ended',1,true))
        return true
    end"#);
    assert_eq!(invoke(&mut runtime), Step::ScalarComplete(Scalar::Boolean(true)));
    assert_eq!(*writes.borrow(), vec![Scalar::String("prefix".into()), Scalar::String("prefix".into())]);
}

#[test]
fn values_string_codec_keeps_known_tail_nul_and_transient_utf16_distinct() {
    let mut runtime = Runtime::new(Limits::default()).unwrap(); reader(&mut runtime);
    runtime.register_values_scalar("echo", |_, mut values| Ok(ScalarReturn::One(values.remove(0)))).unwrap();
    source(&mut runtime, r#"function run()
        assert(echo('a'..string.char(0)..'b')=='a')
        assert(echo(string.char(239,187,191)..'x')==utf8.char(65279)..'x')
        assert(echo(string.char(239,187,191)..string.rep('x',17))==string.rep('x',17))
        assert(echo(string.char(194))==utf8.char(128))
        assert(echo(string.char(226,130))==utf8.char(8320))
        assert(echo(string.char(226,0,130))==utf8.char(8194))
        assert(echo(string.rep('x',17)..string.char(255))==string.rep('x',17)..utf8.char(65533))
        assert(not pcall(echo,string.char(237,160,128)))
        assert(read(nil,string.char(237,160,128))=='NaN')
        return true
    end"#);
    assert_eq!(invoke(&mut runtime), Step::ScalarComplete(Scalar::Boolean(true)));
}

#[test]
fn unknown_tail_is_classified_without_reading_or_executing_foreign_heap_bytes() {
    let corpus: Json = serde_json::from_str(include_str!("../../../tests/fixtures/lua/native-values-cases.json")).unwrap();
    for case in corpus["unknownTails"].as_array().unwrap() {
        let bytes = case["bytes"].as_array().unwrap().iter().map(|value| value.as_u64().unwrap() as u8).collect::<Vec<_>>();
        let expected = format!("unknown UTF-8 tail (lead 0, required {}, owned through {})", case["requiredIndex"], case["rawLength"]);
        assert!(super::value_input::wasmoon_text(&bytes).unwrap_err().to_string().contains(&expected));
    }
    assert_eq!(super::value_input::wasmoon_text(&[194]).unwrap(), [128]);
    assert_eq!(super::value_input::wasmoon_text(&[226,130]).unwrap(), [8320]);
    assert_eq!(super::value_input::wasmoon_text(&[226,0,130]).unwrap(), [8194]);
}

#[test]
fn bounded_conversion_rejects_large_graph_after_real_prefix_without_replay() {
    let mut runtime = Runtime::new(Limits { values: ValueLimits { nodes: 32, ..ValueLimits::default() }, ..Limits::default() }).unwrap();
    let writes = Rc::new(RefCell::new(Vec::new())); let seen = writes.clone();
    runtime.register_values_scalar("write", move |_, values| { seen.borrow_mut().extend(values); Ok(ScalarReturn::NoValues) }).unwrap();
    reader(&mut runtime);
    source(&mut runtime, "function run() write('before'); local t={} for i=1,100 do t[i]=i end; assert(not pcall(read,nil,t)); return true end");
    assert_eq!(invoke(&mut runtime), Step::ScalarComplete(Scalar::Boolean(true)));
    assert_eq!(*writes.borrow(), [Scalar::String("before".into())]);
}

#[test]
fn alias_graph_walks_admit_depth_and_each_repeated_expansion_before_recursing() {
    let cases = [
        (3, 16_384, "local nodes={{}}; for i=2,8 do nodes[i]={nodes[i-1]} end; read(nil,nodes)", "depth limit"),
        (3, 16_384, "local nodes={{}}; local root={{__proto__=nodes[1]}}; for i=2,8 do nodes[i]={nodes[i-1]}; root[i]={__proto__=nodes[i]} end; root[9]=nodes[8]; full(nil,root)", "depth limit"),
        (8, 48, "local leaf={'2'}; local a={leaf,leaf}; local b={a,a}; read(nil,{b,b})", "node limit"),
    ];
    for (depth, nodes, body, expected) in cases {
        let mut runtime = Runtime::new(Limits { values: ValueLimits { depth, nodes, ..ValueLimits::default() }, ..Limits::default() }).unwrap();
        let writes = Rc::new(RefCell::new(Vec::new())); let seen = writes.clone();
        runtime.register_values_scalar("write", move |_, values| { seen.borrow_mut().extend(values); Ok(ScalarReturn::NoValues) }).unwrap();
        reader(&mut runtime);
        runtime.register_history_reader("full", HistoryReader::Full, || Ok("active".into()), |_| Ok("unexpected completion".to_owned())).unwrap();
        source(&mut runtime, &format!("function run() write('before'); local ok,err=pcall(function() {body} end); assert(not ok and tostring(err):find('{expected}',1,true)); return true end"));
        assert_eq!(invoke(&mut runtime), Step::ScalarComplete(Scalar::Boolean(true)), "{body}");
        assert_eq!(*writes.borrow(), [Scalar::String("before".into())]);
    }
}
