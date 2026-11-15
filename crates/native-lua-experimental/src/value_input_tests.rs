use super::*;
use std::sync::Arc;

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
    runtime.register_history_reader_budgeted("catchable",HistoryReader::Recent,||Ok("active".into()),
        |_|Ok("unexpected completion".to_owned()),|_|Ok(Box::new(()))).unwrap();
    source(&mut runtime, r#"function run()
        for _,message in ipairs({'lua-host-limit','lua-memory'}) do
            local key=setmetatable({}, {__tostring=function() error(message) end})
            local ok,err=pcall(catchable,nil,{[key]=1})
            assert(not ok and tostring(err):find(message,1,true))
        end
        local key=setmetatable({}, {__tostring=function() write('prefix'); revoke(); return 'x' end})
        local ok,err=pcall(read,nil,{[key]=2}); assert(not ok and tostring(err):find('Lua invocation ended',1,true))
        return true
    end"#);
    assert_eq!(invoke(&mut runtime), Step::ScalarComplete(Scalar::Boolean(true)));
    assert_eq!(*writes.borrow(), vec![Scalar::String("prefix".into()), Scalar::String("prefix".into())]);
    assert_eq!(runtime.declined(), None);
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

#[test]
fn variable_graph_getters_copy_tables_preserve_aliases_and_nonfinite_numbers() {
    let mut runtime=Runtime::new(Limits::default()).unwrap();
    let pending=Rc::new(RefCell::new(None::<VariableValue>));
    let write=pending.clone();
    runtime.register_variable_api("put",||Ok("active".into()),move |_,values|{
        *write.borrow_mut()=values.into_iter().next();
        Ok(VariableReturn::NoValues)
    },|_|Ok(Box::new(()))).unwrap();
    let read=pending.clone();
    runtime.register_variable_api("get",||Ok("active".into()),move |_,_|{
        Ok(VariableReturn::One(read.borrow().as_ref().unwrap().clone()))
    },|_|Ok(Box::new(()))).unwrap();
    source(&mut runtime,r#"function run()
        local shared={n=1}
        local input={a=shared,b=shared,values={0/0,math.huge,-math.huge}}
        put(input)
        input.a.n=2
        local first=get()
        assert(first.a==first.b and first.a.n==1)
        assert(type(first.values[1])=='number' and first.values[1]~=first.values[1])
        assert(first.values[2]==math.huge and first.values[3]==-math.huge)
        first.a.n=7
        local second=get()
        assert(second.a==second.b and second.b.n==1 and first~=second)
        return true
    end"#);
    assert_eq!(invoke(&mut runtime),Step::ScalarComplete(Scalar::Boolean(true)));
}

#[test]
fn variable_graph_exports_own_properties_and_getters_include_enumerable_prototypes() {
    struct Keys(Vec<String>);
    impl ValueVisitor for Keys {
        fn visit(&mut self,event:ValueEvent<'_>)->Result<(),String>{
            if let ValueEvent::Key(key)=event {self.0.push(String::from_utf16(key).map_err(|_|"key UTF-16")?);}
            Ok(())
        }
    }
    let mut runtime=Runtime::new(Limits::default()).unwrap();
    runtime.register_variable_api("copy",||Ok("active".into()),|_,values|{
        let value=values.into_iter().next().unwrap();
        let mut keys=Keys(Vec::new());
        value.visit_own(&mut keys)?;
        assert_eq!(keys.0,["own"]);
        Ok(VariableReturn::One(value))
    },|_|Ok(Box::new(()))).unwrap();
    source(&mut runtime,"function run() local v=copy({own=1,__proto__={inherited='yes'}});return v.own==1 and v.inherited=='yes' and v.__proto__==nil end");
    assert_eq!(invoke(&mut runtime),Step::ScalarComplete(Scalar::Boolean(true)));
}

#[test]
fn typed_null_roundtrips_use_captured_userdata_and_missing_arguments_remain_undefined() {
    let mut runtime=Runtime::new(Limits::default()).unwrap();
    runtime.install_wasmoon_profile(&[]).unwrap();
    runtime.register_variable_api("copy",||Ok("active".into()),|_,values|{
        Ok(match values.into_iter().next(){Some(value)=>VariableReturn::One(value),None=>VariableReturn::NoValues})
    },|_|Ok(Box::new(()))).unwrap();
    runtime.register_values_scalar("scalarCopy",|_,mut values|Ok(ScalarReturn::One(values.remove(0)))).unwrap();
    source(&mut runtime,r#"function run()
        local original=null
        null=false
        assert(copy(nil)==original and type(copy(nil))=='userdata' and tostring(copy(nil))=='null')
        assert(copy(original)==original and scalarCopy(original)==original)
        assert(copy()==nil and select('#',copy())==0)
        local array=copy({original,{flag=original}})
        assert(array[1]==original and array[2].flag==original)
        return true
    end"#);
    assert_eq!(invoke(&mut runtime),Step::ScalarComplete(Scalar::Boolean(true)));
}

#[test]
fn variable_key_errors_are_catchable_and_preserve_reentrant_prefix_writes() {
    let mut runtime=Runtime::new(Limits::default()).unwrap();
    let writes=Rc::new(RefCell::new(Vec::new()));
    let seen=writes.clone();
    runtime.register_values_scalar("write",move |_,values|{seen.borrow_mut().extend(values);Ok(ScalarReturn::NoValues)}).unwrap();
    runtime.register_variable_api("copy",||Ok("active".into()),|_,values|Ok(VariableReturn::One(values.into_iter().next().unwrap())),|_|Ok(Box::new(()))).unwrap();
    source(&mut runtime,r#"function run()
        for _,message in ipairs({'keybad','lua-host-limit','values input byte limit exceeded'}) do
            local key=setmetatable({}, {__tostring=function() write('prefix');error(message) end})
            local ok,err=pcall(copy,{[key]=1})
            assert(not ok and tostring(err):find(message,1,true))
        end
        local result=copy({after='kept'})
        return result.after=='kept'
    end"#);
    assert_eq!(invoke(&mut runtime),Step::ScalarComplete(Scalar::Boolean(true)));
    assert_eq!(*writes.borrow(),vec![Scalar::String("prefix".into());3]);
    assert_eq!(runtime.declined(),None);
}

#[test]
fn reentrant_variable_conversion_shares_one_quota_and_releases_its_input_leases() {
    struct Lease {used:Rc<Cell<usize>>,bytes:usize}
    impl Drop for Lease {fn drop(&mut self){self.used.set(self.used.get()-self.bytes);}}
    let used=Rc::new(Cell::new(0usize));
    let writes=Rc::new(RefCell::new(Vec::new()));
    let mut runtime=Runtime::new(Limits::default()).unwrap();
    let seen=writes.clone();
    runtime.register_values_scalar("write",move |_,values|{seen.borrow_mut().extend(values);Ok(ScalarReturn::NoValues)}).unwrap();
    let quota=used.clone();
    runtime.register_variable_api("copy",||Ok("active".into()),|_,values|Ok(VariableReturn::One(values.into_iter().next().unwrap())),move |bytes|{
        let next=quota.get().checked_add(bytes).filter(|n|*n<=7000).ok_or("lua-host-limit")?;
        quota.set(next);
        Ok(Box::new(Lease {used:quota.clone(),bytes}) as Box<dyn std::any::Any>)
    }).unwrap();
    source(&mut runtime,r#"function run()
        write('before')
        local key=setmetatable({}, {__tostring=function() copy({inner=1});return 'key' end})
        pcall(copy,{[key]=1})
        write('after')
        return true
    end"#);
    assert_eq!(invoke(&mut runtime),Step::Terminal(Terminal::Cancelled));
    assert_eq!(runtime.declined(),Some("lua-host-limit".into()));
    assert_eq!(*writes.borrow(),vec![Scalar::String("before".into())]);
    assert_eq!(used.get(),0);
}

#[test]
fn effective_graph_property_queries_preserve_own_shadowing_and_bounded_prototype_chains() {
    let mut runtime=Runtime::new(Limits::default()).unwrap();
    runtime.install_wasmoon_profile(&[]).unwrap();
    runtime.register_variable_api("hasVar",||Ok("active".into()),|_,values|{
        let value=values.into_iter().next().unwrap();
        Ok(VariableReturn::One(VariableValue::scalar(Scalar::Boolean(value.has_nonnull_property("var")?),ValueLimits::default())))
    },|_|Ok(Box::new(()))).unwrap();
    runtime.register_variable_api("hasVarLimited",||Ok("active".into()),|_,values|{
        let value=values.into_iter().next().unwrap();
        let mut remaining=2usize;
        let found=value.has_nonnull_property_checked("var",||{
            remaining=remaining.checked_sub(1).ok_or("query work limit")?;Ok(())
        })?;
        Ok(VariableReturn::One(VariableValue::scalar(Scalar::Boolean(found),ValueLimits::default())))
    },|_|Ok(Box::new(()))).unwrap();
    source(&mut runtime,r#"function run()
        assert(hasVar({var={}}))
        assert(hasVar({__proto__={var={}}}))
        assert(not hasVar({var=null,__proto__={var={}}}))
        assert(not hasVar({__proto__={var=null}}))
        assert(hasVar({var=false,__proto__={var={}}}))
        assert(not hasVar({child={var={}}}))
        local chain={var={}}
        for i=1,32 do chain={__proto__=chain} end
        assert(hasVar(chain))
        local ok,err=pcall(hasVarLimited,chain)
        assert(not ok and tostring(err):find('query work limit',1,true))
        return true
    end"#);
    assert_eq!(invoke(&mut runtime),Step::ScalarComplete(Scalar::Boolean(true)));
}

#[test]
fn host_output_allocation_failures_keep_their_memory_label_and_prior_writes() {
    for history in [false, true] {
        let mut runtime=Runtime::new(Limits {memory_bytes:512<<10,..Limits::default()}).unwrap();
        runtime.install_wasmoon_profile(&[]).unwrap();
        let writes=Rc::new(RefCell::new(Vec::new()));
        let seen=writes.clone();
        runtime.register_values_scalar("write",move |_,values|{seen.borrow_mut().extend(values);Ok(ScalarReturn::NoValues)}).unwrap();
        if history {
            runtime.register_history_reader_budgeted("large",HistoryReader::Full,||Ok("active".into()),
                |_|Ok("x".repeat(768<<10)),|_|Ok(Box::new(()))).unwrap();
        } else {
            runtime.register_variable_api("large",||Ok("active".into()),|_,_|{
                Ok(VariableReturn::One(VariableValue::scalar(Scalar::String("x".repeat(768<<10)),ValueLimits::default())))
            },|_|Ok(Box::new(()))).unwrap();
        }
        source(&mut runtime,"function run()write('before');pcall(large);write('after');return true end");
        assert_eq!(invoke(&mut runtime),Step::Terminal(Terminal::Cancelled));
        assert_eq!(runtime.declined(),Some("lua-memory".into()));
        assert_eq!(*writes.borrow(),[Scalar::String("before".into())]);
        assert!(runtime.start_values_call("run",&[],Duration::from_secs(5),Cancellation::default()).is_err());
    }
    let mut fresh=Runtime::new(Limits::default()).unwrap();
    source(&mut fresh,"function run()return true end");
    assert_eq!(invoke(&mut fresh),Step::ScalarComplete(Scalar::Boolean(true)));
}

#[test]
fn raw_key_same_value_zero_preserves_strict_assignment_and_fresh_table_identity() {
    let mut runtime=Runtime::new(Limits::default()).unwrap();
    for name in ["keySame","valueSame"] {
        runtime.register_variable_api(name,||Ok("active".into()),move |_,values|{
            let left=values.first().cloned().unwrap_or_else(||VariableValue::undefined(ValueLimits::default()));
            let right=values.get(1).cloned().unwrap_or_else(||VariableValue::undefined(ValueLimits::default()));
            let same=if name=="keySame"{left.same_value_zero(&right)}else{left==right};
            Ok(VariableReturn::One(VariableValue::scalar(Scalar::Boolean(same),ValueLimits::default())))
        },|_|Ok(Box::new(()))).unwrap();
    }
    let previous=Rc::new(RefCell::new(None::<VariableValue>));
    runtime.register_variable_api("sameAsBefore",||Ok("active".into()),move |_,values|{
        let key=values.into_iter().next().unwrap();
        assert!(key.same_value_zero(&key.clone()));
        let same=previous.borrow().as_ref().is_some_and(|old|old.same_value_zero(&key));
        *previous.borrow_mut()=Some(key);
        Ok(VariableReturn::One(VariableValue::scalar(Scalar::Boolean(same),ValueLimits::default())))
    },|_|Ok(Box::new(()))).unwrap();
    source(&mut runtime,r#"function run()
        assert(keySame(0/0,0/0) and not valueSame(0/0,0/0))
        assert(keySame(0,-0.0) and valueSame(0,-0.0))
        assert(not keySame(7,'7') and not keySame(false,'false') and not keySame(false,0))
        assert(keySame() and keySame(nil,nil) and not keySame(nil))
        local key={nested={'value'}}
        assert(not keySame(key,key) and not valueSame(key,key))
        assert(not sameAsBefore(key) and not sameAsBefore(key))
        local unit=string.char(237,160,128)
        assert(keySame(unit,unit) and valueSame(unit,unit) and not keySame(unit,utf8.char(65533)))
        return true
    end"#);
    assert_eq!(invoke(&mut runtime),Step::ScalarComplete(Scalar::Boolean(true)));

    struct Lease(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Lease {fn drop(&mut self){self.0.store(true,std::sync::atomic::Ordering::SeqCst);}}
    let released=Arc::new(std::sync::atomic::AtomicBool::new(false));
    let key=VariableValue::object(Vec::new(),ValueLimits::default()).retain(Arc::new(Lease(released.clone())));
    let retained=key.clone();
    assert!(key.same_value_zero(&retained));
    assert!(!key.same_value_zero(&VariableValue::object(Vec::new(),ValueLimits::default())));
    drop(key);
    assert!(!released.load(std::sync::atomic::Ordering::SeqCst));
    drop(retained);
    assert!(released.load(std::sync::atomic::Ordering::SeqCst));
}

#[derive(Debug,PartialEq)]
enum KeyPrimitive {Undefined,Null,Boolean(bool),Number(f64),Text(Vec<u16>)}

struct KeyVisitor {result:Option<KeyPrimitive>,remaining_work:usize,remaining_bytes:usize}

impl Default for KeyVisitor {
    fn default()->Self {Self {result:None,remaining_work:10_000,remaining_bytes:100_000}}
}

impl ValueVisitor for KeyVisitor {
    fn visit(&mut self,event:ValueEvent<'_>)->Result<(),String>{
        self.result=Some(match event{
            ValueEvent::Undefined=>KeyPrimitive::Undefined,ValueEvent::Null=>KeyPrimitive::Null,
            ValueEvent::Boolean(value)=>KeyPrimitive::Boolean(value),ValueEvent::Number(value)=>KeyPrimitive::Number(value),
            ValueEvent::Text(value)=>KeyPrimitive::Text(value.to_vec()),_=>return Err("expected primitive key".into()),
        });
        Ok(())
    }
}

impl PropertyKeyVisitor for KeyVisitor {
    fn checkpoint(&mut self)->Result<(),String>{
        self.remaining_work=self.remaining_work.checked_sub(1).ok_or("key work refused")?;Ok(())
    }
    fn reserve(&mut self,bytes:usize)->Result<(),String>{
        self.remaining_bytes=self.remaining_bytes.checked_sub(bytes).ok_or("key allocation refused")?;Ok(())
    }
    fn number_units(&mut self,value:f64)->Result<Vec<u16>,String>{Ok(value.to_string().encode_utf16().collect())}
}

#[test]
fn storage_key_conversion_preserves_ordinary_data_errors_and_exact_utf16() {
    let mut runtime=Runtime::new(Limits::default()).unwrap();
    let values=Rc::new(RefCell::new(Vec::<VariableValue>::new()));
    let captured=values.clone();
    runtime.register_variable_api("capture",||Ok("active".into()),move |_,arguments|{
        *captured.borrow_mut()=arguments;Ok(VariableReturn::NoValues)
    },|_|Ok(Box::new(()))).unwrap();
    source(&mut runtime,r#"function run()
        capture(7,'7',false,nil,{7,'tail'},{{'one','two'},'three'}, {},
            {toString=17},{__proto__={toString=17}},string.char(237,160,128))
        return true
    end"#);
    assert_eq!(invoke(&mut runtime),Step::ScalarComplete(Scalar::Boolean(true)));
    let text=|value:&str|KeyPrimitive::Text(value.encode_utf16().collect());
    let expected=[Ok(KeyPrimitive::Number(7.0)),Ok(text("7")),Ok(KeyPrimitive::Boolean(false)),Ok(KeyPrimitive::Null),
        Ok(text("7,tail")),Ok(text("one,two,three")),Ok(text("[object Object]")),
        Err("lua-script-error:cannot convert data key to primitive"),Err("lua-script-error:cannot convert data key to primitive"),
        Ok(KeyPrimitive::Text(vec![0xd800]))];
    assert_eq!(values.borrow().len(),expected.len());
    assert_eq!(values.borrow()[0].text_units(),None);
    assert_eq!(values.borrow()[1].text_units(),Some([55].as_slice()));
    assert_eq!(values.borrow()[4].text_units(),None);
    assert_eq!(values.borrow()[9].text_units(),Some([0xd800].as_slice()));
    for (value,expected) in values.borrow().iter().zip(expected){
        for storage in [false,true]{
            let mut visitor=KeyVisitor::default();
            let result=if storage{value.visit_storage_key(&mut visitor)}else{value.visit_property_key(&mut visitor)};
            match &expected{
                Ok(expected)=>{result.unwrap();assert_eq!(visitor.result.as_ref(),Some(expected));}
                Err(expected)=>{assert_eq!(result,Err((*expected).into()));assert_eq!(visitor.result,None);}
            }
        }
    }
    let mut visitor=KeyVisitor::default();
    VariableValue::undefined(ValueLimits::default()).visit_storage_key(&mut visitor).unwrap();
    assert_eq!(visitor.result,Some(KeyPrimitive::Undefined));
}

#[test]
fn storage_key_conversion_refuses_before_publishing_when_work_or_allocation_is_exhausted() {
    let value=VariableValue::array(vec![VariableValue::scalar(Scalar::String("bounded".into()),ValueLimits::default())],ValueLimits::default());
    let mut visitor=KeyVisitor {remaining_work:0,..KeyVisitor::default()};
    assert_eq!(value.visit_storage_key(&mut visitor),Err("key work refused".into()));
    assert_eq!(visitor.result,None);
    let mut visitor=KeyVisitor {remaining_bytes:0,..KeyVisitor::default()};
    assert_eq!(value.visit_storage_key(&mut visitor),Err("key allocation refused".into()));
    assert_eq!(visitor.result,None);
    let mut visitor=KeyVisitor::default();
    value.visit_storage_key(&mut visitor).unwrap();
    assert_eq!(visitor.result,Some(KeyPrimitive::Text("bounded".encode_utf16().collect())));
}

#[test]
fn raw_key_metadata_admits_utf16_comparison_work_without_coercion() {
    let text=VariableValue::scalar(Scalar::String("😀x".into()),ValueLimits::default());
    let shorter=VariableValue::scalar(Scalar::String("😀".into()),ValueLimits::default());
    let array=VariableValue::array(vec![text.clone()],ValueLimits::default());
    let number=VariableValue::scalar(Scalar::Number(7.0),ValueLimits::default());
    assert_eq!(text.text_units(),Some([0xd83d,0xde00,120].as_slice()));
    assert_eq!(array.text_units(),None);
    assert_eq!(number.text_units(),None);
    assert_eq!(text.key_comparison_work(&shorter),3);
    assert_eq!(shorter.key_comparison_work(&text),3);
    assert_eq!(text.key_comparison_work(&array),1);
    assert_eq!(array.key_comparison_work(&array.clone()),1);
    assert_eq!(number.key_comparison_work(&number),1);
    let compare=|left:&VariableValue,right:&VariableValue,remaining:&mut usize|->Result<bool,&str>{
        *remaining=remaining.checked_sub(left.key_comparison_work(right)).ok_or("key comparison work refused")?;
        Ok(left.same_value_zero(right))
    };
    assert_eq!(compare(&text,&text,&mut 3),Err("key comparison work refused"));
    let mut remaining=4;
    assert_eq!(compare(&text,&text,&mut remaining),Ok(true));
    assert_eq!(remaining,0);
    assert_eq!(compare(&text,&shorter,&mut 3),Ok(false));
    assert_eq!(compare(&array,&array.clone(),&mut 1),Ok(true));
    assert_eq!(compare(&text,&number,&mut 1),Ok(false));
}

#[test]
fn constructed_utf16_text_keeps_exact_units_and_lease_through_the_existing_lua_encoder() {
    struct Lease(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Lease {fn drop(&mut self){self.0.store(true,std::sync::atomic::Ordering::SeqCst);}}
    let released=Arc::new(std::sync::atomic::AtomicBool::new(false));
    let key=VariableValue::text(vec![0xd800],ValueLimits::default()).retain(Arc::new(Lease(released.clone())));
    assert_eq!(key.text_units(),Some([0xd800].as_slice()));
    assert!(key.same_value_zero(&VariableValue::text(vec![0xd800],ValueLimits::default())));
    assert!(!key.same_value_zero(&VariableValue::text(vec![0xfffd],ValueLimits::default())));
    assert_eq!(key.key_comparison_work(&key),2);
    let mut visitor=KeyVisitor::default();
    key.visit_storage_key(&mut visitor).unwrap();
    assert_eq!(visitor.result,Some(KeyPrimitive::Text(vec![0xd800])));
    let mut runtime=Runtime::new(Limits::default()).unwrap();
    let retained=key.clone();
    runtime.register_variable_api("isolated",||Ok("active".into()),move |_,_|{
        assert_eq!(retained.text_units(),Some([0xd800].as_slice()));
        Ok(VariableReturn::One(retained.clone()))
    },|_|Ok(Box::new(()))).unwrap();
    for (name,units) in [("pair",vec![0xd83d,0xde00]),("nfd",vec![101,769])] {
        runtime.register_variable_api(name,||Ok("active".into()),move |_,_|{
            Ok(VariableReturn::One(VariableValue::text(units.clone(),ValueLimits::default())))
        },|_|Ok(Box::new(()))).unwrap();
    }
    drop(key);
    assert!(!released.load(std::sync::atomic::Ordering::SeqCst));
    source(&mut runtime,r#"function run()
        local value=isolated()
        assert(#value==4 and string.byte(value,1)==240 and string.byte(value,2)==144
            and string.byte(value,3)==128 and string.byte(value,4)==128)
        assert(pair()==utf8.char(128512))
        assert(nfd()=='e' .. utf8.char(769))
        return true
    end"#);
    assert_eq!(invoke(&mut runtime),Step::ScalarComplete(Scalar::Boolean(true)));
    assert!(!released.load(std::sync::atomic::Ordering::SeqCst));
    drop(runtime);
    assert!(released.load(std::sync::atomic::Ordering::SeqCst));
}
