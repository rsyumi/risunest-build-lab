mod terminal;
mod conversion;
mod allocations;
use risunest_native_lua_experimental::{Cancellation, Limits, Runtime, Step, Terminal};
use serde_json::{json, Value as Json};
use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

fn emit(value: Json) {
    println!("{value}");
    io::stdout().flush().unwrap();
}

fn finish(runtime: &mut Runtime) -> Step {
    loop {
        let result = runtime.step();
        if result != Step::Suspended { return result; }
    }
}

fn start(runtime: &mut Runtime, source: &str, duration: Duration) -> Cancellation {
    let cancellation = Cancellation::default();
    runtime.start(source, duration, cancellation.clone()).unwrap();
    cancellation
}

fn complete(runtime: &mut Runtime, source: &str, expected: Json) {
    start(runtime, source, Duration::from_secs(1));
    assert_eq!(finish(runtime), Step::Complete(expected));
}

fn probe(name: &str) {
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    let duration = Duration::from_millis(40);
    let began = Instant::now();
    match name {
        "conversion-alias" | "conversion-limits" | "conversion-parity" | "conversion-reverse" | "conversion-fanout" => {
            runtime.dispose();
            conversion::probe(name);
        }
        "drop-reentrant" | "drop-native" | "allocator-fault" | "retire-churn" | "load-binary-reader" | "gc-native-before" | "gc-native-after" | "drop-reset" | "allocator-init" | "hook-relocation" | "destroy-allocation" => {
            runtime.dispose();
            terminal::probe(name);
        }
        "idle-registration" => {
            let effects = Rc::new(std::cell::Cell::new(0));
            let observed = effects.clone();
            runtime.register_sync("mark", move |_, _| { observed.set(observed.get()+1); Ok(Json::Null) }).unwrap();
            complete(&mut runtime, r#"
                collectgarbage('stop')
                doomed=setmetatable({}, {__gc=function() pcall(mark); while true do end end})
                doomed=nil
                setmetatable(_G,{__newindex=function() while true do end end})
                collectgarbage('restart')
                return true
            "#, json!(true));
            for i in 0..128 { runtime.register_sync(&format!("fresh{i}"), |_, _| Ok(json!(42))).unwrap(); }
            runtime.register_sync("fresh", |_, _| Ok(json!(42))).unwrap();
            assert_eq!(effects.get(), 0);
            // Collection may occur during compilation or the explicit Lua call.
            let _ = runtime.start("collectgarbage('collect'); return fresh()", duration, Cancellation::default());
            assert_eq!(finish(&mut runtime), Step::Terminal(Terminal::Deadline));
            assert_eq!(effects.get(), 1);
            assert!(runtime.start("return 42", duration, Cancellation::default()).is_err());
            runtime.dispose();
            let mut clean = Runtime::new(Limits::default()).unwrap();
            clean.register_sync("fresh", |_, _| Ok(json!(42))).unwrap();
            complete(&mut clean, "return fresh()", json!(42));
            clean.dispose();
        }
        "callback-resources" => {
            let drops = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let resource = terminal::CountDrop(drops.clone());
            runtime.register_sync("resource", move |_, _| { let _ = &resource; Ok(Json::Null) }).unwrap();
            start(&mut runtime, "resource(); return hostAwait('pending', {42})", Duration::from_secs(1));
            assert!(matches!(finish(&mut runtime), Step::Awaiting(_)));
            runtime.dispose();
            assert_eq!(drops.load(std::sync::atomic::Ordering::SeqCst), 1);
        }
        "text-load-admission" => {
            complete(&mut runtime, r#"
                local f=assert(load('return 42')); assert(f()==42)
                local pieces={'return ', '42'}; local i=0
                local f=assert(load(function() i=i+1; return pieces[i] end)); assert(f()==42)
                assert(load('return value','test','t',{value=42})()==42)
                local bytes=string.dump(function() return 42 end)
                local f,e=load(bytes); assert(f==nil and type(e)=='string')
                f,e=load(bytes,'test','b'); assert(f==nil and type(e)=='string')
                f,e=load(function() return bytes end); assert(f==nil and type(e)=='string')
                warn('@on'); warn('synthetic diagnostic'); warn('@off')
                return not pcall(warn,nil)
            "#, json!(true));
            runtime.dispose();
        }
        "finite-native" => {
            complete(&mut runtime, r#"
                assert(string.rep('',10000)=='')
                assert(('abcdef'):reverse()=='fedcba' and ('AbC'):lower()=='abc')
                assert(('aAbB'):upper()=='AABB')
                assert(string.match('123abc','%d+(%a+)')=='abc')
                assert(string.find('abcabc','bc',1,true)==2)
                local s,n=string.gsub('abab','a','xx'); assert(s=='xxbxxb' and n==2)
                assert(string.format('%q %d','abc',42)=='"abc" 42')
                local a,b=string.unpack('<i4',string.pack('<i4',42)); assert(a==42 and b==5)
                local t=setmetatable({}, {__len=function() return 4096 end})
                table.insert(t,1,nil); table.remove(t,1); table.move({},1,4096,1,t)
                assert(utf8.len('abc')==3 and utf8.offset('abc',3)==3)
                local f,s=utf8.codes('a'..string.rep('\128',4096)); pcall(f,s,1)
                local ok,e=pcall(function() error('finite') end); assert(not ok and e:find('finite'))
                local ok,v=xpcall(function() error('finite') end,function() return 42 end)
                return not ok and v==42
            "#, json!(true));
            runtime.dispose();
        }
        "finite-gc-modes" => {
            complete(&mut runtime, r#"
                for _,mode in ipairs({'incremental','generational'}) do
                    collectgarbage(mode)
                    local seen=0
                    for i=1,100 do local v=setmetatable({}, {__gc=function(x) seen=seen+1; resurrect=x end}) end
                    collectgarbage('collect'); assert(seen==100)
                    resurrect=nil; collectgarbage('collect'); assert(seen==100)
                    local weak=setmetatable({}, {__mode='v'}); weak[1]={}; collectgarbage('collect'); assert(weak[1]==nil)
                end
                return true
            "#, json!(true));
            runtime.dispose();
        }
        "retention" => {
            complete(&mut runtime, "counter=(counter or 0)+1; return counter", json!(1));
            complete(&mut runtime, "counter=counter+1; return counter", json!(2));
            runtime.dispose();
            let mut fresh = Runtime::new(Limits::default()).unwrap();
            complete(&mut fresh, "return counter == nil", json!(true));
            fresh.dispose();
        }
        "finite-coroutine-resume" => {
            complete(&mut runtime, r#"
                local pure=coroutine.create(function()
                    local sum=0; for i=1,5000 do sum=sum+i end
                    return sum,"complete"
                end)
                local ok,sum,tag=coroutine.resume(pure)
                assert(ok and sum==12502500 and tag=="complete" and coroutine.status(pure)=="dead")
                local child=coroutine.create(function()
                    local total=0; for i=1,5000 do total=total+i end
                    local reply,number=coroutine.yield("pause",total,nil,"tail")
                    for i=1,5000 do total=total+i end
                    local confirmation=coroutine.yield()
                    return reply,number,total,confirmation
                end)
                local first=table.pack(coroutine.resume(child))
                assert(first.n==5 and first[1] and first[2]=="pause" and first[3]==12502500 and first[4]==nil and first[5]=="tail")
                assert(coroutine.status(child)=="suspended")
                local second=table.pack(coroutine.resume(child,"resumed",42))
                assert(second.n==1 and second[1] and coroutine.status(child)=="suspended")
                local third=table.pack(coroutine.resume(child,"confirmed"))
                assert(third.n==5 and third[1] and third[2]=="resumed" and third[3]==42 and third[4]==25005000 and third[5]=="confirmed")
                return coroutine.status(child)=="dead"
            "#, json!(true));
            runtime.dispose();
        }
        "finite-coroutine-wrap" => {
            complete(&mut runtime, r#"
                local pure=coroutine.wrap(function()
                    local sum=0; for i=1,5000 do sum=sum+i end
                    return sum,"complete"
                end)
                local sum,tag=pure()
                assert(sum==12502500 and tag=="complete")
                local child=coroutine.wrap(function()
                    local total=0; for i=1,5000 do total=total+i end
                    local reply,number=coroutine.yield("pause",total,nil,"tail")
                    for i=1,5000 do total=total+i end
                    local confirmation=coroutine.yield()
                    return reply,number,total,confirmation
                end)
                local first=table.pack(child())
                assert(first.n==4 and first[1]=="pause" and first[2]==12502500 and first[3]==nil and first[4]=="tail")
                local second=table.pack(child("resumed",42))
                assert(second.n==0)
                local third=table.pack(child("confirmed"))
                assert(third.n==4 and third[1]=="resumed" and third[2]==42 and third[3]==25005000 and third[4]=="confirmed")
                return not pcall(child)
            "#, json!(true));
            runtime.dispose();
        }
        "finite-coroutine-nested" => {
            complete(&mut runtime, r#"
                local outer=coroutine.create(function()
                    local inner=coroutine.wrap(function()
                        local sum=0; for i=1,5000 do sum=sum+i end
                        local value=coroutine.yield(sum)
                        for i=1,5000 do sum=sum+i end
                        return sum,value
                    end)
                    assert(inner()==12502500)
                    local sum,value=inner("nested")
                    return sum,value
                end)
                local ok,sum,value=coroutine.resume(outer)
                return ok and sum==25005000 and value=="nested" and coroutine.status(outer)=="dead"
            "#, json!(true));
            runtime.dispose();
        }
        "sync-callbacks" => {
            let state = Rc::new(RefCell::new(json!({"value": 3})));
            let callback_state = state.clone();
            runtime.register_sync("nativeGet", move |_, _| Ok(callback_state.borrow().clone())).unwrap();
            complete(&mut runtime, r#"
                local first=nativeGet(); first.value=99
                local text=string.gsub("abc", ".", function() return tostring(nativeGet().value) end)
                local order={3,1,2}
                table.sort(order,function(a,b) return a*nativeGet().value < b*nativeGet().value end)
                local object=setmetatable({}, {__index=function() return nativeGet().value end})
                return {text=text, fresh=nativeGet().value, first=first.value, order=order, meta=object.x}
            "#, json!({"text":"333","fresh":3,"first":99,"order":[1,2,3],"meta":3}));
            assert_eq!(*state.borrow(), json!({"value":3}));
            runtime.dispose();
        }
        "await" | "await-reject" | "await-cancel" | "await-deadline" => {
            let state = Rc::new(RefCell::new(json!({"value": 0})));
            let callback_state = state.clone();
            runtime.register_sync("nativeGet", move |_, _| Ok(callback_state.borrow().clone())).unwrap();
            let source = if name == "await-reject" {
                r#"local ok,value=pcall(hostAwait,"fetch",7); return {ok=ok, rejected=string.find(value,"service-rejected",1,true) ~= nil}"#
            } else {
                r#"local before=nativeGet().value; local value=hostAwait("fetch",7); return {before=before,after=nativeGet().value,reply=value}"#
            };
            let token = start(&mut runtime, source, if name == "await-deadline" { duration } else { Duration::from_secs(1) });
            let Step::Awaiting(request) = finish(&mut runtime) else { panic!("request did not suspend"); };
            assert_eq!(request.service, "fetch");
            assert_eq!(request.arguments, vec![json!(7)]);
            assert_eq!(runtime.step(), Step::Awaiting(request.clone()));
            assert!(runtime.reply(request.id + 1, Ok(Json::Null)).is_err());
            // The synchronous callback's borrow is gone while the host is pending.
            *state.borrow_mut() = json!({"value": 1});
            if name == "await-cancel" {
                token.cancel();
                assert!(runtime.reply(request.id, Ok(json!("late"))).is_err());
                assert_eq!(runtime.step(), Step::Terminal(Terminal::Cancelled));
                assert!(runtime.start("return 1", duration, Cancellation::default()).is_err());
            } else if name == "await-deadline" {
                let result = loop {
                    match runtime.step() {
                        Step::Awaiting(_) => std::thread::yield_now(),
                        result => break result,
                    }
                };
                assert_eq!(result, Step::Terminal(Terminal::Deadline));
                assert!(runtime.reply(request.id, Ok(json!("late"))).is_err());
            } else if name == "await-reject" {
                runtime.reply(request.id, Err("service-rejected".into())).unwrap();
                assert_eq!(finish(&mut runtime), Step::Complete(json!({"ok":false,"rejected":true})));
            } else {
                runtime.reply(request.id, Ok(json!("reply"))).unwrap();
                assert_eq!(finish(&mut runtime), Step::Complete(json!({"before":0,"after":1,"reply":"reply"})));
                assert!(runtime.reply(request.id, Ok(json!("duplicate"))).is_err());
            }
            runtime.dispose();
        }
        "non-yieldable-await" => {
            complete(&mut runtime, r#"
                local ok,err=pcall(function()
                    string.gsub("a",".",function() return hostAwait("fetch") end)
                end)
                return {ok=ok,rejected=string.find(err,"hostAwait requires",1,true) ~= nil}
            "#, json!({"ok":false,"rejected":true}));
            runtime.dispose();
        }
        "nested-await" => {
            complete(&mut runtime, r#"
                local c=coroutine.create(function()
                    local sum=0; for i=1,5000 do sum=sum+i end
                    return hostAwait("fetch",sum)
                end)
                local ok,err=coroutine.resume(c)
                return {ok=ok,rejected=string.find(tostring(err),"hostAwait requires the entry",1,true) ~= nil,dead=coroutine.status(c)=="dead"}
            "#, json!({"ok":false,"rejected":true,"dead":true}));
            start(&mut runtime, "return hostAwait('after-nested')", Duration::from_secs(1));
            let Step::Awaiting(request) = finish(&mut runtime) else { panic!("entry request did not suspend"); };
            assert_eq!(request.service, "after-nested");
            runtime.reply(request.id, Ok(json!(42))).unwrap();
            assert_eq!(finish(&mut runtime), Step::Complete(json!(42)));
            runtime.dispose();
        }
        "cancel-loop" => {
            let token = start(&mut runtime, "while true do end", Duration::from_secs(1));
            assert_eq!(runtime.step(), Step::Suspended);
            token.cancel();
            assert_eq!(runtime.step(), Step::Terminal(Terminal::Cancelled));
            runtime.dispose();
        }
        "cancel-callback" => {
            let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
            let callback_gate = gate.clone();
            runtime.register_sync("signal", move |_, _| {
                callback_gate.wait();
                callback_gate.wait();
                Ok(Json::Null)
            }).unwrap();
            let token = start(&mut runtime, "string.gsub('a','.',function() signal(); while true do end end)", Duration::from_secs(1));
            let canceller = std::thread::spawn(move || {
                gate.wait();
                token.cancel();
                gate.wait();
            });
            assert_eq!(finish(&mut runtime), Step::Terminal(Terminal::Cancelled));
            canceller.join().unwrap();
            runtime.dispose();
        }
        "compile-recovery" => {
            assert!(runtime.start("local =", duration, Cancellation::default()).is_err());
            complete(&mut runtime, "return 42", json!(42));
            runtime.dispose();
        }
        "allocation" => {
            start(&mut runtime, "return string.rep('x',256*1024*1024)", Duration::from_secs(1));
            let Step::Failed(error) = finish(&mut runtime) else { panic!("allocation was not rejected"); };
            assert!(error.to_lowercase().contains("memory"));
            complete(&mut runtime, "return 42", json!(42));
            runtime.dispose();
        }
        "error-recovery" => {
            start(&mut runtime, "error('synthetic')", Duration::from_secs(1));
            assert!(matches!(finish(&mut runtime), Step::Failed(_)));
            complete(&mut runtime, "return 42", json!(42));
            runtime.dispose();
        }
        "finite-finalizer" => {
            complete(&mut runtime, r#"
                local object=setmetatable({}, {__gc=function() collected=true end})
                object=nil; collectgarbage("collect"); return collected
            "#, json!(true));
            runtime.dispose();
        }
        "dispose-pending" => {
            start(&mut runtime, "return hostAwait('fetch')", Duration::from_secs(1));
            assert!(matches!(finish(&mut runtime), Step::Awaiting(_)));
            runtime.dispose();
        }
        "dispose-finalizer" => {
            complete(&mut runtime, "object=setmetatable({}, {__gc=function() while true do end end}); return true", json!(true));
            emit(json!({"phase":"dispose","case":name}));
            runtime.dispose();
        }
        "churn" => {
            runtime.dispose();
            let mut maximum = 0;
            for _ in 0..30 {
                let mut runtime = Runtime::new(Limits::default()).unwrap();
                complete(&mut runtime, "saved=string.rep('x',256*1024); return #saved", json!(256*1024));
                maximum = maximum.max(runtime.used_memory());
                runtime.dispose();
            }
            emit(json!({"phase":"diagnostic","case":name,"maximumLuaBytes":maximum}));
        }
        _ => {
            let source = match name {
                "loop" => "while true do end",
                "coroutine" => "local c=coroutine.create(function() while true do end end); while true do coroutine.resume(c) end",
                "nested-coroutine" => r#"
                    local outer=coroutine.create(function()
                        local inner=coroutine.create(function() while true do end end)
                        while true do coroutine.resume(inner) end
                    end)
                    while true do coroutine.resume(outer) end
                "#,
                "pcall" => "while true do pcall(function() while true do end end) end",
                "xpcall" => "while true do xpcall(function() while true do end end,function(e) return e end) end",
                "gsub" => "string.gsub('a','.',function() while true do end end)",
                "gsub-catch" => "string.gsub('a','.',function() while true do pcall(function() while true do end end) end end)",
                "sort" => "table.sort({3,2,1},function() while true do end end)",
                "metamethod" => "local t=setmetatable({}, {__index=function() while true do end end}); return t.x",
                "error-handler" => "xpcall(function() error('x') end,function() while true do end end)",
                "close-handler" => "local value <close> = setmetatable({}, {__close=function() while true do end end}); return 1",
                "gc-handler" => "local object=setmetatable({}, {__gc=function() while true do end end}); object=nil; collectgarbage('collect')",
                "empty-rep" => "return string.rep('',math.maxinteger)",
                "table-move-range" => "return table.move({},1,math.maxinteger,1,{})",
                "table-insert-range" => "local t=setmetatable({}, {__len=function() return math.maxinteger-2 end}); table.insert(t,1,nil)",
                "table-remove-range" => "local t=setmetatable({}, {__len=function() return math.maxinteger-2 end}); table.remove(t,1)",
                "table-concat-range" => "return table.concat(setmetatable({}, {__index=function() return '' end}), '',1,math.maxinteger)",
                "load-reader" => "return load(function() return ' ' end)",
                "gc-generational" => "collectgarbage('generational'); local x=setmetatable({}, {__gc=function() while true do end end}); x=nil; collectgarbage('collect')",
                "native-pattern" => "return string.match(string.rep('a',200), string.rep('a*',20)..'z')",
                _ => panic!("unknown case"),
            };
            start(&mut runtime, source, duration);
            let result = finish(&mut runtime);
            assert_eq!(result, Step::Terminal(Terminal::Deadline));
            assert!(runtime.start("return 1", duration, Cancellation::default()).is_err());
            emit(json!({"phase":"dispose","case":name}));
            runtime.dispose();
        }
    }
    emit(json!({"phase":"complete","case":name,"status":"passed","elapsedMs":began.elapsed().as_secs_f64()*1000.0}));
}

fn main() {
    let name = std::env::args().nth(1).expect("one synthetic case name required");
    emit(json!({"phase":"entered","case":name,"lua":"5.4.9","mlua":"0.12.2"}));
    probe(&name);
}
