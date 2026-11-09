use super::access::{Error as AccessError, InvocationPermit, ScriptStoreAccess};
use crate::persistent_store::PersistentStore;
use super::{access::{HistorySelection, NumberArgument}, lua_values};
use risunest_native_lua_experimental::{HistoryReader, PreparedHistory, NumberArgument as InputNumber};
use risunest_native_lua_experimental::{diagnostic, Cancellation, Limits, LuaResult, Runtime, Scalar, ScalarReturn, Step, Terminal};
use std::{cell::RefCell, rc::Rc, sync::atomic::{AtomicU64, Ordering}, time::Duration};

static ADAPTER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
pub(super) enum Permissions { Safe, EditDisplay }

#[derive(Clone)]
pub(super) struct Binding {
    pub access: Rc<ScriptStoreAccess>,
    pub permit: InvocationPermit,
    pub store: Rc<RefCell<PersistentStore>>,
    pub key: String,
    pub permissions: Permissions,
}

#[derive(Debug, PartialEq)]
pub(super) enum HandlerResult {
    Missing,
    Returned(Scalar),
    Failed(String),
    Terminal(Terminal),
    Disposed,
}

#[derive(Debug, PartialEq)]
pub(super) struct Outcome {
    pub result: HandlerResult,
    pub aborted: bool,
    pub cleanup_error: Option<AccessError>,
}

#[derive(Debug, PartialEq)]
pub(super) enum Progress { Suspended, Finished(Outcome) }

pub(super) struct ScalarAdapter {
    runtime: Option<Runtime>,
    binding: Rc<RefCell<Option<Binding>>>,
    emergency_duration: Duration,
    scalar_bytes: usize,
    identity: u64,
    sequence: u64,
    invocation_limit: u64,
    caller_aborted: bool,
    values_profile: bool,
}

fn text(args: &[Scalar], index: usize) -> Result<&str, String> {
    match args.get(index) {
        Some(Scalar::String(value)) => Ok(value),
        _ => Err("scalar host argument must be a string".into()),
    }
}

fn index(args: &[Scalar]) -> Result<f64, String> {
    match args.get(1) {
        Some(Scalar::Number(value)) => Ok(*value),
        _ => Err("history index must be a number".into()),
    }
}

fn copy_text(value: &str, max_bytes: usize, values_profile: bool) -> Result<String, String> {
    if value.len() > max_bytes { return Err("scalar UTF-8 byte limit exceeded".into()); }
    if !values_profile && value.contains('\0') { return Err("scalar strings cannot contain NUL".into()); }
    Ok(value.to_owned())
}

pub(super) fn host(binding: &Rc<RefCell<Option<Binding>>>, name: &str, args: Vec<Scalar>, max_bytes: usize, values_profile: bool) -> Result<ScalarReturn, String> {
    let current = binding.try_borrow().map_err(|_| "Lua host binding is busy")?
        .as_ref().cloned().ok_or("Lua invocation ended")?;
    let writes_message = matches!(name, "setChat" | "setChatRole");
    let writes = writes_message || matches!(name, "setChatVar" | "setChatVarChanged");
    if writes && (!matches!(args.first(), Some(Scalar::String(key)) if key == &current.key)
        || (writes_message && !matches!(current.permissions, Permissions::Safe))) {
        return Ok(ScalarReturn::NoValues);
    }
    let store = current.store.try_borrow().map_err(|_| "Lua store is busy")?;
    let access = &current.access;
    let permit = &current.permit;
    let failure = |error: AccessError| format!("script access: {error:?}");
    let value = match name {
        "getChatVar" => {
            let value = access.read_variable(&store, permit, text(&args, 1)?).map_err(failure)?;
            Scalar::String(copy_text(value.value().as_str().ok_or("unsupported stored variable value")?, max_bytes, values_profile)?)
        }
        "setChatVar" | "setChatVarChanged" => {
            let changed = access.set_variable(&store, permit, text(&args, 1)?.to_owned(), text(&args, 2)?.to_owned()).map_err(failure)?;
            return Ok(if name == "setChatVarChanged" && changed { ScalarReturn::One(Scalar::Boolean(true)) }
                else { ScalarReturn::NoValues });
        }
        "getChatLength" => {
            let count = access.history_length(&store, permit).map_err(failure)?;
            if !(0..=9_007_199_254_740_991).contains(&count) { return Err("history length exceeds JavaScript safe range".into()); }
            Scalar::Number(count as f64)
        }
        "getChatData" | "getChatRole" => {
            let message = access.read_message(&store, permit, index(&args)?).map_err(failure)?;
            let value = message.value().as_ref().and_then(|row| row.get(if name == "getChatData" { "data" } else { "role" }));
            Scalar::String(match value {
                None | Some(serde_json::Value::Null) => String::new(),
                Some(serde_json::Value::String(value)) => copy_text(value, max_bytes, values_profile)?,
                _ => return Err("unsupported stored history scalar".into()),
            })
        }
        "setChat" => {
            let data = match args.get(2) { None | Some(Scalar::Nil) => "", _ => text(&args, 2)? };
            access.set_message_data(&store, permit, index(&args)?, data.to_owned()).map_err(failure)?;
            return Ok(ScalarReturn::NoValues);
        }
        "setChatRole" => {
            let role = if matches!(args.get(2), Some(Scalar::String(value)) if value == "user") { "user" } else { "char" };
            access.set_message_role(&store, permit, index(&args)?, role).map_err(failure)?;
            return Ok(ScalarReturn::NoValues);
        }
        _ => return Err("unsupported scalar host API".into()),
    };
    Ok(ScalarReturn::One(value))
}

/// The host functions `host` serves.
pub(super) const SCALAR_API: [&str; 8] = ["getChatVar", "setChatVar", "setChatVarChanged", "getChatLength", "getChatData", "getChatRole", "setChat", "setChatRole"];

/// Registers `SCALAR_API` with `scalar` on the values profile, and the three history readers on `binding`.
pub(super) fn register_values_api(runtime: &mut Runtime, binding: &Rc<RefCell<Option<Binding>>>, limits: Limits,
    scalar: impl Fn(&str, Vec<Scalar>) -> Result<ScalarReturn, String> + Clone + 'static) -> LuaResult<()> {
    for name in SCALAR_API { runtime.register_values_scalar(name, scalar.clone())?; }
    for (name, reader) in [("getChatMain", HistoryReader::One), ("getRecentChatsMain", HistoryReader::Recent), ("getFullChatMain", HistoryReader::Full)] {
        let fence = binding.clone();
        let host_binding = binding.clone();
        runtime.register_history_reader(name, reader, move || {
            let active = fence.try_borrow().map_err(|_| "Lua host binding is busy")?;
            Ok(active.as_ref().ok_or("Lua invocation ended")?.key.clone())
        }, move |prepared| {
            let current = host_binding.try_borrow().map_err(|_| "Lua host binding is busy")?
                .as_ref().cloned().ok_or("Lua invocation ended")?;
            let number = |value| match value { InputNumber::Value(value) => NumberArgument::Value(value), InputNumber::Invalid => NumberArgument::Invalid };
            let selection = match prepared {
                PreparedHistory::One(value) => HistorySelection::One(number(value)),
                PreparedHistory::Recent(value) => HistorySelection::Recent(number(value)),
                PreparedHistory::Full => HistorySelection::FullExposed,
            };
            let rows = {
                let store = current.store.try_borrow().map_err(|_| "Lua store is busy")?;
                current.access.read_history(&store, &current.permit, selection).map_err(|error| format!("script access: {error:?}"))?
            };
            lua_values::project(&rows, reader == HistoryReader::Full, limits.values).map_err(|error| format!("script access: {error:?}"))
        })?;
    }
    Ok(())
}

impl ScalarAdapter {
    pub(super) fn initialize(source: &str, limits: Limits, emergency_duration: Duration) -> Result<Self, String> {
        Self::with_invocation_limit(source, limits, emergency_duration, u64::MAX)
    }

    pub(super) fn with_invocation_limit(source: &str, limits: Limits, emergency_duration: Duration,
        invocation_limit: u64) -> Result<Self, String> {
        Self::profile(source, limits, emergency_duration, invocation_limit, false)
    }

    pub(super) fn initialize_values(source: &str, limits: Limits, emergency_duration: Duration) -> Result<Self, String> {
        Self::profile(source, limits, emergency_duration, u64::MAX, true)
    }

    fn profile(source: &str, limits: Limits, emergency_duration: Duration,
        invocation_limit: u64, values_profile: bool) -> Result<Self, String> {
        let bootstrap = if values_profile { Some(lua_values::bootstrap(source, limits)?) } else { None };
        let (source, limits) = bootstrap.as_ref().map_or((source, limits), |(source, limits)| (source.as_str(), *limits));
        if invocation_limit == 0 { return Err("invocation limit must be positive".into()); }
        let identity = ADAPTER_SEQUENCE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| "Lua adapter identity exhausted")? + 1;
        let mut runtime = Runtime::new(limits).map_err(|error| diagnostic(&error))?;
        runtime.disable_host_await().map_err(|error| diagnostic(&error))?;
        let binding = Rc::new(RefCell::new(None));
        if values_profile {
            let host_binding = binding.clone();
            register_values_api(&mut runtime, &binding, limits, move |name, args| host(&host_binding, name, args, limits.values.utf8_bytes, true))
                .map_err(|error| diagnostic(&error))?;
        } else {
            for name in SCALAR_API {
                let binding = binding.clone();
                runtime.register_scalar(name, move |name, args| host(&binding, name, args, limits.values.utf8_bytes, false))
                    .map_err(|error| diagnostic(&error))?;
            }
        }
        runtime.start_ignoring_result(source, emergency_duration, Cancellation::default()).map_err(|error| diagnostic(&error))?;
        loop {
            match runtime.step() {
                Step::Suspended => continue,
                Step::IgnoredComplete => break,
                Step::Failed(error) => return Err(error),
                Step::Terminal(reason) => return Err(format!("initialization emergency: {reason:?}")),
                _ => return Err("unsupported initialization suspension/result".into()),
            }
        }
        Ok(Self { runtime: Some(runtime), binding, emergency_duration, scalar_bytes: limits.values.utf8_bytes,
            identity, sequence: 0, invocation_limit, caller_aborted: false, values_profile })
    }

    pub(super) fn begin(&mut self, handler: &str, access: Rc<ScriptStoreAccess>, store: Rc<RefCell<PersistentStore>>,
        permissions: Permissions, already_aborted: bool, emergency: Cancellation) -> Result<Progress, String> {
        if self.binding.borrow().is_some() { return Err("Lua invocation is active".into()); }
        if self.runtime.is_none() { return Err("Lua adapter is disposed".into()); }
        let sequence = self.sequence.checked_add(1).filter(|next| *next <= self.invocation_limit)
            .ok_or("Lua invocation sequence exhausted")?;
        let key = format!("{}:{sequence}", self.identity);
        if key.len() > self.scalar_bytes { return Err("Lua access key exceeds scalar byte limit".into()); }
        let permit = access.begin_invocation(already_aborted).map_err(|error| format!("script access: {error:?}"))?;
        self.sequence = sequence;
        self.caller_aborted = false;
        *self.binding.borrow_mut() = Some(Binding { access, permit, store, key: key.clone(), permissions });
        let runtime = self.runtime.as_mut().unwrap();
        let started = if self.values_profile { runtime.start_values_call(handler, &[Scalar::String(key)], self.emergency_duration, emergency) }
            else { runtime.start_scalar_call(handler, &[Scalar::String(key)], self.emergency_duration, emergency) };
        match started {
            Ok(true) => Ok(Progress::Suspended),
            Ok(false) => Ok(Progress::Finished(self.finish(HandlerResult::Missing))),
            Err(error) => {
                let terminal = self.runtime.as_ref().unwrap().terminal_reason();
                let result = terminal.map_or_else(|| HandlerResult::Failed(diagnostic(&error)), HandlerResult::Terminal);
                let outcome = self.finish(result);
                if terminal.is_some() { self.runtime.take(); }
                Ok(Progress::Finished(outcome))
            }
        }
    }

    fn finish(&mut self, result: HandlerResult) -> Outcome {
        // Clear before any VM cleanup; the scope owner retains publication/recovery.
        let binding = self.binding.borrow_mut().take();
        let caller_aborted = std::mem::take(&mut self.caller_aborted);
        let (aborted, cleanup_error) = match binding {
            Some(binding) => match binding.access.end_invocation(&binding.permit) {
                Ok(aborted) => (aborted, None),
                Err(error) => (false, Some(error)),
            },
            None => (false, None),
        };
        Outcome { result, aborted: aborted || caller_aborted, cleanup_error }
    }

    pub(super) fn step(&mut self) -> Progress {
        if self.binding.borrow().is_none() {
            return Progress::Finished(self.finish(HandlerResult::Failed("no active Lua invocation".into())));
        }
        match self.runtime.as_mut().unwrap().step() {
            Step::Suspended => Progress::Suspended,
            Step::ScalarComplete(value) => Progress::Finished(self.finish(HandlerResult::Returned(value))),
            Step::Failed(error) => Progress::Finished(self.finish(HandlerResult::Failed(error))),
            Step::Terminal(reason) => {
                let outcome = self.finish(HandlerResult::Terminal(reason));
                self.runtime.take();
                Progress::Finished(outcome)
            }
            Step::Awaiting(_) | Step::Complete(_) | Step::ListenerComplete(_) | Step::IgnoredComplete => {
                let outcome = self.finish(HandlerResult::Failed("unexpected scalar runtime await/result".into()));
                self.runtime.take();
                Progress::Finished(outcome)
            }
        }
    }

    pub(super) fn abort_caller(&mut self) -> Result<(), AccessError> {
        if let Some(binding) = self.binding.borrow().as_ref() {
            self.caller_aborted = true;
            binding.access.abort_invocation(&binding.permit)?;
        }
        Ok(())
    }

    pub(super) fn dispose(&mut self) -> Outcome {
        let outcome = self.finish(HandlerResult::Disposed);
        self.runtime.take();
        outcome
    }
}

impl Drop for ScalarAdapter {
    fn drop(&mut self) { self.dispose(); }
}
