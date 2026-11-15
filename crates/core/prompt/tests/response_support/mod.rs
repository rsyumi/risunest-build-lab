//! Stand-ins and drivers shared by the response tests: a literal CBS for the functions the cases use, the real
//! `risunest_regex::Runner` behind the `OutputScripts` seam, and an owner that answers host requests from a tape.
#![allow(dead_code)]

use risunest_ir::budget::Limits;
use risunest_ir::clock::Fixed;
use risunest_ir::json::stringify;
use risunest_ir::stage::{
    Effect, HostReply, HostRequest, PluginHookKind, ScriptMode, ScriptOutput, ScriptRequest, ScriptRunner, StageError, Step,
    TextContext, TextOutput, TextProcessor,
};
use risunest_ir::{Budget, OwnedVec, Text, Value};
use risunest_prompt::response::{
    apply_effect, Emotion, OutputScripts, OutputTriggers, ResponseEffect, ResponseInput, ResponseOutcome, ResponseSettings,
    ResponseStage, ResponseStages,
};
use risunest_regex::cache::ResultLimits;
use risunest_regex::runner::{PluginHooks, Runner, ScriptRunnerConfig};
use risunest_regex::script::CustomScript;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub const NOW: f64 = 1767225600000.0;

pub fn budget() -> Budget { budget_with(Arc::new(AtomicBool::new(false))) }

pub fn budget_with(cancel: Arc<AtomicBool>) -> Budget {
    Budget::new(Limits { max_owned_bytes: 1 << 30, max_json_bytes: 1 << 26, max_text_units: 1 << 24, max_values: 1 << 22,
        max_depth: 64, max_work: 1 << 40 }, cancel).unwrap()
}

pub fn text(value: &str, budget: &Budget) -> Text { Text::from_str(value, budget).unwrap() }
pub fn string(text: &Text) -> String { String::from_utf16_lossy(&text.units().collect::<Vec<_>>()) }
pub fn units(text: &Text) -> Vec<u16> { text.units().collect() }
pub fn field<'a>(value: &'a Value, key: &str, budget: &Budget) -> Option<&'a Value> {
    value.get(key.as_bytes(), budget).unwrap().filter(|value| !matches!(value, Value::Undefined))
}
pub fn json(value: &Value, budget: &Budget) -> String { stringify(value, budget).unwrap().map(|text| string(&text)).unwrap_or_default() }
pub fn parse(source: &str, budget: &Budget) -> Value { risunest_ir::parse_json(source, budget).unwrap() }
pub fn copy(value: &Value, budget: &Budget) -> Value { parse(&json(value, budget), budget) }

/// `risuChatParser` for `{{char}}`, `{{getvar::name}}`, `{{setvar::name::value}}` (a write only under `runVar`) and the
/// documentation-only `{{emotion::name}}`, which stays as written. Any other function is refused, so a case outside
/// these fails instead of passing on a wrong reading.
pub struct LiteralCbs { name: Vec<u16>, variables: HashMap<Vec<u16>, Vec<u16>> }

impl LiteralCbs {
    /// The character's name and the stored `scriptstate` (`$name` keys).
    pub fn new(name: &Text, scriptstate: Option<&Value>, budget: &Budget) -> Self {
        let mut variables = HashMap::new();
        if let Some(Value::Object(state)) = scriptstate {
            for (key, value) in state.as_slice() {
                let key = units(key);
                if key.first() != Some(&0x24) { continue; }
                if let Ok(value) = value.to_text(budget) { variables.insert(key[1..].to_vec(), units(&value)); }
            }
        }
        Self { name: units(name), variables }
    }
}

fn split(units: &[u16]) -> Vec<&[u16]> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut at = 0;
    while at + 1 < units.len() {
        if units[at] == 0x3a && units[at + 1] == 0x3a { parts.push(&units[start..at]); at += 2; start = at; } else { at += 1; }
    }
    parts.push(&units[start..]);
    parts
}

impl TextProcessor for LiteralCbs {
    fn process(&mut self, input: &Text, context: &TextContext, budget: &Budget) -> Result<TextOutput, StageError> {
        let source = units(input);
        let mut output = Vec::new();
        let mut effects = Vec::new();
        let mut at = 0;
        while at < source.len() {
            if !(source[at] == 0x7b && source.get(at + 1) == Some(&0x7b)) { output.push(source[at]); at += 1; continue; }
            let Some(close) = (at + 2..source.len().saturating_sub(1)).find(|&end| source[end] == 0x7d && source[end + 1] == 0x7d)
            else { output.extend_from_slice(&source[at..]); break };
            let inner = &source[at + 2..close];
            if inner.windows(2).any(|pair| pair == [0x7b, 0x7b]) { return Err(StageError::Unsupported(Cow::Borrowed("cbs-nested"))); }
            let parts = split(inner);
            let head = String::from_utf16_lossy(parts[0]);
            match (head.as_str(), parts.len()) {
                ("char", 1) => output.extend_from_slice(&self.name),
                ("getvar", 2) => match self.variables.get(parts[1]) {
                    Some(value) => output.extend_from_slice(value),
                    None => return Err(StageError::Unsupported(Cow::Borrowed("cbs-getvar-missing"))),
                },
                ("setvar", 3) if context.run_var => {
                    self.variables.insert(parts[1].to_vec(), parts[2].to_vec());
                    effects.push(Effect::ChatVariable { name: Text::from_units(parts[1], budget)?, value: Value::Text(Text::from_units(parts[2], budget)?) });
                }
                ("setvar", 3) | ("emotion", 2) => output.extend_from_slice(&source[at..close + 2]),
                _ => return Err(StageError::Unsupported(Cow::Owned(format!("cbs-{head}")))),
            }
            at = close + 2;
        }
        Ok(TextOutput { text: Text::from_units(&output, budget)?, effects })
    }

    fn apply(&mut self, effect: &Effect, budget: &Budget) -> Result<(), StageError> {
        if let Effect::ChatVariable { name, value } = effect {
            self.variables.insert(units(name), units(&value.to_text(budget)?));
        }
        Ok(())
    }
}

/// The real script runner behind the stage's seam.
pub struct Scripts(pub Runner<LiteralCbs>);

impl ScriptRunner for Scripts {
    fn run(&mut self, request: ScriptRequest, budget: &Budget) -> Result<Step<ScriptOutput>, StageError> { self.0.run(request, budget) }
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<ScriptOutput>, StageError> { self.0.resume(reply, budget) }
}

impl OutputScripts for Scripts {
    fn set_message_count(&mut self, count: usize) -> bool { self.0.set_message_count(Some(count)) }
    fn take_emotions(&mut self, budget: &Budget) -> Result<Vec<Emotion>, StageError> {
        self.0.report().emotions.iter().map(|event| Ok(Emotion {
            name: event.name.copy(budget)?, image: event.image.as_ref().map(|image| image.copy(budget)).transpose()?,
        })).collect()
    }
    fn apply(&mut self, effect: &Effect, budget: &Budget) -> Result<(), StageError> { self.0.processor().apply(effect, budget) }
}

/// The stage inputs an oracle entry records at `applyGenerationResponse`.
pub struct Inputs { pub input: ResponseInput, pub scripts: Vec<CustomScript>, pub hooks: usize, pub emotions: Option<Vec<(Text, Text)>>,
    pub lua_listeners: bool, pub name: Text, pub scriptstate: Option<Value> }

pub fn inputs(entry: &Value, budget: &Budget) -> Inputs {
    let character = copy(field(entry, "character", budget).unwrap(), budget);
    let conversation = copy(field(entry, "conversation", budget).unwrap(), budget);
    let response = field(entry, "response", budget).unwrap();
    let kind = match field(response, "type", budget) { Some(Value::Text(kind)) => string(kind), _ => panic!("response type") };
    let reply = match kind.as_str() {
        "success" => match field(response, "result", budget) {
            Some(Value::Text(result)) => risunest_prompt::response::Reply::Success(result.copy(budget).unwrap()),
            _ => panic!("a text result"),
        },
        "multiline" => risunest_prompt::response::Reply::Multiline(field(response, "result", budget).and_then(Value::array).unwrap().iter()
            .map(|pair| {
                let pair = pair.array().unwrap();
                let role = match &pair[0] { Value::Text(role) if string(role) == "user" => risunest_ir::chat::MessageRole::User, _ => risunest_ir::chat::MessageRole::Char };
                let Value::Text(data) = &pair[1] else { panic!("multiline text") };
                (role, data.copy(budget).unwrap())
            }).collect()),
        other => panic!("unexpected response {other}"),
    };
    let flag = |value: Option<&Value>| matches!(value, Some(Value::Bool(true)));
    let settings = field(entry, "settings", budget).unwrap();
    let plugins = field(entry, "plugins", budget).unwrap();
    let count = |key: &str| match field(plugins, key, budget) { Some(Value::Number(count)) => *count as usize, _ => 0 };
    let mut scripts = Vec::new();
    for list in [field(entry, "presetRegex", budget), field(&character, "customscript", budget)] {
        for script in list.and_then(Value::array).unwrap_or(&[]) { scripts.push(CustomScript::from_value(script, budget).unwrap()); }
    }
    let emotions = match field(&character, "type", budget) {
        Some(Value::Text(kind)) if string(kind) == "simple" => None,
        _ => Some(field(&character, "emotionImages", budget).and_then(Value::array).unwrap_or(&[]).iter().map(|pair| {
            let pair = pair.array().unwrap();
            match (&pair[0], &pair[1]) { (Value::Text(name), Value::Text(image)) => (name.copy(budget).unwrap(), image.copy(budget).unwrap()), _ => panic!("emotion pair") }
        }).collect()),
    };
    let lua_listeners = field(&character, "triggerscript", budget).and_then(Value::array).unwrap_or(&[]).iter().any(|trigger| {
        field(trigger, "effect", budget).and_then(Value::array).and_then(|effects| effects.first())
            .and_then(|effect| field(effect, "type", budget)).is_some_and(|kind| matches!(kind, Value::Text(kind) if string(kind) == "triggerlua"))
    });
    let name = match field(&character, "name", budget) { Some(Value::Text(name)) => name.copy(budget).unwrap(), _ => text("", budget) };
    let scriptstate = field(&conversation, "scriptstate", budget).map(|state| copy(state, budget));
    let id = |key: &str| match field(entry, key, budget) { Some(Value::Text(value)) => value.copy(budget).unwrap(), _ => panic!("{key}") };
    let input = ResponseInput {
        character, module_triggers: Vec::new(), conversation, reply,
        continue_generation: flag(field(entry, "continue", budget)),
        generation_id: id("generationId"), saying: id("saying"),
        generation_info: copy(field(entry, "generationInfo", budget).unwrap(), budget),
        prompt_info: copy(field(entry, "promptInfo", budget).unwrap(), budget),
        settings: ResponseSettings {
            remove_incomplete_response: flag(field(settings, "removeIncompleteResponse", budget)),
            tts_auto_speech: flag(field(settings, "ttsAutoSpeech", budget)),
            output_listeners: count("chatOutput"), group_chat: false,
        },
    };
    Inputs { input, scripts, hooks: count("editoutput"), emotions, lua_listeners, name, scriptstate }
}

/// The stages over the real regex runner and the literal CBS.
pub fn stages(inputs: &mut Inputs, triggers: Option<Box<dyn OutputTriggers + Send>>, budget: &Budget) -> ResponseStages {
    let mut config = ScriptRunnerConfig::new(std::mem::take(&mut inputs.scripts));
    config.plugin_hooks = PluginHooks { edit_output: inputs.hooks, ..PluginHooks::default() };
    config.emotions = inputs.emotions.take();
    config.lua_listeners = inputs.lua_listeners;
    let processor = LiteralCbs::new(&inputs.name, inputs.scriptstate.as_ref(), budget);
    let runner = Runner::new(config, processor, ResultLimits::NORMAL);
    ResponseStages {
        text: Box::new(LiteralCbs::new(&inputs.name, inputs.scriptstate.as_ref(), budget)),
        scripts: Box::new(Scripts(runner)),
        triggers,
        clock: Box::new(Fixed(NOW)),
    }
}

/// One answered host request: the plugin hook's input and what the tape answered.
#[derive(Debug)]
pub struct Served { pub data: String, pub reply: String }

/// The owner: answers plugin hooks from `answer`, applies every delivered effect to its own copy of the conversation,
/// and drains the effects at each request when `drain` is set.
pub struct Owner { pub conversation: Value, pub effects: Vec<ResponseEffect>, pub served: Vec<Served>, pub drained: usize }

pub fn drive(stage: &mut ResponseStage, conversation: Value, drain: bool, budget: &Budget,
    mut answer: impl FnMut(usize, &HostRequest, &Budget) -> HostReply) -> (Owner, Result<ResponseOutcome, StageError>) {
    let mut owner = Owner { conversation, effects: Vec::new(), served: Vec::new(), drained: 0 };
    let mut step = stage.start(budget);
    let result = loop {
        match step {
            Ok(Step::NeedsHost(request)) => {
                if drain {
                    let effects = stage.take_effects();
                    owner.drained += effects.len();
                    owner.deliver(effects, budget);
                }
                let reply = answer(owner.served.len(), &request, budget);
                let data = match &request {
                    HostRequest::PluginHook { kind: PluginHookKind::Script(ScriptMode::EditOutput), payload: Value::Text(data) } => string(data),
                    other => format!("{other:?}"),
                };
                owner.served.push(Served { data, reply: format!("{reply:?}") });
                step = stage.resume(reply, budget);
            }
            Ok(Step::Done(outcome)) => break Ok(outcome),
            Err(error) => break Err(error),
        }
    };
    let result = match result {
        Ok(ResponseOutcome::Applied(mut applied)) => {
            let effects = std::mem::take(&mut applied.effects);
            owner.deliver(effects, budget);
            Ok(ResponseOutcome::Applied(applied))
        }
        Ok(ResponseOutcome::Stale { effects }) => { owner.deliver(effects, budget); Ok(ResponseOutcome::Stale { effects: Vec::new() }) }
        Err(error) => { let effects = stage.take_effects(); owner.deliver(effects, budget); Err(error) }
    };
    (owner, result)
}

impl Owner {
    pub fn deliver(&mut self, effects: Vec<ResponseEffect>, budget: &Budget) {
        for effect in effects {
            apply_effect(&mut self.conversation, &effect, budget).unwrap();
            self.effects.push(effect);
        }
    }

    /// `CharEmotion[char.chaId]` after the delivered `@@emo` matches, as `(name, image)` with the clock's time.
    pub fn emotions(&self) -> Option<Vec<(String, String)>> {
        let mut stored: Vec<(String, String)> = Vec::new();
        let mut touched = false;
        for effect in &self.effects {
            let ResponseEffect::Emotion(Emotion { name, image }) = effect else { continue };
            if stored.len() > 4 { stored.remove(0); }
            if let Some(image) = image { stored.push((string(name), string(image))); touched = true; }
        }
        touched.then_some(stored)
    }
}

pub fn values<'a>(value: &'a Value) -> &'a [Value] { value.array().unwrap_or(&[]) }

pub fn empty_array(budget: &Budget) -> Value { Value::Array(OwnedVec::new(budget).unwrap()) }
