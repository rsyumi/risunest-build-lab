//! The route's start trigger: `runTrigger(char, 'start')` (`src/ts/process/triggers.ts`) through the native trigger
//! runner over the records the send reads.

use super::cbs::{HostCapture, ScopedText};
use super::rng::SharedRng;
use risunest_ir::rng::OsEntropy;
use risunest_ir::stage::{
    Effect, HostReply, Pattern, PatternEngine, StageError, Step, TokenCounter, TriggerCbsVariables, TriggerMode, TriggerOutput,
    TriggerRequest, TriggerRunner,
};
use risunest_ir::{Budget, OwnedVec, Text, Value};
use risunest_prompt::machine::PromptInput;
use risunest_records::{modules::SelectedModules, preset::Preset, root::{copy, Root}, settings::Settings};
use risunest_regex::engine::JsPatternEngine;
use risunest_triggers::input::{RandomIds, TriggerInput};
use risunest_triggers::runner::{self, TriggerRun};
use std::borrow::Cow;

// The request the prompt machine runs the start trigger with.
fn start() -> TriggerRequest {
    TriggerRequest { mode: TriggerMode::Start, display: None, manual_name: None, display_mode: false, temp_vars: None,
        trigger_id: None, cbs_variables: TriggerCbsVariables::TriggerScope }
}

/// What the run reads: the character and conversation as the machine holds them, the triggers of the modules the
/// effective `moduleIntergration` selects, and the effective `templateDefaultVariables`, the only root field the run
/// reads. Case mapping follows the host capture as the CBS stage does.
pub(super) fn input(input: &PromptInput, modules: &SelectedModules, host: Option<&HostCapture>, budget: &Budget)
    -> Result<TriggerInput, StageError> {
    let preset = input.preset.as_ref().map(Preset::new).transpose()?;
    let settings = Settings::derive(Root::new(&input.root)?, preset, budget)?;
    let mut root = Value::Object(OwnedVec::new(budget)?);
    if let Some(template) = settings.get("templateDefaultVariables", budget)? {
        root.define(Text::from_str("templateDefaultVariables", budget)?, copy(template, budget)?, budget)?;
    }
    let mut triggers = TriggerInput::from_records_with_modules(&root, &input.character, &input.conversation, modules, budget)?;
    triggers.tailored_case = host.is_none_or(HostCapture::tailored_case);
    triggers.case_tables = HostCapture::trigger_case_tables(host);
    Ok(triggers)
}

/// Whether the run counts tokens: a reachable `v2Tokenize` branches on its count and a system prompt adds one, so the
/// admission that reserves the send counts exactly. A decline the stored triggers decide is the send's decline.
pub(super) fn reaches_counts(input: &TriggerInput, budget: &Budget) -> Result<bool, StageError> {
    runner::reaches_counts(input, &start(), budget).map_err(route_error)
}

/// The start trigger stage: CBS through the prompt's snapshot under the trigger's variables, `Math.random()` from the
/// prompt's draws, the route's token counts, and appended message IDs from operating-system entropy, as TS draws them
/// with `v4()`. The route answers no model or embedding request and does not stop a send, so a run that can reach a
/// low-level model call, a similarity search or `stopSending` declines before it starts.
pub(super) fn stage<C: TokenCounter + Send + 'static>(input: TriggerInput, text: ScopedText, counter: C, rng: SharedRng,
    host: Option<&HostCapture>) -> Box<dyn TriggerRunner + Send> {
    let patterns = Patterns { engine: JsPatternEngine::default(), foreign: host.is_none_or(HostCapture::foreign_regex_unicode) };
    Box::new(StartTrigger(TriggerRun::new(input, text, counter, patterns, rng, RandomIds(OsEntropy)).without_stop_sending()))
}

/// `new RegExp(source, flags)` for the regular expression effects. A pattern whose result follows the Unicode version
/// declines when the WebView's RegExp tables are not the Unicode 17 ones the engine reproduces.
struct Patterns { engine: JsPatternEngine, foreign: bool }
impl PatternEngine for Patterns {
    fn compile(&mut self, source: &Text, flags: &Text, budget: &Budget) -> Result<Box<dyn Pattern + Send>, StageError> {
        budget.work(source.len_units().saturating_add(flags.len_units()))?;
        if self.foreign && risunest_regex::unicode::depends_on_version(&source.units().collect::<Vec<_>>(),
            &String::from_utf16_lossy(&flags.units().collect::<Vec<_>>())) {
            return Err(StageError::Unsupported(Cow::Borrowed("triggers")));
        }
        self.engine.compile(source, flags, budget)
    }
}

/// The runner with its declines named as route refusals.
struct StartTrigger<R>(R);
impl<R: TriggerRunner> TriggerRunner for StartTrigger<R> {
    fn run(&mut self, request: TriggerRequest, budget: &Budget) -> Result<Step<TriggerOutput>, StageError> {
        self.0.run(request, budget).map_err(route_error)
    }
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<TriggerOutput>, StageError> {
        self.0.resume(reply, budget).map_err(route_error)
    }
    fn admit(&self, request: &TriggerRequest, budget: &Budget) -> Result<(), StageError> {
        self.0.admit(request, budget).map_err(route_error)
    }
    fn apply(&mut self, effect: &Effect, budget: &Budget) -> Result<(), StageError> {
        self.0.apply(effect, budget).map_err(route_error)
    }
    fn take_effects(&mut self) -> Vec<Effect> { self.0.take_effects() }
}

// A CBS refusal of the scoped processor and a tokenizer guard keep their names, and so does `stopSending`, which the
// route does not act on yet; any other effect, record shape or regular expression the stage does not run is `triggers`.
// More `Math.random()` draws than an admission records is `cbs.random-draws`, as in the prompt's CBS.
fn route_error(error: StageError) -> StageError {
    match error {
        StageError::Limit(name) if name == "cbs-draws" => StageError::Unsupported(Cow::Borrowed("cbs.random-draws")),
        StageError::Unsupported(name) if name == "trigger-stop-sending" => StageError::Unsupported(Cow::Borrowed("trigger-stop-sending")),
        StageError::Unsupported(name) if name.starts_with("cbs") || name.starts_with("tokenizer-") => StageError::Unsupported(name),
        StageError::Unsupported(_) => StageError::Unsupported(Cow::Borrowed("triggers")),
        error => error,
    }
}
