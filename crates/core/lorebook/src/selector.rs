//! The sans-IO `LoreSelector`.
//!
//! [`LoreSelection`] ports `loadLoreBookV3Prompt` (`src/ts/process/lorebook.svelte.ts:79`). Entries are the
//! character lore, then the chat lore, then the module lorebooks. Each pass parses the decorators of every entry that
//! is not active yet (so `@@probability` draws again and chat variables are read again), searches its keys, and
//! activates it; a recursive activation adds its content to the scanned text and schedules another pass.
//!
//! By default selection plans every pass first: it evaluates, CBS-parses and activates the entries in TS order
//! without a host request, since nothing there reads a token count (only the budget in `finish` does). A decline is found while
//! planning, so it returns before any host request or effect. The token counts then run in the same order and are
//! the one host yield point. Each entry's writes are released where TS makes them: its CBS writes before its count,
//! its `@@keep_activate_after_match` and `@@dont_activate_after_match` writes after it. Where TS throws, selection
//! fails after the counts TS makes first and keeps the writes made before it ([`LoreSelection::take_effects`]).
//! [`LorePolicy::Owned`] instead completes each entry's count and writes before evaluating later work, retaining
//! its completed prefix even when a later capability is unsupported.

use crate::decorators::{self, parse_int};
use crate::entry::{to_number, unsupported, LoreEntry, LoreId};
use crate::matching::{contains, parse_search_keys_with_tables, regex_literal, split_keys, CaseTables, MessageView, SearchKey};
use crate::ordering::{replace_first, sort_descending};
use crate::scan::{message_views_with_tables, slice_start, LoreMessage};
use risunest_ir::budget::{Budget, OwnedVec};
use risunest_ir::chat::Role;
use risunest_ir::rng::Rng;
use risunest_ir::stage::{
    Effect, HostReply, InjectOperation, LoreFragment, LoreInject, LoreMatch, LoreOutput, LoreSelector, Pattern, PatternEngine,
    StageError, Step, TextContext, TextProcessor, TokenCounter, TokenInput,
};
use risunest_ir::text::Text;
use risunest_ir::value::Value;
use risunest_records::modules::{module_lorebooks, select_conversation_modules, strict_equals, ModuleSources, SelectedModules};
use std::borrow::Cow;

/// The lore settings of the character, falling back to the root settings.
#[derive(Debug, Clone, Copy)]
pub struct LoreSettings {
    /// `loreSettings.scanDepth ?? loreBookDepth`.
    pub scan_depth: f64,
    /// `loreSettings.tokenBudget ?? loreBookToken`.
    pub token_budget: f64,
    /// `loreSettings.fullWordMatching ?? false`.
    pub full_word_matching: bool,
    /// `loreSettings.recursiveScanning ?? true`.
    pub recursive_scanning: bool,
}

/// The chat variables `getChatVar` reads: `scriptstate`, then the character's and the template's default variables.
#[derive(Debug, Default)]
pub struct ChatVariables {
    /// `scriptstate['$' + name]` by `name`; `None` for an undefined or null value.
    pub state: Vec<(Text, Option<Text>)>,
    /// `parseKeyValue(character.defaultVariables)` followed by `parseKeyValue(templateDefaultVariables)`.
    pub defaults: Vec<(Text, Text)>,
}

impl ChatVariables {
    /// `getChatVar(name)`.
    pub fn get(&self, name: &Text, budget: &Budget) -> Result<Text, StageError> {
        budget.work(self.state.len().saturating_add(self.defaults.len()))?;
        if let Some((_, Some(value))) = self.state.iter().find(|(key, _)| key == name) { return Ok(value.copy(budget)?); }
        match self.defaults.iter().find(|(key, _)| key == name) {
            Some((_, value)) => Ok(value.copy(budget)?),
            None => Ok(Text::from_str("null", budget)?),
        }
    }
    fn set(&mut self, name: &Text, value: &Text, budget: &Budget) -> Result<(), StageError> {
        budget.work(self.state.len())?;
        match self.state.iter_mut().find(|(key, _)| key == name) {
            Some((_, slot)) => *slot = Some(value.copy(budget)?),
            None => self.state.push((name.copy(budget)?, Some(value.copy(budget)?))),
        }
        Ok(())
    }
}

/// Everything `loadLoreBookV3Prompt` reads, captured before selection.
#[derive(Debug)]
pub struct LoreInput {
    /// Character lore, chat lore, then module lorebooks.
    pub entries: Vec<LoreEntry>,
    /// The whole conversation, oldest first; `history.totalMessages` is its length.
    pub messages: Vec<LoreMessage>,
    /// `` `${DBState.db.username}` ``.
    pub username: Text,
    /// `(chat.fmIndex ?? -1) + 1` when that is a number; `None` when `+` would concatenate.
    pub greeting: Option<f64>,
    pub variables: ChatVariables,
    pub settings: LoreSettings,
    /// The default locale tailors `toLocaleLowerCase` (`tr`, `az`, `lt`): a key or scanned text it would map
    /// differently is declined with `case-locale`. The stored records do not say, so the caller sets it.
    pub tailored_case: bool,
}

static UNDEFINED: Value = Value::Undefined;

fn get<'a>(value: &'a Value, key: &str, budget: &Budget) -> Result<&'a Value, StageError> {
    Ok(value.get(key.as_bytes(), budget)?.unwrap_or(&UNDEFINED))
}
fn nullish(value: &Value) -> bool { matches!(value, Value::Undefined | Value::Null) }
fn invalid(field: &'static str) -> StageError { StageError::Invalid(Cow::Borrowed(field)) }

impl LoreInput {
    /// Reads the selected conversation of `root.characters[character]` from a stored database.
    pub fn from_database(root: &Value, character: usize, budget: &Budget) -> Result<Self, StageError> {
        let character = get(root, "characters", budget)?.array().and_then(|characters| characters.get(character))
            .ok_or_else(|| invalid("character"))?;
        let page = get(character, "chatPage", budget)?;
        let chats = get(character, "chats", budget)?.array().ok_or_else(|| invalid("chats"))?;
        let conversation = match page {
            Value::Number(page) if page.fract() == 0.0 && *page >= 0.0 => chats.get(*page as usize),
            _ => None,
        }.ok_or_else(|| invalid("chatPage"))?;
        Self::from_records(root, character, conversation, budget)
    }

    /// Reads one conversation from its stored root, character and conversation records. `root.characters`, when
    /// present, names the speakers of `saying` messages.
    pub fn from_records(root: &Value, character: &Value, conversation: &Value, budget: &Budget) -> Result<Self, StageError> {
        let entries = Self::character_entries(character, conversation, budget)?;
        let sources = ModuleSources::for_conversation(root, character, conversation, budget)?;
        let modules = select_conversation_modules(&sources, budget)?;
        Self::from_records_with_selected(root, character, conversation, entries, &modules, budget)
    }

    /// Uses the same captured module records as the caller's other preparation stages.
    pub fn from_records_with_modules(root: &Value, character: &Value, conversation: &Value, modules: &SelectedModules, budget: &Budget) -> Result<Self, StageError> {
        let entries = Self::character_entries(character, conversation, budget)?;
        let modules: Vec<&Value> = modules.records().iter().collect();
        Self::from_records_with_selected(root, character, conversation, entries, &modules, budget)
    }

    fn character_entries(character: &Value, conversation: &Value, budget: &Budget) -> Result<Vec<LoreEntry>, StageError> {
        let mut entries = Vec::new();
        for list in [get(character, "globalLore", budget)?, get(conversation, "localLore", budget)?] {
            match list {
                Value::Array(values) => for value in values.as_slice() { entries.push(LoreEntry::from_value(value, budget)?); },
                other if nullish(other) => {}
                _ => return Err(unsupported("lore-list")),
            }
        }
        Ok(entries)
    }

    fn from_records_with_selected(root: &Value, character: &Value, conversation: &Value, mut entries: Vec<LoreEntry>, modules: &[&Value], budget: &Budget) -> Result<Self, StageError> {
        for value in module_lorebooks(modules, budget)? {
            entries.push(LoreEntry::from_value(value, budget)?);
        }

        let lore_settings = get(character, "loreSettings", budget)?;
        let full_word = lore_setting(lore_settings, root, "fullWordMatching", None, budget)?;
        let recursive = lore_setting(lore_settings, root, "recursiveScanning", None, budget)?;
        let settings = LoreSettings {
            scan_depth: to_number(lore_setting(lore_settings, root, "scanDepth", Some("loreBookDepth"), budget)?, "scanDepth", budget)?,
            token_budget: to_number(lore_setting(lore_settings, root, "tokenBudget", Some("loreBookToken"), budget)?, "tokenBudget", budget)?,
            full_word_matching: full_word.truthy(),
            recursive_scanning: nullish(recursive) || recursive.truthy(),
        };

        let character_name = get(character, "name", budget)?;
        let characters = get(root, "characters", budget)?.array().unwrap_or(&[]);
        let mut messages = Vec::new();
        match get(conversation, "message", budget)? {
            Value::Array(values) => for message in values.as_slice() {
                budget.work(1)?;
                let role = get(message, "role", budget)?;
                let data = match get(message, "data", budget)? { Value::Text(text) => Some(text.copy(budget)?), _ => None };
                // `msg.name ?? (msg.saying ? findCharacterbyId(msg.saying)?.name : null) ?? char.name`.
                let name = get(message, "name", budget)?;
                let saying = get(message, "saying", budget)?;
                let name = if !nullish(name) {
                    name.to_text(budget)?
                } else if saying.truthy() {
                    match characters.iter().find(|other| get(other, "chaId", budget).is_ok_and(|id| strict_equals(id, saying))) {
                        Some(other) if !nullish(get(other, "name", budget)?) => get(other, "name", budget)?.to_text(budget)?,
                        Some(_) => character_name.to_text(budget)?,
                        None => Text::from_str("Unknown Character", budget)?,
                    }
                } else {
                    character_name.to_text(budget)?
                };
                messages.push(LoreMessage { user: matches!(role, Value::Text(role) if role.equals_ascii(b"user")), data, name });
            },
            other if nullish(other) => {}
            _ => return Err(unsupported("messages")),
        }

        let greeting = match get(conversation, "fmIndex", budget)? {
            Value::Undefined | Value::Null => Some(0.0),
            Value::Number(index) => Some(index + 1.0),
            Value::Bool(value) => Some(if *value { 2.0 } else { 1.0 }),
            _ => None,
        };

        let mut variables = ChatVariables::default();
        if let Some(state) = get(conversation, "scriptstate", budget)?.object() {
            for (key, value) in state {
                budget.work(1)?;
                let units: Vec<u16> = key.units().collect();
                if units.first() != Some(&(b'$' as u16)) { continue; }
                let value = if nullish(value) { None } else { Some(value.to_text(budget)?) };
                variables.state.push((Text::from_units(&units[1..], budget)?, value));
            }
        }
        for template in [get(character, "defaultVariables", budget)?, get(root, "templateDefaultVariables", budget)?] {
            parse_key_value(template, &mut variables.defaults, budget)?;
        }

        Ok(Self {
            entries, messages, username: get(root, "username", budget)?.to_text(budget)?, greeting, variables, settings,
            tailored_case: false,
        })
    }
}

/// `char.loreSettings?.<name> ?? root.<fallback>`.
fn lore_setting<'a>(settings: &'a Value, root: &'a Value, name: &str, fallback: Option<&str>, budget: &Budget) -> Result<&'a Value, StageError> {
    let value = if nullish(settings) { &UNDEFINED } else { get(settings, name, budget)? };
    match fallback {
        Some(fallback) if nullish(value) => get(root, fallback, budget),
        _ => Ok(value),
    }
}

/// `parseKeyValue(template)` (`src/ts/util.ts:939`): `key=value` lines whose key and value are both nonempty; only
/// the text between the first and second `=` is the value.
fn parse_key_value(template: &Value, output: &mut Vec<(Text, Text)>, budget: &Budget) -> Result<(), StageError> {
    let Value::Text(template) = template else { return Ok(()) };
    let units: Vec<u16> = template.units().collect();
    budget.work(units.len())?;
    for line in units.split(|unit| *unit == b'\n' as u16) {
        let mut parts = line.split(|unit| *unit == b'=' as u16);
        let key = parts.next().unwrap_or(&[]);
        let value = parts.next().unwrap_or(&[]);
        if !key.is_empty() && !value.is_empty() { output.push((Text::from_units(key, budget)?, Text::from_units(value, budget)?)); }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Force { None, Activate, Deactivate }

struct Inject { operation: InjectOperation, location: Vec<u16>, param: Vec<u16>, lore: bool }

struct Active {
    depth: f64,
    position: Text,
    prompt: OwnedVec<u16>,
    role: Role,
    order: f64,
    tokens: u32,
    priority: f64,
    source: Text,
    inject: Option<Inject>,
}

struct Query { keys: Vec<SearchKey>, negative: bool, all: bool }

/// An entry the pass in progress activates.
struct Pending {
    index: usize,
    active: Active,
    keep: bool,
    dont: bool,
    recursive: Option<bool>,
}

/// The token count of an activated entry and the writes on either side of it.
struct Planned {
    index: usize,
    /// The CBS output, until it is counted.
    text: Option<Text>,
    before: Vec<Effect>,
    after: Vec<Effect>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase { Ready, Counting, Finished }

/// Whether selection can be discarded and repeated by a compatibility implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LorePolicy { FallbackAllowed, Owned }

/// The operation that returned selection's terminal error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LorePhase { Evaluate, Content, Count, Keep, Dont, Recursive, Finish }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoreFailureSite { pub entry: Option<usize>, pub phase: LorePhase }

/// `loadLoreBookV3Prompt` over a [`LoreInput`], with CBS, token counting, regex keys and `Math.random()` supplied
/// by the caller's stages.
pub struct LoreSelection<P, C, E, R> {
    input: LoreInput,
    processor: P,
    counter: C,
    patterns: E,
    rng: R,
    phase: Phase,
    policy: LorePolicy,
    case_tables: CaseTables,
    current_site: LoreFailureSite,
    failure_site: Option<LoreFailureSite>,
    pending: Option<(Pending, Text)>,
    started: bool,
    /// The entry the pass in progress looks at next.
    index: usize,
    /// `matching`: a recursive activation in this pass asks for another one.
    again: bool,
    activated: Vec<bool>,
    actives: Vec<Active>,
    recursive_views: Vec<MessageView>,
    base_views: Vec<(usize, OwnedVec<MessageView>)>,
    regexes: Vec<(Vec<u16>, Option<Box<dyn Pattern + Send>>)>,
    primary_keys: Vec<Option<Vec<SearchKey>>>,
    secondary_keys: Vec<Option<Vec<SearchKey>>>,
    match_log: Vec<LoreMatch>,
    effects: Vec<Effect>,
    /// One per entry of `actives`.
    planned: Vec<Planned>,
    counted: usize,
    /// Where planning stopped on an error other than a decline, which follows the planned counts.
    failure: Option<(StageError, LoreFailureSite)>,
    /// A token count TS could not repeat has gone to the host (`risunest_ir::stage::HostKind::commits`): a Google
    /// Cloud `countTokens` request, not a plugin tokenizer count.
    committed: bool,
}

impl<P, C, E, R> LoreSelection<P, C, E, R>
where P: TextProcessor, C: TokenCounter, E: PatternEngine, R: Rng {
    pub fn new(input: LoreInput, processor: P, counter: C, patterns: E, rng: R) -> Self {
        Self::with_policy(input, processor, counter, patterns, rng, LorePolicy::FallbackAllowed)
    }

    pub fn with_policy(input: LoreInput, processor: P, counter: C, patterns: E, rng: R, policy: LorePolicy) -> Self {
        let count = input.entries.len();
        Self {
            input, processor, counter, patterns, rng, phase: Phase::Ready, policy, case_tables: CaseTables::Native,
            current_site: LoreFailureSite { entry: None, phase: LorePhase::Evaluate }, failure_site: None, pending: None,
            started: false, index: 0, again: false,
            activated: vec![false; count], actives: Vec::new(), recursive_views: Vec::new(), base_views: Vec::new(),
            regexes: Vec::new(), primary_keys: (0..count).map(|_| None).collect(), secondary_keys: (0..count).map(|_| None).collect(),
            match_log: Vec::new(), effects: Vec::new(), planned: Vec::new(), counted: 0, failure: None, committed: false,
        }
    }

    /// Sets the captured case tables before selection begins; existing callers use Rust's native tables.
    pub fn with_case_tables(mut self, tables: CaseTables) -> Self { self.case_tables = tables; self }

    /// The first terminal error site, retained after draining; suspended work has no terminal site.
    pub fn failure_site(&self) -> Option<LoreFailureSite> { self.failure_site }

    fn at(&mut self, phase: LorePhase, entry: Option<usize>) { self.current_site = LoreFailureSite { entry, phase }; }

    /// `matchLog` before completion or after an error. A finished selection moves it into [`LoreOutput::match_log`].
    pub fn match_log(&self) -> &[LoreMatch] { &self.match_log }

    /// The chat variable writes made so far and not yet returned. TS writes each one to the store as it happens, so
    /// after `select` or `resume` returns an error these are the writes TS made before it threw: an owner that does
    /// not fall back to TS applies them, then surfaces the error. A fallback-allowed decline returns none, since
    /// the owner then runs TS, which has made none of them yet. After completion they are in [`LoreOutput::effects`] instead
    /// and this is empty.
    pub fn take_effects(&mut self) -> Vec<Effect> { std::mem::take(&mut self.effects) }

    /// Returns the stages, so an owner can keep using them after selection. After an error, a decline included, the
    /// processor has already been given (`apply`) the completed keep/dont writes. Fallback-allowed planning can
    /// also have applied speculative writes that reached no store and that TS never makes
    /// because it throws first: an owner discards it after an error.
    pub fn into_parts(self) -> (P, C, E, R) { (self.processor, self.counter, self.patterns, self.rng) }

    /// Every pass up to the token counts, which nothing here reads.
    fn plan(&mut self, budget: &Budget) -> Result<(), StageError> {
        loop {
            self.at(LorePhase::Evaluate, (self.index < self.input.entries.len()).then_some(self.index));
            budget.work(1)?;
            if self.index >= self.input.entries.len() {
                if !self.again { return Ok(()); }
                self.again = false;
                self.index = 0;
                continue;
            }
            let Some(pending) = self.evaluate(self.index, budget)? else {
                self.index += 1;
                continue;
            };
            // `tokenize(risuChatParser(content, {chara: char}))`; the parse runs without `runVar`.
            self.at(LorePhase::Content, Some(pending.index));
            let content = Text::from_units(pending.active.prompt.as_slice(), budget)?;
            let output = self.processor.process(&content, &TextContext::default(), budget)?;
            for effect in &output.effects { Self::observe(&mut self.input.variables, effect, budget)?; }
            let index = pending.index;
            self.activate(pending, Planned { index, text: Some(output.text), before: output.effects, after: Vec::new() }, budget)?;
        }
    }

    /// The counts in activation order, then the error planning stopped at or the output.
    fn count(&mut self, budget: &Budget) -> Result<Step<LoreOutput>, StageError> {
        while self.counted < self.planned.len() {
            self.at(LorePhase::Count, Some(self.planned[self.counted].index));
            let planned = &mut self.planned[self.counted];
            self.effects.append(&mut planned.before);
            let text = planned.text.as_ref().ok_or_else(|| invalid("count"))?;
            let step = self.counter.count(TokenInput::Text(text), budget)?;
            match step {
                Step::NeedsHost(request) => {
                    self.committed |= request.kind().commits();
                    self.phase = Phase::Counting;
                    return Ok(Step::NeedsHost(request));
                }
                Step::Done(tokens) => self.record_count(tokens),
            }
        }
        if let Some((error, site)) = self.failure.take() { self.current_site = site; return Err(error); }
        self.at(LorePhase::Finish, None);
        self.finish(budget).map(Step::Done)
    }

    fn record_count(&mut self, tokens: u32) {
        let planned = &mut self.planned[self.counted];
        planned.text = None;
        self.actives[self.counted].tokens = tokens;
        self.effects.append(&mut planned.after);
        self.counted += 1;
    }

    /// An error once planning is done. Until a request that commits a decline stays one and returns no writes; after
    /// it, falling back to TS would repeat the request, so the decline fails selection under its name.
    fn settle(&mut self, error: StageError) -> StageError {
        self.phase = Phase::Finished;
        self.failure_site.get_or_insert(self.current_site);
        if self.policy == LorePolicy::Owned { return error; }
        match error {
            StageError::Unsupported(name) if !self.committed => {
                self.effects.clear();
                StageError::Unsupported(name)
            }
            StageError::Unsupported(name) => StageError::Invalid(name),
            error => error,
        }
    }

    /// A write made by the CBS evaluation, kept for later `getChatVar` reads.
    fn observe(variables: &mut ChatVariables, effect: &Effect, budget: &Budget) -> Result<(), StageError> {
        if let Effect::ChatVariable { name, value } = effect {
            let value = value.to_text(budget)?;
            variables.set(name, &value, budget)?;
        }
        Ok(())
    }

    fn variable_effect(&mut self, prefix: &str, index: usize, budget: &Budget) -> Result<Effect, StageError> {
        let mut name = Text::from_str(prefix, budget)?;
        name.append(&self.input.entries[index].variable_suffix(budget)?)?;
        let value = Text::from_str("true", budget)?;
        self.input.variables.set(&name, &value, budget)?;
        Ok(Effect::ChatVariable { name, value: Value::Text(value) })
    }

    fn write_variable(&mut self, prefix: &str, index: usize, budget: &Budget) -> Result<Effect, StageError> {
        let effect = self.variable_effect(prefix, index, budget)?;
        self.processor.apply(&effect, budget)?;
        Ok(effect)
    }

    fn write_owned(&mut self, prefix: &str, index: usize, budget: &Budget) -> Result<(), StageError> {
        let effect = self.variable_effect(prefix, index, budget)?;
        self.effects.push(effect);
        self.processor.apply(self.effects.last().expect("completed variable"), budget)
    }

    fn activate(&mut self, pending: Pending, mut planned: Planned, budget: &Budget) -> Result<(), StageError> {
        let Pending { index, active, keep, dont, recursive } = pending;
        self.at(LorePhase::Recursive, Some(index));
        let view = if recursive.unwrap_or(self.input.settings.recursive_scanning) {
            let mut source = Text::from_str("lorebook ", budget)?;
            source.append(&active.source)?;
            let content = Text::from_units(active.prompt.as_slice(), budget)?;
            Some(MessageView::with_case_tables(source, content.copy(budget)?, content, self.input.tailored_case, self.case_tables, budget)?)
        } else { None };
        if keep { self.at(LorePhase::Keep, Some(index)); planned.after.push(self.write_variable("__internal_ka_", index, budget)?); }
        if dont { self.at(LorePhase::Dont, Some(index)); planned.after.push(self.write_variable("__internal_da_", index, budget)?); }
        self.actives.push(active);
        self.planned.push(planned);
        self.activated[index] = true;
        if let Some(view) = view {
            self.again = true;
            self.recursive_views.push(view);
        }
        self.index = index + 1;
        Ok(())
    }

    fn run_owned(&mut self, budget: &Budget) -> Result<Step<LoreOutput>, StageError> {
        loop {
            self.at(LorePhase::Evaluate, (self.index < self.input.entries.len()).then_some(self.index));
            budget.work(1)?;
            if self.index >= self.input.entries.len() {
                if !self.again {
                    self.at(LorePhase::Finish, None);
                    return self.finish(budget).map(Step::Done);
                }
                self.again = false;
                self.index = 0;
                continue;
            }
            let Some(pending) = self.evaluate(self.index, budget)? else {
                self.index += 1;
                continue;
            };
            self.at(LorePhase::Content, Some(pending.index));
            let content = Text::from_units(pending.active.prompt.as_slice(), budget)?;
            let output = self.processor.process(&content, &TextContext::default(), budget)?;
            let first = self.effects.len();
            self.effects.extend(output.effects);
            for effect in &self.effects[first..] { Self::observe(&mut self.input.variables, effect, budget)?; }
            self.at(LorePhase::Count, Some(pending.index));
            self.pending = Some((pending, output.text));
            let text = &self.pending.as_ref().expect("pending count").1;
            match self.counter.count(TokenInput::Text(text), budget)? {
                Step::NeedsHost(request) => {
                    self.committed |= request.kind().commits();
                    self.phase = Phase::Counting;
                    return Ok(Step::NeedsHost(request));
                }
                Step::Done(tokens) => self.complete_owned(tokens, budget)?,
            }
        }
    }

    fn complete_owned(&mut self, tokens: u32, budget: &Budget) -> Result<(), StageError> {
        let (pending, _) = self.pending.take().ok_or_else(|| invalid("count"))?;
        let Pending { index, mut active, keep, dont, recursive } = pending;
        active.tokens = tokens;
        if keep { self.at(LorePhase::Keep, Some(index)); self.write_owned("__internal_ka_", index, budget)?; }
        if dont { self.at(LorePhase::Dont, Some(index)); self.write_owned("__internal_da_", index, budget)?; }
        self.at(LorePhase::Recursive, Some(index));
        let view = if recursive.unwrap_or(self.input.settings.recursive_scanning) {
            let mut source = Text::from_str("lorebook ", budget)?;
            source.append(&active.source)?;
            let content = Text::from_units(active.prompt.as_slice(), budget)?;
            Some(MessageView::with_case_tables(source, content.copy(budget)?, content, self.input.tailored_case, self.case_tables, budget)?)
        } else { None };
        self.actives.push(active);
        self.activated[index] = true;
        if let Some(view) = view { self.again = true; self.recursive_views.push(view); }
        self.index = index + 1;
        Ok(())
    }

    /// One iteration of the entry loop up to the token count: `None` when the entry stays inactive.
    fn evaluate(&mut self, i: usize, budget: &Budget) -> Result<Option<Pending>, StageError> {
        if self.activated[i] { return Ok(None); }
        {
            let entry = &self.input.entries[i];
            if !entry.always_active && !entry.has_key() { return Ok(None); }
        }
        let mut activated = true;
        if self.input.entries[i].is_child() {
            activated = false;
            let parent = (0..i).find(|j| self.input.entries[*j].id.strict_equals(&self.input.entries[i].id));
            // Each entry looked at compares its id, a string one unit by unit.
            let id_units = match &self.input.entries[i].id { LoreId::Text(id) => id.len_units(), _ => 0 };
            budget.work(parent.map_or(i, |j| j + 1).saturating_mul(id_units.saturating_add(1)))?;
            if let Some(j) = parent {
                if !self.activated[j] {
                    let comment = self.input.entries[j].comment.copy(budget)?;
                    let content = self.input.entries[j].content.as_ref().map(|content| content.copy(budget)).transpose()?;
                    let entry = &mut self.input.entries[i];
                    entry.comment = comment;
                    entry.content = content;
                    entry.always_active = true;
                    activated = true;
                }
            }
        }

        let settings = self.input.settings;
        let chat_length = self.input.messages.len() as f64 + 1.0;
        let mut keep = false;
        let mut dont = false;
        let mut position = Vec::<u16>::new();
        let mut inject: Option<Inject> = None;
        let mut depth = 0.0;
        let mut scan_depth = settings.scan_depth;
        let order = self.input.entries[i].insertorder.ok_or_else(|| unsupported("lore-insertorder"))?;
        let mut priority = order;
        let mut force = Force::None;
        let mut role = Role::System;
        let mut queries: Vec<Query> = Vec::new();
        let mut full_word = settings.full_word_matching;
        let mut dont_search_recursive = false;
        let mut item_recursive: Option<bool> = None;

        let entry = &self.input.entries[i];
        let variables = &self.input.variables;
        let greeting = self.input.greeting;
        let tailored = self.input.tailored_case;
        let tables = self.case_tables;
        let rng = &mut self.rng;
        let content = decorators::parse(entry.content()?, budget, |name, args| {
            let first = args.first();
            let arg_is = |values: &[&str]| first.and_then(Text::as_unicode).is_some_and(|arg| values.contains(&arg));
            let joined = || -> Vec<u16> {
                let mut joined = Vec::new();
                for (index, arg) in args.iter().enumerate() {
                    if index != 0 { joined.push(b' ' as u16); }
                    joined.extend(arg.units());
                }
                joined
            };
            let name = String::from_utf16(name).unwrap_or_default();
            Ok(match name.as_str() {
                "end" => { position = "depth".encode_utf16().collect(); depth = 0.0; true }
                "activate_only_after" => {
                    let int = parse_int(first);
                    if int.is_nan() { return Ok(false); }
                    if chat_length < int { activated = false; }
                    true
                }
                "activate_only_every" => {
                    let int = parse_int(first);
                    if int.is_nan() { return Ok(false); }
                    if chat_length % int != 0.0 { activated = false; }
                    true
                }
                "keep_activate_after_match" | "dont_activate_after_match" => {
                    let keep_name = name == "keep_activate_after_match";
                    let mut variable = Text::from_str(if keep_name { "__internal_ka_" } else { "__internal_da_" }, budget)?;
                    variable.append(&entry.variable_suffix(budget)?)?;
                    if variables.get(&variable, budget)?.equals_ascii(b"true") {
                        force = if keep_name { Force::Activate } else { Force::Deactivate };
                    } else if keep_name { keep = true; } else { dont = true; }
                    false
                }
                "depth" | "reverse_depth" => {
                    let int = parse_int(first);
                    if int.is_nan() { return Ok(false); }
                    depth = int;
                    position = name.encode_utf16().collect();
                    true
                }
                "role" => match first.and_then(Text::as_unicode) {
                    Some("user") => { role = Role::User; true }
                    Some("assistant") => { role = Role::Assistant; true }
                    Some("system") => { role = Role::System; true }
                    _ => false,
                },
                "scan_depth" => { scan_depth = parse_int(first); true }
                "is_greeting" => {
                    let int = parse_int(first);
                    if int.is_nan() { return Ok(false); }
                    if greeting != Some(int) { activated = false; }
                    true
                }
                "position" => {
                    // `arg[0].startsWith` throws on a missing argument.
                    let Some(first) = first else { return Err(invalid("lore-position-argument")) };
                    let units: Vec<u16> = first.units().collect();
                    if units.starts_with(&[b'p' as u16, b't' as u16, b'_' as u16])
                        || arg_is(&["after_desc", "before_desc", "personality", "scenario"]) {
                        position = units;
                        true
                    } else { false }
                }
                "inject_lore" | "inject_at" => {
                    let lore = name == "inject_lore";
                    let inject = inject.get_or_insert_with(|| Inject { operation: InjectOperation::Append, location: Vec::new(), param: Vec::new(), lore });
                    inject.location = joined();
                    inject.lore = lore;
                    true
                }
                "inject_replace" | "inject_prepend" => {
                    let operation = if name == "inject_replace" { InjectOperation::Replace } else { InjectOperation::Prepend };
                    let inject = inject.get_or_insert_with(|| Inject { operation, location: Vec::new(), param: Vec::new(), lore: false });
                    inject.operation = operation;
                    inject.param = joined();
                    true
                }
                "ignore_on_max_context" => { priority = -1000.0; true }
                "additional_keys" | "exclude_keys" | "exclude_keys_all" => {
                    let keys: Vec<Vec<u16>> = args.iter().map(|arg| arg.units().collect()).collect();
                    queries.push(Query {
                        keys: parse_search_keys_with_tables(&keys, tailored, tables, budget)?,
                        negative: name != "additional_keys",
                        all: name == "exclude_keys_all",
                    });
                    true
                }
                "match_full_word" => { full_word = true; true }
                "match_partial_word" => { full_word = false; true }
                "activate" => { force = Force::Activate; true }
                "dont_activate" => { force = Force::Deactivate; true }
                "disable_ui_prompt" => arg_is(&["post_history_instructions", "system_prompt"]),
                "probability" => {
                    if rng.random()? * 100.0 > parse_int(first) { activated = false; }
                    true
                }
                "priority" => { priority = parse_int(first); true }
                "unrecursive" => { item_recursive = Some(false); true }
                "recursive" => { item_recursive = Some(true); true }
                "no_recursive_search" => { dont_search_recursive = true; true }
                // `instruct_depth`, `reverse_instruct_depth`, `instruct_scan_depth`, `is_user_icon` and unknown names.
                _ => false,
            })
        })?;

        let always_active = self.input.entries[i].always_active;
        if activated && force == Force::None && !always_active {
            queries.push(Query { keys: self.search_keys(i, false, budget)?, negative: false, all: false });
            let entry = &self.input.entries[i];
            if entry.has_secondkey() && entry.selective {
                queries.push(Query { keys: self.search_keys(i, true, budget)?, negative: false, all: false });
            }
            let regex = self.input.entries[i].use_regex;
            for query in &queries {
                let result = self.search(&query.keys, scan_depth, regex, full_word, query.all, dont_search_recursive, budget)?;
                if result == query.negative { activated = false; break; }
            }
        }
        match force {
            Force::Activate => activated = true,
            Force::Deactivate => activated = false,
            Force::None => {}
        }
        if !activated { return Ok(None); }

        let entry = &self.input.entries[i];
        // `comment || `lorebook ${i}``.
        let source = match entry.comment.truthy_text("lore-comment")? {
            Some(comment) => comment.copy(budget)?,
            None => Text::from_str(&format!("lorebook {i}"), budget)?,
        };
        let mut prompt = OwnedVec::new(budget)?;
        prompt.extend(&content.units().collect::<Vec<_>>())?;
        Ok(Some(Pending {
            index: i,
            active: Active { depth, position: Text::from_units(&position, budget)?, prompt, role, order, tokens: 0, priority, source, inject },
            keep, dont, recursive: item_recursive,
        }))
    }

    /// `getPrimarySearchKeys(i)` or `getSecondarySearchKeys(i)`.
    fn search_keys(&mut self, i: usize, secondary: bool, budget: &Budget) -> Result<Vec<SearchKey>, StageError> {
        let cache = if secondary { &mut self.secondary_keys } else { &mut self.primary_keys };
        if cache[i].is_none() {
            let entry = &self.input.entries[i];
            let (key, field) = if secondary { (&entry.secondkey, "lore-secondkey") } else { (&entry.key, "lore-key") };
            // Only a truthy key is searched; `key.split` throws on a non-string.
            let key = key.truthy_text(field)?.ok_or_else(|| invalid(field))?;
            cache[i] = Some(parse_search_keys_with_tables(&split_keys(key, budget)?, self.input.tailored_case, self.case_tables, budget)?);
        }
        let keys = cache[i].as_ref().expect("cached keys");
        let mut copy = Vec::with_capacity(keys.len());
        for key in keys {
            copy.push(SearchKey { raw: key.raw.copy(budget)?, raw_units: key.raw_units.clone(), lower: key.lower.clone(), no_space: key.no_space.clone() });
        }
        Ok(copy)
    }

    /// `getRegex(raw)`: the compiled `/source/flags` key, `None` when it is not one or JS rejects it.
    fn regex(&mut self, raw: &[u16], budget: &Budget) -> Result<Option<usize>, StageError> {
        if let Some(index) = self.regexes.iter().position(|(key, _)| key == raw) {
            return Ok(self.regexes[index].1.as_ref().map(|_| index));
        }
        let compiled = match regex_literal(raw) {
            None => None,
            Some((source, flags)) => {
                match self.patterns.compile(&Text::from_units(source, budget)?, &Text::from_units(flags, budget)?, budget) {
                    Ok(pattern) => Some(pattern),
                    // The `SyntaxError` `new RegExp` throws. A pattern the engine cannot run declines instead.
                    Err(StageError::Invalid(name)) if name == "regex-syntax" => None,
                    Err(error) => return Err(error),
                }
            }
        };
        let found = compiled.is_some();
        self.regexes.push((raw.to_vec(), compiled));
        Ok(found.then_some(self.regexes.len() - 1))
    }

    /// `searchMatch`.
    #[allow(clippy::too_many_arguments)]
    fn search(&mut self, keys: &[SearchKey], scan_depth: f64, regex: bool, full_word: bool, all: bool, dont_search_recursive: bool,
        budget: &Budget) -> Result<bool, StageError> {
        let start = slice_start(self.input.messages.len(), scan_depth);
        let base = match self.base_views.iter().position(|(cached, _)| *cached == start) {
            Some(index) => index,
            None => {
                let views = message_views_with_tables(&self.input.messages, start, &self.input.username, self.input.tailored_case, self.case_tables, budget)?;
                self.base_views.push((start, views));
                self.base_views.len() - 1
            }
        };
        if regex {
            let mut patterns = Vec::with_capacity(keys.len());
            for key in keys {
                match self.regex(&key.raw_units, budget)? {
                    Some(index) => patterns.push((index, key)),
                    None => return Ok(false),
                }
            }
            let recursive: &[MessageView] = if dont_search_recursive { &[] } else { &self.recursive_views };
            for view in self.base_views[base].1.as_slice().iter().chain(recursive) {
                for (index, key) in &patterns {
                    budget.work(1)?;
                    let pattern = self.regexes[*index].1.as_ref().expect("compiled regex");
                    if pattern.exec(&view.data, 0, budget)?.is_some() {
                        self.match_log.push(LoreMatch { prompt: view.prompt.copy(budget)?, source: view.source.copy(budget)?, activated: key.raw.copy(budget)? });
                        return Ok(true);
                    }
                }
            }
            return Ok(false);
        }
        let recursive: &[MessageView] = if dont_search_recursive { &[] } else { &self.recursive_views };
        let mut all_matched = true;
        for view in self.base_views[base].1.as_slice().iter().chain(recursive) {
            for key in keys {
                budget.work(1)?;
                let hit = if full_word {
                    view.has_word(&key.lower, budget)?
                } else {
                    contains(view.no_space_data.as_slice(), &key.no_space, budget)?
                };
                if hit {
                    self.match_log.push(LoreMatch { prompt: view.normalized_prompt.copy(budget)?, source: view.source.copy(budget)?, activated: key.raw.copy(budget)? });
                    if !all { return Ok(true); }
                } else if all {
                    all_matched = false;
                }
            }
        }
        Ok(all && all_matched)
    }

    /// Priority sort, token budget, order sort, `@@inject_lore` and the final reverse.
    fn finish(&mut self, budget: &Budget) -> Result<LoreOutput, StageError> {
        self.phase = Phase::Finished;
        let mut actives: Vec<Option<Active>> = std::mem::take(&mut self.actives).into_iter().map(Some).collect();
        let priorities: Vec<f64> = actives.iter().map(|active| active.as_ref().map_or(0.0, |active| active.priority)).collect();
        let orders: Vec<f64> = actives.iter().map(|active| active.as_ref().map_or(0.0, |active| active.order)).collect();
        let tokens: Vec<f64> = actives.iter().map(|active| active.as_ref().map_or(0.0, |active| active.tokens as f64)).collect();
        let mut sorted: Vec<usize> = (0..actives.len()).collect();
        sort_descending(&mut sorted, &|index| priorities[index], budget)?;
        let mut used = 0.0;
        let mut kept: Vec<usize> = sorted.into_iter().filter(|index| {
            if used + tokens[*index] <= self.input.settings.token_budget { used += tokens[*index]; true } else { false }
        }).collect();
        sort_descending(&mut kept, &|index| orders[index], budget)?;
        let is_lore = |active: &Option<Active>| active.as_ref().and_then(|active| active.inject.as_ref()).is_some_and(|inject| inject.lore);
        let injections: Vec<usize> = kept.iter().copied().filter(|index| is_lore(&actives[*index])).collect();
        let mut remaining: Vec<usize> = kept.into_iter().filter(|index| !is_lore(&actives[*index])).collect();
        for lore in injections {
            let lore = actives[lore].take().expect("injected lore");
            let inject = lore.inject.as_ref().expect("inject");
            // Each candidate compares its source with the location.
            budget.work(remaining.len().saturating_add(1).saturating_mul(inject.location.len().saturating_add(1)))?;
            let Some(target) = remaining.iter().copied().find(|index| {
                actives[*index].as_ref().is_some_and(|active| active.source.units().eq(inject.location.iter().copied()))
            }) else { continue };
            let target = actives[target].as_mut().expect("target lore");
            // Prepending and replacing copy the target, and the copy is charged, so many injections into one long
            // target stop at the work limit.
            match inject.operation {
                InjectOperation::Append => {
                    target.prompt.push(b' ' as u16)?;
                    target.prompt.extend(lore.prompt.as_slice())?;
                }
                InjectOperation::Prepend => {
                    let mut prompt = OwnedVec::new(budget)?;
                    prompt.extend(lore.prompt.as_slice())?;
                    prompt.push(b' ' as u16)?;
                    prompt.extend(target.prompt.as_slice())?;
                    target.prompt = prompt;
                }
                InjectOperation::Replace => {
                    target.prompt = replace_first(target.prompt.as_slice(), &inject.param, lore.prompt.as_slice(), budget)?;
                }
            }
        }
        remaining.reverse();
        let mut fragments = Vec::with_capacity(remaining.len());
        for index in remaining {
            let active = actives[index].take().expect("active lore");
            let inject = match active.inject {
                Some(inject) => Some(LoreInject {
                    operation: inject.operation, location: Text::from_units(&inject.location, budget)?,
                    param: Text::from_units(&inject.param, budget)?, lore: inject.lore,
                }),
                None => None,
            };
            fragments.push(LoreFragment {
                position: active.position, prompt: Text::from_units(active.prompt.as_slice(), budget)?, role: active.role,
                depth: active.depth, order: active.order, priority: active.priority, tokens: active.tokens,
                source: active.source, inject,
            });
        }
        Ok(LoreOutput { fragments, effects: std::mem::take(&mut self.effects), match_log: std::mem::take(&mut self.match_log) })
    }
}

impl<P, C, E, R> LoreSelector for LoreSelection<P, C, E, R>
where P: TextProcessor, C: TokenCounter, E: PatternEngine, R: Rng {
    fn take_effects(&mut self) -> Vec<Effect> { LoreSelection::take_effects(self) }

    fn select(&mut self, budget: &Budget) -> Result<Step<LoreOutput>, StageError> {
        if self.started { return Err(invalid("select")); }
        self.started = true;
        if self.policy == LorePolicy::Owned { return self.run_owned(budget).map_err(|error| self.settle(error)); }
        match self.plan(budget) {
            Ok(()) => {}
            Err(StageError::Unsupported(name)) => {
                return Err(self.settle(StageError::Unsupported(name)));
            }
            Err(error) => self.failure = Some((error, self.current_site)),
        }
        self.count(budget).map_err(|error| self.settle(error))
    }

    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<LoreOutput>, StageError> {
        if self.phase != Phase::Counting { return Err(invalid("resume")); }
        let step = match self.counter.resume(reply, budget) {
            Ok(Step::NeedsHost(request)) => Ok(Step::NeedsHost(request)),
            Ok(Step::Done(tokens)) => {
                self.phase = Phase::Ready;
                if self.policy == LorePolicy::Owned {
                    self.complete_owned(tokens, budget).and_then(|()| self.run_owned(budget))
                } else {
                    self.record_count(tokens);
                    self.count(budget)
                }
            }
            Err(error) => Err(error),
        };
        step.map_err(|error| self.settle(error))
    }
}
