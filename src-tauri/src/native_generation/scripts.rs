//! The stored editprocess and editoutput rules through the native regex runner. Preparation refuses unsupported
//! output plans before reservation; an output-dependent refusal after dispatch is a terminal native failure.

use super::inputs::{unsupported, PreparationError, PreparationSnapshot};
use risunest_ir::stage::{
    Effect, HostReply, HostRequest, ScriptMode, ScriptOutput, ScriptRequest, ScriptRunner, StageError, Step, TextContext, TextOutput, TextProcessor,
};
use risunest_ir::{Budget, Text, Value as Ir};
use risunest_records::{modules::SelectedModules, root::copy};
use risunest_regex::cache::ResultLimits;
use risunest_regex::runner::{Runner, ScriptRunnerConfig};
use risunest_regex::script::CustomScript;
use risunest_regex::directives::{self, Directive};
use risunest_prompt::response::{Emotion, OutputScripts};
use serde_json::Value;
use std::borrow::Cow;
use std::sync::{Arc, Mutex, PoisonError};

// `char.customscript` after the load's `??= []`, and `db.presetRegex ?? []`.
fn list<'a>(value: &'a Value) -> Result<&'a [Value], PreparationError> {
    match value {
        Value::Null => Ok(&[]),
        Value::Array(scripts) => Ok(scripts),
        _ => Err(unsupported("regex.scripts")),
    }
}

/// The stored scripts for one mode, preset then character, with the startup regex exclusion applied.
fn scripts<'a>(snapshot: &'a PreparationSnapshot, db: &'a Value, mode: &str) -> Result<Vec<&'a Value>, PreparationError> {
    let preset = list(&db["presetRegex"])?;
    let character = list(&snapshot.history.character["customscript"])?;
    if preset.iter().chain(character).any(|script| !script.is_object()) { return Err(unsupported("regex.scripts")); }
    let Some(host) = snapshot.host.as_ref() else {
        if preset.iter().chain(character).any(|script| script["type"] == mode) { return Err(unsupported("regex.host")); }
        return Ok(Vec::new());
    };
    let preset = if host.regex_excluded() { &[] } else { preset };
    let scripts: Vec<&Value> = preset.iter().chain(character).filter(|script| script["type"] == mode).collect();
    // `CustomScript::from_value` reads text `in` and `out` and an optional text `flag`.
    if scripts.iter().any(|script| !script["in"].is_string() || !script["out"].is_string()
        || !matches!(script.get("flag"), None | Some(Value::Null | Value::String(_)))) {
        return Err(unsupported("regex.scripts"));
    }
    Ok(scripts)
}

pub(super) fn edit_process_scripts<'a>(snapshot: &'a PreparationSnapshot, db: &'a Value) -> Result<Vec<&'a Value>, PreparationError> {
    scripts(snapshot, db, "editprocess")
}

pub(super) fn edit_output_scripts<'a>(snapshot: &'a PreparationSnapshot, db: &'a Value) -> Result<Vec<&'a Value>, PreparationError> {
    scripts(snapshot, db, "editoutput")
}

// Module regex uses truthy concat; startup exclusion skips even reading this contribution.
pub(super) fn with_modules(snapshot: &PreparationSnapshot, base: &[&Value], modules: &SelectedModules, mode: &str,
    budget: &Budget) -> Result<Vec<Ir>, StageError> {
    let mut scripts = base.iter().map(|script| super::prepare::ir_value(script, 2, budget)).collect::<Result<Vec<_>, _>>()?;
    if snapshot.host.as_ref().is_some_and(|host| host.regex_excluded()) { return Ok(scripts); }
    let mut append = |script: &Ir| -> Result<(), StageError> {
        let refusal = || StageError::Unsupported(Cow::Borrowed("regex.scripts"));
        if !matches!(script, Ir::Object(_)) { return Err(refusal()); }
        if !matches!(script.get(b"type", budget)?, Some(Ir::Text(value)) if value.equals_ascii(mode.as_bytes())) { return Ok(()); }
        if snapshot.host.is_none() { return Err(StageError::Unsupported(Cow::Borrowed("regex.host"))); }
        if !matches!(script.get(b"in", budget)?, Some(Ir::Text(_)))
            || !matches!(script.get(b"out", budget)?, Some(Ir::Text(_)))
            || !matches!(script.get(b"flag", budget)?, None | Some(Ir::Undefined | Ir::Null | Ir::Text(_))) {
            return Err(refusal());
        }
        scripts.push(copy(script, budget)?);
        Ok(())
    };
    for module in modules.records() {
        if !module.truthy() { continue; }
        if let Some(regex) = module.get(b"regex", budget)?.filter(|value| value.truthy()) {
            match regex {
                Ir::Array(entries) => for entry in entries.as_slice() { append(entry)?; },
                entry => append(entry)?,
            }
        }
    }
    Ok(scripts)
}

/// The prompt machine's CBS processor, which the script runner shares: TS runs every `risuChatParser` of a send
/// against one chat, so the runner's CBS takes the next recorded draw and reads what the machine's CBS wrote.
#[derive(Clone)]
pub(super) struct SharedText(Arc<Mutex<Box<dyn TextProcessor + Send>>>);

impl SharedText {
    pub(super) fn new(text: Box<dyn TextProcessor + Send>) -> Self { Self(Arc::new(Mutex::new(text))) }
}

impl TextProcessor for SharedText {
    fn process(&mut self, input: &Text, context: &TextContext, budget: &Budget) -> Result<TextOutput, StageError> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).process(input, context, budget)
    }
    fn apply(&mut self, effect: &Effect, budget: &Budget) -> Result<(), StageError> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).apply(effect, budget)
    }
}

pub(super) struct RouteScripts(Runner<SharedText>);

impl RouteScripts {
    /// `scripts` are the stored records of [`edit_process_scripts`]; `foreign_unicode` is
    /// `HostCapture::foreign_regex_unicode`.
    pub(super) fn new(scripts: &[Ir], foreign_unicode: bool, text: SharedText, budget: &Budget) -> Result<Self, StageError> {
        let scripts = scripts.iter().map(|script| CustomScript::from_value(script, budget)).collect::<Result<Vec<_>, _>>()?;
        let mut config = ScriptRunnerConfig::new(scripts);
        // TS keeps `processScriptCache` across sends, so a later send can return what an earlier one stored. The route
        // reproduces a send whose every call misses it, which is what the oracles record: each send starts cold.
        config.result_cache = true;
        config.foreign_unicode = foreign_unicode;
        Ok(Self(Runner::new(config, text, ResultLimits::NORMAL)))
    }

    pub(super) fn admit_output(&mut self, chat_index: i64, budget: &Budget,
        mut replacement: impl FnMut(&[u16]) -> Result<(), StageError>) -> Result<(), StageError> {
        self.0.admit_static(ScriptMode::EditOutput, chat_index, budget, |entry| {
            if entry.dynamic_pattern { return Err(StageError::Unsupported(Cow::Borrowed("regex.dynamicPattern"))); }
            if directives::repeats_back(entry, chat_index) { return Err(StageError::Unsupported(Cow::Borrowed("regex.repeatBack"))); }
            match directives::on_match(entry, chat_index) {
                Directive::Emotion => return Err(StageError::Unsupported(Cow::Borrowed("regex.emo"))),
                Directive::Inject => return Err(StageError::Unsupported(Cow::Borrowed("regex.inject"))),
                _ => replacement(&entry.replacement)?,
            }
            Ok(())
        }).map_err(|error| match &error {
            StageError::Unsupported(name) if name.starts_with("regex.") => error,
            _ => script_error(error),
        })
    }
}

fn script_error(error: StageError) -> StageError {
    match error {
        StageError::Unsupported(name) if name.starts_with("cbs") => StageError::Unsupported(name),
        StageError::Unsupported(name) => match &*name {
            "@@inject" => StageError::Unsupported(Cow::Borrowed("regex.inject")),
            "regex-unicode" => StageError::Unsupported(Cow::Borrowed("regex.unicode")),
            _ => StageError::Unsupported(Cow::Borrowed("regex.engine")),
        },
        error => error,
    }
}

impl ScriptRunner for RouteScripts {
    fn run(&mut self, request: ScriptRequest, budget: &Budget) -> Result<Step<ScriptOutput>, StageError> {
        let output = match self.0.run(request, budget) {
            Ok(Step::Done(output)) => output,
            // The route does not read the conversation for a `@@repeat_back` source.
            Ok(Step::NeedsHost(HostRequest::Conversation(_))) => return Err(StageError::Unsupported(Cow::Borrowed("regex.repeatBack"))),
            // editoutput can call Lua; reservation refuses every active Lua trigger, module and plugin.
            Ok(Step::NeedsHost(_)) => return Err(StageError::Unsupported(Cow::Borrowed("regex.host"))),
            Err(error) => return Err(script_error(error)),
        };
        let report = self.0.report();
        // The shared processor already holds the CBS writes, and a send writes no chat variable before its request.
        if !output.effects.is_empty() { return Err(StageError::Unsupported(Cow::Borrowed("regex.effect"))); }
        // `@@emo` updates the character's emotion store.
        if !report.emotions.is_empty() { return Err(StageError::Unsupported(Cow::Borrowed("regex.emo"))); }
        // A search past the native limits, which TS finishes.
        if report.skipped.iter().any(|rule| matches!(rule.error, StageError::Limit(_))) {
            return Err(StageError::Unsupported(Cow::Borrowed("regex.limit")));
        }
        Ok(Step::Done(output))
    }
    fn resume(&mut self, _: HostReply, _: &Budget) -> Result<Step<ScriptOutput>, StageError> { Err(StageError::Invalid(Cow::Borrowed("resume"))) }
}

impl OutputScripts for RouteScripts {
    fn set_message_count(&mut self, count: usize) -> bool { self.0.set_message_count(Some(count)) }
    fn take_emotions(&mut self, budget: &Budget) -> Result<Vec<Emotion>, StageError> {
        self.0.report().emotions.iter().map(|event| Ok(Emotion {
            name: event.name.copy(budget)?, image: event.image.as_ref().map(|image| image.copy(budget)).transpose()?,
        })).collect()
    }
    fn apply(&mut self, effect: &Effect, budget: &Budget) -> Result<(), StageError> { self.0.processor().apply(effect, budget) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::stage::{CbsConditions, ScriptMode};
    use risunest_ir::Limits;
    use std::sync::atomic::AtomicBool;

    // CBS that writes a chat variable on every parse, which the route's processor does only under `run_var`.
    struct Writes;
    impl TextProcessor for Writes {
        fn process(&mut self, input: &Text, _: &TextContext, budget: &Budget) -> Result<TextOutput, StageError> {
            Ok(TextOutput { text: input.copy(budget)?, effects: vec![Effect::ChatVariable { name: Text::from_str("seen", budget)?, value: Ir::Undefined }] })
        }
        fn apply(&mut self, _: &Effect, _: &Budget) -> Result<(), StageError> { Ok(()) }
    }

    #[test]
    fn a_run_that_writes_declines() {
        let budget = Budget::new(Limits { max_owned_bytes: 1 << 24, max_json_bytes: 1 << 20, max_text_units: 1 << 20, max_values: 1 << 16,
            max_depth: 64, max_work: 1 << 30 }, Arc::new(AtomicBool::new(false))).unwrap();
        let mut scripts = RouteScripts::new(&[], false, SharedText::new(Box::new(Writes)), &budget).unwrap();
        let request = |input: &str| ScriptRequest { mode: ScriptMode::EditProcess, input: Text::from_str(input, &budget).unwrap(), chat_index: -1,
            conditions: CbsConditions::default() };
        assert!(matches!(scripts.run(request("plain"), &budget), Ok(Step::Done(_))));
        assert_eq!(scripts.run(request("{{x}}"), &budget).err(), Some(StageError::Unsupported(Cow::Borrowed("regex.effect"))));
    }
}
