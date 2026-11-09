use super::cbs::{Draws, Entropy, Recorder};
use super::inputs::{array, effective_settings, selected_persona, truth, unsupported, PreparationError, PreparationSnapshot};
use super::preparation_effects::{preparation_plan, preparation_variables, PreparedVariablePlan};
use super::modules::Evaluation;
use super::scripts::{edit_output_scripts, edit_process_scripts, RouteScripts, SharedText};
use risunest_ir::chat::Role;
use risunest_ir::stage::{
    Effect, HostReply, HostRequest, MemoryInput, MemoryOutput, MemoryStage, StageError, Step, TokenCounter, TokenInput,
};
use risunest_ir::{Budget, Limits, OwnedVec, Text, Value as Ir};
use risunest_prompt::accounting::{message_accounting, MessageAccounting};
use risunest_prompt::machine::{PromptFailure, PromptInput, PromptMachine, PromptOutcome, PromptOutput, PromptPolicy, PromptStages};
use risunest_records::{preset::Preset, root::Root, settings::Settings};
use serde::Serialize;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::{atomic::AtomicBool, Arc, Mutex, PoisonError};

const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

// One send's stored records; the store already bounds the history to 4096 messages and 8 MiB.
const PREPARATION_LIMITS: Limits = Limits { max_owned_bytes: 1 << 30, max_json_bytes: 1 << 26, max_text_units: 1 << 26,
    max_values: 1 << 26, max_depth: 128, max_work: usize::MAX / 4 };

// TS adds one to `statics.messages` on the defaulted object; a counter it would not count exactly is refused.
pub(crate) fn next_statics(root: &Value) -> Option<Value> {
    match root.get("statics") {
        None | Some(Value::Null) => Some(json!({"messages": 1, "imports": 0})),
        Some(Value::Object(statics)) => {
            let messages = statics.get("messages")?.as_u64().filter(|value| *value < MAX_SAFE_INTEGER)?;
            let mut next = statics.clone();
            next.insert("messages".into(), json!(messages + 1));
            Some(Value::Object(next))
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PreparedMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreparedOllamaTurn {
    pub messages: Vec<PreparedMessage>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub max_context: u64,
    pub last_memory: Option<String>,
    pub character_name: String,
    pub user_name: String,
}

#[derive(Debug)]
pub(crate) struct PreparationAttempt {
    pub result: Result<PreparedOllamaTurn, PreparationError>,
    pub variables: PreparedVariablePlan,
}

// The triggers the route leaves to TS: `request` and `output` triggers, which have no runner yet, and one whose first
// effect is `triggerlua` or `triggercode`, which TS runs in every mode (it has no effect case for `triggercode`, so that
// effect does nothing). The start trigger stage runs or declines the `start` triggers.
fn active_triggers(value: &Value) -> bool {
    array(value).iter().any(|trigger| {
        !array(&trigger["effect"]).is_empty() && (matches!(trigger["type"].as_str(), Some("request" | "output"))
            || matches!(trigger["effect"][0]["type"].as_str(), Some("triggerlua" | "triggercode")))
    })
}

// The route refusals: options the prompt machine prepares but this route cannot send, and stages without a native
// runner yet. The machine refuses the inputs it does not prepare. Returns the `editprocess` scripts the send runs.
fn admit<'a>(snapshot: &'a PreparationSnapshot, db: &'a Value, allow_nullish_ids: bool) -> Result<Vec<&'a Value>, PreparationError> {
    let chara = &snapshot.history.character;
    if db["aiModel"] != "ollama-hosted" { return Err(unsupported("model")); }
    // The send effects run after admission, so a behavior they cannot reproduce is refused here.
    if next_statics(&snapshot.root).is_none() { return Err(unsupported("statics")); }
    if truth(&db["seperateModelsForAxModels"]) && truth(&db["seperateModels"]["model"]) { return Err(unsupported("modelOverride")); }
    let scripts = edit_process_scripts(snapshot, db)?;
    if active_triggers(&chara["triggerscript"]) { return Err(unsupported("triggers")); }
    if array(&db["plugins"]).iter().any(|plugin| truth(&plugin["enabled"]) && plugin["version"] == "3.0") {
        return Err(unsupported("plugins"));
    }
    // The load sets a nullish `supaModelType` to `none`, and any other value is a memory model.
    if truth(&chara["supaMemory"]) && ((!db["supaModelType"].is_null() && db["supaModelType"] != "none")
        || truth(&db["hanuraiEnable"]) || truth(&db["hypav2"]) || truth(&db["hypaV3"])) { return Err(unsupported("memory")); }
    if truth(&chara["inlayViewScreen"]) && matches!(chara["viewScreen"].as_str(), Some("emotion" | "imggen")) { return Err(unsupported("inlayViewScreen")); }
    if truth(&db["enableCustomFlags"]) { return Err(unsupported("customFlags")); }
    if !array(&db["modelTools"]).is_empty() { return Err(unsupported("tools")); }
    if truth(&db["applyAdditionalParamsToAll"]) && array(&db["additionalParams"]).iter().any(|pair| truth(&pair[0]) && truth(&pair[1])) {
        return Err(unsupported("additionalParams"));
    }
    if array(&db["fallbackModels"]["model"]).iter().any(truth) { return Err(unsupported("fallbackModels")); }
    for field in ["ttsAutoSpeech", "removeIncompleteResponse", "autoContinueChat", "autoTranslate", "inlayErrorResponse"] {
        if truth(&db[field]) { return Err(unsupported(field)); }
    }
    if !array(&db["banCharacterset"]).is_empty() { return Err(unsupported("banCharacterset")); }
    // The current-chat parser visits even disabled rows before selecting history.
    for entry in &snapshot.history.messages {
        entry["data"].as_str().ok_or_else(|| PreparationError::Invalid("message.data".into()))?;
    }
    let start = snapshot.history.messages.iter().rposition(|entry| entry["disabled"] == "allBefore");
    let mut ids = BTreeSet::new();
    for entry in snapshot.history.messages.iter().skip(start.map(|index| index + 1).unwrap_or(0)) {
        if entry["disabled"] == true || entry["disabled"] == "allBefore" { continue; }
        let id = &entry["chatId"];
        if !allow_nullish_ids || !id.is_null() {
            // `message.chatId || createId()` stores a fresh ID for a falsy one and keeps a truthy non-string one.
            match id {
                Value::String(id) if !id.is_empty() && ids.insert(id.as_str()) => {}
                id if !id.is_string() && truth(id) => return Err(unsupported("message.chatId")),
                _ => return Err(unsupported("messageIdNormalization")),
            }
        }
        if entry["role"] == "char" && truth(&entry["saying"]) && entry["saying"] != chara["chaId"] {
            return Err(unsupported("historySpeakerLookup"));
        }
    }
    // `v[0].replaceAll` throws on a pair without a string key; the machine reads the entries that are not pairs.
    for bias in array(&db["bias"]).iter().chain(array(&chara["bias"])).filter(|bias| bias.is_array()) {
        bias[0].as_str().ok_or_else(|| PreparationError::Invalid("bias".into()))?;
    }
    Ok(scripts)
}

struct Names { character_name: String, user_name: String }

// The greeting index after `characterFormatUpdate`, which sets `chat.fmIndex ??= cha.firstMsgIndex ?? -1`.
fn greeting_index(snapshot: &PreparationSnapshot) -> Value {
    let stored = &snapshot.history.conversation["fmIndex"];
    if !stored.is_null() { return stored.clone(); }
    let first = &snapshot.history.character["firstMsgIndex"];
    if first.is_null() { json!(-1) } else { first.clone() }
}

// The names the response markers read, and the numeric settings the turn reads as whole numbers. The markers take a
// truthy name through a template string: `currentChar?.name ?? ''`, and `getUserName()`, which is a bound persona's
// `name` as stored and otherwise `username ?? 'User'`.
fn names(snapshot: &PreparationSnapshot, db: &Value) -> Result<Names, PreparationError> {
    fn marker(name: &Value) -> Result<String, PreparationError> {
        match name {
            Value::String(name) => Ok(name.clone()),
            name if !truth(name) => Ok(String::new()),
            _ => Err(unsupported("names")),
        }
    }
    let character_name = marker(&snapshot.history.character["name"])?;
    let (_, bound) = selected_persona(&snapshot.root, &snapshot.history.conversation);
    let user_name = match bound {
        Some(bound) => marker(&bound["name"])?,
        None if db["username"].is_null() => "User".to_owned(),
        None => marker(&db["username"])?,
    };
    // A stored `4000.0` is the whole number `JSON.parse` reads.
    let whole = |value: &Value| value.as_f64()
        .is_some_and(|value| value.is_sign_positive() && value.fract() == 0.0 && value <= MAX_SAFE_INTEGER as f64);
    if !whole(&db["maxContext"]) { return Err(unsupported("maxContext")); }
    if !whole(&db["maxResponse"]) { return Err(unsupported("maxResponse")); }
    Ok(Names { character_name, user_name })
}

// `JSON.parse` of a stored record: JS property order (array-index keys ascending, then the others in stored order) and
// JS numbers. serde_json holds each key once, so no key is defined twice.
pub(super) fn ir_value(value: &Value, depth: usize, budget: &Budget) -> Result<Ir, risunest_ir::Error> {
    budget.work(1)?;
    if depth > PREPARATION_LIMITS.max_depth { return Err(risunest_ir::Error::Limit("json-depth")); }
    Ok(match value {
        Value::Null => Ir::Null,
        Value::Bool(value) => Ir::Bool(*value),
        Value::Number(number) => Ir::Number(number.as_f64().ok_or(risunest_ir::Error::Invariant("json-number"))?),
        Value::String(value) => Ir::Text(Text::from_str(value, budget)?),
        Value::Array(values) => {
            let mut array = OwnedVec::new(budget)?;
            for value in values { array.push(ir_value(value, depth + 1, budget)?)?; }
            Ir::Array(array)
        }
        Value::Object(entries) => {
            let (mut indexed, mut named) = (Vec::new(), Vec::new());
            for (key, value) in entries {
                match array_index(key) { Some(index) => indexed.push((index, key, value)), None => named.push((key, value)) }
            }
            indexed.sort_unstable_by_key(|(index, ..)| *index);
            let mut object = OwnedVec::new(budget)?;
            for (key, value) in indexed.into_iter().map(|(_, key, value)| (key, value)).chain(named) {
                object.push((Text::from_str(key, budget)?, ir_value(value, depth + 1, budget)?))?;
            }
            Ir::Object(object)
        }
    })
}

// A canonical array index: `0` or digits without a leading zero, below 2^32 - 1.
fn array_index(key: &str) -> Option<u32> {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > 10 || !bytes.iter().all(u8::is_ascii_digit) || (bytes[0] == b'0' && bytes.len() > 1) { return None; }
    key.parse::<u64>().ok().filter(|index| *index < u64::from(u32::MAX)).map(|index| index as u32)
}

// The stored records as the machine reads them, the conversation with its complete `message` array. A row with a
// nullish ID carries a placeholder: admission reads the history before `generation_ensure_history_ids` stores those
// IDs, and preparation refuses such a row inside the prompt (`admit`), so a placeholder never reaches a sent prompt.
pub(super) fn prompt_input(snapshot: &PreparationSnapshot, budget: &Budget) -> Result<PromptInput, risunest_ir::Error> {
    let mut messages = OwnedVec::new(budget)?;
    for (index, entry) in snapshot.history.messages.iter().enumerate() {
        let mut message = ir_value(entry, 2, budget)?;
        if entry.get("chatId").is_none_or(Value::is_null) {
            message.define(Text::from_str("chatId", budget)?, Ir::Text(Text::from_str(&format!("admission-{index}"), budget)?), budget)?;
        }
        messages.push(message)?;
    }
    let mut conversation = ir_value(&snapshot.history.conversation, 0, budget)?;
    conversation.define(Text::from_str("message", budget)?, Ir::Array(messages), budget)?;
    // `characterFormatUpdate` sets these defaults on the selected character and its chats before TS sends.
    if snapshot.history.conversation.get("fmIndex").is_none_or(Value::is_null) {
        conversation.define(Text::from_str("fmIndex", budget)?, ir_value(&greeting_index(snapshot), 1, budget)?, budget)?;
    }
    let mut character = ir_value(&snapshot.history.character, 0, budget)?;
    if snapshot.history.character.is_object() && snapshot.history.character.get("exampleMessage").is_none_or(Value::is_null) {
        character.define(Text::from_str("exampleMessage", budget)?, Ir::Text(Text::from_str("", budget)?), budget)?;
    }
    if snapshot.history.character.is_object() && snapshot.history.character.get("triggerscript").is_none_or(Value::is_null) {
        character.define(Text::from_str("triggerscript", budget)?, Ir::Array(OwnedVec::new(budget)?), budget)?;
    }
    Ok(PromptInput { root: ir_value(&snapshot.root, 0, budget)?, preset: Some(ir_value(&snapshot.preset, 0, budget)?), character,
        conversation, engine_case: super::cbs::engine_case(snapshot.host.as_ref()) })
}

fn invalid(name: &'static str) -> StageError { StageError::Invalid(Cow::Borrowed(name)) }

/// Admission refuses every memory mode.
struct NoMemory;
impl MemoryStage for NoMemory {
    fn run(&mut self, _: MemoryInput, _: &Budget) -> Result<Step<MemoryOutput>, StageError> {
        Err(StageError::Unsupported(Cow::Borrowed("memory")))
    }
    fn resume(&mut self, _: HostReply, _: &Budget) -> Result<Step<MemoryOutput>, StageError> { Err(invalid("resume")) }
}

type TokenizerFailure = Arc<Mutex<Option<String>>>;

/// `tokenizeChat` under cl100k, the tokenizer `ollama-hosted` counts with; a tokenizer error leaves its code in
/// `failure`. Probes whose lore or start trigger reads counts count exactly; other probes only screen capabilities.
/// Text counts have no message overhead, and supported tokenizer failures retain their production-boundary identity.
struct Tokens { accounting: MessageAccounting, count: bool, screen: bool, failure: TokenizerFailure }
impl Tokens {
    fn encode(&self, text: &Text) -> Result<u32, StageError> {
        use crate::native_tokenizer::{cl100k_declines, Cl100kDecline};
        let lossy;
        let text = match text.as_unicode() {
            Some(text) => text,
            None => { lossy = String::from_utf16_lossy(&text.units().collect::<Vec<_>>()); &lossy }
        };
        if self.screen {
            match cl100k_declines(text) {
                Some(Cl100kDecline::InputLimit) => return Err(StageError::Unsupported(Cow::Borrowed("tokenizer-input-limit"))),
                Some(Cl100kDecline::RegexLimit) => return Err(StageError::Unsupported(Cow::Borrowed("tokenizer-regex-limit"))),
                None => {},
            }
        }
        if !self.count { return Ok(0); }
        crate::native_tokenizer::count_cl100k_text(text).map_err(|error| {
            *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(error.code);
            invalid("tokenizer")
        })
    }
}
impl TokenCounter for Tokens {
    fn count(&mut self, input: TokenInput<'_>, _: &Budget) -> Result<Step<u32>, StageError> {
        let message = match input {
            TokenInput::Text(text) => return self.encode(text).map(Step::Done),
            TokenInput::Message(message) => message,
            TokenInput::Messages(_) => return Err(invalid("token-input")),
        };
        let mut total = self.accounting.additional_tokens.saturating_add(self.encode(&message.content)?);
        if let Some(name) = message.name.as_ref().filter(|name| self.accounting.use_name && name.len_units() != 0) {
            total = total.saturating_add(self.encode(name)?).saturating_add(1);
        }
        Ok(Step::Done(if self.count { total } else { 0 }))
    }
    fn resume(&mut self, _: HostReply, _: &Budget) -> Result<Step<u32>, StageError> { Err(invalid("resume")) }
}

// The owner does not run Lua (L11) or plugin callbacks (L25) yet; the machine ends a send that reaches one.
fn host_refusal(request: &HostRequest) -> &'static str {
    match request {
        HostRequest::Lua { .. } => "lua",
        HostRequest::PluginHook { .. } | HostRequest::PluginTokenizer { .. } => "plugins",
        _ => "hostRequest",
    }
}

fn preparation_error(error: StageError, failure: &TokenizerFailure) -> PreparationError {
    if let Some(code) = failure.lock().unwrap_or_else(PoisonError::into_inner).take() { return PreparationError::Tokenizer(code); }
    match error {
        StageError::Unsupported(name) => PreparationError::Unsupported(name.into_owned()),
        StageError::Invalid(name) => PreparationError::Invalid(name.into_owned()),
        StageError::Limit(name) => PreparationError::Invalid(format!("limit:{name}")),
        StageError::Cancelled => PreparationError::Invalid("cancelled".into()),
        _ => PreparationError::Invalid("stage".into()),
    }
}

fn evaluation(snapshot: &PreparationSnapshot) -> Result<(Budget, Evaluation), PreparationError> {
    let capture = || -> Result<(Budget, Evaluation), StageError> {
        let budget = Budget::new(PREPARATION_LIMITS, Arc::new(AtomicBool::new(false)))?;
        let evaluation = Evaluation::capture(prompt_input(snapshot, &budget)?, &budget)?;
        Ok((budget, evaluation))
    };
    capture().map_err(|error| preparation_error(error, &TokenizerFailure::default()))
}

pub(super) fn admit_response(snapshot: &PreparationSnapshot) -> Result<Evaluation, StageError> {
    let check = || -> Result<Evaluation, PreparationError> {
        let db = effective_settings(snapshot)?;
        let chara = &snapshot.history.character;
        // Revalidate response hosts and effects only. Prompt admission also checks the next send's statics and IDs,
        // which this response neither increments nor normalizes again.
        if active_triggers(&chara["triggerscript"]) { return Err(unsupported("triggers")); }
        if array(&db["plugins"]).iter().any(|plugin| truth(&plugin["enabled"]) && plugin["version"] == "3.0") {
            return Err(unsupported("plugins"));
        }
        if truth(&chara["inlayViewScreen"]) && matches!(chara["viewScreen"].as_str(), Some("emotion" | "imggen")) {
            return Err(unsupported("inlayViewScreen"));
        }
        for field in ["ttsAutoSpeech", "removeIncompleteResponse", "autoContinueChat", "autoTranslate", "inlayErrorResponse", "promptInfoInsideChat"] {
            if truth(&db[field]) { return Err(unsupported(field)); }
        }
        let (budget, evaluation) = evaluation(snapshot)?;
        admit_output_scripts(snapshot, &db, &evaluation, &budget)?;
        Ok(evaluation)
    };
    check().map_err(output_script_error)
}

fn output_script_error(error: PreparationError) -> StageError {
    match error {
        PreparationError::Unsupported(name) => StageError::Unsupported(Cow::Owned(name)),
        PreparationError::Invalid(name) => StageError::Invalid(Cow::Owned(name)),
        _ => invalid("output-scripts"),
    }
}

pub(super) fn output_scripts(snapshot: &PreparationSnapshot, evaluation: &Evaluation, budget: &Budget, draws: &Draws)
    -> Result<RouteScripts, StageError> {
    let db = effective_settings(snapshot).map_err(output_script_error)?;
    let scripts = edit_output_scripts(snapshot, &db).map_err(output_script_error)?;
    let text = SharedText::new(super::cbs::processor(&evaluation.input, budget, draws, snapshot.host.as_ref())?);
    let scripts = super::scripts::with_modules(snapshot, &scripts, &evaluation.modules, "editoutput", budget)?;
    RouteScripts::new(&scripts, snapshot.host.as_ref().is_none_or(|host| host.foreign_regex_unicode()), text, budget)
}

fn admit_output_scripts(snapshot: &PreparationSnapshot, db: &Value, evaluation: &Evaluation, budget: &Budget)
    -> Result<(), PreparationError> {
    // Unlike editprocess, editoutput can invoke Lua and dynamic assets. Active Lua triggers, module assets and plugins
    // are refused by the route; dynamic assets can run even when no editoutput rule matched.
    let chara = &snapshot.history.character;
    if truth(&db["dynamicAssets"]) && matches!(chara["type"].as_str(), Some("simple" | "character"))
        && !array(&chara["additionalAssets"]).is_empty() { return Err(unsupported("regex.dynamicAssets")); }
    let run = || -> Result<(), StageError> {
        let mut scripts = output_scripts(snapshot, evaluation, budget, &Draws::Fresh)?;
        scripts.admit_output(snapshot.history.messages.len() as i64, budget, |replacement| {
            super::cbs::admit_output_replacement(&evaluation.input, snapshot.host.as_ref(), replacement, budget)
        })
    };
    run().map_err(|error| preparation_error(error, &TokenizerFailure::default()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeDisposition { Complete, RuntimeFailure, Decline }

struct Machine {
    outcome: Result<PromptOutcome, PreparationError>, effects: Vec<Effect>, probe: ProbeDisposition, exact_counts: bool,
    /// An admission's conversion of the effects its probe completed, and whether the send can store them.
    #[cfg_attr(not(test), allow(dead_code))] plan: Option<PreparedVariablePlan>,
    stored: Result<(), PreparationError>,
    #[cfg_attr(not(test), allow(dead_code))] lore_failure: Option<risunest_lorebook::selector::LoreFailureSite>,
    #[cfg_attr(not(test), allow(dead_code))] host_requests: usize,
}

fn probe_disposition(outcome: &Result<PromptOutcome, StageError>, failure: &TokenizerFailure) -> ProbeDisposition {
    match outcome {
        Ok(PromptOutcome::Ready(_)) => ProbeDisposition::Complete,
        Ok(PromptOutcome::Failed { reason: PromptFailure::ContextLimit { .. }, .. }) => ProbeDisposition::RuntimeFailure,
        Err(StageError::Invalid(name)) if name == "tokenizer" && failure.lock().unwrap_or_else(PoisonError::into_inner).is_some() =>
            ProbeDisposition::RuntimeFailure,
        _ => ProbeDisposition::Decline,
    }
}

fn probe_result(machine: Machine) -> Result<(), PreparationError> {
    match machine.probe {
        ProbeDisposition::Complete | ProbeDisposition::RuntimeFailure => machine.stored,
        ProbeDisposition::Decline => machine.outcome.and_then(refused).map(|_| ()),
    }
}

// Runs the prompt machine over the snapshot with the `editprocess` scripts `admit` returned, and answers each host
// request it makes.
fn run_machine(snapshot: &PreparationSnapshot, scripts: &[&Value], evaluation: Evaluation, budget: &Budget, admission: bool, draws: &Draws) -> Machine {
    let failure = TokenizerFailure::default();
    let mut host_requests = 0;
    let mut effects = Vec::new();
    let mut exact_counts = false;
    let lore_failure = super::lore::LoreFailureTrace::default();
    let run = || -> Result<PromptOutcome, StageError> {
        let Evaluation { input, modules } = evaluation;
        let preset = input.preset.as_ref().map(Preset::new).transpose()?;
        let accounting = message_accounting(&Settings::derive(Root::new(&input.root)?, preset, budget)?, budget)?;
        let capture = super::lore::capture(&input, &modules, snapshot.host.as_ref(), budget)?;
        let trigger = super::triggers::input(&input, &modules, snapshot.host.as_ref(), budget)?;
        let trigger_counts = super::triggers::reaches_counts(&trigger, budget)?;
        exact_counts = capture.bearing() || trigger_counts;
        let runtime = super::rng::RunEntropy::new(draws);
        let text = SharedText::new(super::cbs::processor_with_runtime(&input, budget, runtime.rng.clone(), runtime.clock.clone(),
            snapshot.host.as_ref())?);
        // The trigger evaluates CBS against its own variables, drawing from the same `Math.random()`.
        let scoped = super::cbs::scoped_processor(&input, budget, runtime.rng.clone(), runtime.clock, snapshot.host.as_ref())?;
        let scripts = super::scripts::with_modules(snapshot, scripts, &modules, "editprocess", budget)?;
        // `admit` refuses scripts without a host capture.
        let foreign_unicode = snapshot.host.as_ref().is_none_or(|host| host.foreign_regex_unicode());
        let scripts = RouteScripts::new(&scripts, foreign_unicode, text.clone(), budget)?;
        let tokens = || Tokens { accounting, count: !admission || exact_counts, screen: admission || exact_counts, failure: failure.clone() };
        let lore = super::lore::stage(capture, text.clone(), tokens(), runtime.rng.clone(), lore_failure.clone());
        let triggers = super::triggers::stage(trigger, scoped, tokens(), runtime.rng, snapshot.host.as_ref());
        let stages = PromptStages { text: Box::new(text), scripts: Box::new(scripts), lore, triggers,
            memory: Box::new(NoMemory), tokens: Box::new(tokens()) };
        let mut machine = PromptMachine::with_captured_modules(input, stages, PromptPolicy::Owned, modules);
        drive_machine(&mut machine, budget, &mut effects, &mut host_requests)
    };
    let outcome = run();
    let probe = probe_disposition(&outcome, &failure);
    let outcome = outcome.map_err(|error| preparation_error(error, &failure));
    // Admission simulates the preparation only; none of its effects belong to the send, which must be able to store
    // the ones it will make.
    let (plan, stored) = match admission && probe != ProbeDisposition::Decline {
        true => { let (plan, stored) = preparation_plan(snapshot, &effects); (Some(plan), stored) }
        false => (None, Ok(())),
    };
    if admission { effects.clear(); }
    Machine { outcome, effects, probe, exact_counts, plan, stored, lore_failure: lore_failure.site(), host_requests }
}

fn drive_machine(machine: &mut PromptMachine, budget: &Budget, effects: &mut Vec<Effect>, host_requests: &mut usize)
    -> Result<PromptOutcome, StageError>
{
    let mut step = machine.start(budget);
    loop {
        match step {
            Err(error) => { effects.extend(machine.take_effects()); return Err(error); }
            Ok(Step::Done(mut outcome)) => {
                effects.extend(take_outcome_effects(&mut outcome));
                return Ok(outcome);
            }
            Ok(Step::NeedsHost(request)) => {
                *host_requests += 1;
                step = machine.resume(HostReply::Unsupported(Cow::Borrowed(host_refusal(&request))), budget);
            }
        }
    }
}

fn take_outcome_effects(outcome: &mut PromptOutcome) -> Vec<Effect> {
    match outcome {
        PromptOutcome::Ready(output) => std::mem::take(&mut output.effects),
        PromptOutcome::Failed { effects, .. } => std::mem::take(effects),
    }
}

fn whole(value: f64, field: &str) -> Result<u64, PreparationError> {
    if value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value <= MAX_SAFE_INTEGER as f64 { Ok(value as u64) }
    else { Err(PreparationError::Invalid(field.into())) }
}

fn string(text: &Text, field: &str) -> Result<String, PreparationError> {
    String::from_utf16(&text.units().collect::<Vec<_>>()).map_err(|_| PreparationError::Invalid(field.into()))
}

// `risuUnescape`, which `requestChatData` applies after the token recheck.
fn unescape(text: &Text) -> Result<String, PreparationError> {
    string(text, "message.content").map(|content| content.chars().map(|c| match c {
        '\u{e9b8}' => "{".into(), '\u{e9b9}' => "}".into(), '\u{e9ba}' => "(".into(), '\u{e9bb}' => ")".into(),
        '\u{e9bc}' => "&lt;".into(), '\u{e9bd}' => "&gt;".into(), '\u{e9be}' => ":".into(), '\u{e9bf}' => ";".into(),
        c => c.to_string(),
    }).collect())
}

fn prepared_turn(output: PromptOutput, effects: Vec<Effect>, names: Names) -> Result<PreparedOllamaTurn, PreparationError> {
    let mut last_memory = None;
    for effect in effects {
        match effect {
            Effect::ChatField { field, value } if field.equals_ascii(b"lastMemory") => last_memory = match value {
                Ir::Undefined => None,
                Ir::Text(memo) => Some(string(&memo, "lastMemory")?),
                _ => return Err(PreparationError::Invalid("lastMemory".into())),
            },
            Effect::ChatVariable { .. } | Effect::Message(_) => {},
            Effect::ChatField { field, .. } if field.equals_ascii(b"note") => {},
            // Variable, author note and message effects were validated by the narrow plan converter.
            _ => return Err(PreparationError::Invalid("effect".into())),
        }
    }
    let mut messages = Vec::with_capacity(output.messages.len());
    for message in &output.messages {
        let role = match message.role { Role::System => "system", Role::User => "user", Role::Assistant => "assistant", Role::Function => "function" };
        let name = message.name.as_ref().map(|name| string(name, "message.name")).transpose()?;
        messages.push(PreparedMessage { role: role.into(), content: unescape(&message.content)?, name });
    }
    Ok(PreparedOllamaTurn { messages, input_tokens: whole(output.input_tokens, "inputTokens")?,
        output_tokens: whole(output.output_tokens, "outputTokens")?, max_context: whole(output.max_context, "maxContext")?,
        last_memory, character_name: names.character_name, user_name: names.user_name })
}

// A machine refusal found after its first host request ends the run as a failed outcome; both forms refuse here.
fn refused(outcome: PromptOutcome) -> Result<PromptOutcome, PreparationError> {
    match outcome {
        PromptOutcome::Failed { reason: PromptFailure::Unsupported(name), .. } => Err(PreparationError::Unsupported(name.into_owned())),
        PromptOutcome::Failed { reason: PromptFailure::Memory(_), .. } => Err(PreparationError::Invalid("memory".into())),
        PromptOutcome::Failed { reason: PromptFailure::StopSending, .. } => Err(PreparationError::Invalid("stopSending".into())),
        outcome => Ok(outcome),
    }
}

// Admission simulates one isolated run. A reached capability declines before reservation; a supported terminal
// runtime failure belongs to the owned send. Its captured clock and shared draw tape replay the same path.
pub(crate) fn admit_ollama(snapshot: &PreparationSnapshot) -> Result<Entropy, PreparationError> {
    let db = effective_settings(snapshot)?;
    let scripts = admit(snapshot, &db, true)?;
    names(snapshot, &db)?;
    let (budget, evaluation) = evaluation(snapshot)?;
    admit_output_scripts(snapshot, &db, &evaluation, &budget)?;
    let recorder = Recorder::new();
    let draws = snapshot.entropy.as_ref().map_or(Draws::Record(&recorder), Draws::Replay);
    let machine = run_machine(snapshot, &scripts, evaluation, &budget, true, &draws);
    let exact_counts = machine.exact_counts;
    probe_result(machine)?;
    let entropy = snapshot.entropy.clone().unwrap_or_else(|| recorder.entropy());
    if !exact_counts && entropy.drew() && memory_card_repeats(snapshot, &db) { return Err(unsupported("cbs.random-draws")); }
    Ok(entropy)
}

// A memory card formats each memory chat the trimmed history keeps, so how often its CBS runs depends on token counts
// the admission does not take, and draws after it would not replay.
fn memory_card_repeats(snapshot: &PreparationSnapshot, db: &Value) -> bool {
    array(&db["promptTemplate"]).iter().any(|card| card["type"] == "memory" && truth(&card["innerFormat"]))
        && snapshot.history.messages.iter().any(|row| matches!(row["chatId"].as_str(), Some("supaMemory" | "hypaMemory")))
}

#[cfg(test)]
pub(crate) fn admission_host_requests(snapshot: &PreparationSnapshot) -> (Result<(), PreparationError>, usize) {
    let db = match effective_settings(snapshot) { Ok(db) => db, Err(error) => return (Err(error), 0) };
    let scripts = match edit_process_scripts(snapshot, &db) { Ok(scripts) => scripts, Err(error) => return (Err(error), 0) };
    let (budget, evaluation) = match evaluation(snapshot) { Ok(capture) => capture, Err(error) => return (Err(error), 0) };
    if let Err(error) = admit_output_scripts(snapshot, &db, &evaluation, &budget) { return (Err(error), 0); }
    let recorder = Recorder::new();
    let machine = run_machine(snapshot, &scripts, evaluation, &budget, true,
        &snapshot.entropy.as_ref().map_or(Draws::Record(&recorder), Draws::Replay));
    let requests = machine.host_requests;
    (probe_result(machine), requests)
}

pub(crate) fn prepare_ollama_owned(snapshot: &PreparationSnapshot) -> PreparationAttempt {
    let mut variables = PreparedVariablePlan::default();
    let result = (|| {
        let db = effective_settings(snapshot)?;
        let scripts = admit(snapshot, &db, false)?;
        let names = names(snapshot, &db)?;
        let (budget, evaluation) = evaluation(snapshot)?;
        admit_output_scripts(snapshot, &db, &evaluation, &budget)?;
        // A send reserved before admissions recorded their draws takes fresh ones.
        let draws = snapshot.entropy.as_ref().map_or(Draws::Fresh, Draws::Replay);
        let machine = run_machine(snapshot, &scripts, evaluation, &budget, false, &draws);
        finish_preparation(snapshot, machine, names, &mut variables)
    })();
    PreparationAttempt { result, variables }
}

fn finish_preparation(snapshot: &PreparationSnapshot, machine: Machine, names: Names, variables: &mut PreparedVariablePlan)
    -> Result<PreparedOllamaTurn, PreparationError>
{
    let (plan, conversion) = preparation_variables(snapshot, &machine.effects);
    *variables = plan;
    // Preserve the original failure even when a later effect cannot be represented.
    match refused(machine.outcome?)? {
        PromptOutcome::Ready(output) => { conversion?; prepared_turn(output, machine.effects, names) }
        PromptOutcome::Failed { reason: PromptFailure::ContextLimit { .. }, .. } => Err(PreparationError::ContextLimit),
        PromptOutcome::Failed { .. } => Err(PreparationError::Invalid("outcome".into())),
    }
}

pub(crate) fn prepare_ollama(snapshot: &PreparationSnapshot) -> Result<PreparedOllamaTurn, PreparationError> {
    prepare_ollama_owned(snapshot).result
}

#[cfg(test)]
pub(crate) fn preparation_from_outcome(snapshot: &PreparationSnapshot, mut outcome: Result<PromptOutcome, PreparationError>,
    mut effects: Vec<Effect>) -> PreparationAttempt
{
    if let Ok(outcome) = &mut outcome { effects.extend(take_outcome_effects(outcome)); }
    let mut variables = PreparedVariablePlan::default();
    let names = names(snapshot, &effective_settings(snapshot).unwrap()).unwrap();
    let result = finish_preparation(snapshot, Machine { outcome, effects, probe: ProbeDisposition::Decline, exact_counts: false,
        plan: None, stored: Ok(()), lore_failure: None, host_requests: 0 }, names, &mut variables);
    PreparationAttempt { result, variables }
}

/// The variable and author note plan the admission that reserves the send converts from its probe's effects.
#[cfg(test)]
pub(crate) fn admission_variable_plan(snapshot: &PreparationSnapshot) -> Option<PreparedVariablePlan> {
    let db = effective_settings(snapshot).unwrap();
    let scripts = admit(snapshot, &db, true).unwrap();
    let (budget, evaluation) = evaluation(snapshot).unwrap();
    let recorder = Recorder::new();
    let draws = snapshot.entropy.as_ref().map_or(Draws::Record(&recorder), Draws::Replay);
    run_machine(snapshot, &scripts, evaluation, &budget, true, &draws).plan
}

#[cfg(test)]
pub(crate) fn admission_lore_failure_site(snapshot: &PreparationSnapshot) -> Option<risunest_lorebook::selector::LoreFailureSite> {
    let db = effective_settings(snapshot).unwrap();
    let scripts = admit(snapshot, &db, true).unwrap();
    let draws = snapshot.entropy.as_ref().map_or(Draws::Fresh, Draws::Replay);
    let (budget, evaluation) = evaluation(snapshot).unwrap();
    run_machine(snapshot, &scripts, evaluation, &budget, true, &draws).lore_failure
}

#[cfg(test)]
pub(crate) fn admission_effect_count(snapshot: &PreparationSnapshot) -> usize {
    let db = effective_settings(snapshot).unwrap();
    let scripts = admit(snapshot, &db, true).unwrap();
    let (budget, evaluation) = evaluation(snapshot).unwrap();
    run_machine(snapshot, &scripts, evaluation, &budget, true, &Draws::Fresh).effects.len()
}

#[cfg(test)]
pub(crate) fn record_value(value: &Value, budget: &Budget) -> Result<Ir, risunest_ir::Error> { ir_value(value, 0, budget) }
