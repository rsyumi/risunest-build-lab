//! Isolated Lua 5.4 terminal-interrupt experiment. Production use is not enabled.
mod conversion;
mod scalar;
mod value_input;
#[cfg(test)]
mod value_input_tests;
mod wasmoon;
#[cfg(test)]
mod wasmoon_tests;
pub use value_input::{HistoryReader, NumberArgument, PreparedHistory, VariableValue, ValueEvent, ValueVisitor, PropertyKeyVisitor};
pub use wasmoon::{js_string_bytes, js_string_units, GateArgument};
#[cfg(test)]
mod scalar_tests;
use conversion::Conversion;
use scalar::ScalarConversion;
pub use scalar::{Scalar, ScalarReturn};
pub use mlua::{Error as LuaError, Result as LuaResult};
use mlua::{HookTriggers, Lua, LuaOptions, MultiValue, StdLib, Thread, TerminalReason, UserData, Value, VmState};
use mlua::thread::ThreadStatus;
use serde_json::Value as Json;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::{Duration, Instant};

#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub fn cancel(&self) { self.0.store(true, Ordering::Release); }
    pub fn is_cancelled(&self) -> bool { self.0.load(Ordering::Acquire) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Terminal {
    Cancelled,
    Deadline,
}

struct Budget {
    deadline: Instant,
    cancellation: Cancellation,
    terminal: Option<Terminal>,
}

impl Budget {
    fn check(&mut self) -> Option<Terminal> {
        if self.terminal.is_none() {
            self.terminal = if self.cancellation.is_cancelled() {
                Some(Terminal::Cancelled)
            } else if Instant::now() >= self.deadline {
                Some(Terminal::Deadline)
            } else {
                None
            };
        }
        self.terminal
    }
}

pub enum VariableReturn {
    NoValues,
    One(VariableValue),
    Unsupported(String),
}

/// Per conversion boundary, including every repeated alias expansion and map key.
#[derive(Clone, Copy, Debug)]
pub struct ValueLimits {
    pub depth: usize,
    pub nodes: usize,
    pub utf8_bytes: usize,
}

impl Default for ValueLimits {
    fn default() -> Self { Self { depth: 128, nodes: 16_384, utf8_bytes: 8 * 1024 * 1024 } }
}

#[derive(Clone, Copy)]
pub struct Limits {
    pub memory_bytes: usize,
    pub source_bytes: usize,
    pub instructions_per_slice: u32,
    pub values: ValueLimits,
}

impl Default for Limits {
    fn default() -> Self {
        Self { memory_bytes: 16 * 1024 * 1024, source_bytes: 256 * 1024, instructions_per_slice: 1000, values: ValueLimits::default() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HostRequest {
    pub id: u64,
    pub service: String,
    pub arguments: Vec<Json>,
}

impl UserData for HostRequest {}

#[derive(Debug, PartialEq)]
pub enum Step {
    Suspended,
    Awaiting(HostRequest),
    Complete(Json),
    ScalarComplete(Scalar),
    /// The listener's string result as Wasmoon reads it into JS, or `None` when it keeps the data.
    ListenerComplete(Option<Vec<u16>>),
    IgnoredComplete,
    Failed(String),
    Terminal(Terminal),
}

pub fn diagnostic(error: &mlua::Error) -> String {
    use std::fmt::Write;
    struct Output { text: String, truncated: bool }
    impl std::fmt::Write for Output {
        fn write_str(&mut self, value: &str) -> std::fmt::Result {
            let remaining = 4093 - self.text.len();
            if value.len() <= remaining { self.text.push_str(value); return Ok(()); }
            let mut end = remaining;
            while !value.is_char_boundary(end) { end -= 1; }
            self.text.push_str(&value[..end]);
            self.truncated = true;
            Err(std::fmt::Error)
        }
    }
    let mut output = Output { text: String::new(), truncated: false };
    let _ = write!(&mut output, "{error}");
    if output.truncated { output.text.push_str("..."); }
    output.text
}

struct PlainScalarReturn(ScalarReturn);
impl AsRef<ScalarReturn> for PlainScalarReturn { fn as_ref(&self) -> &ScalarReturn { &self.0 } }

#[derive(Clone, Copy)]
enum ResultMode { Json, Scalar, Values, Listener, Ignore }

struct Invocation {
    thread: Thread,
    arguments: MultiValue,
    waiting: Option<HostRequest>,
    result_mode: ResultMode,
}

/// A single owner retains globals across completed invocations. A terminal
/// invocation retires the entire VM. Only external Cancellation handles are Send.
pub struct Runtime {
    lua: Lua,
    limits: Limits,
    budget: Rc<RefCell<Option<Budget>>>,
    active: Option<Invocation>,
    retired: bool,
    entry: Rc<Cell<usize>>,
    fresh: bool,
    declined: Rc<RefCell<Option<String>>>,
    profile: Option<wasmoon::Profile>,
    panicked: Rc<Cell<bool>>,
}

/// Runs host code a Lua callback calls. A panic retires the VM before it resumes into mlua, so card code that catches
/// the resumed error (`coroutine.resume`) cannot continue, and `Runtime::panicked` reports it.
fn guarded<T>(lua: &Lua, panicked: &Cell<bool>, run: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(value) => value,
        Err(payload) => {
            panicked.set(true);
            lua.retire_terminal(TerminalReason::Cancelled);
            std::panic::resume_unwind(payload)
        }
    }
}

impl Runtime {
    pub fn new(limits: Limits) -> mlua::Result<Self> {
        if limits.instructions_per_slice == 0 || limits.memory_bytes == 0 || limits.source_bytes == 0
            || limits.values.nodes == 0 || limits.values.utf8_bytes == 0 || limits.values.depth == 0 {
            return Err(mlua::Error::runtime("limits must be positive"));
        }
        if limits.values.depth > 128 {
            return Err(mlua::Error::runtime("JSON depth cannot exceed 128"));
        }
        // Every Rust callback runs under `guarded`, which retires the VM before a panic resumes, so `pcall`, `xpcall`
        // and `coroutine.resume` re-raise it as terminal and card code never continues. The standard `pcall` stays
        // yieldable, which a listener awaiting inside `pcall` needs.
        let lua = Lua::new_with(
            StdLib::COROUTINE | StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8,
            LuaOptions::default(),
        )?;
        lua.enable_terminal_control();
        lua.restrict_to_text_chunks();
        lua.remove_warning_function();
        lua.set_memory_limit(limits.memory_bytes)?;
        // No file, process, package searcher, native module or debug access.
        for name in ["dofile", "loadfile", "print"] {
            lua.globals().set(name, Value::Nil)?;
        }
        // No bytecode dump, and no collector control beyond a full collection, whatever profile is installed later.
        lua.globals().get::<mlua::Table>("string")?.raw_set("dump", Value::Nil)?;
        let collect: mlua::Function = lua.load(r#"
            local collect, raise = collectgarbage, error
            return function(option)
                if option ~= nil and option ~= 'collect' then raise('collectgarbage option is not available', 2) end
                return collect('collect')
            end
        "#).set_name("collectgarbage").call(())?;
        lua.globals().set("collectgarbage", collect)?;
        let budget = Rc::new(RefCell::new(None::<Budget>));
        let entry = Rc::new(Cell::new(0_usize));
        let panicked = Rc::new(Cell::new(false));
        let hook_entry = entry.clone();
        let hook_panicked = panicked.clone();
        lua.set_global_hook(
            HookTriggers::new().every_nth_instruction(limits.instructions_per_slice),
            move |lua, _| guarded(lua, &hook_panicked, || {
                // A hook yield in a child coroutine is visible to Lua resume/wrap.
                // Only the entry can suspend invisibly back to the Rust scheduler.
                Ok(if lua.current_thread().to_pointer() as usize == hook_entry.get() {
                    VmState::Yield
                } else {
                    VmState::Continue
                })
            }),
        )?;
        let request_entry = entry.clone();
        let request_panicked = panicked.clone();
        let sequence = Cell::new(0_u64);
        let request_budget = budget.clone();
        let new_request = lua.create_function_with_argument_limit(limits.values.nodes, move |lua, (service, arguments): (mlua::LuaString, MultiValue)| guarded(lua, &request_panicked, || {
            if lua.current_thread().to_pointer() as usize != request_entry.get() {
                return Err(mlua::Error::runtime("hostAwait requires the entry coroutine"));
            }
            let next = sequence.get().checked_add(1).ok_or_else(|| mlua::Error::runtime("request id exhausted"))?;
            sequence.set(next);
            let conversion = Conversion::new(lua, &request_budget, limits.values);
            let service = conversion.service(service)?;
            let arguments = arguments.into_iter().map(|value| conversion.decode(value)).collect::<mlua::Result<_>>()?;
            conversion.check()?;
            lua.create_userdata(HostRequest { id: next, service, arguments })
        }))?;
        let host_await: mlua::Function = lua.load(r#"
            local new_request = ...
            local yield, isyieldable, raise = coroutine.yield, coroutine.isyieldable, error
            return function(service, ...)
                if not isyieldable() then raise("hostAwait requires a yieldable entry coroutine", 2) end
                local ok, value = yield(new_request(service, ...))
                if not ok then raise(value, 2) end
                return value
            end
        "#).call(new_request)?;
        lua.globals().set("hostAwait", host_await)?;
        Ok(Self { lua, limits, budget, active: None, retired: false, entry, fresh: true,
            declined: Rc::new(RefCell::new(None)), profile: None, panicked })
    }

    /// The callback completes synchronously, including inside gsub/sort callbacks.
    /// It must not block or retain a store lock across a later hostAwait.
    pub fn register_sync(
        &mut self,
        name: &str,
        callback: impl Fn(&str, Vec<Json>) -> Result<Json, String> + 'static,
    ) -> mlua::Result<()> {
        if self.active.is_some() || self.retired {
            return Err(mlua::Error::runtime("runtime is not idle"));
        }
        if name.len() > self.limits.values.utf8_bytes {
            return Err(mlua::Error::runtime("host name byte limit exceeded"));
        }
        let name_owned = name.to_string();
        let callback_budget = self.budget.clone();
        let value_limits = self.limits.values;
        let panicked = self.panicked.clone();
        // Registration cannot execute retained script finalizers or _G metamethods.
        let gc_running = self.lua.gc_is_running();
        self.lua.gc_stop();
        let result = (|| {
            let function = self.lua.create_function_with_argument_limit(value_limits.nodes, move |lua, arguments: MultiValue| guarded(lua, &panicked, || {
                let conversion = Conversion::new(lua, &callback_budget, value_limits);
                let arguments = arguments.into_iter().map(|value| conversion.decode(value)).collect::<mlua::Result<_>>()?;
                conversion.check()?;
                let result = guarded(lua, &panicked, || callback(&name_owned, arguments));
                let conversion = Conversion::new(lua, &callback_budget, value_limits);
                match result {
                    Ok(value) => conversion.encode(&value),
                    Err(error) => {
                        conversion.rejection(&error)?;
                        Err(mlua::Error::runtime(error))
                    }
                }
            }))?;
            self.lua.globals().raw_set(name, function)
        })();
        if gc_running { self.lua.gc_restart(); }
        result
    }

    /// Compilation shares the VM deadline, source limit and Lua memory limit.
    pub fn start(&mut self, source: &str, duration: Duration, cancellation: Cancellation) -> mlua::Result<()> {
        if self.active.is_some() || self.retired {
            return Err(mlua::Error::runtime("runtime is not idle"));
        }
        if source.len() > self.limits.source_bytes {
            return Err(mlua::Error::runtime("source limit exceeded"));
        }
        let deadline = Instant::now().checked_add(duration).ok_or_else(|| mlua::Error::runtime("invalid deadline"))?;
        self.lua.arm_terminal_budget(deadline, cancellation.0.clone())?;
        *self.budget.borrow_mut() = Some(Budget { deadline, cancellation, terminal: None });
        if let Some(reason) = self.terminal() {
            self.retire(reason);
            return Err(mlua::Error::runtime("invocation already terminated"));
        }
        let prepared = self.lua.load(source).set_name("invocation").into_function()
            .and_then(|function| self.lua.create_thread(function));
        let thread = match prepared {
            Ok(thread) => thread,
            Err(error) => {
                if let Some(reason) = self.terminal() {
                    self.retire(reason);
                } else {
                    self.lua.disarm_terminal_budget();
                    *self.budget.borrow_mut() = None;
                }
                return Err(error);
            }
        };
        self.activate(thread, MultiValue::new(), ResultMode::Json);
        Ok(())
    }

    fn activate(&mut self, thread: Thread, arguments: MultiValue, result_mode: ResultMode) {
        self.fresh = false;
        self.entry.set(thread.to_pointer() as usize);
        self.active = Some(Invocation { thread, arguments, waiting: None, result_mode });
    }

    /// Initializer return values are discarded without traversing or serializing them.
    pub fn start_ignoring_result(&mut self, source: &str, duration: Duration, cancellation: Cancellation) -> mlua::Result<()> {
        self.start(source, duration, cancellation)?;
        self.active.as_mut().ok_or_else(|| mlua::Error::runtime("no active invocation"))?.result_mode = ResultMode::Ignore;
        Ok(())
    }

    /// Primitive callbacks preserve zero returns and never serialize through JSON.
    pub fn register_scalar(
        &mut self,
        name: &str,
        callback: impl Fn(&str, Vec<Scalar>) -> Result<ScalarReturn, String> + 'static,
    ) -> mlua::Result<()> {
        self.register_scalar_owned(name, move |name, arguments| callback(name, arguments).map(PlainScalarReturn))
    }

    /// Retains the callback's result owner until its scalar has been encoded into Lua.
    pub fn register_scalar_owned<R: AsRef<ScalarReturn> + 'static>(
        &mut self, name: &str,
        callback: impl Fn(&str, Vec<Scalar>) -> Result<R, String> + 'static,
    ) -> mlua::Result<()> {
        if self.active.is_some() || self.retired { return Err(mlua::Error::runtime("runtime is not idle")); }
        if name.len() > self.limits.values.utf8_bytes || name.contains('\0') {
            return Err(mlua::Error::runtime("invalid scalar host name"));
        }
        let name_owned = name.to_owned();
        let budget = self.budget.clone();
        let limits = self.limits.values;
        let slot = self.declined.clone();
        let panicked = self.panicked.clone();
        let gc_running = self.lua.gc_is_running();
        self.lua.gc_stop();
        let result = (|| {
            let function = self.lua.create_function_with_argument_limit(limits.nodes, move |lua, arguments: MultiValue| guarded(lua, &panicked, || {
                let conversion = ScalarConversion::new(lua, &budget, limits);
                let arguments = arguments.into_iter().map(|value| conversion.decode(value)).collect::<mlua::Result<_>>()?;
                conversion.check()?;
                let result = guarded(lua, &panicked, || callback(&name_owned, arguments));
                let conversion = ScalarConversion::new(lua, &budget, limits);
                match result {
                    Ok(result) => match result.as_ref() {
                        ScalarReturn::NoValues => { conversion.check()?; Ok(MultiValue::new()) }
                        ScalarReturn::One(value) => Ok(MultiValue::from_vec(vec![conversion.encode(value)?])),
                        ScalarReturn::Unsupported(label) => Err(wasmoon::decline_error(lua, &slot, label.as_bytes())),
                    },
                    Err(error) => {
                        conversion.rejection(&error)?;
                        Err(mlua::Error::runtime(error))
                    }
                }
            }))?;
            self.lua.globals().raw_set(name, function)
        })();
        if gc_running { self.lua.gc_restart(); }
        result
    }

    /// Call before scalar-profile source initialization. General runtimes keep hostAwait.
    pub fn disable_host_await(&mut self) -> mlua::Result<()> {
        if self.active.is_some() || self.retired { return Err(mlua::Error::runtime("runtime is not idle")); }
        let gc_running = self.lua.gc_is_running();
        self.lua.gc_stop();
        let result = self.lua.globals().raw_set("hostAwait", Value::Nil);
        if gc_running { self.lua.gc_restart(); }
        result
    }

    /// The explicit values profile follows the pinned string codec. Scalar registration is unchanged.
    pub fn register_values_scalar(
        &mut self, name: &str,
        callback: impl Fn(&str, Vec<Scalar>) -> Result<ScalarReturn, String> + 'static,
    ) -> mlua::Result<()> {
        self.register_values_scalar_owned(name, move |name, arguments| callback(name, arguments).map(PlainScalarReturn))
    }

    /// Retains the callback's result owner until its scalar has been encoded into Lua.
    pub fn register_values_scalar_owned<R: AsRef<ScalarReturn> + 'static>(
        &mut self, name: &str,
        callback: impl Fn(&str, Vec<Scalar>) -> Result<R, String> + 'static,
    ) -> mlua::Result<()> {
        if self.active.is_some() || self.retired { return Err(mlua::Error::runtime("runtime is not idle")); }
        if name.len() > self.limits.values.utf8_bytes || name.contains('\0') { return Err(mlua::Error::runtime("invalid values host name")); }
        let name_owned = name.to_owned();
        let budget = self.budget.clone();
        let limits = self.limits.values;
        let slot = self.declined.clone();
        let wasmoon = self.profile.is_some();
        let null=self.profile.as_ref().map(|profile|profile.null.clone());
        let panicked = self.panicked.clone();
        let gc_running = self.lua.gc_is_running();
        self.lua.gc_stop();
        let result = (|| {
            let function = self.lua.create_function_with_argument_limit(limits.nodes, move |lua, arguments: MultiValue| guarded(lua, &panicked, || {
                // Wasmoon converts any argument; the Wasmoon profile declines one this host does not model.
                if wasmoon && !arguments.iter().all(|value|value_input::scalar_like(value,null.as_ref())) {
                    return Err(wasmoon::decline_error(lua, &slot, format!("lua-host-args:{name_owned}").as_bytes()));
                }
                let conversion = value_input::Input::new(lua, &budget, limits, None).with_null(null.clone());
                let arguments = arguments.into_iter().map(|value| conversion.scalar(value)).collect::<mlua::Result<_>>()?;
                conversion.check()?;
                let result = guarded(lua, &panicked, || callback(&name_owned, arguments));
                match result {
                    Ok(result) => match result.as_ref() {
                        ScalarReturn::NoValues => { conversion.check()?; Ok(MultiValue::new()) }
                        ScalarReturn::One(value) => Ok(MultiValue::from_vec(vec![conversion.encode_scalar(value, &budget)?])),
                        ScalarReturn::Unsupported(label) => Err(wasmoon::decline_error(lua, &slot, label.as_bytes())),
                    },
                    Err(error) => {
                        ScalarConversion::new(lua, &budget, limits).rejection(&error)?;
                        Err(mlua::Error::runtime(error))
                    }
                }
            }))?;
            self.lua.globals().raw_set(name, function)
        })();
        if gc_running { self.lua.gc_restart(); }
        result
    }

    /// Variable callbacks use the same bounded Wasmoon graph as history arguments. No host borrow spans key
    /// conversion; the fence is checked again before a converted value can enter the conversation.
    pub fn register_variable_api(
        &mut self, name:&str,
        fence:impl Fn()->Result<String,String>+'static,
        callback:impl Fn(&str,Vec<VariableValue>)->Result<VariableReturn,String>+'static,
        reserve:impl Fn(usize)->Result<Box<dyn std::any::Any>,String>+'static,
    )->mlua::Result<()> {
        if self.active.is_some() || self.retired {return Err(mlua::Error::runtime("runtime is not idle"));}
        if name.len()>self.limits.values.utf8_bytes || name.contains('\0') {return Err(mlua::Error::runtime("invalid variable host name"));}
        let global_name=name.to_owned();
        let name=global_name.clone();
        let key_string=self.lua.globals().raw_get::<mlua::Function>("tostring")?;
        let null=self.profile.as_ref().map(|profile|profile.null.clone());
        let budget=self.budget.clone();
        let limits=self.limits.values;
        let slot=self.declined.clone();
        let panicked=self.panicked.clone();
        let gc_running=self.lua.gc_is_running();
        self.lua.gc_stop();
        let result=(|| {
            let function=self.lua.create_function_with_argument_limit(limits.nodes,move |lua,arguments:MultiValue| guarded(lua,&panicked,|| {
                let original=guarded(lua,&panicked,&fence).map_err(mlua::Error::runtime)?;
                let mut conversion=value_input::Input::new(lua,&budget,limits,Some(key_string.clone())).with_null(null.clone()).with_reserver(&reserve);
                let arguments=conversion.variables(arguments).map_err(|error| {
                    if lua.terminal_reason().is_some(){return error;}
                    if wasmoon::is_memory_error(&error){return wasmoon::decline_error(lua,&slot,b"lua-memory");}
                    if let Some(label)=value_input::quota_label(&error){return wasmoon::decline_error(lua,&slot,label.as_bytes());}
                    if value_input::is_limit(&error){return wasmoon::decline_error(lua,&slot,b"lua-host-limit");}
                    if value_input::is_unsupported(&error){return wasmoon::decline_error(lua,&slot,format!("lua-host-args:{name}").as_bytes());}
                    error
                })?;
                if guarded(lua,&panicked,&fence).map_err(mlua::Error::runtime)?!=original {return Err(mlua::Error::runtime("Lua invocation ended"));}
                conversion.check()?;
                match guarded(lua,&panicked,|| callback(&name,arguments)) {
                    Ok(VariableReturn::NoValues)=>{conversion.check()?;Ok(MultiValue::new())}
                    Ok(VariableReturn::One(value))=>{
                        let bytes=conversion.variable_scratch(&value).map_err(|error|if lua.terminal_reason().is_some(){error}else{wasmoon::decline_error(lua,&slot,b"lua-host-limit")})?;
                        let _lease=guarded(lua,&panicked,||reserve(bytes)).map_err(|label|wasmoon::decline_error(lua,&slot,label.as_bytes()))?;
                        conversion.precharged_output();
                        Ok(MultiValue::from_vec(vec![conversion.encode_variable(&value,&budget).map_err(|error|{
                            if lua.terminal_reason().is_some(){error}
                            else if wasmoon::is_memory_error(&error){wasmoon::decline_error(lua,&slot,b"lua-memory")}
                            else{wasmoon::decline_error(lua,&slot,b"lua-host-limit")}
                        })?]))
                    },
                    Ok(VariableReturn::Unsupported(label))=>Err(wasmoon::decline_error(lua,&slot,label.as_bytes())),
                    Err(error)=>{
                        ScalarConversion::new(lua,&budget,limits).rejection(&error)?;
                        Err(mlua::Error::runtime(error))
                    }
                }
            }))?;
            self.lua.globals().raw_set(global_name,function)
        })();
        if gc_running {self.lua.gc_restart();}
        result
    }

    /// Conversion may call Lua key metamethods. No service borrow may span this boundary.
    /// The returned text owner is retained until the Lua string copy has completed.
    pub fn register_history_reader<R: AsRef<str> + 'static>(
        &mut self, name: &str, reader: HistoryReader,
        fence: impl Fn() -> Result<String, String> + 'static,
        callback: impl Fn(PreparedHistory) -> Result<R, String> + 'static,
    ) -> mlua::Result<()> {
        self.register_history_reader_with_quota(name,reader,fence,callback,|_|Ok(Box::new(())),false)
    }

    pub fn register_history_reader_budgeted<R:AsRef<str>+'static>(
        &mut self,name:&str,reader:HistoryReader,
        fence:impl Fn()->Result<String,String>+'static,
        callback:impl Fn(PreparedHistory)->Result<R,String>+'static,
        reserve:impl Fn(usize)->Result<Box<dyn std::any::Any>,String>+'static,
    )->mlua::Result<()> {
        self.register_history_reader_with_quota(name,reader,fence,callback,reserve,true)
    }

    fn register_history_reader_with_quota<R:AsRef<str>+'static>(
        &mut self,name:&str,reader:HistoryReader,
        fence:impl Fn()->Result<String,String>+'static,
        callback:impl Fn(PreparedHistory)->Result<R,String>+'static,
        reserve:impl Fn(usize)->Result<Box<dyn std::any::Any>,String>+'static,
        terminal_limits:bool,
    )->mlua::Result<()> {
        if self.active.is_some() || self.retired { return Err(mlua::Error::runtime("runtime is not idle")); }
        if name.len() > self.limits.values.utf8_bytes || name.contains('\0') { return Err(mlua::Error::runtime("invalid history host name")); }
        let key_string = self.lua.globals().raw_get::<mlua::Function>("tostring")?;
        let null=self.profile.as_ref().map(|profile|profile.null.clone());
        let slot=self.declined.clone();
        let budget = self.budget.clone();
        let limits = self.limits.values;
        let panicked = self.panicked.clone();
        let gc_running = self.lua.gc_is_running();
        self.lua.gc_stop();
        let result = (|| {
            let function = self.lua.create_function_with_argument_limit(limits.nodes, move |lua, arguments: MultiValue| guarded(lua, &panicked, || {
                let original = guarded(lua, &panicked, &fence).map_err(mlua::Error::runtime)?;
                let mut conversion = value_input::Input::new(lua, &budget, limits, Some(key_string.clone())).with_null(null.clone()).with_reserver(&reserve);
                let prepared = conversion.history(arguments, reader).map_err(|error| {
                    if lua.terminal_reason().is_some(){return error;}
                    if let Some(label)=value_input::quota_label(&error){return wasmoon::decline_error(lua,&slot,label.as_bytes());}
                    if terminal_limits&&value_input::is_limit(&error){return wasmoon::decline_error(lua,&slot,b"lua-host-limit");}
                    error
                })?;
                if guarded(lua, &panicked, &fence).map_err(mlua::Error::runtime)? != original {
                    return Err(mlua::Error::runtime("Lua invocation ended"));
                }
                conversion.check()?;
                let text = guarded(lua, &panicked, || callback(prepared));
                conversion.check()?;
                // This label comes only from the trusted host callback, after Lua argument conversion.
                let text = text.map_err(|error| if terminal_limits && error == "lua-host-limit" {
                    wasmoon::decline_error(lua, &slot, b"lua-host-limit")
                } else { mlua::Error::runtime(error) })?;
                if text.as_ref().len() > limits.utf8_bytes {
                    return Err(if terminal_limits { wasmoon::decline_error(lua, &slot, b"lua-host-limit") }
                        else { mlua::Error::runtime("values output byte limit exceeded") });
                }
                let value = lua.create_string(text.as_ref().as_bytes()).map_err(|error| {
                    if lua.terminal_reason().is_none() && terminal_limits && wasmoon::is_memory_error(&error) {
                        wasmoon::decline_error(lua, &slot, b"lua-memory")
                    } else { error }
                })?;
                conversion.check()?;
                Ok(value)
            }))?;
            self.lua.globals().raw_set(name, function)
        })();
        if gc_running { self.lua.gc_restart(); }
        result
    }

    pub fn start_values_call(&mut self, name: &str, args: &[Scalar], duration: Duration,
        cancellation: Cancellation) -> mlua::Result<bool> {
        let started = self.start_scalar_call(name, args, duration, cancellation)?;
        if started {
            self.active.as_mut().ok_or_else(|| mlua::Error::runtime("no active invocation"))?.result_mode = ResultMode::Values;
        }
        Ok(started)
    }

    /// A missing/false handler returns false. Only its first result crosses the boundary.
    pub fn start_scalar_call(&mut self, name: &str, args: &[Scalar], duration: Duration,
        cancellation: Cancellation) -> mlua::Result<bool> {
        if self.active.is_some() || self.retired { return Err(mlua::Error::runtime("runtime is not idle")); }
        if args.len() > self.limits.values.nodes { return Err(mlua::Error::runtime("scalar argument limit exceeded")); }
        let deadline = Instant::now().checked_add(duration).ok_or_else(|| mlua::Error::runtime("invalid deadline"))?;
        self.lua.arm_terminal_budget(deadline, cancellation.0.clone())?;
        *self.budget.borrow_mut() = Some(Budget { deadline, cancellation, terminal: None });
        let result = (|| {
            let conversion = ScalarConversion::new(&self.lua, &self.budget, self.limits.values);
            conversion.rejection(name)?;
            // get is protected by mlua and the armed budget, including an __index handler.
            let function = match self.lua.globals().get::<Value>(name)? {
                Value::Nil | Value::Boolean(false) => return Ok(false),
                Value::Function(function) => function,
                _ => return Err(mlua::Error::runtime("Lua handler is not a function")),
            };
            let conversion = ScalarConversion::new(&self.lua, &self.budget, self.limits.values);
            let arguments = args.iter().map(|value| conversion.encode(value)).collect::<mlua::Result<Vec<_>>>()?;
            let thread = self.lua.create_thread(function)?;
            conversion.check()?;
            self.activate(thread, MultiValue::from_vec(arguments), ResultMode::Scalar);
            Ok(true)
        })();
        if let Some(reason) = self.terminal() {
            self.retire(reason);
            return Err(mlua::Error::runtime("invocation already terminated"));
        }
        if !matches!(&result, Ok(true)) {
            self.lua.disarm_terminal_budget();
            *self.budget.borrow_mut() = None;
        }
        result
    }

    fn terminal(&self) -> Option<Terminal> {
        match self.lua.terminal_reason() {
            Some(TerminalReason::Cancelled) => Some(Terminal::Cancelled),
            Some(_) => Some(Terminal::Deadline),
            None => self.budget.borrow_mut().as_mut().and_then(Budget::check),
        }
    }

    fn retire(&mut self, reason: Terminal) -> Step {
        self.lua.retire_terminal(match reason {
            Terminal::Cancelled => TerminalReason::Cancelled,
            Terminal::Deadline => TerminalReason::Deadline,
        });
        self.active = None;
        self.retired = true;
        Step::Terminal(reason)
    }

    /// Poll after scheduler yields and while a host request is pending. Never
    /// resume an expired/cancelled VM, including when its host reply arrives late.
    pub fn step(&mut self) -> Step {
        if let Some(reason) = self.terminal() { return self.retire(reason); }
        let Some(active) = self.active.as_mut() else {
            return Step::Failed("no active invocation".into());
        };
        if let Some(request) = &active.waiting { return Step::Awaiting(request.clone()); }
        let arguments = std::mem::take(&mut active.arguments);
        let result = active.thread.resume_first(arguments);
        if let Some(reason) = self.terminal() { return self.retire(reason); }
        match result {
            Err(error) => {
                // Wasmoon's heap grows where this VM meets its memory limit.
                if self.profile.is_some() && wasmoon::is_memory_error(&error) {
                    let _ = wasmoon::decline_error(&self.lua, &self.declined, b"lua-memory");
                }
                self.active = None;
                self.lua.disarm_terminal_budget();
                *self.budget.borrow_mut() = None;
                Step::Failed(diagnostic(&error))
            }
            Ok(value) => {
                let Some(active) = self.active.as_mut() else { return Step::Failed("no active invocation".into()) };
                if active.thread.status() == ThreadStatus::Finished {
                    let result = match active.result_mode {
                        ResultMode::Scalar => ScalarConversion::new(&self.lua, &self.budget, self.limits.values).decode(value).map(Step::ScalarComplete),
                        ResultMode::Values => value_input::Input::new(&self.lua, &self.budget, self.limits.values, None).scalar(value).map(Step::ScalarComplete),
                        ResultMode::Json => Conversion::new(&self.lua, &self.budget, self.limits.values).decode(value).map(Step::Complete),
                        ResultMode::Listener => match value {
                            Value::Nil => Ok(Step::ListenerComplete(None)),
                            Value::String(text) => value_input::wasmoon_text(&text.as_bytes()).map(|text| Step::ListenerComplete(Some(text))),
                            _ => Err(mlua::Error::runtime("unsupported listener result")),
                        },
                        ResultMode::Ignore => Ok(Step::IgnoredComplete),
                    };
                    if let Some(reason) = self.terminal() { return self.retire(reason); }
                    self.active = None;
                    self.lua.disarm_terminal_budget();
                    *self.budget.borrow_mut() = None;
                    match result {
                        Ok(result) => result,
                        Err(error) => Step::Failed(diagnostic(&error)),
                    }
                } else if let Value::UserData(value) = value {
                    match value.borrow::<HostRequest>() {
                        Ok(request) => {
                            active.waiting = Some(request.clone());
                            Step::Awaiting(request.clone())
                        }
                        Err(_) => Step::Suspended,
                    }
                } else {
                    Step::Suspended
                }
            }
        }
    }

    /// No service is dispatched by the VM. The caller owns the request and must
    /// cancel/drop it when this returns Terminal or the runtime is disposed.
    pub fn reply(&mut self, id: u64, reply: Result<Json, String>) -> mlua::Result<()> {
        if let Some(reason) = self.terminal() {
            self.retire(reason);
            return Err(mlua::Error::runtime("late host reply"));
        }
        let active = self.active.as_ref().ok_or_else(|| mlua::Error::runtime("no active invocation"))?;
        if active.waiting.as_ref().map(|request| request.id) != Some(id) {
            return Err(mlua::Error::runtime("host reply id mismatch"));
        }
        let conversion = Conversion::new(&self.lua, &self.budget, self.limits.values);
        let (ok, value) = match reply {
            Ok(value) => (true, value),
            Err(error) => (false, Json::String(error)),
        };
        let converted = conversion.encode(&value);
        if let Some(reason) = self.terminal() {
            self.retire(reason);
            return Err(mlua::Error::runtime("late host reply"));
        }
        let values = vec![Value::Boolean(ok), converted?];
        let active = self.active.as_mut().ok_or_else(|| mlua::Error::runtime("no active invocation"))?;
        active.arguments = MultiValue::from_vec(values);
        active.waiting = None;
        Ok(())
    }

    /// Adds the main-thread Wasmoon engine globals, with `modules` (name, source) as the files `require` finds. Call once
    /// on a fresh runtime, before any source runs or host function is registered.
    pub fn install_wasmoon_profile(&mut self, modules: &[(&str, &str)]) -> mlua::Result<()> {
        if !self.fresh || self.retired || self.profile.is_some() { return Err(mlua::Error::runtime("runtime is not fresh")); }
        self.profile = Some(wasmoon::install(&self.lua, &self.declined, &self.entry, &self.panicked, modules)?);
        Ok(())
    }

    /// A host function this VM cannot serve. When `admitted` passes the TS permission check on its arguments the
    /// invocation is declined; otherwise it returns what TS returns for a refused call: nothing, or for an async
    /// function a promise fulfilled with nothing.
    pub fn register_unsupported(&mut self, name: &str, asynchronous: bool,
        admitted: impl Fn(&[GateArgument]) -> bool + 'static) -> mlua::Result<()> {
        if self.active.is_some() || self.retired { return Err(mlua::Error::runtime("runtime is not idle")); }
        if name.len() > self.limits.values.utf8_bytes || name.contains('\0') { return Err(mlua::Error::runtime("invalid host name")); }
        let profile = self.profile.as_ref().ok_or_else(|| mlua::Error::runtime("Wasmoon profile is not installed"))?;
        let fulfilled_nil = profile.fulfilled_nil.clone();
        let label = format!("lua-host:{name}");
        let slot = self.declined.clone();
        let panicked = self.panicked.clone();
        let gc_running = self.lua.gc_is_running();
        self.lua.gc_stop();
        let result = (|| {
            let function = self.lua.create_function_with_argument_limit(self.limits.values.nodes, move |lua, arguments: MultiValue| guarded(lua, &panicked, || {
                let arguments = wasmoon::gate_arguments(&arguments);
                if guarded(lua, &panicked, || admitted(&arguments)) { return Err(wasmoon::decline_error(lua, &slot, label.as_bytes())); }
                if asynchronous { fulfilled_nil.call::<MultiValue>(()) } else { Ok(MultiValue::new()) }
            }))?;
            self.lua.globals().raw_set(name, function)
        })();
        if gc_running { self.lua.gc_restart(); }
        result
    }

    /// The private entry for `callListenMain(mode, key, value, meta)`, each argument a JS string.
    pub fn start_listener(&mut self, arguments: [&[u16]; 4], duration: Duration, cancellation: Cancellation) -> mlua::Result<()> {
        if self.active.is_some() || self.retired { return Err(mlua::Error::runtime("runtime is not idle")); }
        let listen = self.profile.as_ref().ok_or_else(|| mlua::Error::runtime("Wasmoon profile is not installed"))?.listen.clone();
        let encoded = arguments.map(js_string_bytes);
        if encoded.iter().try_fold(0_usize, |total, bytes| total.checked_add(bytes.len()))
            .is_none_or(|total| total > self.limits.values.utf8_bytes) {
            return Err(mlua::Error::runtime("listener argument byte limit exceeded"));
        }
        let deadline = Instant::now().checked_add(duration).ok_or_else(|| mlua::Error::runtime("invalid deadline"))?;
        self.lua.arm_terminal_budget(deadline, cancellation.0.clone())?;
        *self.budget.borrow_mut() = Some(Budget { deadline, cancellation, terminal: None });
        let result = (|| {
            let arguments = encoded.iter().map(|bytes| self.lua.create_string(bytes).map(Value::String)).collect::<mlua::Result<Vec<_>>>()?;
            let thread = self.lua.create_thread(listen)?;
            self.activate(thread, MultiValue::from_vec(arguments), ResultMode::Listener);
            Ok(())
        })();
        if let Some(reason) = self.terminal() {
            self.retire(reason);
            return Err(mlua::Error::runtime("invocation already terminated"));
        }
        if result.is_err() {
            self.lua.disarm_terminal_budget();
            *self.budget.borrow_mut() = None;
        }
        result
    }

    /// The private entry for `res = await getFunction(name)(...arguments)` (`runScripted`), the name and each argument a
    /// JS string. The handler's first result once settled completes as `Step::ScalarComplete`; a nil, null or
    /// non-scalar result completes as `Scalar::Nil`, and a falsy global as `Scalar::Nil` without a call.
    pub fn start_handler(&mut self, name: &[u16], arguments: &[&[u16]], duration: Duration, cancellation: Cancellation) -> mlua::Result<()> {
        if self.active.is_some() || self.retired { return Err(mlua::Error::runtime("runtime is not idle")); }
        let handle = self.profile.as_ref().ok_or_else(|| mlua::Error::runtime("Wasmoon profile is not installed"))?.handle.clone();
        let name = js_string_bytes(name);
        let encoded = arguments.iter().map(|units| js_string_bytes(units)).collect::<Vec<_>>();
        if encoded.iter().chain([&name]).try_fold(0_usize, |total, bytes| total.checked_add(bytes.len()))
            .is_none_or(|total| total > self.limits.values.utf8_bytes) {
            return Err(mlua::Error::runtime("handler argument byte limit exceeded"));
        }
        let deadline = Instant::now().checked_add(duration).ok_or_else(|| mlua::Error::runtime("invalid deadline"))?;
        self.lua.arm_terminal_budget(deadline, cancellation.0.clone())?;
        *self.budget.borrow_mut() = Some(Budget { deadline, cancellation, terminal: None });
        let result = (|| {
            let arguments = [&name].into_iter().chain(&encoded).map(|bytes| self.lua.create_string(bytes).map(Value::String))
                .collect::<mlua::Result<Vec<_>>>()?;
            let thread = self.lua.create_thread(handle)?;
            self.activate(thread, MultiValue::from_vec(arguments), ResultMode::Values);
            Ok(())
        })();
        if let Some(reason) = self.terminal() {
            self.retire(reason);
            return Err(mlua::Error::runtime("invocation already terminated"));
        }
        if result.is_err() {
            self.lua.disarm_terminal_budget();
            *self.budget.borrow_mut() = None;
        }
        result
    }

    /// The first unsupported operation this VM met. A decline retires the VM.
    pub fn declined(&self) -> Option<String> { self.declined.borrow().clone() }

    /// Whether host code a callback ran panicked. The panic retired the VM.
    pub fn panicked(&self) -> bool { self.panicked.get() }

    pub fn used_memory(&self) -> usize { self.lua.used_memory() }

    pub fn terminal_reason(&self) -> Option<Terminal> { if self.retired { self.terminal() } else { None } }

    /// Retires the VM and releases trusted native resources during close.
    pub fn dispose(self) { drop(self); }
}
