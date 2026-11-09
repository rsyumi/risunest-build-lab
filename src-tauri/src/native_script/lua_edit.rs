//! The Lua edit listeners `runLuaEditTriggerBatch` runs (`src/ts/process/scriptings.ts`), on the main-thread Wasmoon
//! engine profile, and the reply to `HostRequest::Lua` for the edit modes.
use super::access::{Error as AccessError, FullMessage, ScriptStoreAccess};
use super::lua_scalar_adapter::{host, register_values_api, Binding, Permissions};
use crate::persistent_store::PersistentStore;
use risunest_ir::chat::MessageRole;
use risunest_ir::stage::{Effect, HostReply, HostRequest, LuaMode, LuaReply, LuaTrigger, MessageEdit, ScriptMode};
use risunest_ir::{json, Budget, Error as IrError, Limits as IrLimits, OwnedVec, Text, Value};
use risunest_native_lua_experimental::{diagnostic, js_string_units, Cancellation, GateArgument, Limits, LuaError, LuaResult, Runtime,
    Scalar, ScalarReturn, Step, Terminal};
use std::{borrow::Cow, cell::RefCell, collections::HashMap, rc::Rc, sync::{atomic::AtomicBool, Arc}, time::Duration};

const WRAPPER: &str = include_str!("../../../src/ts/process/luaCodeWrapper.ts");
const JSON_MODULE: &str = include_str!("../../../public/lua/json.lua");

/// `luaCodeWrapper(code)` is `prefix + code + suffix`, read from the template literal of its TS source.
pub(super) fn wrapper() -> Result<(String, String), String> {
    // A template literal reads a CRLF line break as LF.
    let source = WRAPPER.replace("\r\n", "\n");
    let start = source.find("return `").ok_or("luaCodeWrapper template not found")? + "return `".len();
    let template = &source[start..];
    let template = &template[..template.find('`').ok_or("luaCodeWrapper template is not closed")?];
    // Escapes or another substitution would make the template text differ from its value.
    if template.contains('\\') || template.matches("${").count() != 1 { return Err("luaCodeWrapper template is not plain".into()); }
    let (prefix, suffix) = template.split_once("${code}").ok_or("luaCodeWrapper has no code substitution")?;
    Ok((prefix.to_owned(), suffix.to_owned()))
}

/// The listener modes of `callListenMain`; `editprocess` runs no Lua.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum EditMode { Input, Output, Display, Request }

impl EditMode {
    fn listener(self) -> &'static str {
        match self { Self::Input => "editInput", Self::Output => "editOutput", Self::Display => "editDisplay", Self::Request => "editRequest" }
    }
}

/// A conversation write a listener made, in call order.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Edit {
    Variable { key: String, value: String },
    MessageData { index: f64, data: String },
    MessageRole { index: f64, role: &'static str },
    Remove { index: f64 },
    /// With the id `insertChat` drew.
    Insert { index: f64, role: &'static str, data: String, chat_id: String },
    /// The rows `setFullChatMain` assigns after its `chatId` merge.
    Replace { messages: Vec<serde_json::Value> },
}

#[derive(Debug, PartialEq)]
pub(super) enum Status {
    Completed,
    /// The batch reached something this host does not do as TS does.
    Declined(String),
    /// A listener ran out of time; TS keeps that engine, this host cannot.
    TimedOut,
    Cancelled,
}

#[derive(Debug)]
pub(super) struct EditOutcome {
    /// The batch result; the original content when an initializer failed.
    pub value: Value,
    /// Made through the access scope whatever the status; TS commits them in every case.
    pub edits: Vec<Edit>,
    pub status: Status,
}

/// A native execution failure. The access scope already contains these writes; the owner must settle it once,
/// terminate the operation and never replay the edits or retry through another engine.
#[derive(Debug)]
pub(super) struct EditFailure {
    pub reason: String,
    pub edits: Vec<Edit>,
}

impl EditFailure {
    fn new(mut reason: String, edits: Vec<Edit>) -> Self {
        if reason.len() > 4096 {
            let mut end = 4093;
            while !reason.is_char_boundary(end) { end -= 1; }
            reason.truncate(end);
            reason.push_str("...");
        }
        Self { reason, edits }
    }
}

/// What a host name of `scriptings.ts` does for an edit listener here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Gate {
    /// Served by `host`, the history readers and `restructure`.
    Served,
    /// Declined on every call.
    Always,
    /// Declined when the first argument is a `ScriptingSafeIds` key, otherwise refused as TS refuses it.
    Safe,
    /// Refused: edit listeners never hold a `ScriptingLowLevelIds` key.
    LowLevel,
    /// `LLMMain` and `axLLMMain` parse their prompt before the low-level check: declined when it would throw.
    ParsedPrompt,
    /// `logMain`: `JSON.parse(value)` is all an edit listener observes; declined when it would throw.
    Log,
    /// `stopChat`: the stop flag it sets is ignored by every edit batch.
    Ignored,
}

/// Every host name `scriptings.ts` declares for Lua, with whether it is `async`, in declaration order.
pub(super) const HOST_API: [(&str, bool, Gate); 54] = [
    ("getChatVar", false, Gate::Served), ("setChatVar", false, Gate::Served), ("setChatVarChanged", false, Gate::Served),
    ("getGlobalVar", false, Gate::Always), ("stopChat", false, Gate::Ignored), ("alertError", false, Gate::Safe),
    ("alertNormal", false, Gate::Safe), ("alertInput", false, Gate::Safe), ("alertSelect", false, Gate::Safe),
    ("alertConfirm", false, Gate::Safe), ("getChatMain", false, Gate::Served), ("getChatData", false, Gate::Served),
    ("getChatRole", false, Gate::Served), ("getRecentChatsMain", false, Gate::Served), ("setChat", false, Gate::Served),
    ("setChatRole", false, Gate::Served), ("cutChat", false, Gate::Safe), ("removeChat", false, Gate::Served),
    ("addChat", false, Gate::Safe), ("insertChat", false, Gate::Served), ("getTokens", true, Gate::Safe),
    ("getChatLength", false, Gate::Served), ("getFullChatMain", false, Gate::Served), ("sleep", false, Gate::Safe),
    ("cbs", false, Gate::Always), ("setFullChatMain", false, Gate::Served), ("logMain", false, Gate::Log),
    ("reloadDisplay", false, Gate::Safe), ("reloadChat", false, Gate::Safe), ("similarity", true, Gate::LowLevel),
    ("request", true, Gate::LowLevel), ("generateImage", true, Gate::LowLevel), ("getCharacterImageMain", true, Gate::Always),
    ("getPersonaImageMain", true, Gate::Always), ("hash", true, Gate::Always), ("LLMMain", true, Gate::ParsedPrompt),
    ("simpleLLM", true, Gate::LowLevel), ("getName", false, Gate::Always), ("setName", false, Gate::Safe),
    ("getDescription", false, Gate::Safe), ("setDescription", false, Gate::Safe), ("getCharacterFirstMessage", false, Gate::Always),
    ("setCharacterFirstMessage", false, Gate::Safe), ("getPersonaName", false, Gate::Always),
    ("getPersonaDescription", false, Gate::Always), ("getAuthorsNote", false, Gate::Always),
    ("getBackgroundEmbedding", false, Gate::Safe), ("setBackgroundEmbedding", false, Gate::Safe),
    ("getLoreBooksMain", false, Gate::Always), ("upsertLocalLoreBook", false, Gate::Safe), ("loadLoreBooksMain", true, Gate::LowLevel),
    ("axLLMMain", true, Gate::ParsedPrompt), ("getCharacterLastMessage", false, Gate::Always), ("getUserLastMessage", false, Gate::Always),
];

/// The history writes `restructure` serves beside `SCALAR_API`.
pub(super) const EDIT_API: [&str; 3] = ["removeChat", "insertChat", "setFullChatMain"];

pub(super) fn ir_limits() -> IrLimits {
    IrLimits { max_owned_bytes: 64 << 20, max_json_bytes: 8 << 20, max_text_units: 8 << 20, max_values: 1 << 20, max_depth: 128, max_work: 1 << 36 }
}

/// Whether `JSON.parse(String(argument))` returns, for an argument as Wasmoon passes it to JS.
fn json_parses(argument: Option<&GateArgument>) -> bool {
    match argument {
        // A Lua nil reaches JS as null.
        Some(GateArgument::Nil | GateArgument::Boolean(_)) => true,
        Some(GateArgument::Number(value)) => value.is_finite(),
        Some(GateArgument::String(bytes)) => js_string_units(bytes)
            .is_some_and(|units| Budget::new(ir_limits(), Arc::new(AtomicBool::new(false)))
                .is_ok_and(|budget| Text::from_units(&units, &budget)
                    .is_ok_and(|text| json::parse_json_text(&text, &budget).is_ok()))),
        _ => false,
    }
}

fn holds_safe_key(binding: &Rc<RefCell<Option<Binding>>>, arguments: &[GateArgument]) -> bool {
    let Ok(current) = binding.try_borrow() else { return true };
    current.as_ref().is_some_and(|current| matches!(current.permissions, Permissions::Safe)
        && matches!(arguments.first(), Some(GateArgument::String(id)) if id == current.key.as_bytes()))
}

// TS order: the access key check, then the arguments the native store access models.
fn serve(binding: &Rc<RefCell<Option<Binding>>>, edits: &Rc<RefCell<Vec<Edit>>>, name: &str, args: Vec<Scalar>,
    max_bytes: usize) -> Result<ScalarReturn, String> {
    let unsupported = |kind: &str| Ok(ScalarReturn::Unsupported(format!("lua-host-{kind}:{name}")));
    let (holds_key, safe) = {
        let current = binding.try_borrow().map_err(|_| "Lua host binding is busy")?;
        let current = current.as_ref().ok_or("Lua invocation ended")?;
        (matches!(args.first(), Some(Scalar::String(id)) if *id == current.key), matches!(current.permissions, Permissions::Safe))
    };
    let text = |index: usize| match args.get(index) { Some(Scalar::String(value)) => Some(value.clone()), _ => None };
    let number = |index: usize| match args.get(index) { Some(Scalar::Number(value)) => Some(*value), _ => None };
    let edit = match name {
        "getChatVar" if text(1).is_none() => return unsupported("args"),
        "getChatData" | "getChatRole" if number(1).is_none() => return unsupported("args"),
        "setChatVar" | "setChatVarChanged" if !holds_key => return Ok(ScalarReturn::NoValues),
        "setChatVar" | "setChatVarChanged" => match (text(1), text(2)) {
            (Some(key), Some(value)) => Some(Edit::Variable { key, value }),
            _ => return unsupported("args"),
        },
        "setChat" | "setChatRole" if !(holds_key && safe) => return Ok(ScalarReturn::NoValues),
        "setChat" => match (number(1), args.get(2)) {
            (Some(index), None | Some(Scalar::Nil)) => Some(Edit::MessageData { index, data: String::new() }),
            (Some(index), Some(Scalar::String(data))) => Some(Edit::MessageData { index, data: data.clone() }),
            _ => return unsupported("args"),
        },
        "setChatRole" => match number(1) {
            Some(index) => Some(Edit::MessageRole { index, role: if text(2).as_deref() == Some("user") { "user" } else { "char" } }),
            None => return unsupported("args"),
        },
        "removeChat" | "insertChat" | "setFullChatMain" if !(holds_key && safe) => return Ok(ScalarReturn::NoValues),
        "removeChat" | "insertChat" | "setFullChatMain" => return restructure(binding, edits, name, &args),
        _ => None,
    };
    match host(binding, name, args, max_bytes, true) {
        Ok(result) => {
            edits.borrow_mut().extend(edit);
            Ok(result)
        }
        // A store value or limit the native access scope does not model.
        Err(_) => unsupported("error"),
    }
}

/// `JSON.parse(value)` as `setFullChatMain` maps it: an array of objects with string `role` and `data`.
fn full_chat(value: Option<&Scalar>) -> Option<Vec<FullMessage>> {
    let Some(Scalar::String(text)) = value else { return None };
    let serde_json::Value::Array(items) = serde_json::from_str(text).ok()? else { return None };
    items.into_iter().map(|item| {
        let serde_json::Value::Object(mut item) = item else { return None };
        let chat_id = match item.remove("chatId") { Some(serde_json::Value::String(id)) => Some(id), _ => None };
        match (item.remove("role"), item.remove("data")) {
            (Some(serde_json::Value::String(role)), Some(serde_json::Value::String(data))) => Some(FullMessage { role, data, chat_id }),
            _ => None,
        }
    }).collect()
}

// `removeChat`, `insertChat` and `setFullChatMain` after the key check: the rows change in the access scope.
fn restructure(binding: &Rc<RefCell<Option<Binding>>>, edits: &Rc<RefCell<Vec<Edit>>>, name: &str, args: &[Scalar]) -> Result<ScalarReturn, String> {
    let unsupported = |kind: &str| Ok(ScalarReturn::Unsupported(format!("lua-host-{kind}:{name}")));
    let current = binding.try_borrow().map_err(|_| "Lua host binding is busy")?.as_ref().cloned().ok_or("Lua invocation ended")?;
    let store = current.store.try_borrow().map_err(|_| "Lua store is busy")?;
    let (access, permit) = (&current.access, &current.permit);
    let index = match args.get(1) { Some(Scalar::Number(index)) => Some(*index), _ => None };
    let edit = match (name, index) {
        ("removeChat", Some(index)) => access.remove_message(&store, permit, index).map(|()| Edit::Remove { index }),
        ("insertChat", Some(index)) => {
            let role = if matches!(args.get(2), Some(Scalar::String(role)) if role == "user") { "user" } else { "char" };
            let data = match args.get(3) { None | Some(Scalar::Nil) => String::new(), Some(Scalar::String(data)) => data.clone(), _ => return unsupported("args") };
            let chat_id = uuid::Uuid::new_v4().to_string();
            let mut message = serde_json::Map::new();
            message.insert("role".into(), role.into());
            message.insert("data".into(), data.clone().into());
            message.insert("chatId".into(), chat_id.clone().into());
            access.insert_message(&store, permit, index, message.into()).map(|()| Edit::Insert { index, role, data, chat_id })
        }
        ("setFullChatMain", _) => match full_chat(args.get(1)) {
            Some(messages) => access.replace_messages(&store, permit, messages).map(|messages| Edit::Replace { messages }),
            None => return unsupported("args"),
        },
        _ => return unsupported("args"),
    };
    match edit {
        Ok(edit) => {
            edits.borrow_mut().push(edit);
            Ok(ScalarReturn::NoValues)
        }
        // A windowed history, a store value or a limit the native access scope does not model.
        Err(_) => unsupported("error"),
    }
}

enum Run { Returned(Option<Vec<u16>>), Failed, Declined(String), Terminal(Terminal) }

struct EditEngine {
    runtime: Runtime,
    binding: Rc<RefCell<Option<Binding>>>,
    edits: Rc<RefCell<Vec<Edit>>>,
}

fn scope_error(error: AccessError) -> String { format!("script access: {error:?}") }

impl EditEngine {
    fn new(limits: Limits) -> Result<Self, String> {
        let failure = |error: LuaError| diagnostic(&error);
        let mut runtime = Runtime::new(limits).map_err(failure)?;
        runtime.install_wasmoon_profile(&[("json", JSON_MODULE)]).map_err(failure)?;
        runtime.disable_host_await().map_err(failure)?;
        let binding = Rc::new(RefCell::new(None));
        let edits = Rc::new(RefCell::new(Vec::new()));
        let (api_binding, api_edits) = (binding.clone(), edits.clone());
        let api = move |name: &str, args: Vec<Scalar>| serve(&api_binding, &api_edits, name, args, limits.values.utf8_bytes);
        register_values_api(&mut runtime, &binding, limits, api.clone()).map_err(failure)?;
        for name in EDIT_API { runtime.register_values_scalar(name, api.clone()).map_err(failure)?; }
        for (name, asynchronous, gate) in HOST_API {
            let gate_binding = binding.clone();
            let admitted: Box<dyn Fn(&[GateArgument]) -> bool> = match gate {
                Gate::Served => continue,
                Gate::Always => Box::new(|_| true),
                Gate::Safe => Box::new(move |arguments| holds_safe_key(&gate_binding, arguments)),
                Gate::LowLevel | Gate::Ignored => Box::new(|_| false),
                Gate::ParsedPrompt => Box::new(|arguments| !json_parses(arguments.get(1))),
                Gate::Log => Box::new(|arguments| !json_parses(arguments.first())),
            };
            runtime.register_unsupported(name, asynchronous, admitted).map_err(failure)?;
        }
        Ok(Self { runtime, binding, edits })
    }

    fn classify(&self, step: Step) -> Result<Run, String> {
        if let Some(label) = self.runtime.declined() { return Ok(Run::Declined(label)); }
        match step {
            Step::ListenerComplete(value) => Ok(Run::Returned(value)),
            Step::IgnoredComplete => Ok(Run::Returned(None)),
            Step::Failed(_) => Ok(Run::Failed),
            Step::Terminal(reason) => Ok(Run::Terminal(reason)),
            _ => Err("unexpected Lua edit step".into()),
        }
    }

    /// One invocation under its own access key; `start` receives the key.
    fn run(&mut self, access: &Rc<ScriptStoreAccess>, store: &Rc<RefCell<PersistentStore>>, permissions: Permissions,
        start: impl FnOnce(&mut Runtime, &str) -> LuaResult<()>, syntax_fails: bool) -> Result<Run, String> {
        let permit = access.begin_invocation(false).map_err(scope_error)?;
        let key = uuid::Uuid::new_v4().to_string();
        *self.binding.borrow_mut() = Some(Binding { access: access.clone(), permit: permit.clone(), store: store.clone(), key: key.clone(), permissions });
        let run = match start(&mut self.runtime, &key) {
            Err(error) => Ok(match (self.runtime.declined(), self.runtime.terminal_reason()) {
                (Some(label), _) => Run::Declined(label),
                (None, Some(reason)) => Run::Terminal(reason),
                // TS reports a syntax error of top-level code as an initialization error; it has no other limit here.
                (None, None) if syntax_fails && matches!(error, LuaError::SyntaxError { .. }) => Run::Failed,
                (None, None) => Run::Declined("lua-edit-limit".into()),
            }),
            Ok(()) => loop {
                match self.runtime.step() {
                    Step::Suspended => continue,
                    step => break self.classify(step),
                }
            },
        };
        self.binding.borrow_mut().take();
        access.end_invocation(&permit).map_err(scope_error)?;
        run
    }
}

/// One `runLuaEditTriggerBatch` call.
pub(super) struct Batch<'a> {
    /// `ownerChaId` of the engine cache key.
    pub owner: &'a str,
    pub mode: EditMode,
    /// The string, or for `editRequest` the prompt array, the batch starts from.
    pub content: Value,
    /// `meta.index`; `None` when TS passes no meta.
    pub index: Option<i64>,
    /// The code of each trigger whose first effect is `triggerlua`: character triggers, then module triggers.
    pub triggers: &'a [String],
    pub access: &'a Rc<ScriptStoreAccess>,
    pub store: &'a Rc<RefCell<PersistentStore>>,
    pub cancellation: &'a Cancellation,
    pub budget: &'a Budget,
}

/// Deep copy, keeping the property order.
pub(super) fn copy_value(value: &Value, budget: &Budget) -> Result<Value, IrError> {
    Ok(match value {
        Value::Undefined => Value::Undefined,
        Value::Null => Value::Null,
        Value::Bool(value) => Value::Bool(*value),
        Value::Number(value) => Value::Number(*value),
        Value::Text(text) => Value::Text(text.copy(budget)?),
        Value::Array(items) => {
            let mut copy = OwnedVec::new(budget)?;
            for item in items.as_slice() { copy.push(copy_value(item, budget)?)?; }
            Value::Array(copy)
        }
        Value::Object(entries) => {
            let mut copy = OwnedVec::new(budget)?;
            for (key, item) in entries.as_slice() { copy.push((key.copy(budget)?, copy_value(item, budget)?))?; }
            Value::Object(copy)
        }
    })
}

fn units(text: &Text) -> Vec<u16> { text.units().collect() }

/// Owns the engines across batches, keyed as TS keys them for one listener timeout: owner, mode and code.
pub(super) struct EditHost {
    /// Each engine with the use count of its last use.
    engines: HashMap<(String, EditMode, String), (EditEngine, u64)>,
    uses: u64,
    capacity: usize,
    limits: Limits,
    timeout: Duration,
    wrapper: (String, String),
}

impl EditHost {
    /// `timeout` bounds the top-level code and each listener call, as `LUA_LISTENER_TIMEOUT_MS` does; `capacity` is
    /// the `scriptingEngineCacheEntries` budget.
    pub(super) fn new(limits: Limits, timeout: Duration, capacity: usize) -> Result<Self, String> {
        Ok(Self { engines: HashMap::new(), uses: 0, capacity, limits, timeout, wrapper: wrapper()? })
    }

    // `evictIdleScriptingEngines` after each use: the least recently used engines go first.
    fn evict_idle(&mut self) {
        while self.engines.len() > self.capacity {
            let Some(oldest) = self.engines.iter().min_by_key(|(_, (_, used))| *used).map(|(key, _)| key.clone()) else { return };
            self.engines.remove(&oldest);
        }
    }

    pub(super) fn run(&mut self, batch: Batch<'_>) -> Result<EditOutcome, EditFailure> {
        let mut edits = Vec::new();
        let result = (|| {
            let ir = |error: IrError| format!("ir: {error:?}");
            let budget = batch.budget;
            let mut data = copy_value(&batch.content, budget).map_err(ir)?;
            let display = batch.mode == EditMode::Display;
            let finish = |value: Value, status: Status| Ok((value, status));
            let mut meta = OwnedVec::new(budget).map_err(ir)?;
            if let Some(index) = batch.index { meta.push((Text::from_str("index", budget).map_err(ir)?, Value::Number(index as f64))).map_err(ir)?; }
            let meta = units(&json::stringify(&Value::Object(meta), budget).map_err(ir)?.ok_or("meta has no JSON text")?);
            let mode: Vec<u16> = batch.mode.listener().encode_utf16().collect();
            for code in batch.triggers {
                self.evict_idle();
                if batch.cancellation.is_cancelled() { return finish(batch.content, Status::Cancelled); }
                let key = (batch.owner.to_owned(), batch.mode, code.clone());
                if !self.engines.contains_key(&key) {
                    let source = format!("{}{code}{}", self.wrapper.0, self.wrapper.1);
                    let limits = Limits { source_bytes: self.limits.source_bytes.saturating_add(source.len() - code.len()), ..self.limits };
                    let mut engine = EditEngine::new(limits)?;
                    let (timeout, cancellation) = (self.timeout, batch.cancellation.clone());
                    // Top-level code receives no key, so its writes are refused.
                    let run = engine.run(batch.access, batch.store, Permissions::Safe,
                        |runtime, _| runtime.start_ignoring_result(&source, timeout, cancellation), true);
                    edits.extend(engine.edits.borrow_mut().drain(..));
                    let run = run?;
                    match run {
                        Run::Returned(_) => { self.engines.insert(key.clone(), (engine, 0)); }
                        // TS discards the engine and the batch returns its original content.
                        Run::Failed | Run::Terminal(Terminal::Deadline) => return finish(batch.content, Status::Completed),
                        Run::Declined(label) => return finish(batch.content, Status::Declined(label)),
                        Run::Terminal(Terminal::Cancelled) => return finish(batch.content, Status::Cancelled),
                    }
                }
                let value = match (&data, display) {
                    (Value::Text(text), true) => units(text),
                    (_, true) => return finish(batch.content, Status::Declined("lua-edit-display-data".into())),
                    (_, false) => match json::stringify(&data, budget).map_err(ir)? {
                        Some(text) => units(&text),
                        None => return finish(batch.content, Status::Declined("lua-edit-undefined-data".into())),
                    },
                };
                let (engine, used) = self.engines.get_mut(&key).expect("initialized engine");
                self.uses += 1;
                *used = self.uses;
                let (timeout, cancellation) = (self.timeout, batch.cancellation.clone());
                let permissions = if display { Permissions::EditDisplay } else { Permissions::Safe };
                let run = engine.run(batch.access, batch.store, permissions, |runtime, access_key| {
                    let access_key: Vec<u16> = access_key.encode_utf16().collect();
                    runtime.start_listener([&mode, &access_key, &value, &meta], timeout, cancellation)
                }, false);
                edits.extend(engine.edits.borrow_mut().drain(..));
                let run = run?;
                let status = match run {
                    Run::Returned(None) | Run::Failed => continue,
                    Run::Returned(Some(result)) if display => { data = Value::Text(Text::from_units(&result, budget).map_err(ir)?); continue }
                    Run::Returned(Some(result)) => {
                        let text = Text::from_units(&result, budget).map_err(ir)?;
                        match json::parse_json_text(&text, budget) {
                            Ok(Value::Null | Value::Undefined) => continue,
                            Ok(value) => { data = value; continue }
                            // `JSON.parse` threw after `res` held the string, and the batch keeps it.
                            Err(IrError::Syntax { .. }) => { data = Value::Text(text); continue }
                            Err(_) => Status::Declined("lua-edit-limit".into()),
                        }
                    },
                    Run::Declined(label) => Status::Declined(label),
                    Run::Terminal(Terminal::Deadline) => Status::TimedOut,
                    Run::Terminal(Terminal::Cancelled) => Status::Cancelled,
                };
                // A declined, timed-out or cancelled engine is never reused.
                self.engines.remove(&key);
                return finish(batch.content, status);
            }
            self.evict_idle();
            finish(data, Status::Completed)
        })();
        match result {
            Ok((value, status)) => Ok(EditOutcome { value, edits, status }),
            Err(reason) => Err(EditFailure::new(reason, edits)),
        }
    }
}

/// What the edit owner supplies beside the request. Triggers are the selected character's first-effect Lua
/// scripts followed by module scripts. Keep the access/store target captured for the complete batch and use the
/// same scope for read-after-write. The owner constructs `EditHost` with `scriptingEngineCacheEntries`.
pub(super) struct Context<'a> {
    pub owner: &'a str,
    pub triggers: &'a [String],
    pub access: &'a Rc<ScriptStoreAccess>,
    pub store: &'a Rc<RefCell<PersistentStore>>,
    pub cancellation: &'a Cancellation,
    pub budget: &'a Budget,
}

/// The one seam from this host's types to the IR host protocol.
pub(super) fn answer(host: &mut EditHost, request: HostRequest, context: Context<'_>) -> Result<HostReply, EditFailure> {
    let unsupported = |label: &'static str| HostReply::Unsupported(Cow::Borrowed(label));
    let HostRequest::Lua { trigger: LuaTrigger::Edit { index }, mode, payload } = request else { return Ok(unsupported("lua-trigger")) };
    let mode = match mode {
        // `runLuaEditTrigger` returns the content for `editprocess` without running Lua.
        LuaMode::Script(ScriptMode::EditProcess) => return Ok(HostReply::Lua(LuaReply { value: payload, effects: Vec::new(), stop_sending: false })),
        LuaMode::Script(ScriptMode::EditInput) => EditMode::Input,
        LuaMode::Script(ScriptMode::EditOutput) => EditMode::Output,
        LuaMode::Script(ScriptMode::EditDisplay) => EditMode::Display,
        LuaMode::EditRequest => EditMode::Request,
        _ => return Ok(unsupported("lua-trigger")),
    };
    let batch = Batch { owner: context.owner, mode, content: payload, index, triggers: context.triggers, access: context.access,
        store: context.store, cancellation: context.cancellation, budget: context.budget };
    let outcome = host.run(batch)?;
    let reason = match outcome.status {
        Status::Declined(label) => Some(label),
        Status::TimedOut => Some("lua-edit-timeout".into()),
        Status::Cancelled if outcome.edits.is_empty() => return Ok(HostReply::Cancelled),
        Status::Cancelled => Some("lua-edit-cancelled".into()),
        Status::Completed => None,
    };
    if let Some(reason) = reason { return Err(EditFailure::new(reason, outcome.edits)); }
    match edit_effects(&outcome.edits, context.budget) {
        Ok(effects) => Ok(HostReply::Lua(LuaReply { value: outcome.value, effects, stop_sending: false })),
        Err(error) => Err(EditFailure::new(format!("lua-edit-effect-conversion: {error:?}"), outcome.edits)),
    }
}

/// The effects of a batch's edits, in call order.
pub(super) fn edit_effects(edits: &[Edit], budget: &Budget) -> Result<Vec<Effect>, IrError> {
    let text = |value: &str| Text::from_str(value, budget);
    let role = |role: &str| if role == "user" { MessageRole::User } else { MessageRole::Char };
    edits.iter().map(|edit| -> Result<Effect, IrError> { Ok(match edit {
        Edit::Variable { key, value } => Effect::ChatVariable { name: text(key)?, value: Value::Text(text(value)?) },
        Edit::MessageData { index, data } => Effect::Message(MessageEdit::SetData { index: *index, data: Value::Text(text(data)?) }),
        Edit::MessageRole { index, role: name } => Effect::Message(MessageEdit::SetRole { index: *index, role: role(name) }),
        Edit::Remove { index } => Effect::Message(MessageEdit::Remove { index: *index }),
        Edit::Insert { index, role: name, data, chat_id } => Effect::Message(MessageEdit::Insert {
            index: *index, role: role(name), data: Value::Text(text(data)?), chat_id: text(chat_id)?,
        }),
        Edit::Replace { messages } => {
            let messages = serde_json::to_string(messages).map_err(|_| IrError::Invariant("message-replace"))?;
            Effect::Message(MessageEdit::Replace { messages: json::parse_json(&messages, budget)? })
        }
    }) }).collect()
}
