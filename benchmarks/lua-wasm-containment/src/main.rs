mod runtime;

use runtime::{Compiled, Observed, Runtime, Step, Terminal, ENGINE_CONFIG, NORMAL_FUEL};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::sync::{atomic::{AtomicUsize, Ordering}, mpsc, Arc};
use std::time::{Duration, Instant};
use wasmtime::{bail, Result, Trap};

fn emit(value: Value) {
    println!("{value}");
    std::io::stdout().flush().unwrap();
}

fn complete(runtime: &mut Runtime, source: &str, expected: i32) -> Result<()> {
    runtime.arm(NORMAL_FUEL, Duration::from_secs(1))?;
    assert_eq!(runtime.start(source)?, Step::Complete(expected));
    Ok(())
}

// The service waits outside Wasm. Dropping its owner cancels and joins it.
struct PendingService {
    cancel: mpsc::Sender<()>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl PendingService {
    fn new(drops: Arc<AtomicUsize>) -> Self {
        let (cancel, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = receiver.recv();
            drops.fetch_add(1, Ordering::AcqRel);
        });
        Self { cancel, worker: Some(worker) }
    }
}
impl Drop for PendingService {
    fn drop(&mut self) {
        let _ = self.cancel.send(());
        self.worker.take().unwrap().join().unwrap();
    }
}

fn terminal_name(reason: Terminal) -> &'static str {
    match reason { Terminal::Fuel => "fuel", Terminal::Deadline => "deadline",
        Terminal::Cancelled => "cancelled", Terminal::EngineTrap => "engine-trap" }
}

fn probe(name: &str, compiled: &Compiled) -> Result<(&'static str, &'static str)> {
    let observed = Arc::new(Observed::default());
    if name == "cold-init-fuel" {
        let error = match Runtime::new(compiled, observed, 1) {
            Ok(_) => panic!("cold initialization ignored fuel"), Err(error) => error,
        };
        assert!(matches!(error.downcast_ref::<Trap>(), Some(Trap::OutOfFuel)));
        return Ok(("passed", "fuel"));
    }
    let mut runtime = Runtime::new(compiled, observed.clone(), NORMAL_FUEL)?;
    match name {
        "retention" => {
            complete(&mut runtime, "saved=(saved or 0)+1; return saved", 1)?;
            complete(&mut runtime, "saved=(saved or 0)+1; return saved", 2)?;
            drop(runtime);
            let mut fresh = Runtime::new(compiled, observed, NORMAL_FUEL)?;
            complete(&mut fresh, "return saved or 0", 0)?;
        },
        "nested-errors" => complete(&mut runtime, r#"
            local ok, value=pcall(function()
                local inner, reason=pcall(function() error('synthetic') end)
                assert(not inner and reason:find('synthetic',1,true))
                error('outer')
            end)
            assert(not ok and value:find('outer',1,true))
            local good, handled=xpcall(function() error('inner') end,function() return 42 end)
            assert(not good and handled==42)
            return 42
        "#, 42)?,
        "coroutine-yield" => complete(&mut runtime, r#"
            local child=coroutine.create(function()
                local reply=coroutine.yield('pause',nil,'tail')
                return reply+1
            end)
            local first=table.pack(coroutine.resume(child))
            assert(first.n==4 and first[1] and first[2]=='pause' and first[3]==nil and first[4]=='tail')
            local ok,value=coroutine.resume(child,41)
            assert(ok and coroutine.status(child)=='dead')
            return value
        "#, 42)?,
        "entry-yield" => {
            runtime.arm(NORMAL_FUEL, Duration::from_secs(1))?;
            assert_eq!(runtime.start("return coroutine.yield(7)+1")?, Step::Yielded);
            assert_eq!(runtime.resume(41)?, Step::Complete(42));
        },
        "async-reentry" => {
            observed.value.store(1, Ordering::Release);
            runtime.arm(NORMAL_FUEL, Duration::from_secs(1))?;
            let step = runtime.start(r#"
                callback=function(value) return hostGet()+value end
                local before=hostGet()
                local reply=hostAwait()
                return before+hostReenter(reply)
            "#)?;
            let Step::Awaiting(id) = step else { panic!("expected host request"); };
            assert!(runtime.reply(id + 1, true, 32).is_err());
            observed.value.store(9, Ordering::Release);
            assert_eq!(runtime.reply(id, true, 32)?, Step::Complete(42));
            assert!(runtime.reply(id, true, 32).is_err());
        },
        "non-yieldable-await" => {
            complete(&mut runtime, r#"
                local ok,reason=pcall(function()
                    return string.gsub('a','.',function() return hostAwait() end)
                end)
                assert(not ok and reason:find('hostAwait requires a yieldable entry coroutine',1,true))
                return 42
            "#, 42)?;
            return Ok(("limitation-confirmed", "non-yieldable-await-rejected"));
        },
        "await-reject-reuse" => {
            runtime.arm(NORMAL_FUEL, Duration::from_secs(1))?;
            let Step::Awaiting(first) = runtime.start(r#"
                local ok,reason=pcall(hostAwait)
                assert(not ok and reason:find('synthetic host rejection',1,true))
                return hostAwait()+1
            "#)? else { panic!("expected first request"); };
            let Step::Awaiting(second) = runtime.reply(first, false, 0)?
                else { panic!("rejected await did not reach second request"); };
            assert!(second > first);
            assert!(runtime.reply(first, true, 999).is_err());
            assert_eq!(runtime.reply(second, true, 41)?, Step::Complete(42));
        },
        "invocation-state" => {
            let stale = runtime.arm(NORMAL_FUEL, Duration::from_secs(1))?;
            assert!(runtime.resume(0).is_err());
            assert!(runtime.arm(NORMAL_FUEL, Duration::from_secs(1)).is_err());
            let Step::Awaiting(id) = runtime.start("return hostAwait()")?
                else { panic!("expected pending request"); };
            assert!(runtime.start("return 999").is_err());
            assert_eq!(runtime.reply(id, true, 42)?, Step::Complete(42));
            runtime.arm(NORMAL_FUEL, Duration::from_secs(1))?;
            stale.cancel();
            assert_eq!(runtime.start("return 42")?, Step::Complete(42));
            assert!(runtime.start("return 999").is_err());
        },
        "ordinary-error-recovery" => {
            runtime.arm(NORMAL_FUEL, Duration::from_secs(1))?;
            assert_eq!(runtime.start("error('synthetic')")?, Step::LuaError);
            complete(&mut runtime, "return 42", 42)?;
        },
        "cancel-running" => {
            let token = runtime.arm(u64::MAX / 2, Duration::from_secs(1))?;
            let signal = observed.clone();
            let canceller = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(1);
                while !signal.signalled.load(Ordering::Acquire) {
                    assert!(Instant::now() < deadline, "guest did not signal");
                    std::thread::yield_now();
                }
                token.cancel();
            });
            let result = runtime.start("hostAcquire(); hostSignal(); while true do pcall(function() while true do end end) end")?;
            canceller.join().unwrap();
            assert_eq!(result, Step::Terminal(Terminal::Cancelled));
            assert!(runtime.retired());
            assert_eq!(*observed.drops.lock().unwrap(), [(1, 1)].into());
            return Ok(("passed", "cancelled"));
        },
        "cancel-pending" | "deadline-pending" => {
            let token = runtime.arm(NORMAL_FUEL, if name == "deadline-pending" {
                Duration::from_millis(40)
            } else { Duration::from_secs(1) })?;
            let Step::Awaiting(id) = runtime.start("hostAcquire(); hostCommit(7); return hostAwait()")?
                else { panic!("expected pending request"); };
            let service_drops = Arc::new(AtomicUsize::new(0));
            let service = PendingService::new(service_drops.clone());
            let expected = if name == "cancel-pending" { token.cancel(); Terminal::Cancelled }
                else { Terminal::Deadline };
            let until = Instant::now() + Duration::from_secs(1);
            let terminal = loop {
                if let Some(step) = runtime.poll() { break step; }
                assert!(Instant::now() < until, "pending budget did not terminate");
                std::thread::yield_now();
            };
            assert_eq!(terminal, Step::Terminal(expected));
            drop(service);
            assert_eq!(service_drops.load(Ordering::Acquire), 1);
            assert!(runtime.reply(id, true, 42).is_err());
            assert_eq!(*observed.drops.lock().unwrap(), [(1, 1)].into());
            assert_eq!(*observed.effects.lock().unwrap(), [7]);
            return Ok(("passed", terminal_name(expected)));
        },
        "dispose-finalizer" => {
            complete(&mut runtime, r#"
                hostAcquire(); hostCommit(7)
                object=setmetatable({}, {__gc=function() hostCommit(999); while true do end end})
                return 42
            "#, 42)?;
            drop(runtime);
            assert_eq!(*observed.effects.lock().unwrap(), [7]);
            assert_eq!(*observed.drops.lock().unwrap(), [(1, 1)].into());
            return Ok(("passed", "store-dropped"));
        },
        "deadline-native-pattern" | "deadline-gc-finalizer" => {
            let setup = "local text=string.rep('a',200); local pattern=string.rep('a*',20)..'z';";
            let source = setup.to_string() + if name == "deadline-native-pattern" {
                "hostSignal(); return string.match(text,pattern)"
            } else {
                "local o=setmetatable({}, {__gc=function() hostSignal(); string.match(text,pattern) end}); o=nil; collectgarbage('collect'); return 0"
            };
            runtime.arm(u64::MAX / 2, Duration::from_millis(40))?;
            assert_eq!(runtime.start(&source)?, Step::Terminal(Terminal::Deadline));
            assert!(observed.signalled.load(Ordering::Acquire), "workload phase was not reached");
            let failure = runtime.failure().expect("deadline did not trap guest execution");
            assert!(matches!(failure.trap, Some(Trap::Interrupt)), "deadline was not an epoch interrupt");
            assert!(failure.fuel_remaining.is_some_and(|fuel| fuel > 0), "fuel was exhausted or unreadable");
            assert!(runtime.retired());
            assert!(runtime.arm(NORMAL_FUEL, Duration::from_secs(1)).is_err());
            return Ok(("passed", "epoch-deadline"));
        },
        _ => {
            let source = match name {
                "nested-pcall" => "hostSignal(); while true do pcall(function() while true do pcall(function() while true do end end) end end) end".into(),
                "error-handler" => "xpcall(function() error('synthetic') end,function() hostSignal(); while true do end end)".into(),
                "reentry-loop" => "callback=function() hostSignal(); while true do end end; return hostReenter(0)".into(),
                "endless-reader" => "return load(function() hostSignal(); return ' ' end)".into(),
                "compiler" => "local a=0;".to_string() + &"a=a+1;".repeat(50_000) + "return a",
                "native-pattern" => "local text=string.rep('a',200); local pattern=string.rep('a*',20)..'z'; hostSignal(); return string.match(text,pattern)".into(),
                "empty-string-rep" => "hostSignal(); assert(string.rep('',2147483647)==''); return 42".into(),
                "virtual-table" => "local t=setmetatable({}, {__len=function() return 2147483647 end,__index=function() hostSignal(); return '' end}); return #table.concat(t)".into(),
                "gc-finalizer" => "local o=setmetatable({}, {__gc=function() hostSignal(); while true do end end}); o=nil; collectgarbage('collect'); return 0".into(),
                "handles-effects" => "local a=hostAcquire(); hostAcquire(); assert(hostRelease(a)); assert(not hostRelease(a)); hostCommit(7); hostSignal(); while true do pcall(function() while true do end end) end".into(),
                _ => bail!("unknown synthetic case"),
            };
            runtime.arm(if name == "compiler" { 10_000 } else { 5_000_000 }, Duration::from_millis(40))?;
            let step = runtime.start(&source)?;
            assert!(if name == "compiler" { observed.compiling.load(Ordering::Acquire) }
                else { observed.signalled.load(Ordering::Acquire) }, "workload phase was not reached");
            if name == "empty-string-rep" && step == Step::Complete(42) {
                return Ok(("passed", "completed-bounded"));
            }
            let Step::Terminal(reason @ (Terminal::Fuel | Terminal::Deadline)) = step
                else { panic!("containment returned unexpected result: {step:?}"); };
            assert!(runtime.retired());
            assert!(runtime.arm(NORMAL_FUEL, Duration::from_secs(1)).is_err());
            drop(runtime);
            if name == "handles-effects" {
                assert_eq!(*observed.drops.lock().unwrap(), [(1, 1), (2, 1)].into());
                assert_eq!(*observed.effects.lock().unwrap(), [7]);
                let mut fresh = Runtime::new(compiled, observed.clone(), NORMAL_FUEL)?;
                complete(&mut fresh, "return 42", 42)?;
                assert_eq!(*observed.effects.lock().unwrap(), [7]);
                assert_eq!(*observed.drops.lock().unwrap(), [(1, 1), (2, 1)].into());
            }
            return Ok(("passed", terminal_name(reason)));
        },
    }
    Ok(("passed", "completed"))
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 { bail!("expected guest.wasm backend case"); }
    let bytes = std::fs::read(&args[0])?;
    if bytes.len() > 4 * 1024 * 1024 { bail!("guest module exceeds experiment input limit"); }
    let name = &args[2];
    emit(json!({"phase":"entered", "case":name, "lua":"5.4.9", "wasmtime":"48.0.7",
        "backend":args[1], "engineConfig":ENGINE_CONFIG, "guestSha256":format!("{:x}", Sha256::digest(&bytes))}));
    let compiled = Compiled::new(&bytes, &args[1])?;
    let began = Instant::now();
    let (status, outcome) = probe(name, &compiled)?;
    emit(json!({"phase":"complete", "case":name, "status":status, "outcome":outcome,
        "elapsedMs":began.elapsed().as_secs_f64() * 1000.0}));
    Ok(())
}
