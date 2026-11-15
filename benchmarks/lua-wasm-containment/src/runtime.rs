use std::collections::BTreeMap;
use std::sync::{atomic::{AtomicBool, AtomicI32, Ordering}, mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use wasmtime::{bail, Caller, Collector, Config, Engine, Error, ExternType, Linker,
    Memory, Module, Result, Store, StoreLimits, StoreLimitsBuilder, Trap, TypedFunc};

pub const ENGINE_CONFIG: &str = "fuel+epoch;gc-null;wasm-gc-off;linear-memory-32MiB";
pub const NORMAL_FUEL: u64 = 100_000_000;

#[derive(Default)]
pub struct Observed {
    next_handle: AtomicI32,
    pub value: AtomicI32,
    pub signalled: AtomicBool,
    pub compiling: AtomicBool,
    pub effects: Mutex<Vec<i32>>,
    pub drops: Mutex<BTreeMap<i32, usize>>,
}

struct Handle { id: i32, observed: Arc<Observed> }
impl Drop for Handle {
    fn drop(&mut self) {
        *self.observed.drops.lock().unwrap().get_mut(&self.id).unwrap() += 1;
    }
}

struct Host {
    observed: Arc<Observed>,
    handles: BTreeMap<i32, Handle>,
    reentry_depth: usize,
    limits: StoreLimits,
}

#[derive(Clone)]
pub struct Cancellation {
    active: Arc<Mutex<bool>>,
    cancelled: Arc<AtomicBool>,
    engine: Engine,
}
impl Cancellation {
    pub fn cancel(&self) {
        let active = self.active.lock().unwrap();
        if *active {
            self.cancelled.store(true, Ordering::Release);
            self.engine.increment_epoch();
        }
    }
}

struct Budget {
    cancellation: Cancellation,
    expired: Arc<AtomicBool>,
    stop: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}
impl Budget {
    fn new(engine: &Engine, duration: Duration) -> Self {
        let active = Arc::new(Mutex::new(true));
        let expired = Arc::new(AtomicBool::new(false));
        let cancellation = Cancellation {
            active: active.clone(), cancelled: Arc::new(AtomicBool::new(false)), engine: engine.clone(),
        };
        let deadline_engine = engine.clone();
        let deadline_expired = expired.clone();
        let (stop, receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            if receiver.recv_timeout(duration) == Err(mpsc::RecvTimeoutError::Timeout) {
                let active = active.lock().unwrap();
                if *active {
                    deadline_expired.store(true, Ordering::Release);
                    deadline_engine.increment_epoch();
                }
            }
        });
        Self { cancellation, expired, stop, thread: Some(thread) }
    }

    fn terminal(&self) -> Option<Terminal> {
        if self.cancellation.cancelled.load(Ordering::Acquire) { Some(Terminal::Cancelled) }
        else if self.expired.load(Ordering::Acquire) { Some(Terminal::Deadline) }
        else { None }
    }

    fn finish(&self) -> Option<Terminal> {
        let mut active = self.cancellation.active.lock().unwrap();
        *active = false;
        self.terminal()
    }
}
impl Drop for Budget {
    fn drop(&mut self) {
        *self.cancellation.active.lock().unwrap() = false;
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() { thread.join().unwrap(); }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal { Fuel, Deadline, Cancelled, EngineTrap }

#[derive(Debug, PartialEq, Eq)]
pub enum Step { Complete(i32), Yielded, Awaiting(i32), LuaError, Terminal(Terminal) }

pub struct FailureEvidence {
    pub trap: Option<Trap>,
    pub fuel_remaining: Option<u64>,
}

pub struct Compiled { engine: Engine, module: Module }
impl Compiled {
    pub fn new(bytes: &[u8], backend: &str) -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true).epoch_interruption(true).wasm_exceptions(true)
            .wasm_gc(false).collector(Collector::Null).max_wasm_stack(512 * 1024);
        match backend {
            "cranelift" => {},
            "pulley" => { config.target("pulley64")?; },
            _ => bail!("unsupported engine backend"),
        }
        let engine = Engine::new(&config)?;
        let module = Module::new(&engine, bytes)?;
        for import in module.imports() {
            let permitted = match import.module() {
                "risunest_probe" => ["get", "commit", "acquire", "release", "reenter", "signal", "compiling"].contains(&import.name()),
                "wasi_snapshot_preview1" => ["clock_time_get", "environ_sizes_get", "environ_get",
                    "fd_write", "fd_read", "fd_close", "fd_seek", "proc_exit"].contains(&import.name()),
                _ => false,
            };
            if !permitted || !matches!(import.ty(), ExternType::Func(_)) {
                bail!("unsupported guest import: {}.{}", import.module(), import.name());
            }
        }
        Ok(Self { engine, module })
    }
}

fn memory(caller: &mut Caller<'_, Host>) -> Result<Memory> {
    caller.get_export("memory").and_then(|export| export.into_memory())
        .ok_or_else(|| Error::msg("missing guest memory"))
}

fn linker(engine: &Engine) -> Result<Linker<Host>> {
    let mut linker = Linker::new(engine);
    linker.func_wrap("risunest_probe", "get", |caller: Caller<'_, Host>| {
        caller.data().observed.value.load(Ordering::Acquire)
    })?;
    linker.func_wrap("risunest_probe", "commit", |caller: Caller<'_, Host>, value: i32| -> Result<()> {
        let mut effects = caller.data().observed.effects.lock().unwrap();
        if effects.len() >= 64 { bail!("synthetic host effect limit"); }
        effects.push(value);
        Ok(())
    })?;
    linker.func_wrap("risunest_probe", "acquire", |mut caller: Caller<'_, Host>| -> Result<i32> {
        let host = caller.data_mut();
        let id = host.observed.next_handle.fetch_add(1, Ordering::AcqRel) + 1;
        if id > 32 { bail!("synthetic host handle limit"); }
        host.observed.drops.lock().unwrap().insert(id, 0);
        host.handles.insert(id, Handle { id, observed: host.observed.clone() });
        Ok(id)
    })?;
    linker.func_wrap("risunest_probe", "release", |mut caller: Caller<'_, Host>, id: i32| {
        i32::from(caller.data_mut().handles.remove(&id).is_some())
    })?;
    linker.func_wrap("risunest_probe", "signal", |caller: Caller<'_, Host>| {
        caller.data().observed.signalled.store(true, Ordering::Release);
    })?;
    linker.func_wrap("risunest_probe", "compiling", |caller: Caller<'_, Host>| {
        caller.data().observed.compiling.store(true, Ordering::Release);
    })?;
    linker.func_wrap("risunest_probe", "reenter", |mut caller: Caller<'_, Host>, value: i32| -> Result<i32> {
        if caller.data().reentry_depth != 0 { bail!("synthetic reentry depth limit"); }
        let callback = caller.get_export("probe_callback").and_then(|value| value.into_func())
            .ok_or_else(|| Error::msg("missing callback export"))?.typed::<i32, i32>(&caller)?;
        caller.data_mut().reentry_depth = 1;
        // No Host borrow or host lock survives across this guest call.
        let answer = callback.call(&mut caller, value);
        caller.data_mut().reentry_depth = 0;
        answer
    })?;
    linker.func_wrap("wasi_snapshot_preview1", "clock_time_get",
        |mut caller: Caller<'_, Host>, _: i32, _: i64, pointer: i32| -> Result<i32> {
            memory(&mut caller)?.write(&mut caller, pointer as u32 as usize, &1_000_000_000_u64.to_le_bytes())?;
            Ok(0)
        })?;
    linker.func_wrap("wasi_snapshot_preview1", "environ_sizes_get",
        |mut caller: Caller<'_, Host>, count: i32, bytes: i32| -> Result<i32> {
            let memory = memory(&mut caller)?;
            memory.write(&mut caller, count as u32 as usize, &0_u32.to_le_bytes())?;
            memory.write(&mut caller, bytes as u32 as usize, &0_u32.to_le_bytes())?;
            Ok(0)
        })?;
    linker.func_wrap("wasi_snapshot_preview1", "environ_get", |_: i32, _: i32| 0_i32)?;
    linker.func_wrap("wasi_snapshot_preview1", "fd_write", |_: i32, _: i32, _: i32, _: i32| 8_i32)?;
    linker.func_wrap("wasi_snapshot_preview1", "fd_read", |_: i32, _: i32, _: i32, _: i32| 8_i32)?;
    linker.func_wrap("wasi_snapshot_preview1", "fd_close", |_: i32| 8_i32)?;
    linker.func_wrap("wasi_snapshot_preview1", "fd_seek", |_: i32, _: i64, _: i32, _: i32| 8_i32)?;
    linker.func_wrap("wasi_snapshot_preview1", "proc_exit", |_: i32| -> Result<()> { bail!("guest proc_exit") })?;
    Ok(linker)
}

pub struct Runtime {
    store: Option<Store<Host>>,
    memory: Memory,
    input: usize,
    capacity: usize,
    load: TypedFunc<i32, i32>,
    resume: TypedFunc<(i32, i32, i32), i32>,
    result: TypedFunc<(), i32>,
    pending: TypedFunc<(), i32>,
    budget: Option<Budget>,
    waiting: Option<i32>,
    active: bool,
    started: bool,
    failure: Option<FailureEvidence>,
}
impl Runtime {
    pub fn new(compiled: &Compiled, observed: Arc<Observed>, fuel: u64) -> Result<Self> {
        let limits = StoreLimitsBuilder::new().memory_size(32 * 1024 * 1024)
            .table_elements(4096).instances(1).memories(1).tables(1).build();
        let mut store = Store::new(&compiled.engine, Host {
            observed, handles: BTreeMap::new(), reentry_depth: 0, limits,
        });
        store.limiter(|host| &mut host.limits);
        store.set_fuel(fuel)?;
        store.set_epoch_deadline(1);
        store.epoch_deadline_trap();
        let budget = Budget::new(&compiled.engine, Duration::from_secs(1));
        let instance = linker(&compiled.engine)?.instantiate(&mut store, &compiled.module)?;
        instance.get_typed_func::<(), ()>(&mut store, "_initialize")?.call(&mut store, ())?;
        let init = instance.get_typed_func::<(), i32>(&mut store, "probe_init")?.call(&mut store, ())?;
        if init != 0 { bail!("guest initialization failed: {init}"); }
        let input = instance.get_typed_func::<(), i32>(&mut store, "probe_input_ptr")?.call(&mut store, ())? as u32 as usize;
        let capacity = instance.get_typed_func::<(), i32>(&mut store, "probe_input_capacity")?.call(&mut store, ())? as u32 as usize;
        let memory = instance.get_memory(&mut store, "memory").ok_or_else(|| Error::msg("missing guest memory"))?;
        let load = instance.get_typed_func(&mut store, "probe_load")?;
        let resume = instance.get_typed_func(&mut store, "probe_resume")?;
        let result = instance.get_typed_func(&mut store, "probe_result")?;
        let pending = instance.get_typed_func(&mut store, "probe_pending")?;
        if let Some(reason) = budget.finish() { bail!("guest initialization terminated: {reason:?}"); }
        drop(budget);
        Ok(Self { store: Some(store), memory, input, capacity, load, resume, result, pending,
            budget: None, waiting: None, active: false, started: false, failure: None })
    }

    pub fn arm(&mut self, fuel: u64, duration: Duration) -> Result<Cancellation> {
        if self.active { bail!("invocation already active"); }
        let store = self.store.as_mut().ok_or_else(|| Error::msg("runtime retired"))?;
        store.set_fuel(fuel)?;
        store.set_epoch_deadline(1);
        self.budget = Some(Budget::new(store.engine(), duration));
        self.active = true;
        self.started = false;
        Ok(self.budget.as_ref().unwrap().cancellation.clone())
    }

    fn retire(&mut self, reason: Terminal) -> Step {
        self.budget = None;
        self.waiting = None;
        self.active = false;
        self.started = false;
        // Drop only Rust ownership. Never call lua_close or resume a trapped guest.
        self.store = None;
        Step::Terminal(reason)
    }

    fn terminal(&self) -> Option<Terminal> { self.budget.as_ref().and_then(Budget::terminal) }

    fn failed(&mut self, error: Error) -> Step {
        self.failure = Some(FailureEvidence {
            trap: error.downcast_ref::<Trap>().copied(),
            fuel_remaining: self.store.as_ref().and_then(|store| store.get_fuel().ok()),
        });
        let reason = self.terminal().unwrap_or_else(|| match error.downcast_ref::<Trap>() {
            Some(Trap::OutOfFuel) => Terminal::Fuel,
            _ => Terminal::EngineTrap,
        });
        self.retire(reason)
    }

    pub fn start(&mut self, source: &str) -> Result<Step> {
        if !self.active { bail!("invocation not armed"); }
        if self.started { bail!("invocation already started"); }
        if let Some(reason) = self.terminal() { return Ok(self.retire(reason)); }
        if source.len() > self.capacity { bail!("source limit"); }
        self.started = true;
        let store = self.store.as_mut().unwrap();
        self.memory.write(&mut *store, self.input, source.as_bytes())?;
        match self.load.call(store, source.len() as i32) {
            Err(error) => Ok(self.failed(error)),
            Ok(0) => self.run(0, true, 0),
            Ok(_) => Ok(self.end(Step::LuaError)),
        }
    }

    fn end(&mut self, step: Step) -> Step {
        if let Some(reason) = self.budget.as_ref().and_then(Budget::finish) {
            return self.retire(reason);
        }
        self.budget = None;
        self.waiting = None;
        self.active = false;
        self.started = false;
        step
    }

    fn run(&mut self, id: i32, accepted: bool, value: i32) -> Result<Step> {
        if !self.active { bail!("invocation not active"); }
        if let Some(reason) = self.terminal() { return Ok(self.retire(reason)); }
        let store = self.store.as_mut().unwrap();
        let call = self.resume.call(&mut *store, (id, i32::from(accepted), value));
        let code = match call { Ok(code) => code, Err(error) => return Ok(self.failed(error)) };
        if let Some(reason) = self.terminal() { return Ok(self.retire(reason)); }
        match code {
            0 => {
                match self.result.call(self.store.as_mut().unwrap(), ()) {
                    Ok(value) => Ok(self.end(Step::Complete(value))),
                    Err(error) => Ok(self.failed(error)),
                }
            },
            1 => Ok(Step::Yielded),
            2 => {
                match self.pending.call(self.store.as_mut().unwrap(), ()) {
                    Ok(id) if id > 0 => { self.waiting = Some(id); Ok(Step::Awaiting(id)) },
                    Ok(_) => Ok(self.retire(Terminal::EngineTrap)),
                    Err(error) => Ok(self.failed(error)),
                }
            },
            3 => Ok(self.end(Step::LuaError)),
            _ => Ok(self.retire(Terminal::EngineTrap)),
        }
    }

    pub fn resume(&mut self, value: i32) -> Result<Step> {
        if !self.started { bail!("invocation not started"); }
        if self.waiting.is_some() { bail!("host reply required"); }
        self.run(0, true, value)
    }

    pub fn reply(&mut self, id: i32, accepted: bool, value: i32) -> Result<Step> {
        if let Some(reason) = self.terminal() { return Ok(self.retire(reason)); }
        if self.waiting != Some(id) { bail!("host reply id mismatch or runtime retired"); }
        self.waiting = None;
        self.run(id, accepted, value)
    }

    pub fn poll(&mut self) -> Option<Step> { self.terminal().map(|reason| self.retire(reason)) }
    pub fn retired(&self) -> bool { self.store.is_none() }
    pub fn failure(&self) -> Option<&FailureEvidence> { self.failure.as_ref() }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.budget = None;
        self.store = None;
    }
}
