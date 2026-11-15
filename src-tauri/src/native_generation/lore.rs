use super::cbs::HostCapture;
use super::rng::SharedRng;
use super::scripts::SharedText;
use risunest_ir::stage::{Effect, HostReply, LoreOutput, LoreSelector, Pattern, PatternEngine, StageError, Step, TokenCounter};
use risunest_ir::{Budget, OwnedVec, Text, Value};
use risunest_lorebook::matching::CaseTables;
use risunest_lorebook::selector::{LoreFailureSite, LoreInput, LorePolicy, LoreSelection};
use risunest_prompt::machine::PromptInput;
use risunest_records::{modules::{module_lorebooks, SelectedModules, strict_equals}, preset::Preset, root::{copy, Root}, settings::Settings};
use risunest_regex::engine::JsPatternEngine;
use std::borrow::Cow;
use std::sync::{Arc, Mutex, PoisonError};

pub(super) struct CapturedLore { input: Option<LoreInput>, tables: CaseTables, foreign_regex: bool }

impl CapturedLore { pub(super) fn bearing(&self) -> bool { self.input.is_some() } }

pub(super) fn capture(input: &PromptInput, modules: &SelectedModules, host: Option<&HostCapture>, budget: &Budget) -> Result<CapturedLore, StageError> {
    let settings = Settings::derive(Root::new(&input.root)?, input.preset.as_ref().map(Preset::new).transpose()?, budget)?;
    let mut root = copy(&input.root, budget)?;
    for name in ["templateDefaultVariables"] {
        let value = settings.get(name, budget)?.map(|value| copy(value, budget)).transpose()?.unwrap_or(Value::Undefined);
        root.define(Text::from_str(name, budget)?, value, budget)?;
    }
    let selected = modules.records().iter().collect::<Vec<_>>();
    let mut bearing = !module_lorebooks(&selected, budget)?.is_empty();
    for list in [input.character.get(b"globalLore", budget)?, input.conversation.get(b"localLore", budget)?] {
        match list {
            None | Some(Value::Undefined | Value::Null) => {},
            Some(Value::Array(entries)) => bearing |= !entries.as_slice().is_empty(),
            _ => return Err(StageError::Unsupported(Cow::Borrowed("lore-list"))),
        }
    }
    let input = if bearing {
        // The stored root has no character library; the selected card is the captured speaker lookup.
        let id = input.character.get(b"chaId", budget)?;
        for message in input.conversation.get(b"message", budget)?.and_then(Value::array).unwrap_or_default() {
            let named = message.get(b"name", budget)?.is_some_and(|name| !matches!(name, Value::Undefined | Value::Null));
            if let Some(saying) = message.get(b"saying", budget)?.filter(|saying| saying.truthy()) {
                if !named && !id.is_some_and(|id| strict_equals(id, saying)) {
                    return Err(StageError::Unsupported(Cow::Borrowed("historySpeakerLookup")));
                }
            }
        }
        let mut characters = OwnedVec::new(budget)?;
        characters.push(copy(&input.character, budget)?)?;
        root.define(Text::from_str("characters", budget)?, Value::Array(characters), budget)?;
        let mut lore = LoreInput::from_records_with_modules(&root, &input.character, &input.conversation, modules, budget)?;
        lore.tailored_case = host.is_none_or(HostCapture::tailored_case);
        Some(lore)
    } else { None };
    Ok(CapturedLore { input, tables: host.map_or(CaseTables::Foreign, HostCapture::lore_case_tables),
        foreign_regex: host.is_none_or(HostCapture::foreign_regex_unicode) })
}

#[derive(Clone, Default)]
pub(super) struct LoreFailureTrace(Arc<Mutex<Option<LoreFailureSite>>>);
impl LoreFailureTrace {
    pub(super) fn site(&self) -> Option<LoreFailureSite> { *self.0.lock().unwrap_or_else(PoisonError::into_inner) }
}

struct Patterns { engine: JsPatternEngine, foreign: bool }
impl PatternEngine for Patterns {
    fn compile(&mut self, source: &Text, flags: &Text, budget: &Budget) -> Result<Box<dyn Pattern + Send>, StageError> {
        budget.work(source.len_units().saturating_add(flags.len_units()))?;
        if self.foreign && risunest_regex::unicode::depends_on_version(&source.units().collect::<Vec<_>>(),
            &String::from_utf16_lossy(&flags.units().collect::<Vec<_>>())) {
            return Err(StageError::Unsupported(Cow::Borrowed("regex-unicode")));
        }
        self.engine.compile(source, flags, budget)
    }
}

struct Selection<C> { selector: Option<LoreSelection<SharedText, C, Patterns, SharedRng>>, failure: LoreFailureTrace }
impl<C: TokenCounter> Selection<C> {
    fn result(&self, result: Result<Step<LoreOutput>, StageError>) -> Result<Step<LoreOutput>, StageError> {
        if result.is_err() {
            let mut site = self.failure.0.lock().unwrap_or_else(PoisonError::into_inner);
            if site.is_none() { *site = self.selector.as_ref().and_then(LoreSelection::failure_site); }
        }
        result.map_err(|error| match error {
            StageError::Limit(ref name) if name == "cbs-draws" => StageError::Unsupported(Cow::Borrowed("cbs.random-draws")),
            error => error,
        })
    }
}
impl<C: TokenCounter> LoreSelector for Selection<C> {
    fn select(&mut self, budget: &Budget) -> Result<Step<LoreOutput>, StageError> {
        let result = match self.selector.as_mut() {
            Some(selector) => selector.select(budget),
            None => Ok(Step::Done(LoreOutput { fragments: Vec::new(), effects: Vec::new(), match_log: Vec::new() })),
        };
        self.result(result)
    }
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<LoreOutput>, StageError> {
        let result = match self.selector.as_mut() {
            Some(selector) => selector.resume(reply, budget),
            None => Err(StageError::Invalid(Cow::Borrowed("resume"))),
        };
        self.result(result)
    }
    fn take_effects(&mut self) -> Vec<Effect> { self.selector.as_mut().map_or_else(Vec::new, LoreSelection::take_effects) }
}

pub(super) fn stage<C: TokenCounter + Send + 'static>(capture: CapturedLore, text: SharedText, counter: C, rng: SharedRng,
    failure: LoreFailureTrace) -> Box<dyn LoreSelector + Send> {
    let selector = capture.input.map(|input| LoreSelection::with_policy(input, text, counter,
        Patterns { engine: JsPatternEngine::default(), foreign: capture.foreign_regex }, rng, LorePolicy::Owned)
        .with_case_tables(capture.tables));
    Box::new(Selection { selector, failure })
}
