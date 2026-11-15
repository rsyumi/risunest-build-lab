//! The Lua host behind `HostRequest::Lua`: the edit listeners `runLuaEditTriggerBatch` runs and the `triggerlua` calls
//! `runScripted` makes (`src/ts/process/scriptings.ts`), on the main-thread Wasmoon engine profile.
use super::access::FullMessage;
use super::lua_scalar_adapter::{host, register_budgeted_values_api, Binding, Permissions, ScalarOutput};
use super::lua_variables::{RawOperation, ScopeKind, VariableFinish};
use super::lua_view::{Assigned, Conversation, ConversationSnapshot, Overlay, PersonaProcessor, persona_field, author_note, record_value, with_records, property, ir_value, variable_input};
use risunest_ir::chat::MessageRole;
use risunest_ir::stage::{Effect, HostReply, HostRequest, LuaCall, LuaDeadline, LuaMode, LuaReply, LuaTrigger, LuaView, MessageEdit, ScriptMode};
use risunest_ir::{json, Budget, Charge, Error as IrError, Limits as IrLimits, OwnedVec, Text, TextBuilder, Value};
use risunest_native_lua_experimental::{diagnostic, js_string_units, Cancellation, GateArgument, Limits, LuaError, LuaResult, Runtime,
    Scalar, ScalarReturn, Step, Terminal, VariableValue, VariableReturn, ValueLimits, ValueVisitor, ValueEvent, PropertyKeyVisitor};
use std::{borrow::Cow, cell::{Cell, RefCell}, collections::HashMap, panic::{catch_unwind, AssertUnwindSafe}, rc::Rc,
    sync::{atomic::AtomicBool, Arc}, time::Duration};
#[cfg(test)]
use super::{access::ScriptStoreAccess, lua_view::StoreSource};
#[cfg(test)]
use crate::persistent_store::PersistentStore;

const WRAPPER: &str = include_str!("../../../src/ts/process/luaCodeWrapper.ts");
const JSON_MODULE: &str = include_str!("../../../public/lua/json.lua");

/// A host panic during a request: the engine is dropped and the request fails.
pub(super) const PANICKED: &str = "lua-host-panic";
/// A runtime step no host request expects, such as a `hostAwait` suspension.
const UNEXPECTED: &str = "lua-unexpected-step";
/// A test-only host function whose call panics, to exercise panic containment.
#[cfg(test)]
pub(super) const TEST_PANIC: &str = "risunestTestPanic";
#[cfg(test)]
pub(super) const TEST_UNWIND: &str = "risunestTestUnwind";
#[cfg(test)]
pub(super) const TEST_BUDGET_REMAINING:&str="risunestTestBudgetRemaining";

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

/// A conversation write a call made, in call order.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Edit {
    /// `setVar(key, value)`, which writes the trigger local of that name when one exists.
    Variable { key: String, value: Assigned },
    RawVariable { key: VariableValue, value: Assigned },
    MessageData { index: f64, data: String },
    MessageRole { index: f64, role: &'static str },
    Cut { start: f64, end: Option<f64> },
    Append { role: &'static str, data: String, chat_id: String },
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
    Cancelled,
}

#[derive(Debug)]
pub(super) struct EditOutcome {
    /// The batch result; the original content when an initializer failed.
    pub value: Value,
    /// Made through the source whatever the status; TS keeps them in every case.
    pub edits: Vec<Edit>,
    pub saved: SavedEdits,
    pub status: Status,
}

/// A native execution failure. The source already holds these writes; the owner must settle them once, terminate the
/// operation and never replay the edits or retry through another engine.
#[derive(Debug)]
pub(super) struct EditFailure {
    pub reason: String,
    pub edits: Vec<Edit>,
    pub saved: SavedEdits,
}

impl EditFailure {
    fn new(mut reason: String, edits: Vec<Edit>, saved: SavedEdits) -> Self {
        if reason.len() > 4096 {
            let mut end = 4093;
            while !reason.is_char_boundary(end) { end -= 1; }
            reason.truncate(end);
            reason.push_str("...");
        }
        Self { reason, edits, saved }
    }
}

/// Reply effects are converted and charged before their writes become visible. The parallel edit charge accounts for
/// the compatibility journal's owned payload and vector capacity until the owner consumes the result.
#[derive(Debug, Default)]
pub(super) struct SavedEdits {
    pub effects: Vec<Effect>,
    pub variables: Vec<RawOperation>,
    charges: Vec<Charge>,
}

struct Journal { budget: Budget, saved: SavedEdits }
struct Prepared { edit: Edit, effect: Option<Effect>, variable: Option<RawOperation>, charge: Charge }
type ActiveJournal = Rc<RefCell<Option<Rc<RefCell<Journal>>>>>;

impl Journal {
    fn new(budget: &Budget) -> Rc<RefCell<Self>> { Rc::new(RefCell::new(Self { budget: budget.clone(), saved: SavedEdits::default() })) }
    fn take(&mut self) -> SavedEdits { std::mem::take(&mut self.saved) }
}

fn prepare(journal: &ActiveJournal, edit: Edit) -> Result<Prepared, String> {
    use crate::persistent_store::script_access::owned_value_bytes;
    let current = journal.borrow().as_ref().cloned().ok_or("lua-host-journal")?;
    let current = current.borrow();
    let payload = match &edit {
        Edit::Variable { key, value: Assigned::Text(value) } => key.capacity().saturating_add(value.capacity()),
        Edit::Variable { key, .. } => key.capacity(),
        Edit::RawVariable { key, value } => key.owned_bytes() + match value { Assigned::Text(value) => value.capacity(), _ => 0 },
        Edit::MessageData { data, .. } => data.capacity(),
        Edit::Append { data, chat_id, .. } | Edit::Insert { data, chat_id, .. } => data.capacity().saturating_add(chat_id.capacity()),
        Edit::Replace { messages } => messages.capacity().saturating_mul(std::mem::size_of::<serde_json::Value>())
            .saturating_add(messages.iter().map(owned_value_bytes).sum::<usize>()),
        _ => 0,
    };
    let overhead = 4 * (std::mem::size_of::<Edit>() + std::mem::size_of::<Effect>() + std::mem::size_of::<Charge>());
    let charge = current.budget.reserve_owned(payload.saturating_add(overhead)).map_err(|_| "lua-host-limit".to_owned())?;
    let effect = edit_effects(std::slice::from_ref(&edit), &current.budget).map_err(|_| "lua-host-limit".to_owned())?
        .pop().ok_or("lua-host-journal")?;
    Ok(Prepared { edit, effect: Some(effect), variable: None, charge })
}


fn prepare_raw(journal: &ActiveJournal, operation: &RawOperation) -> Result<Prepared, String> {
    let current = journal.borrow().as_ref().cloned().ok_or("lua-host-journal")?;
    let current = current.borrow();
    let key_bytes = operation.key.text_units().map_or(0, |key| key.len().saturating_mul(3));
    let value_bytes = match &operation.value {
        Assigned::Text(value) => value.capacity().saturating_mul(2),
        Assigned::Structured { input, .. } => input.text_units().map_or(0, |units| units.len().saturating_mul(4)),
        _ => 0,
    };
    let overhead = 4 * (std::mem::size_of::<Edit>() + std::mem::size_of::<Effect>()
        + std::mem::size_of::<Charge>() + std::mem::size_of::<RawOperation>());
    let charge = current.budget.reserve_owned(key_bytes.saturating_add(value_bytes).saturating_add(overhead))
        .map_err(|_| "lua-host-limit".to_owned())?;
    current.budget.work(operation.key.text_units().map_or(1, |key| key.len().saturating_add(1)))
        .map_err(|_| "lua-host-limit".to_owned())?;
    let edit = match operation.key.text_units().and_then(|key| String::from_utf16(key).ok()) {
        Some(key) => Edit::Variable { key, value: operation.value.clone() },
        None => Edit::RawVariable { key: operation.key.clone(), value: operation.value.clone() },
    };
    Ok(Prepared { edit, effect: None, variable: Some(operation.clone()), charge })
}

fn legacy_variable_effects(saved: &mut SavedEdits, edits: &[Edit]) -> Result<(), String> {
    let variable_count = edits.iter().filter(|edit| matches!(edit, Edit::Variable { .. } | Edit::RawVariable { .. })).count();
    if saved.variables.len() != variable_count || saved.effects.len() != edits.len() - variable_count {
        return Err("lua-host-journal".into());
    }
    // The journal admitted these output slots before publication. Merge prepared owners without reconversion.
    let mut chronological = Vec::with_capacity(edits.len());
    let mut effects = std::mem::take(&mut saved.effects).into_iter();
    let mut variables = saved.variables.iter();
    for edit in edits {
        match edit {
            Edit::Variable { .. } | Edit::RawVariable { .. } => {
                if let Some(operation) = variables.next() {
                    if operation.local || operation.key.text_units().is_some() {
                        if let Some(effect) = operation.take_effect() { chronological.push(effect); }
                    }
                }
            },
            _ => if let Some(effect) = effects.next() { chronological.push(effect); },
        }
    }
    saved.effects = chronological;
    Ok(())
}

fn record_edit(journal: &ActiveJournal, edits: &Rc<RefCell<Vec<Edit>>>, prepared: Prepared) -> Result<(), String> {
    let current = journal.borrow().as_ref().cloned().ok_or("lua-host-journal")?;
    let mut current = current.borrow_mut();
    if let Some(effect) = prepared.effect { current.saved.effects.push(effect); }
    if let Some(variable) = prepared.variable { current.saved.variables.push(variable); }
    current.saved.charges.push(prepared.charge);
    edits.borrow_mut().push(prepared.edit);
    Ok(())
}

/// What a host name of `scriptings.ts` does here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Gate {
    /// Served by `host`, the history readers and `restructure`.
    Served,
    /// Declined on every call.
    Always,
    /// Declined when the first argument is a `ScriptingSafeIds` key, otherwise refused as TS refuses it.
    Safe,
    /// Declined when the first argument is a `ScriptingLowLevelIds` key, otherwise refused as TS refuses it.
    LowLevel,
    /// `LLMMain` and `axLLMMain` parse their prompt before the low-level check: declined when it would throw, or when
    /// the call holds a `ScriptingLowLevelIds` key.
    ParsedPrompt,
    /// `logMain`: `JSON.parse(value)` is all a call observes; declined when it would throw.
    Log,
    /// `stopChat`: sets the stop of a call holding a `ScriptingSafeIds` key. Every edit batch ignores it.
    Stop,
}

/// Every host name `scriptings.ts` declares for Lua, with whether it is `async`, in declaration order.
pub(super) const HOST_API: [(&str, bool, Gate); 54] = [
    ("getChatVar", false, Gate::Served), ("setChatVar", false, Gate::Served), ("setChatVarChanged", false, Gate::Served),
    ("getGlobalVar", false, Gate::Served), ("stopChat", false, Gate::Stop), ("alertError", false, Gate::Safe),
    ("alertNormal", false, Gate::Safe), ("alertInput", false, Gate::Safe), ("alertSelect", false, Gate::Safe),
    ("alertConfirm", false, Gate::Safe), ("getChatMain", false, Gate::Served), ("getChatData", false, Gate::Served),
    ("getChatRole", false, Gate::Served), ("getRecentChatsMain", false, Gate::Served), ("setChat", false, Gate::Served),
    ("setChatRole", false, Gate::Served), ("cutChat", false, Gate::Served), ("removeChat", false, Gate::Served),
    ("addChat", false, Gate::Served), ("insertChat", false, Gate::Served), ("getTokens", true, Gate::Safe),
    ("getChatLength", false, Gate::Served), ("getFullChatMain", false, Gate::Served), ("sleep", false, Gate::Safe),
    ("cbs", false, Gate::Always), ("setFullChatMain", false, Gate::Served), ("logMain", false, Gate::Log),
    ("reloadDisplay", false, Gate::Safe), ("reloadChat", false, Gate::Safe), ("similarity", true, Gate::LowLevel),
    ("request", true, Gate::LowLevel), ("generateImage", true, Gate::LowLevel), ("getCharacterImageMain", true, Gate::Always),
    ("getPersonaImageMain", true, Gate::Always), ("hash", true, Gate::Always), ("LLMMain", true, Gate::ParsedPrompt),
    ("simpleLLM", true, Gate::LowLevel), ("getName", false, Gate::Served), ("setName", false, Gate::Safe),
    ("getDescription", false, Gate::Served), ("setDescription", false, Gate::Safe), ("getCharacterFirstMessage", false, Gate::Served),
    ("setCharacterFirstMessage", false, Gate::Safe), ("getPersonaName", false, Gate::Served),
    ("getPersonaDescription", false, Gate::Served), ("getAuthorsNote", false, Gate::Served),
    ("getBackgroundEmbedding", false, Gate::Safe), ("setBackgroundEmbedding", false, Gate::Safe),
    ("getLoreBooksMain", false, Gate::Always), ("upsertLocalLoreBook", false, Gate::Safe), ("loadLoreBooksMain", true, Gate::LowLevel),
    ("axLLMMain", true, Gate::ParsedPrompt), ("getCharacterLastMessage", false, Gate::Served), ("getUserLastMessage", false, Gate::Served),
];

/// The history writes `restructure` serves beside `SCALAR_API`.
pub(super) const EDIT_API: [&str; 5] = ["cutChat", "addChat", "removeChat", "insertChat", "setFullChatMain"];
pub(super) const RECORD_API: [&str; 9] = ["getGlobalVar", "getName", "getDescription", "getCharacterFirstMessage", "getPersonaName",
    "getPersonaDescription", "getAuthorsNote", "getCharacterLastMessage", "getUserLastMessage"];

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

/// Whether the first argument is the invocation's key and that key is in `ScriptingSafeIds`, or with `low_level` in
/// `ScriptingLowLevelIds`. A busy binding counts as held, so the gate declines.
fn holds_key(binding: &Rc<RefCell<Option<Binding>>>, arguments: &[GateArgument], low_level: bool) -> bool {
    let Ok(current) = binding.try_borrow() else { return true };
    current.as_ref().is_some_and(|current| {
        let held = if low_level { current.permissions == Permissions::LowLevel } else { current.permissions.safe() };
        held && matches!(arguments.first(), Some(GateArgument::String(id)) if id == current.key.as_bytes())
    })
}

// TS order: the access key check, then the arguments the native source models.
fn serve(binding: &Rc<RefCell<Option<Binding>>>, edits: &Rc<RefCell<Vec<Edit>>>, journal: &ActiveJournal, name: &str, args: Vec<Scalar>,
    max_bytes: usize) -> Result<ScalarOutput, String> {
    let mut ir = None;
    let mut utf8 = None;
    let result = (|| -> Result<ScalarReturn, String> {
    let unsupported = |kind: &str| Ok(ScalarReturn::Unsupported(format!("lua-host-{kind}:{name}")));
    let (holds_key, safe) = {
        let current = binding.try_borrow().map_err(|_| "Lua host binding is busy")?;
        let current = current.as_ref().ok_or("Lua invocation ended")?;
        (matches!(args.first(), Some(Scalar::String(id)) if *id == current.key), current.permissions.safe())
    };
    let text = |index: usize| match args.get(index) { Some(Scalar::String(value)) => Some(value.clone()), _ => None };
    let number = |index: usize| match args.get(index) { Some(Scalar::Number(value)) => Some(*value), _ => None };
    let edit = match name {
        "getChatVar" if text(1).is_none() => return unsupported("args"),
        "getChatData" | "getChatRole" if number(1).is_none() => return unsupported("args"),
        "setChatVar" | "setChatVarChanged" if !holds_key => return Ok(ScalarReturn::NoValues),
        // TS stores the value as given.
        "setChatVar" | "setChatVarChanged" => match text(1) {
            Some(key) => {
                let current = binding.borrow().as_ref().cloned().ok_or("Lua invocation ended")?;
                let value = Assigned::argument(args.get(2));
                let budget = journal.borrow().as_ref().cloned().ok_or("lua-host-journal")?.borrow().budget.clone();
                let raw_key = match super::lua_variables::string_key(&key, &budget) {
                    Ok(key) => key, Err(_) => return Ok(ScalarReturn::Unsupported("lua-host-limit".into())),
                };
                let mut prepared = None;
                return match current.source.set_raw_variable(raw_key, value, &mut |operation| {
                    prepared = Some(prepare_raw(journal, operation)?); Ok(())
                }) {
                    Ok(changed) => {
                        if changed { record_edit(journal, edits, prepared.ok_or("lua-host-journal")?)?; }
                        Ok(if name == "setChatVarChanged" && changed { ScalarReturn::One(Scalar::Boolean(true)) } else { ScalarReturn::NoValues })
                    }
                    Err(_) => unsupported("error"),
                };
            },
            None => return unsupported("args"),
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
        "cutChat" | "addChat" | "removeChat" | "insertChat" | "setFullChatMain" if !(holds_key && safe) => return Ok(ScalarReturn::NoValues),
        "cutChat" | "addChat" | "removeChat" | "insertChat" | "setFullChatMain" => return restructure(binding, edits, journal, name, &args),
        _ => None,
    };
    let prepared = match edit.map(|edit| prepare(journal, edit)).transpose() {
        Ok(prepared) => prepared, Err(label) => return Ok(ScalarReturn::Unsupported(label)),
    };
    match host(binding, name, args, max_bytes, true) {
        Ok(result) => {
            if let Some(prepared) = prepared { record_edit(journal, edits, prepared)?; }
            ir = result.ir;
            utf8 = result.utf8;
            Ok(result.value)
        }
        // A stored value or limit the native source does not model.
        Err(_) => unsupported("error"),
    }
    })()?;
    Ok(ScalarOutput { value: result, ir, utf8 })
}

struct KeyBuilder<'a> {budget:&'a Budget,text:TextBuilder,scratch:OwnedVec<Charge>}

impl ValueVisitor for KeyBuilder<'_> {
    fn visit(&mut self,event:ValueEvent<'_>)->Result<(),String>{
        let value=match event{
            ValueEvent::Undefined=>Value::Undefined,ValueEvent::Null=>Value::Null,ValueEvent::Boolean(value)=>Value::Bool(value),
            ValueEvent::Number(value)=>Value::Number(value),
            ValueEvent::Text(text)=>Value::Text(Text::from_units(text,self.budget).map_err(|error|format!("ir: {error:?}"))?),
            _=>return Err("unsupported variable key event".into()),
        };
        let text=value.to_text(self.budget).map_err(|error|format!("ir: {error:?}"))?;
        for unit in text.units(){self.text.unit(unit).map_err(|error|format!("ir: {error:?}"))?;}
        Ok(())
    }
}

impl PropertyKeyVisitor for KeyBuilder<'_> {
    fn checkpoint(&mut self)->Result<(),String>{self.budget.work(1).map_err(|error|format!("ir: {error:?}"))}
    fn reserve(&mut self,bytes:usize)->Result<(),String>{
        let charge=self.budget.reserve_owned(bytes).map_err(|error|format!("ir: {error:?}"))?;
        self.scratch.push(charge).map_err(|error|format!("ir: {error:?}"))
    }
    fn number_units(&mut self,value:f64)->Result<Vec<u16>,String>{
        let text=Value::Number(value).to_text(self.budget).map_err(|error|format!("ir: {error:?}"))?;
        self.reserve(text.len_units().saturating_mul(2))?;
        Ok(text.units().collect())
    }
}

pub(super) fn property_key(argument:Option<&VariableValue>,budget:&Budget)->Result<Text,String>{
    let mut builder=KeyBuilder {budget,text:TextBuilder::new(budget).map_err(|error|format!("ir: {error:?}"))?,
        scratch:OwnedVec::new(budget).map_err(|error|format!("ir: {error:?}"))?};
    match argument{Some(value)=>value.visit_property_key(&mut builder)?,None=>builder.visit(ValueEvent::Undefined)?};
    builder.text.finish().map_err(|error|format!("ir: {error:?}"))
}

pub(super) fn storage_key(argument:&VariableValue,budget:&Budget)->Result<Text,String>{
    let mut builder=KeyBuilder {budget,text:TextBuilder::new(budget).map_err(|error|format!("ir: {error:?}"))?,
        scratch:OwnedVec::new(budget).map_err(|error|format!("ir: {error:?}"))?};
    argument.visit_storage_key(&mut builder)?;
    builder.text.finish().map_err(|error|format!("ir: {error:?}"))
}

fn variable_error(name: &str, error: String) -> Result<VariableReturn, String> {
    if error.starts_with("lua-script-error:") { return Err(error); }
    let label = if error == "lua-raw-scope-required" { error }
        else if error.contains("limit") || error.contains("Limit") { "lua-host-limit".into() }
        else { format!("lua-host-error:{name}") };
    Ok(VariableReturn::Unsupported(label))
}

fn serve_variable(binding:&Rc<RefCell<Option<Binding>>>,edits:&Rc<RefCell<Vec<Edit>>>,journal:&ActiveJournal,
    name:&str,args:Vec<VariableValue>,limits:ValueLimits)->Result<VariableReturn,String> {
    let current=binding.try_borrow().map_err(|_|"Lua host binding is busy")?.as_ref().cloned().ok_or("Lua invocation ended")?;
    if name!="getChatVar"&&!matches!(args.first().and_then(VariableValue::as_scalar),Some(Scalar::String(key)) if key==current.key){return Ok(VariableReturn::NoValues);}
    let undefined = VariableValue::undefined(limits);
    let key = args.get(1).unwrap_or(&undefined);
    if name == "getChatVar" {
        return match current.source.raw_variable_input(key, limits) {
            Ok(value) if value.is_undefined() => Ok(VariableReturn::NoValues),
            Ok(value) => Ok(VariableReturn::One(value)),
            Err(error) => variable_error(name, error),
        };
    }
    let budget=journal.borrow().as_ref().cloned().ok_or("lua-host-journal")?.borrow().budget.clone();
    let value = match Assigned::converted(args.get(2).cloned(), &budget) {
        Ok(value) => value, Err(error) => return variable_error(name, error),
    };
    let mut prepared = None;
    let changed = current.source.set_raw_variable(key.clone(), value, &mut |operation| {
        prepared = Some(prepare_raw(journal, operation)?); Ok(())
    });
    let changed = match changed { Ok(changed) => changed, Err(error) => return variable_error(name, error) };
    if changed { record_edit(journal, edits, prepared.ok_or("lua-host-journal")?)?; }
    Ok(if changed && name == "setChatVarChanged" { VariableReturn::One(VariableValue::scalar(Scalar::Boolean(true), limits)) }
        else { VariableReturn::NoValues })
}

fn record(binding: &Rc<RefCell<Option<Binding>>>, journal: &ActiveJournal, name: &str, args: &[VariableValue], limits: ValueLimits) -> Result<VariableReturn, String> {
    let current = binding.borrow().as_ref().cloned().ok_or("Lua invocation ended")?;
    if name == "getDescription" && (!current.permissions.safe()
        || !matches!(args.first().and_then(VariableValue::as_scalar), Some(Scalar::String(key)) if key == current.key)) { return Ok(VariableReturn::NoValues); }
    let source = &current.source;
    let active=journal.borrow().as_ref().cloned().ok_or("lua-host-journal")?;
    let budget=active.borrow().budget.clone();
    let result = (|| -> Result<Option<VariableValue>, String> {
        let value = match name {
            "getName" => record_value(&**source, "character", "name", &budget)?,
            "getDescription" => record_value(&**source, "character", "desc", &budget)?,
            "getCharacterFirstMessage" => record_value(&**source, "character", "firstMessage", &budget)?,
            "getPersonaName" => persona_field(&**source, "name", "User", &budget)?,
            "getPersonaDescription" => return source.persona_description(&active.borrow().saved.effects)
                .and_then(|value| value.map(|value| variable_input(&value, limits, &budget)).transpose()),
            "getAuthorsNote" => Some(author_note(&**source, &budget)?),
            "getGlobalVar" => {
                let key = property_key(args.get(1), &budget)?;
                Some(with_records(&**source, &budget, |records| {
                    let local = records.field("conversation", "GLGlobalVariables")?;
                    if let Some(value) = local.as_ref().map(|values| property(values, &key, &budget)).transpose()?.flatten()
                        .filter(|value| value.truthy() && !matches!(value, Value::Text(text) if text.equals_ascii(b"null"))) {
                        return copy_value(value, &budget).map_err(|error| format!("ir: {error:?}"));
                    }
                    drop(local);
                    let values = records.field("root", "globalChatVariables")?
                        .filter(|value| !matches!(value, Value::Null | Value::Undefined))
                        .ok_or("lua-script-error:global variables are nullish")?;
                    match property(&values, &key, &budget)?.filter(|value| !matches!(value, Value::Null | Value::Undefined)) {
                        Some(value) => copy_value(value, &budget).map_err(|error| format!("ir: {error:?}")),
                        None => Ok(Value::Text(Text::from_str("null", &budget).map_err(|error| format!("ir: {error:?}"))?)),
                    }
                })?)
            }
            "getCharacterLastMessage" | "getUserLastMessage" => {
                let role = if name == "getCharacterLastMessage" { "char" } else { "user" };
                for index in (0..source.length()?).rev() {
                    if matches!(source.message_value(index as f64, "role", &budget)?, Some(Value::Text(text)) if text.equals_ascii(role.as_bytes())) {
                        return source.message_value(index as f64, "data", &budget)?
                            .map(|value| variable_input(&value, limits, &budget)).transpose();
                    }
                }
                if role == "char" { Some(match record_value(&**source, "character", "firstMessage", &budget)? {
                    None | Some(Value::Null | Value::Undefined) => Value::Text(Text::from_str("", &budget).map_err(|error| format!("ir: {error:?}"))?),
                    Some(value) => value,
                }) }
                else { Some(Value::Text(Text::from_str("", &budget).map_err(|error| format!("ir: {error:?}"))?)) }
            }
            _ => return Err("unsupported record API".into()),
        };
        value.map(|value| variable_input(&value, limits, &budget)).transpose()
    })();
    Ok(match result {
        Ok(Some(value)) => VariableReturn::One(value),
        Ok(None)=>VariableReturn::NoValues,
        Err(error) if error.starts_with("lua-script-error:") => return Err(error),
        Err(error) if error.contains("limit")||error.contains("Limit")=>VariableReturn::Unsupported("lua-host-limit".into()),
        _ => VariableReturn::Unsupported(format!("lua-host-error:{name}")),
    })
}

fn numeric(value: Option<&Scalar>) -> Option<f64> {
    match value {
        None => Some(f64::NAN), Some(Scalar::Nil) => Some(0.0), Some(Scalar::Boolean(value)) => Some(if *value { 1.0 } else { 0.0 }),
        Some(Scalar::Number(value)) => Some(*value),
        Some(Scalar::String(value)) => {
            let budget = Budget::new(ir_limits(), Default::default()).ok()?;
            risunest_ir::num::to_number(&Text::from_str(value, &budget).ok()?, &budget).ok()
        }
    }
}

/// `JSON.parse(value)` as `setFullChatMain` maps it: an array of objects with string `role` and `data`.
struct FullChat { messages: Vec<FullMessage>, _held: Charge }

fn full_chat(value: Option<&Scalar>, budget: &Budget) -> Result<Option<FullChat>, IrError> {
    let Some(Scalar::String(text)) = value else { return Ok(None); };
    let value = json::parse_json(text, budget)?;
    let Value::Array(rows) = &value else { return Ok(None); };
    let mut bytes = rows.as_slice().len().saturating_mul(4 * std::mem::size_of::<FullMessage>());
    for row in rows.as_slice() {
        if !matches!(row, Value::Object(_)) { return Ok(None); }
        for key in [b"role".as_slice(), b"data".as_slice()] {
            let Some(Value::Text(text)) = row.get(key, budget)? else { return Ok(None); };
            let Some(text) = text.as_unicode() else { return Ok(None); };
            bytes = bytes.saturating_add(text.len());
        }
        if let Some(Value::Text(text)) = row.get(b"chatId", budget)? {
            if let Some(text) = text.as_unicode() { bytes = bytes.saturating_add(text.len()); }
        }
    }
    budget.work(bytes)?;
    let held = budget.reserve_owned(bytes)?;
    let mut messages = Vec::with_capacity(rows.as_slice().len());
    for row in rows.as_slice() {
        let field = |key: &[u8]| -> Result<Option<&str>, IrError> {
            Ok(match row.get(key, budget)? { Some(Value::Text(text)) => text.as_unicode(), _ => None })
        };
        let (Some(role), Some(data)) = (field(b"role")?, field(b"data")?) else { return Err(IrError::Invariant("full-chat-fields")); };
        messages.push(FullMessage { role: role.to_owned(), data: data.to_owned(), chat_id: field(b"chatId")?.map(str::to_owned) });
    }
    Ok(Some(FullChat { messages, _held: held }))
}

// `removeChat`, `insertChat` and `setFullChatMain` after the key check: the rows change in the source.
fn restructure(binding: &Rc<RefCell<Option<Binding>>>, edits: &Rc<RefCell<Vec<Edit>>>, journal: &ActiveJournal,
    name: &str, args: &[Scalar]) -> Result<ScalarReturn, String> {
    let unsupported = |kind: &str| Ok(ScalarReturn::Unsupported(format!("lua-host-{kind}:{name}")));
    let current = binding.try_borrow().map_err(|_| "Lua host binding is busy")?.as_ref().cloned().ok_or("Lua invocation ended")?;
    let source = &current.source;
    // `addChat` goes through `appendMessage`, which opens the message edits rather than the operation.
    source.open_variable_scope(if name == "addChat" { ScopeKind::Edits } else { ScopeKind::Operation })?;
    let index = match args.get(1) { Some(Scalar::Number(index)) => Some(*index), _ => None };
    let prepared = if name == "setFullChatMain" {
        let budget = journal.borrow().as_ref().cloned().ok_or("lua-host-journal")?.borrow().budget.clone();
        let full = match full_chat(args.get(1), &budget) {
            Ok(Some(full)) => full,
            Ok(None) => return unsupported("args"),
            Err(IrError::Syntax { .. }) => return Err("lua-script-error:invalid full-chat JSON".into()),
            Err(IrError::Limit(_) | IrError::Cancelled) => return Ok(ScalarReturn::Unsupported("lua-host-limit".into())),
            Err(_) => return unsupported("invariant"),
        };
        let FullChat { messages, _held } = full;
        let mut prepared = None;
        let mut reserve = |rows: &[serde_json::Value]| {
            prepared = Some(prepare(journal, Edit::Replace { messages: rows.to_vec() })?);
            Ok(())
        };
        match source.replace_prepared(messages, &mut reserve) {
            Ok(_) => prepared.ok_or("lua-host-journal")?,
            Err(label) if label == "lua-host-limit" => return Ok(ScalarReturn::Unsupported(label)),
            Err(_) => return unsupported("error"),
        }
    } else {
        let edit = match (name, index) {
            ("cutChat", _) => {
                let Some(start) = numeric(args.get(1)) else { return unsupported("args") };
                let end = if args.len() > 2 { match numeric(args.get(2)) { Some(end) => Some(end), None => return unsupported("args") } } else { None };
                Edit::Cut { start, end }
            }
            ("addChat", _) => {
                let role = if matches!(args.get(1), Some(Scalar::String(role)) if role == "user") { "user" } else { "char" };
                let data = match args.get(2) { None | Some(Scalar::Nil) => String::new(), Some(Scalar::String(data)) => data.clone(), _ => return unsupported("args") };
                Edit::Append { role, data, chat_id: uuid::Uuid::new_v4().to_string() }
            }
            ("removeChat", Some(index)) => Edit::Remove { index },
            ("insertChat", Some(index)) => {
                let role = if matches!(args.get(2), Some(Scalar::String(role)) if role == "user") { "user" } else { "char" };
                let data = match args.get(3) { None | Some(Scalar::Nil) => String::new(), Some(Scalar::String(data)) => data.clone(), _ => return unsupported("args") };
                Edit::Insert { index, role, data, chat_id: uuid::Uuid::new_v4().to_string() }
            }
            _ => return unsupported("args"),
        };
        let prepared = match prepare(journal, edit) { Ok(prepared) => prepared, Err(label) => return Ok(ScalarReturn::Unsupported(label)) };
        let applied = match &prepared.edit {
            Edit::Cut { start, end } => source.cut(*start, *end),
            Edit::Remove { index } => source.remove(*index),
            Edit::Insert { index, role, data, chat_id } => source.insert(*index, serde_json::json!({"role": role, "data": data, "chatId": chat_id})),
            Edit::Append { role, data, chat_id } => source.append(serde_json::json!({"role": role, "data": data, "chatId": chat_id})),
            _ => return unsupported("error"),
        };
        if applied.is_err() { return unsupported("error"); }
        prepared
    };
    record_edit(journal, edits, prepared)?;
    Ok(ScalarReturn::NoValues)
}

enum Run {
    Listener(Option<Vec<u16>>),
    Handler(Scalar),
    Initialized,
    Failed(String),
    Declined(String),
    Terminal(Terminal),
    Panicked,
    Unexpected,
}

fn classify(runtime: &Runtime, step: Step) -> Run {
    if let Some(label) = runtime.declined() { return Run::Declined(label); }
    match step {
        Step::ListenerComplete(value) => Run::Listener(value),
        Step::ScalarComplete(value) => Run::Handler(value),
        Step::IgnoredComplete => Run::Initialized,
        Step::Failed(error) => Run::Failed(error),
        Step::Terminal(reason) => Run::Terminal(reason),
        Step::Suspended | Step::Awaiting(_) | Step::Complete(_) => Run::Unexpected,
    }
}

/// Whether TS would admit a call to an unserved host name with these arguments; an admitted call declines the request.
type Admit = Box<dyn Fn(&[GateArgument]) -> bool>;

/// One engine of the cache: a VM with the whole host surface registered before any card code runs.
struct Engine {
    runtime: Runtime,
    binding: Rc<RefCell<Option<Binding>>>,
    edits: Rc<RefCell<Vec<Edit>>>,
    journal: ActiveJournal,
    /// `ScriptingEngineState.stopSending`, reset for each call.
    stop: Rc<Cell<bool>>,
    #[cfg(test)]
    unwind: Rc<Cell<bool>>,
    #[cfg(test)]
    held_budget:Rc<RefCell<Option<Charge>>>,
}

impl Engine {
    fn new(limits: Limits) -> Result<Self, String> {
        let failure = |error: LuaError| diagnostic(&error);
        let mut runtime = Runtime::new(limits).map_err(failure)?;
        runtime.install_wasmoon_profile(&[("json", JSON_MODULE)]).map_err(failure)?;
        runtime.disable_host_await().map_err(failure)?;
        let binding = Rc::new(RefCell::new(None));
        let edits = Rc::new(RefCell::new(Vec::new()));
        let journal: ActiveJournal = Rc::new(RefCell::new(None));
        let stop = Rc::new(Cell::new(false));
        let (api_binding, api_edits, api_journal) = (binding.clone(), edits.clone(), journal.clone());
        let api = move |name: &str, args: Vec<Scalar>| serve(&api_binding, &api_edits, &api_journal, name, args, limits.values.utf8_bytes);
        let quota_journal=journal.clone();
        let quota:Rc<dyn Fn(usize)->Result<Box<dyn std::any::Any>,String>>=Rc::new(move |bytes| {
            let current=quota_journal.borrow().as_ref().cloned().ok_or("lua-host-journal")?;
            let charge=current.borrow().budget.reserve_owned(bytes.saturating_add(std::mem::size_of::<Charge>())).map_err(|_|"lua-host-limit".to_owned())?;
            Ok(Box::new(charge) as Box<dyn std::any::Any>)
        });
        register_budgeted_values_api(&mut runtime, &binding, limits, api.clone(),quota.clone()).map_err(failure)?;
        for name in ["getChatVar","setChatVar","setChatVarChanged"].into_iter().chain(RECORD_API) {
            let (fence_binding,variable_binding,variable_edits,variable_journal)=
                (binding.clone(),binding.clone(),edits.clone(),journal.clone());
            let quota=quota.clone();
            runtime.register_variable_api(name,move || {
                let current=fence_binding.try_borrow().map_err(|_|"Lua host binding is busy")?;
                Ok(current.as_ref().ok_or("Lua invocation ended")?.key.clone())
            },move |name,args| {
                if RECORD_API.contains(&name){record(&variable_binding,&variable_journal,name,&args,limits.values)}
                else{serve_variable(&variable_binding,&variable_edits,&variable_journal,name,args,limits.values)}
            },move |bytes| {
                quota(bytes)
            }).map_err(failure)?;
        }
        for name in EDIT_API { runtime.register_values_scalar_owned(name, api.clone()).map_err(failure)?; }
        for (name, asynchronous, gate) in HOST_API {
            let gate_binding = binding.clone();
            let admitted: Admit = match gate {
                Gate::Served => continue,
                Gate::Always => Box::new(|_| true),
                Gate::Safe => Box::new(move |arguments| holds_key(&gate_binding, arguments, false)),
                Gate::LowLevel => Box::new(move |arguments| holds_key(&gate_binding, arguments, true)),
                Gate::ParsedPrompt => Box::new(move |arguments| !json_parses(arguments.get(1)) || holds_key(&gate_binding, arguments, true)),
                Gate::Log => Box::new(|arguments| !json_parses(arguments.first())),
                Gate::Stop => {
                    let stop = stop.clone();
                    Box::new(move |arguments| {
                        if holds_key(&gate_binding, arguments, false) { stop.set(true); }
                        false
                    })
                }
            };
            runtime.register_unsupported(name, asynchronous, admitted).map_err(failure)?;
        }
        #[cfg(test)]
        runtime.register_values_scalar(TEST_PANIC, |_, _| -> Result<ScalarReturn, String> { panic!("injected Lua host panic") })
            .map_err(failure)?;
        #[cfg(test)]
        let unwind = Rc::new(Cell::new(false));
        #[cfg(test)]
        {
            let armed = unwind.clone();
            runtime.register_values_scalar(TEST_UNWIND, move |_, _| {
                armed.set(true);
                Ok(ScalarReturn::NoValues)
            }).map_err(failure)?;
        }
        #[cfg(test)]
        let held_budget=Rc::new(RefCell::new(None));
        #[cfg(test)]
        {
            let (held,active)=(held_budget.clone(),journal.clone());
            runtime.register_values_scalar(TEST_BUDGET_REMAINING,move |_,arguments|{
                let Some(Scalar::Number(remaining))=arguments.first() else{return Err("expected remaining byte count".into())};
                if !remaining.is_finite()||*remaining<0.0{return Err("invalid remaining byte count".into());}
                held.borrow_mut().take();
                let budget=active.borrow().as_ref().cloned().ok_or("lua-host-journal")?.borrow().budget.clone();
                let available=budget.limits().max_owned_bytes.saturating_sub(budget.owned_bytes());
                *held.borrow_mut()=Some(budget.reserve_owned(available.saturating_sub(*remaining as usize)).map_err(|error|format!("ir: {error:?}"))?);
                Ok(ScalarReturn::NoValues)
            }).map_err(failure)?;
        }
        Ok(Self { runtime, binding, edits, journal, stop, #[cfg(test)] unwind, #[cfg(test)] held_budget })
    }

    /// One invocation under its own access key; `start` receives the key. Every runtime call is contained: a host panic
    /// is `Run::Panicked`, after which the caller drops this engine.
    fn run(&mut self, source: &Rc<dyn Conversation>, permissions: Permissions, journal: &Rc<RefCell<Journal>>,
        start: impl FnOnce(&mut Runtime, &str) -> LuaResult<()>, syntax_fails: bool) -> Result<Run, String> {
        let budget = journal.borrow().budget.clone();
        source.bind_variable_budget(&budget)?;
        source.begin()?;
        *self.journal.borrow_mut() = Some(journal.clone());
        let key = uuid::Uuid::new_v4().to_string();
        self.stop.set(false);
        *self.binding.borrow_mut() = Some(Binding { source: source.clone(), key: key.clone(), permissions, budget: budget.clone() });
        let runtime = &mut self.runtime;
        let run = catch_unwind(AssertUnwindSafe(|| match start(runtime, &key) {
            Err(error) => match (runtime.declined(), runtime.terminal_reason()) {
                (Some(label), _) => Run::Declined(label),
                (None, Some(reason)) => Run::Terminal(reason),
                // TS reports a syntax error of top-level code as an initialization error; it has no other limit here.
                (None, None) if syntax_fails && matches!(error, LuaError::SyntaxError { .. }) => Run::Failed(diagnostic(&error)),
                (None, None) => Run::Declined("lua-edit-limit".into()),
            },
            Ok(()) => loop {
                let step = runtime.step();
                #[cfg(test)]
                if self.unwind.replace(false) { panic!("injected Lua runtime boundary panic"); }
                match step {
                    Step::Suspended => continue,
                    step => break classify(runtime, step),
                }
            },
        }));
        // A panic in host code retired the VM, so the run ended as terminal whether card code caught it or not; a panic
        // in Rust code outside a callback unwinds to here.
        let run = match run { Ok(run) if !self.runtime.panicked() => run, _ => Run::Panicked };
        self.binding.borrow_mut().take();
        self.journal.borrow_mut().take();
        #[cfg(test)]
        self.held_budget.borrow_mut().take();
        source.end()?;
        if !matches!(&run, Run::Panicked) && matches!(budget.check(), Err(IrError::Cancelled)) {
            return Ok(Run::Terminal(Terminal::Cancelled));
        }
        Ok(run)
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
    pub source: &'a Rc<dyn Conversation>,
    pub cancellation: &'a Cancellation,
    pub budget: &'a Budget,
}

/// One `triggerlua` effect: `runScripted(effect.code, { mode, lowLevelAccess, userStarted })` with no data or meta.
pub(super) struct Call<'a> {
    pub owner: &'a str,
    pub code: &'a str,
    /// The `runScripted` mode string.
    pub entry: &'a str,
    pub low_level: bool,
    pub deadline: LuaDeadline,
    pub source: &'a Rc<dyn Conversation>,
    pub cancellation: &'a Cancellation,
    pub budget: &'a Budget,
}

#[derive(Debug, PartialEq)]
pub(super) enum CallStatus {
    /// The call ran; an error or a timeout inside it was logged, as TS catches them.
    Returned,
    /// The top-level code failed, which `runScripted` throws.
    InitFailed(String),
    Declined(String),
    Cancelled,
}

#[derive(Debug)]
pub(super) struct CallOutcome {
    /// `res`: the handler's first result, `Value::Undefined` when there is none.
    pub value: Value,
    pub edits: Vec<Edit>,
    pub saved: SavedEdits,
    /// `stopChat()` ran or `res === false`.
    pub stop: bool,
    pub status: CallStatus,
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

fn utf16(text: &str) -> Vec<u16> { text.encode_utf16().collect() }

/// `console.error({ code, mode })` as `runScripted` logs a caught failure, without the card's message.
fn note(code: &str, mode: &str) {
    const MODES: [&str; 10] = ["input", "output", "start", "onButtonClick", "onFormSubmit", "editRequest", "editDisplay", "editInput",
        "editOutput", "manual"];
    let mode = if MODES.contains(&mode) { mode } else { "custom" };
    crate::native_log::log_global("warn", "lua", format!("{code} mode={mode}"));
}

/// The function the `runScripted` switch calls for a mode string (`scriptings.ts:1475-1534`).
enum Entry<'a> {
    /// The global named `name`, with the access key and then `extra`.
    Handler { name: &'a str, extra: &'static [&'static str] },
    /// `callListenMain(mode, key, value, meta)`.
    Listener { mode: &'a str, display: bool },
}

fn entry(mode: &str) -> Entry<'_> {
    match mode {
        "input" => Entry::Handler { name: "onInput", extra: &[] },
        "output" => Entry::Handler { name: "onOutput", extra: &[] },
        "start" => Entry::Handler { name: "onStart", extra: &[] },
        // A trigger passes no data (`''`) and no meta (`{}`).
        "onButtonClick" => Entry::Handler { name: "onButtonClick", extra: &[""] },
        "onFormSubmit" => Entry::Handler { name: "onFormSubmit", extra: &["", "{}"] },
        "editRequest" | "editInput" | "editOutput" => Entry::Listener { mode, display: false },
        "editDisplay" => Entry::Listener { mode, display: true },
        name => Entry::Handler { name, extra: &[] },
    }
}

/// `functionTimeout` of each deadline class.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Deadlines {
    /// `LUA_LISTENER_TIMEOUT_MS`.
    pub listener: Duration,
    /// `LUA_USER_HANDLER_TIMEOUT_MS`.
    pub user_started: Duration,
}

impl Default for Deadlines {
    fn default() -> Self { Self { listener: Duration::from_secs(5), user_started: Duration::from_secs(60) } }
}

impl Deadlines {
    fn of(self, class: LuaDeadline) -> Duration {
        match class { LuaDeadline::Listener => self.listener, LuaDeadline::UserStarted => self.user_started }
    }
}

/// The engine cache key of `scriptings.ts:1649`: owner, mode string, code and deadline class.
type EngineKey = (String, String, String, LuaDeadline);

enum Init { Ready, Failed(String), Declined(String), Cancelled, Panicked, Unexpected }

/// Owns the engines across requests, keyed as TS keys them.
pub(super) struct LuaHost {
    /// Each engine with the use count of its last use.
    engines: HashMap<EngineKey, (Engine, u64)>,
    uses: u64,
    capacity: usize,
    limits: Limits,
    deadlines: Deadlines,
    wrapper: (String, String),
}

impl LuaHost {
    /// `deadlines` bound the top-level code and each call; `capacity` is the `scriptingEngineCacheEntries` budget.
    pub(super) fn new(limits: Limits, deadlines: Deadlines, capacity: usize) -> Result<Self, String> {
        Ok(Self { engines: HashMap::new(), uses: 0, capacity, limits, deadlines, wrapper: wrapper()? })
    }

    // `evictIdleScriptingEngines` after each use: the least recently used engines go first.
    fn evict_idle(&mut self) {
        while self.engines.len() > self.capacity {
            let Some(oldest) = self.engines.iter().min_by_key(|(_, (_, used))| *used).map(|(key, _)| key.clone()) else { return };
            self.engines.remove(&oldest);
        }
    }

    /// Runs the wrapper and the code once for an engine the cache does not hold.
    fn initialize(&mut self, key: &EngineKey, source: &Rc<dyn Conversation>, cancellation: &Cancellation,
        edits: &mut Vec<Edit>, journal: &Rc<RefCell<Journal>>) -> Result<Init, String> {
        if self.engines.contains_key(key) { return Ok(Init::Ready); }
        let code = &key.2;
        let text = format!("{}{code}{}", self.wrapper.0, self.wrapper.1);
        let limits = Limits { source_bytes: self.limits.source_bytes.saturating_add(text.len() - code.len()), ..self.limits };
        let mut engine = Engine::new(limits)?;
        let (duration, cancellation) = (self.deadlines.of(key.3), cancellation.clone());
        // Top-level code receives no key, so its writes are refused.
        let run = engine.run(source, Permissions::Safe, journal, |runtime, _| runtime.start_ignoring_result(&text, duration, cancellation), true);
        edits.extend(std::mem::take(&mut *engine.edits.borrow_mut()));
        Ok(match run? {
            Run::Initialized => {
                self.engines.insert(key.clone(), (engine, 0));
                Init::Ready
            }
            // TS discards the engine and throws.
            Run::Failed(message) => Init::Failed(message),
            Run::Terminal(Terminal::Deadline) => Init::Failed("Lua initialization timed out".into()),
            Run::Declined(label) => Init::Declined(label),
            Run::Terminal(Terminal::Cancelled) => Init::Cancelled,
            Run::Panicked => Init::Panicked,
            Run::Listener(_) | Run::Handler(_) | Run::Unexpected => Init::Unexpected,
        })
    }

    fn engine(&mut self, key: &EngineKey) -> Result<&mut Engine, String> {
        let (engine, used) = self.engines.get_mut(key).ok_or("initialized engine is missing")?;
        self.uses += 1;
        *used = self.uses;
        Ok(engine)
    }

    pub(super) fn run(&mut self, batch: Batch<'_>) -> Result<EditOutcome, EditFailure> {
        let mut edits = Vec::new();
        let journal = Journal::new(batch.budget);
        let result = (|| {
            let ir = |error: IrError| format!("ir: {error:?}");
            let budget = batch.budget;
            let mut data = copy_value(&batch.content, budget).map_err(ir)?;
            let display = batch.mode == EditMode::Display;
            let finish = |value: Value, status: Status| Ok((value, status));
            let mut meta = OwnedVec::new(budget).map_err(ir)?;
            if let Some(index) = batch.index { meta.push((Text::from_str("index", budget).map_err(ir)?, Value::Number(index as f64))).map_err(ir)?; }
            let meta = units(&json::stringify(&Value::Object(meta), budget).map_err(ir)?.ok_or("meta has no JSON text")?);
            let name = batch.mode.listener();
            let mode = utf16(name);
            for code in batch.triggers {
                self.evict_idle();
                if batch.cancellation.is_cancelled() { return finish(batch.content, Status::Cancelled); }
                let key = (batch.owner.to_owned(), name.to_owned(), code.clone(), LuaDeadline::Listener);
                match self.initialize(&key, batch.source, batch.cancellation, &mut edits, &journal)? {
                    Init::Ready => {}
                    // TS discards the engine and the batch returns its original content.
                    Init::Failed(_) => return finish(batch.content, Status::Completed),
                    Init::Declined(label) => return finish(batch.content, Status::Declined(label)),
                    Init::Cancelled => return finish(batch.content, Status::Cancelled),
                    Init::Panicked => return Err(PANICKED.into()),
                    Init::Unexpected => return Err(UNEXPECTED.into()),
                }
                let value = match (&data, display) {
                    (Value::Text(text), true) => units(text),
                    (_, true) => return finish(batch.content, Status::Declined("lua-edit-display-data".into())),
                    (_, false) => match json::stringify(&data, budget).map_err(ir)? {
                        Some(text) => units(&text),
                        None => return finish(batch.content, Status::Declined("lua-edit-undefined-data".into())),
                    },
                };
                let (timeout, cancellation) = (self.deadlines.listener, batch.cancellation.clone());
                let permissions = if display { Permissions::EditDisplay } else { Permissions::Safe };
                let engine = self.engine(&key)?;
                let run = engine.run(batch.source, permissions, &journal, |runtime, access_key| {
                    runtime.start_listener([&mode, &utf16(access_key), &value, &meta], timeout, cancellation)
                }, false);
                edits.extend(std::mem::take(&mut *engine.edits.borrow_mut()));
                let status = match run? {
                    Run::Listener(None) => continue,
                    Run::Failed(_) => { note("lua-listener-error", name); continue }
                    Run::Listener(Some(result)) if display => { data = Value::Text(Text::from_units(&result, budget).map_err(ir)?); continue }
                    Run::Listener(Some(result)) => {
                        let text = Text::from_units(&result, budget).map_err(ir)?;
                        match json::parse_json_text(&text, budget) {
                            Ok(Value::Null | Value::Undefined) => continue,
                            Ok(value) => { data = value; continue }
                            // `JSON.parse` threw after `res` held the string, and the batch keeps it.
                            Err(IrError::Syntax { .. }) => { data = Value::Text(text); continue }
                            Err(_) => Status::Declined("lua-edit-limit".into()),
                        }
                    }
                    // TS logs the timeout and goes on with the next listener. This host also drops the engine, whose
                    // retired VM cannot run again.
                    Run::Terminal(Terminal::Deadline) => {
                        self.engines.remove(&key);
                        note("lua-listener-timeout", name);
                        continue
                    }
                    Run::Declined(label) => Status::Declined(label),
                    Run::Terminal(Terminal::Cancelled) => Status::Cancelled,
                    Run::Panicked => { self.engines.remove(&key); return Err(PANICKED.into()) }
                    Run::Initialized | Run::Handler(_) | Run::Unexpected => { self.engines.remove(&key); return Err(UNEXPECTED.into()) }
                };
                // A declined or cancelled engine is never reused.
                self.engines.remove(&key);
                return finish(batch.content, status);
            }
            self.evict_idle();
            finish(data, Status::Completed)
        })();
        let saved = journal.borrow_mut().take();
        match result {
            Ok((value, status)) => Ok(EditOutcome { value, edits, saved, status }),
            Err(reason) => Err(EditFailure::new(reason, edits, saved)),
        }
    }

    /// The `runScripted` call of one `triggerlua` effect.
    pub(super) fn call(&mut self, call: Call<'_>) -> Result<CallOutcome, EditFailure> {
        let mut edits = Vec::new();
        let journal = Journal::new(call.budget);
        let mut stop = false;
        let result = (|| -> Result<(Value, CallStatus), String> {
            let ir = |error: IrError| format!("ir: {error:?}");
            let budget = call.budget;
            self.evict_idle();
            if call.cancellation.is_cancelled() { return Ok((Value::Undefined, CallStatus::Cancelled)); }
            let key = (call.owner.to_owned(), call.entry.to_owned(), call.code.to_owned(), call.deadline);
            match self.initialize(&key, call.source, call.cancellation, &mut edits, &journal)? {
                Init::Ready => {}
                Init::Failed(message) => return Ok((Value::Undefined, CallStatus::InitFailed(message))),
                Init::Declined(label) => return Ok((Value::Undefined, CallStatus::Declined(label))),
                Init::Cancelled => return Ok((Value::Undefined, CallStatus::Cancelled)),
                Init::Panicked => return Err(PANICKED.into()),
                Init::Unexpected => return Err(UNEXPECTED.into()),
            }
            let entry = entry(call.entry);
            let display = matches!(entry, Entry::Listener { display: true, .. });
            let permissions = if display { Permissions::EditDisplay } else if call.low_level { Permissions::LowLevel } else { Permissions::Safe };
            let (duration, cancellation) = (self.deadlines.of(call.deadline), call.cancellation.clone());
            let engine = self.engine(&key)?;
            let run = engine.run(call.source, permissions, &journal, |runtime, access_key| {
                let access_key = utf16(access_key);
                match entry {
                    Entry::Handler { name, extra } => {
                        let extra = extra.iter().map(|value| utf16(value)).collect::<Vec<_>>();
                        let arguments = [access_key.as_slice()].into_iter().chain(extra.iter().map(Vec::as_slice)).collect::<Vec<_>>();
                        runtime.start_handler(&utf16(name), &arguments, duration, cancellation)
                    }
                    // The data is `''`: JSON text, or as is for `editDisplay`.
                    Entry::Listener { mode, display } =>
                        runtime.start_listener([&utf16(mode), &access_key, &utf16(if display { "" } else { "\"\"" }), &utf16("{}")],
                            duration, cancellation),
                }
            }, false);
            edits.extend(std::mem::take(&mut *engine.edits.borrow_mut()));
            // A stop `stopChat` set stays after an error, as `ScriptingEngineState.stopSending` does.
            stop = engine.stop.get();
            let value = match run? {
                Run::Handler(value) => {
                    if value == Scalar::Boolean(false) { stop = true; }
                    match value {
                        Scalar::Nil => Value::Undefined,
                        Scalar::Boolean(value) => Value::Bool(value),
                        Scalar::Number(value) => Value::Number(value),
                        Scalar::String(value) => Value::Text(Text::from_str(&value, budget).map_err(ir)?),
                    }
                }
                Run::Listener(Some(result)) if display => Value::Text(Text::from_units(&result, budget).map_err(ir)?),
                Run::Listener(Some(result)) => {
                    let text = Text::from_units(&result, budget).map_err(ir)?;
                    match json::parse_json_text(&text, budget) {
                        Ok(value) => {
                            if matches!(value, Value::Bool(false)) { stop = true; }
                            value
                        }
                        // `JSON.parse` threw after `res` held the string.
                        Err(IrError::Syntax { .. }) => Value::Text(text),
                        Err(_) => {
                            self.engines.remove(&key);
                            return Ok((Value::Undefined, CallStatus::Declined("lua-edit-limit".into())));
                        }
                    }
                }
                // `callListenMain` returns the data it was given, `''` as JSON text or as is.
                Run::Listener(None) => Value::Text(Text::from_str("", budget).map_err(ir)?),
                Run::Failed(_) => { note("lua-handler-error", call.entry); Value::Undefined }
                // TS logs the timeout and keeps the engine; this host drops it, since its retired VM cannot run again.
                Run::Terminal(Terminal::Deadline) => {
                    self.engines.remove(&key);
                    note("lua-listener-timeout", call.entry);
                    Value::Undefined
                }
                Run::Terminal(Terminal::Cancelled) => {
                    self.engines.remove(&key);
                    return Ok((Value::Undefined, CallStatus::Cancelled));
                }
                Run::Declined(label) => {
                    self.engines.remove(&key);
                    return Ok((Value::Undefined, CallStatus::Declined(label)));
                }
                Run::Panicked => { self.engines.remove(&key); return Err(PANICKED.into()) }
                Run::Initialized | Run::Unexpected => { self.engines.remove(&key); return Err(UNEXPECTED.into()) }
            };
            self.evict_idle();
            Ok((value, CallStatus::Returned))
        })();
        let saved = journal.borrow_mut().take();
        match result {
            Ok((value, status)) => Ok(CallOutcome { value, edits, saved, stop, status }),
            Err(reason) => Err(EditFailure::new(reason, edits, saved)),
        }
    }
}

/// The records a request reads.
pub(super) enum Records<'a> {
    /// The preparation snapshot, with the request's view replayed over it.
    Snapshot(&'a Arc<ConversationSnapshot>),
    ProcessedSnapshot(&'a Arc<ConversationSnapshot>, &'a PersonaProcessor),
    /// An owner retains this source and selects its actual flush/seal boundary separately.
    Live(&'a Rc<dyn Conversation>),
    /// The store path, which reads the store through an access scope and ignores the view.
    #[cfg(test)]
    Store { access: &'a Rc<ScriptStoreAccess>, store: &'a Rc<RefCell<PersistentStore>> },
}

impl Records<'_> {
    fn source(&self, view: &LuaView, trigger: bool, budget: &Budget) -> Result<Rc<dyn Conversation>, String> {
        match self {
            Self::Snapshot(snapshot) => Ok(Rc::new(Overlay::new(Arc::clone(snapshot), view, trigger, budget)?)),
            Self::ProcessedSnapshot(snapshot, processor) => Ok(Rc::new(Overlay::new(Arc::clone(snapshot), view, trigger, budget)?
                .with_processor(Rc::clone(processor)))),
            Self::Live(source) => {
                if !view.effects.is_empty() || !view.locals.is_empty() { return Err("lua-live-view-owner-boundary".into()); }
                Ok(Rc::clone(source))
            },
            #[cfg(test)]
            Self::Store { access, store } => Ok(Rc::new(StoreSource::new(Rc::clone(access), Rc::clone(store), budget.clone())
                .with_locals(if trigger { &view.locals } else { &[] })?)),
        }
    }
}

/// What the owner supplies beside the request. Triggers are the selected character's first-effect Lua scripts followed by
/// module scripts, for an edit batch. The owner constructs `LuaHost` with `scriptingEngineCacheEntries`.
pub(super) struct Context<'a> {
    pub owner: &'a str,
    pub triggers: &'a [String],
    pub records: Records<'a>,
    pub cancellation: &'a Cancellation,
    pub budget: &'a Budget,
}

fn unsupported(label: Cow<'static, str>) -> HostReply { HostReply::Unsupported(label) }

/// The one seam from this host's types to the IR host protocol.
pub(super) fn answer(host: &mut LuaHost, request: HostRequest, context: Context<'_>) -> Result<HostReply, EditFailure> {
    answer_boundary(host, request, context, None)
}

pub(super) fn answer_owned(host: &mut LuaHost, request: HostRequest, context: Context<'_>, finish: VariableFinish)
    -> Result<HostReply, EditFailure> {
    answer_boundary(host, request, context, Some(finish))
}

fn answer_boundary(host: &mut LuaHost, request: HostRequest, context: Context<'_>, finish: Option<VariableFinish>)
    -> Result<HostReply, EditFailure> {
    let HostRequest::Lua { trigger, mode, payload, view } = request else { return Ok(unsupported(Cow::Borrowed("lua-request"))) };
    let index = match trigger {
        LuaTrigger::Edit { index } => index,
        LuaTrigger::Effect { call, .. } => return trigger_reply(host, call, &view, &context, finish),
    };
    let mode = match mode {
        // `runLuaEditTrigger` returns the content for `editprocess` without running Lua.
        LuaMode::Script(ScriptMode::EditProcess) => return Ok(HostReply::Lua(LuaReply { value: payload, effects: Vec::new(), stop_sending: false })),
        LuaMode::Script(ScriptMode::EditInput) => EditMode::Input,
        LuaMode::Script(ScriptMode::EditOutput) => EditMode::Output,
        LuaMode::Script(ScriptMode::EditDisplay) => EditMode::Display,
        LuaMode::EditRequest => EditMode::Request,
        _ => return Ok(unsupported(Cow::Borrowed("lua-trigger"))),
    };
    let source = match context.records.source(&view, false, context.budget) {
        Ok(source) => source,
        Err(label) => return Ok(unsupported(Cow::Owned(label))),
    };
    if finish.is_none() && matches!(context.records, Records::Snapshot(_) | Records::ProcessedSnapshot(_, _)) {
        source.legacy_variable_effects_only();
    }
    let batch = Batch { owner: context.owner, mode, content: payload, index, triggers: context.triggers, source: &source,
        cancellation: context.cancellation, budget: context.budget };
    let result = host.run(batch);
    let mut outcome = match result {
        Ok(outcome) => outcome,
        Err(mut failure) => {
            if let Some(finish) = finish {
                match source.finish_variables(finish) {
                    Ok(effects) => failure.saved.effects.extend(effects),
                    Err(reason) => return Err(EditFailure::new(format!("{}; variable settlement: {reason}", failure.reason), failure.edits, failure.saved)),
                }
            }
            else if !matches!(context.records, Records::Live(_)) {
                if let Err(reason) = legacy_variable_effects(&mut failure.saved, &failure.edits) {
                    return Err(EditFailure::new(format!("{}; legacy effects: {reason}", failure.reason), failure.edits, failure.saved));
                }
            }
            return Err(failure);
        }
    };
    if let Some(finish) = finish {
        match source.finish_variables(finish) {
            Ok(effects) => outcome.saved.effects.extend(effects),
            Err(reason) => return Err(EditFailure::new(reason, outcome.edits, outcome.saved)),
        }
    }
    else if !matches!(context.records, Records::Live(_)) {
        if let Err(reason) = legacy_variable_effects(&mut outcome.saved, &outcome.edits) {
            return Err(EditFailure::new(reason, outcome.edits, outcome.saved));
        }
    }
    let reason = match outcome.status {
        Status::Declined(label) => Some(label),
        Status::Cancelled if outcome.edits.is_empty() && outcome.saved.effects.is_empty() => return Ok(HostReply::Cancelled),
        Status::Cancelled => Some("lua-edit-cancelled".into()),
        Status::Completed => None,
    };
    if let Some(reason) = reason { return Err(EditFailure::new(reason, outcome.edits, outcome.saved)); }
    Ok(HostReply::Lua(LuaReply { value: outcome.value, effects: outcome.saved.effects, stop_sending: false }))
}

fn trigger_reply(host: &mut LuaHost, call: LuaCall, view: &LuaView, context: &Context<'_>, finish: Option<VariableFinish>) -> Result<HostReply, EditFailure> {
    let (Some(code), Some(entry)) = (call.code.as_unicode(), call.entry.as_unicode()) else {
        return Ok(unsupported(Cow::Borrowed("lua-call-text")));
    };
    let source = match context.records.source(view, true, context.budget) {
        Ok(source) => source,
        Err(label) => return Ok(unsupported(Cow::Owned(label))),
    };
    if finish.is_none() && matches!(context.records, Records::Snapshot(_) | Records::ProcessedSnapshot(_, _)) {
        source.legacy_variable_effects_only();
    }
    let result = host.call(Call { owner: context.owner, code, entry, low_level: call.low_level, deadline: call.deadline, source: &source,
        cancellation: context.cancellation, budget: context.budget });
    let mut outcome = match result {
        Ok(outcome) => outcome,
        Err(mut failure) => {
            if let Some(finish) = finish {
                match source.finish_variables(finish) {
                    Ok(effects) => failure.saved.effects.extend(effects),
                    Err(reason) => return Err(EditFailure::new(format!("{}; variable settlement: {reason}", failure.reason), failure.edits, failure.saved)),
                }
            }
            else if !matches!(context.records, Records::Live(_)) {
                if let Err(reason) = legacy_variable_effects(&mut failure.saved, &failure.edits) {
                    return Err(EditFailure::new(format!("{}; legacy effects: {reason}", failure.reason), failure.edits, failure.saved));
                }
            }
            return Err(failure);
        }
    };
    if let Some(finish) = finish {
        match source.finish_variables(finish) {
            Ok(effects) => outcome.saved.effects.extend(effects),
            Err(reason) => return Err(EditFailure::new(reason, outcome.edits, outcome.saved)),
        }
    }
    else if !matches!(context.records, Records::Live(_)) {
        if let Err(reason) = legacy_variable_effects(&mut outcome.saved, &outcome.edits) {
            return Err(EditFailure::new(reason, outcome.edits, outcome.saved));
        }
    }
    match outcome.status {
        CallStatus::Returned => Ok(HostReply::Lua(LuaReply { value: outcome.value, effects: outcome.saved.effects, stop_sending: outcome.stop })),
        // Top-level code holds no key, so a failed initialization wrote nothing.
        CallStatus::InitFailed(message) if outcome.edits.is_empty() => match Text::from_str(&message, context.budget) {
            Ok(message) => Ok(HostReply::Failed { message }),
            Err(error) => Err(EditFailure::new(format!("ir: {error:?}"), outcome.edits, outcome.saved)),
        },
        CallStatus::InitFailed(_) => Err(EditFailure::new("lua-init-effects".into(), outcome.edits, outcome.saved)),
        CallStatus::Declined(label) => Err(EditFailure::new(label, outcome.edits, outcome.saved)),
        CallStatus::Cancelled if outcome.edits.is_empty() && outcome.saved.effects.is_empty() => Ok(HostReply::Cancelled),
        CallStatus::Cancelled => Err(EditFailure::new("lua-edit-cancelled".into(), outcome.edits, outcome.saved)),
    }
}

/// The effects of a call's edits, in call order.
pub(super) fn edit_effects(edits: &[Edit], budget: &Budget) -> Result<Vec<Effect>, IrError> {
    let text = |value: &str| Text::from_str(value, budget);
    let role = |role: &str| if role == "user" { MessageRole::User } else { MessageRole::Char };
    edits.iter().map(|edit| -> Result<Effect, IrError> { Ok(match edit {
        Edit::Variable { key, value } => Effect::ChatVariable { name: text(key)?, value: value.value(budget)? },
        Edit::RawVariable { .. } => return Err(IrError::Invariant("raw-variable-owner-boundary")),
        Edit::MessageData { index, data } => Effect::Message(MessageEdit::SetData { index: *index, data: Value::Text(text(data)?) }),
        Edit::MessageRole { index, role: name } => Effect::Message(MessageEdit::SetRole { index: *index, role: role(name) }),
        Edit::Cut { start, end } => Effect::Message(MessageEdit::Cut { start: *start, end: *end }),
        Edit::Append { role: name, data, chat_id } => Effect::Message(MessageEdit::Append {
            role: role(name), data: Value::Text(text(data)?), chat_id: text(chat_id)?,
        }),
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
