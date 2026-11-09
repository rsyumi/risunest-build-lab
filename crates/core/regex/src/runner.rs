//! The sans-IO `ScriptRunner`.
//!
//! [`Runner`] is `processScriptFullImpl` (`src/ts/process/scripts.ts`) over the scripts in its
//! [`ScriptRunnerConfig`]. Outside a capture context it first yields the Lua edit listeners (`runLuaEditTrigger`, not
//! for `editprocess` or a character without `triggerlua`) and then each `pluginV2[mode]` hook in registration order.
//! It then parses CBS through the [`TextProcessor`], consults the result cache, and runs the regex execution plan,
//! reading the conversation from the host for each `@@repeat_back` that runs. A live `editdisplay` run (the display
//! trigger) and dynamic assets belong to other stages and return `Unsupported`.
//!
//! The Lua listeners' writes arrive in their reply. The runner feeds each to its processor and returns them first in
//! [`ScriptOutput::effects`], followed by the CBS writes and the `@@inject` edits in the order the rules make them.
//!
//! A rule that fails keeps the data it was given and the run goes on, as the host callback of
//! `processScriptFullImpl` does after `console.error` or a skipped rule; the run records it in
//! [`RunReport::skipped`]. Cancellation, a decline and a limit of the run's own [`Budget`] end the run.
//!
//! The owner runs a declined call in TS, which makes every host call again, so a decline comes before the first
//! host request: what the scripts and the configuration show, including a pattern past the compile limits, is checked
//! before the Lua listeners and hooks. A decline that still arises after a served host request fails the run as
//! `Invalid("<name>-after-host")`.

use crate::cache::{PatternCache, PatternLimits, ResultCache, ResultLimits};
use crate::directives::{self, Directive, MoveMatch};
use crate::engine::{regress_engine, CompileError, CompiledPattern, DEFAULT_MATCH_LIMITS};
use crate::replacement::UnitMatch;
use crate::script::{self, ascii, has_cbs_syntax, has_cbs_syntax_str, mode_name, CustomScript, Held, PlanEntry, Units16};
use crate::unicode;
use regex_compat_experimental::js::MatchLimits;
use risunest_ir::budget::Budget;
use risunest_ir::profile::EngineClass;
use risunest_ir::stage::{
    CbsConditions, ConversationRead, Effect, HostKind, HostReply, HostRequest, LuaMode, LuaReply, LuaTrigger, MessageEdit,
    PluginHookKind, ScriptMode, ScriptOutput, ScriptRequest, ScriptRunner, StageError, Step, TextContext, TextProcessor,
};
use risunest_ir::{Text, Value};
use std::borrow::Cow;
use std::sync::Arc;

fn unsupported(name: &'static str) -> StageError { StageError::Unsupported(Cow::Borrowed(name)) }
fn invalid(name: &'static str) -> StageError { StageError::Invalid(Cow::Borrowed(name)) }

/// `pluginV2[mode].size` for each mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PluginHooks { pub edit_input: usize, pub edit_output: usize, pub edit_process: usize, pub edit_display: usize }

impl PluginHooks {
    pub fn get(&self, mode: ScriptMode) -> usize {
        match mode {
            ScriptMode::EditInput => self.edit_input,
            ScriptMode::EditOutput => self.edit_output,
            ScriptMode::EditProcess => self.edit_process,
            ScriptMode::EditDisplay => self.edit_display,
        }
    }
}

/// What `processScriptFull` reads besides its arguments.
#[derive(Debug)]
pub struct ScriptRunnerConfig {
    /// `presetRegex`, `char.customscript` and the module scripts in that order, or only the character's scripts when
    /// regex is excluded at startup.
    pub scripts: Vec<CustomScript>,
    /// `options.captureContext` is set: no host callbacks, and CBS gets the condition's role.
    pub capture_context: bool,
    /// The display recovery mark is set for the character; it applies to `editdisplay` only.
    pub display_recovery: bool,
    /// The character or a module has a trigger whose first effect is `triggerlua`.
    pub lua_listeners: bool,
    pub plugin_hooks: PluginHooks,
    /// `(name, image)` of `char.emotionImages`; `None` for a simple character.
    pub emotions: Option<Vec<(Text, Text)>>,
    /// `dynamicAssets` is on and the character has additional assets.
    pub dynamic_assets: bool,
    pub dynamic_assets_edit_display: bool,
    /// `options.cache !== 'bypass'`.
    pub result_cache: bool,
    pub match_limits: MatchLimits,
    pub pattern_limits: PatternLimits,
    /// The length of the selected chat's `message` array over the whole conversation, which `@@inject` indexes
    /// (`chat.message[chatID].data = data`, which throws for a message that does not exist). The run counts the Lua
    /// listeners' message edits into it; `None` declines a run that could inject into a message.
    pub message_count: Option<usize>,
    /// The RegExp engine whose results the run reproduces takes its Unicode tables from a version other than
    /// [`crate::unicode::TABLE_VERSION`], so a pattern [`crate::unicode::depends_on_version`] declines as `regex-unicode`.
    pub foreign_unicode: bool,
}

impl ScriptRunnerConfig {
    pub fn new(scripts: Vec<CustomScript>) -> Self {
        Self {
            scripts, capture_context: false, display_recovery: false, lua_listeners: false, plugin_hooks: PluginHooks::default(),
            emotions: None, dynamic_assets: false, dynamic_assets_edit_display: false, result_cache: true,
            match_limits: DEFAULT_MATCH_LIMITS, pattern_limits: PatternLimits::default(), message_count: None, foreign_unicode: false,
        }
    }
}

/// The stage an [`EngineUse`] belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineStage {
    LuaEdit,
    /// The plugin hook at this registration index.
    PluginHook(usize),
    /// The script at this index of [`ScriptRunnerConfig::scripts`].
    Script(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineUse { pub stage: EngineStage, pub engine: EngineClass }

/// One matching `@@emo` script. The owner updates `CharEmotion` from it: it drops the oldest of more than four stored
/// entries, then appends `[name, image, Date.now()]` when `image` is set.
#[derive(Debug)]
pub struct EmotionEvent { pub name: Text, pub image: Option<Text> }

/// A rule that failed and left the data as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedRule {
    /// The index of the script in [`ScriptRunnerConfig::scripts`], the `sourceIndex` TS reports.
    pub rule: usize,
    pub mode: ScriptMode,
    /// A `regex-` limit of the search or the pattern, `Invalid("regex-syntax")` for a pattern the RegExp constructor
    /// rejects, a CBS failure inside the rule, or the `TypeError` of an `@@inject` into a message that does not exist
    /// (`Invalid("inject-message")`) or of a `@@repeat_back` whose source is not text or does not match
    /// (`Invalid("repeat-back-source")`, `Invalid("repeat-back-match")`). A failed conversation read is `Host`.
    pub error: StageError,
}

/// What the last run did besides its output.
#[derive(Debug, Default)]
pub struct RunReport {
    pub engines: Vec<EngineUse>,
    pub emotions: Vec<EmotionEvent>,
    /// The message of a plugin hook that threw, which fails the run.
    pub host_failure: Option<Text>,
    pub cache_hit: bool,
    pub skipped: Vec<SkippedRule>,
}

struct Pending {
    mode: ScriptMode,
    data: Text,
    chat_index: i64,
    conditions: CbsConditions,
    /// The Lua listeners' writes, already fed to the processor.
    effects: Vec<Effect>,
    /// [`ScriptRunnerConfig::message_count`] after those writes.
    messages: Option<usize>,
    /// The first of those writes the processor refused.
    stale: Option<StageError>,
}

/// A run between two rules, waiting for the `@@repeat_back` source of the rule at `next`.
struct Rules {
    mode: ScriptMode,
    chat_index: i64,
    next: usize,
    /// The input of the rule that waits.
    data: Data,
    key: Option<Units16>,
    state: RunState,
    /// The pattern of the rule that waits.
    compiled: Option<Arc<CompiledPattern>>,
}

enum Phase { Idle, Lua(Pending), Hook(Pending, usize), Rules(Box<Rules>) }

/// One script's share of `generateScriptCacheKey`, around the chat index.
struct KeyPart {
    /// The pattern of a `<cbs>` script, which is parsed into the key on every run.
    cbs_pattern: Option<Vec<u16>>,
    /// The pattern of any other script, then `|||` and the replacement.
    before_chat: Vec<u16>,
    /// `|||`, the flag, `|||` and `ableFlag` as `1` or `0`.
    after_chat: Vec<u16>,
}

/// The execution plan and cache key parts of one mode, kept while the scripts are unchanged like
/// `getRegexExecutionPlan`'s plan cache.
struct ModePlan { entries: Vec<PlanEntry>, key: Vec<KeyPart> }

impl ModePlan {
    fn build(scripts: &[CustomScript], mode: ScriptMode, budget: &Budget) -> Result<Self, StageError> {
        let entries = script::plan(scripts, mode, budget)?;
        let separator = ascii("|||");
        let key = scripts.iter().filter(|script| script.mode == Some(mode)).map(|script| {
            let cbs = contains(&script.flag, "<cbs>");
            let mut before_chat: Vec<u16> = if cbs { Vec::new() } else { script.pattern.units().collect() };
            before_chat.extend_from_slice(&separator);
            before_chat.extend(script.replacement.units());
            let mut after_chat = separator.clone();
            after_chat.extend(script.flag.units());
            after_chat.extend_from_slice(&separator);
            after_chat.push(if script.able_flag { 49 } else { 48 });
            KeyPart { cbs_pattern: cbs.then(|| script.pattern.units().collect()), before_chat, after_chat }
        }).collect();
        Ok(Self { entries, key })
    }
}

fn mode_slot(mode: ScriptMode) -> usize {
    match mode {
        ScriptMode::EditInput => 0,
        ScriptMode::EditOutput => 1,
        ScriptMode::EditProcess => 2,
        ScriptMode::EditDisplay => 3,
    }
}

/// A failure `executeScript` throws for one rule, which the rule's caller logs before it goes on with the rule's
/// input. A decline, cancellation and the run's own limits end the run.
fn skips_rule(error: &StageError) -> bool {
    match error {
        StageError::Limit(name) => name.starts_with("regex-"),
        StageError::Invalid(_) | StageError::Host(_) => true,
        _ => false,
    }
}

/// `ToIntegerOrInfinity` of a relative `slice` or `splice` index, clamped into `0..=length`.
fn relative_index(value: f64, length: usize) -> usize {
    if value.is_nan() { return 0; }
    let integer = value.trunc();
    if integer < 0.0 { (length as f64 + integer).max(0.0) as usize } else { integer.min(length as f64) as usize }
}

/// The length of the `message` array after `effect`, or `None` when the runner cannot tell.
fn messages_after(length: usize, effect: &Effect) -> Option<usize> {
    let edit = match effect {
        Effect::Message(edit) => edit,
        Effect::ChatVariable { .. } | Effect::ChatField { .. } | Effect::CharacterField { .. } => return Some(length),
        _ => return None,
    };
    match edit {
        MessageEdit::SetData { .. } | MessageEdit::SetRole { .. } => Some(length),
        MessageEdit::Cut { start, end } => {
            let start = relative_index(*start, length);
            Some(end.map_or(length, |end| relative_index(end, length)).saturating_sub(start))
        }
        MessageEdit::Remove { index } => Some(if relative_index(*index, length) < length { length - 1 } else { length }),
        MessageEdit::Append { .. } | MessageEdit::Insert { .. } => Some(length + 1),
        MessageEdit::Replace { messages } => messages.array().map(<[Value]>::len),
        _ => None,
    }
}

/// The source a `@@repeat_back` read returned.
enum Source { Text(Text), Other, Failed(Vec<u16>) }

pub struct Runner<P> {
    config: ScriptRunnerConfig,
    processor: P,
    cache: ResultCache,
    patterns: PatternCache,
    plans: [Option<ModePlan>; 4],
    phase: Phase,
    report: RunReport,
    /// The host answered a request of this run.
    served: bool,
}

impl<P: TextProcessor> Runner<P> {
    pub fn new(config: ScriptRunnerConfig, processor: P, cache_limits: ResultLimits) -> Self {
        let patterns = PatternCache::new(config.pattern_limits);
        Self { config, processor, cache: ResultCache::new(cache_limits), patterns, plans: Default::default(), phase: Phase::Idle,
            report: RunReport::default(), served: false }
    }

    pub fn config(&self) -> &ScriptRunnerConfig { &self.config }
    /// The scripts and settings of later runs; `None` while a run waits for the host. The kept plans are rebuilt.
    pub fn config_mut(&mut self) -> Option<&mut ScriptRunnerConfig> {
        if !matches!(self.phase, Phase::Idle) { return None; }
        self.plans = Default::default();
        Some(&mut self.config)
    }
    /// [`ScriptRunnerConfig::message_count`] for later runs, keeping the plans; `false` while a run waits for the host.
    pub fn set_message_count(&mut self, count: Option<usize>) -> bool {
        if !matches!(self.phase, Phase::Idle) { return false; }
        self.config.message_count = count;
        true
    }
    pub fn processor(&mut self) -> &mut P { &mut self.processor }
    pub fn report(&self) -> &RunReport { &self.report }
    pub fn result_cache(&self) -> &ResultCache { &self.cache }
    /// `resetScriptCache`, or a profile change with new limits.
    pub fn reset_result_cache(&mut self, limits: ResultLimits) { self.cache = ResultCache::new(limits); }

    fn live(&self, mode: ScriptMode) -> bool {
        !self.config.capture_context && !(mode == ScriptMode::EditDisplay && self.config.display_recovery)
    }

    fn next_hook(&mut self, pending: Pending, index: usize, budget: &Budget) -> Result<Step<ScriptOutput>, StageError> {
        let count = if self.live(pending.mode) { self.config.plugin_hooks.get(pending.mode) } else { 0 };
        if index < count {
            let payload = Value::Text(pending.data.copy(budget)?);
            let kind = PluginHookKind::Script(pending.mode);
            self.report.engines.push(EngineUse { stage: EngineStage::PluginHook(index), engine: EngineClass::Host(HostKind::PluginHook) });
            self.phase = Phase::Hook(pending, index);
            return Ok(Step::NeedsHost(HostRequest::PluginHook { kind, payload }));
        }
        let served = self.served;
        self.process(pending, budget).map_err(|error| after_host(error, served))
    }

    /// Feeds one Lua listener write to the processor and keeps it for the output.
    fn absorb(&mut self, pending: &mut Pending, effect: Effect, budget: &Budget) -> Result<(), StageError> {
        pending.messages = pending.messages.and_then(|length| messages_after(length, &effect));
        match self.processor.apply(&effect, budget) {
            Ok(()) => {}
            Err(StageError::Unsupported(name)) => { pending.stale.get_or_insert(StageError::Unsupported(name)); }
            Err(error) => return Err(error),
        }
        pending.effects.push(effect);
        Ok(())
    }

    /// The declines a run can show before its first host request.
    /// Compiles static patterns and visits runnable entries in execution order, without parsing CBS or serving hosts.
    /// Empty patterns and syntax-error rules cannot execute. Dynamic patterns are visited without compilation.
    pub fn admit_static(&mut self, mode: ScriptMode, chat_index: i64, budget: &Budget,
        mut validate: impl FnMut(&PlanEntry) -> Result<(), StageError>) -> Result<(), StageError> {
        if !matches!(self.phase, Phase::Idle) { return Err(invalid("script-runner-busy")); }
        self.admit(mode, chat_index, budget)?;
        let Some(plan) = &self.plans[mode_slot(mode)] else { return Ok(()) };
        for entry in &plan.entries {
            if entry.pattern.is_empty() { continue; }
            if !entry.dynamic_pattern && matches!(self.patterns.get_or_compile(&entry.pattern, &entry.flags, budget)?.0,
                Err(CompileError::Syntax)) { continue; }
            validate(entry)?;
        }
        Ok(())
    }

    fn admit(&mut self, mode: ScriptMode, chat_index: i64, budget: &Budget) -> Result<(), StageError> {
        let live = self.live(mode);
        if live && mode == ScriptMode::EditDisplay { return Err(unsupported("editdisplay-live")); }
        if (mode == ScriptMode::EditDisplay && self.config.display_recovery) || self.config.scripts.is_empty() { return Ok(()); }
        let assets = mode == ScriptMode::EditOutput || (mode == ScriptMode::EditDisplay && self.config.dynamic_assets_edit_display);
        if self.config.dynamic_assets && assets { return Err(unsupported("dynamicAssets")); }
        let slot = mode_slot(mode);
        if self.plans[slot].is_none() { self.plans[slot] = Some(ModePlan::build(&self.config.scripts, mode, budget)?); }
        let Self { config, patterns, plans, .. } = self;
        let entries = &plans[slot].as_ref().expect("plan").entries;
        // Only the message count tells whether `chat.message[chatID]` exists.
        let unknown_message = !config.capture_context && chat_index >= 0 && config.message_count.is_none();
        for entry in entries.iter().filter(|entry| !entry.pattern.is_empty()) {
            if unknown_message && directives::on_match(entry, chat_index) == Directive::Inject { return Err(unsupported("@@inject")); }
            // A `<cbs>` pattern exists only after CBS parses it.
            if entry.dynamic_pattern { continue; }
            if config.foreign_unicode && unicode::depends_on_version(&entry.pattern, &entry.flags) { return Err(unsupported("regex-unicode")); }
            match patterns.get_or_compile(&entry.pattern, &entry.flags, budget)?.0 {
                Err(CompileError::Unsupported(declined)) => return Err(unsupported(declined)),
                _ => {}
            }
        }
        Ok(())
    }

    fn process(&mut self, pending: Pending, budget: &Budget) -> Result<Step<ScriptOutput>, StageError> {
        let Pending { mode, data, chat_index, conditions, effects, messages, stale } = pending;
        let role = match (&conditions.chat_role, self.config.capture_context) {
            (Some(role), true) => Some(role.copy(budget)?),
            _ => None,
        };
        let context = TextContext { chat_index, role, run_var: false, rm_var: false, visualize: false, conditions, ..TextContext::default() };
        let recovery = mode == ScriptMode::EditDisplay && self.config.display_recovery;
        let slot = mode_slot(mode);
        if !recovery && self.plans[slot].is_none() { self.plans[slot] = Some(ModePlan::build(&self.config.scripts, mode, budget)?); }
        let Self { config, processor, cache, patterns, report, plans, .. } = self;
        let config: &ScriptRunnerConfig = config;
        let has_scripts = !recovery && !config.scripts.is_empty();
        let empty = ModePlan { entries: Vec::new(), key: Vec::new() };
        let plan = if recovery { &empty } else { plans[slot].as_ref().expect("plan") };
        let state = RunState { context, effects, emotion_changed: false, messages, stale };
        let mut run = Run { config, processor, patterns, report, budget, state };
        let mut data = run.parse(Data::from_text(data))?;
        let key = if config.result_cache { Some(run.cache_key(plan, mode, &mut data)?) } else {
            for pattern in plan.key.iter().filter_map(|part| part.cbs_pattern.as_ref()) {
                run.parse(Data::from_units(pattern.clone(), budget)?)?;
            }
            None
        };
        if let Some(key) = &key {
            if let Some(hit) = cache.get(&key.data) {
                let text = Text::from_units(hit, budget)?;
                run.report.cache_hit = true;
                return Ok(Step::Done(ScriptOutput { text, emotion_changed: false, effects: run.state.effects }));
            }
        }
        if !has_scripts {
            if let Some(key) = &key { cache.set(&key.data, data.units(budget)?.to_vec()); }
            let RunState { effects, emotion_changed, .. } = run.state;
            return Ok(Step::Done(ScriptOutput { text: data.into_text(budget)?, emotion_changed, effects }));
        }
        let state = run.state;
        self.rules(Box::new(Rules { mode, chat_index, next: 0, data, key, state, compiled: None }), None, budget)
    }

    /// Runs the plan from `rules.next`, first finishing the waiting `@@repeat_back` with its source.
    fn rules(&mut self, mut rules: Box<Rules>, source: Option<Source>, budget: &Budget) -> Result<Step<ScriptOutput>, StageError> {
        let Self { config, processor, cache, patterns, report, plans, phase, .. } = self;
        let config: &ScriptRunnerConfig = config;
        let plan = plans[mode_slot(rules.mode)].as_ref().expect("plan");
        let mut run = Run { config, processor, patterns, report, budget, state: std::mem::take(&mut rules.state) };
        let mode = rules.mode;
        let mut index = rules.next;
        if let Some(source) = source {
            let entry = &plan.entries[index];
            let compiled = rules.compiled.take().expect("waiting rule");
            match run.repeat_back(entry, &compiled, &mut rules.data, source) {
                Ok(Some(next)) => rules.data = next,
                Ok(None) => {}
                Err(error) => skip_rule(run.report, entry.source_index, mode, error)?,
            }
            index += 1;
        }
        while let Some(entry) = plan.entries.get(index) {
            match run.execute(entry, &mut rules.data, rules.chat_index) {
                Ok(Outcome::Keep) => {}
                Ok(Outcome::Data(next)) => rules.data = next,
                Ok(Outcome::Read(compiled)) => {
                    run.record(entry, EngineClass::Host(HostKind::Conversation));
                    let chat_index = rules.chat_index;
                    rules.state = run.state;
                    rules.next = index;
                    rules.compiled = Some(compiled);
                    *phase = Phase::Rules(rules);
                    return Ok(Step::NeedsHost(HostRequest::Conversation(ConversationRead::RepeatBackSource { chat_index })));
                }
                Err(error) => skip_rule(run.report, entry.source_index, mode, error)?,
            }
            index += 1;
        }
        let Rules { mut data, key, .. } = *rules;
        if let Some(key) = &key { cache.set(&key.data, data.units(budget)?.to_vec()); }
        let RunState { effects, emotion_changed, .. } = run.state;
        Ok(Step::Done(ScriptOutput { text: data.into_text(budget)?, emotion_changed, effects }))
    }
}

impl<P: TextProcessor> ScriptRunner for Runner<P> {
    fn run(&mut self, request: ScriptRequest, budget: &Budget) -> Result<Step<ScriptOutput>, StageError> {
        if !matches!(self.phase, Phase::Idle) { return Err(invalid("script-runner-busy")); }
        self.report = RunReport::default();
        self.served = false;
        let ScriptRequest { mode, input, chat_index, conditions } = request;
        self.admit(mode, chat_index, budget)?;
        let live = self.live(mode);
        let pending = Pending { mode, data: input, chat_index, conditions, effects: Vec::new(), messages: self.config.message_count, stale: None };
        if live && self.config.lua_listeners && mode != ScriptMode::EditProcess {
            let payload = Value::Text(pending.data.copy(budget)?);
            self.report.engines.push(EngineUse { stage: EngineStage::LuaEdit, engine: EngineClass::Host(HostKind::Lua) });
            self.phase = Phase::Lua(pending);
            return Ok(Step::NeedsHost(HostRequest::Lua { trigger: LuaTrigger::Edit { index: Some(chat_index) }, mode: LuaMode::Script(mode), payload }));
        }
        self.next_hook(pending, 0, budget)
    }

    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<ScriptOutput>, StageError> {
        let served = self.served;
        self.served |= !matches!(reply, HostReply::Unsupported(_) | HostReply::Cancelled);
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Idle => Err(invalid("script-runner-resume")),
            // `runLuaEditTriggerBatch` keeps the content when a listener throws or returns nothing, and reads only the
            // result: a stop belongs to the send.
            Phase::Lua(mut pending) => {
                let (value, effects) = match reply {
                    HostReply::Lua(LuaReply { value, effects, .. }) => (value, effects),
                    HostReply::Value(value) => (value, Vec::new()),
                    HostReply::Failed { .. } => (Value::Undefined, Vec::new()),
                    HostReply::Unsupported(name) => return Err(after_host(StageError::Unsupported(name), served)),
                    HostReply::Cancelled => return Err(StageError::Cancelled),
                    _ => return Err(invalid("lua-edit-reply")),
                };
                match value {
                    Value::Text(text) => pending.data = text,
                    Value::Undefined | Value::Null => {}
                    _ => return Err(invalid("lua-edit-result")),
                }
                for effect in effects { self.absorb(&mut pending, effect, budget)?; }
                self.next_hook(pending, 0, budget)
            }
            // A hook result other than `null` or `undefined` replaces the data; a hook that throws fails the call.
            Phase::Hook(mut pending, index) => {
                match reply {
                    HostReply::Value(Value::Text(text)) => pending.data = text,
                    HostReply::Value(Value::Undefined | Value::Null) => {}
                    HostReply::Value(_) => return Err(invalid("plugin-hook-result")),
                    HostReply::Failed { message } => {
                        let units = message.units().collect();
                        self.report.host_failure = Some(message);
                        return Err(StageError::Host(units));
                    }
                    HostReply::Unsupported(name) => return Err(after_host(StageError::Unsupported(name), served)),
                    HostReply::Cancelled => return Err(StageError::Cancelled),
                    _ => return Err(invalid("plugin-hook-reply")),
                }
                self.next_hook(pending, index + 1, budget)
            }
            Phase::Rules(rules) => {
                let source = match reply {
                    HostReply::Value(Value::Text(text)) => Source::Text(text),
                    HostReply::Value(_) => Source::Other,
                    HostReply::Failed { message } => Source::Failed(message.units().collect()),
                    HostReply::Unsupported(name) => return Err(after_host(StageError::Unsupported(name), served)),
                    HostReply::Cancelled => return Err(StageError::Cancelled),
                    _ => return Err(invalid("repeat-back-reply")),
                };
                self.rules(rules, Some(source), budget).map_err(|error| after_host(error, true))
            }
        }
    }
}

/// A decline after a served host request fails the run, since the owner's TS fallback would repeat that request.
fn after_host(error: StageError, served: bool) -> StageError {
    match error {
        StageError::Unsupported(name) if served => StageError::Invalid(Cow::Owned(format!("{name}-after-host"))),
        error => error,
    }
}

/// Records a failure that skips its rule; any other failure ends the run.
fn skip_rule(report: &mut RunReport, rule: usize, mode: ScriptMode, error: StageError) -> Result<(), StageError> {
    if !skips_rule(&error) { return Err(error); }
    report.skipped.push(SkippedRule { rule, mode, error });
    Ok(())
}

fn contains(text: &Text, needle: &str) -> bool {
    let units: Vec<u16> = text.units().collect();
    script::find(&units, &ascii(needle), 0).is_some()
}

/// The data between operations, as text, UTF-16 units or both; each form is built only when an operation needs it.
struct Data { text: Option<Text>, units: Option<Units16> }

impl Data {
    fn from_text(text: Text) -> Self { Self { text: Some(text), units: None } }
    fn from_units(units: Vec<u16>, budget: &Budget) -> Result<Self, StageError> { Ok(Self::from_units16(Units16::from_vec(units, budget)?)) }
    fn from_units16(units: Units16) -> Self { Self { text: None, units: Some(units) } }

    fn units(&mut self, budget: &Budget) -> Result<&[u16], StageError> {
        if self.units.is_none() { self.units = Some(Units16::of(self.text.as_ref().expect("data"), budget)?); }
        Ok(&self.units.as_ref().expect("units").data)
    }

    fn into_units(mut self, budget: &Budget) -> Result<Units16, StageError> {
        self.units(budget)?;
        Ok(self.units.take().expect("units"))
    }

    fn text(&mut self, budget: &Budget) -> Result<&Text, StageError> {
        if self.text.is_none() { self.text = Some(Text::from_units(&self.units.as_ref().expect("data").data, budget)?); }
        Ok(self.text.as_ref().expect("text"))
    }

    fn into_text(mut self, budget: &Budget) -> Result<Text, StageError> {
        self.text(budget)?;
        Ok(self.text.take().expect("text"))
    }

    fn has_cbs_syntax(&mut self, budget: &Budget) -> Result<bool, StageError> {
        if let Some(view) = self.text.as_ref().and_then(Text::as_unicode) { return Ok(has_cbs_syntax_str(view)); }
        Ok(has_cbs_syntax(self.units(budget)?))
    }
}

/// What a run carries from rule to rule, across a host request too.
#[derive(Default)]
struct RunState {
    context: TextContext,
    effects: Vec<Effect>,
    emotion_changed: bool,
    /// [`ScriptRunnerConfig::message_count`] as the run stands; `None` once a Lua listener changed the messages in a way
    /// the runner does not count.
    messages: Option<usize>,
    /// The first Lua listener write the processor refused. CBS after it would read a conversation the processor was
    /// not told of, so the run fails where CBS next runs.
    stale: Option<StageError>,
}

/// What [`Run::execute`] leaves for the rule loop.
enum Outcome { Keep, Data(Data), Read(Arc<CompiledPattern>) }

struct Run<'a, P> {
    config: &'a ScriptRunnerConfig,
    processor: &'a mut P,
    patterns: &'a mut PatternCache,
    report: &'a mut RunReport,
    budget: &'a Budget,
    state: RunState,
}

impl<P: TextProcessor> Run<'_, P> {
    /// `parseCbs(value)`, which returns CBS-free text unchanged.
    fn parse(&mut self, mut data: Data) -> Result<Data, StageError> {
        if !data.has_cbs_syntax(self.budget)? { return Ok(data); }
        if let Some(error) = &self.state.stale { return Err(error.clone()); }
        let output = self.processor.process(data.text(self.budget)?, &self.state.context, self.budget)?;
        self.state.effects.extend(output.effects);
        Ok(Data::from_text(output.text))
    }

    /// `generateScriptCacheKey`.
    fn cache_key(&mut self, plan: &ModePlan, mode: ScriptMode, data: &mut Data) -> Result<Units16, StageError> {
        let budget = self.budget;
        let separator = ascii("|||");
        let chat = ascii(&self.state.context.chat_index.to_string());
        let input = data.units(budget)?;
        let mut key = Units16::with_capacity(input.len() + 2 * separator.len() + mode_name(mode).len(), budget)?;
        key.extend(input, budget)?;
        key.extend(&separator, budget)?;
        key.extend(&ascii(mode_name(mode)), budget)?;
        key.extend(&separator, budget)?;
        for part in &plan.key {
            if let Some(pattern) = &part.cbs_pattern {
                let mut pattern = self.parse(Data::from_units(pattern.clone(), budget)?)?;
                key.extend(pattern.units(budget)?, budget)?;
            }
            key.extend(&part.before_chat, budget)?;
            key.extend(&chat, budget)?;
            key.extend(&part.after_chat, budget)?;
        }
        Ok(key)
    }

    fn record(&mut self, entry: &PlanEntry, engine: EngineClass) {
        let used = EngineUse { stage: EngineStage::Script(entry.source_index), engine };
        if self.report.engines.last() != Some(&used) { self.report.engines.push(used); }
    }

    fn test(&mut self, entry: &PlanEntry, compiled: &CompiledPattern, data: &mut Data) -> Result<bool, StageError> {
        if compiled.has_safe_plan() {
            if let Some((safe, view)) = compiled.safe_for(data.text(self.budget)?) {
                self.budget.work(view.len())?;
                let matched = safe.is_match(view);
                self.record(entry, EngineClass::Rust);
                return Ok(matched);
            }
        }
        let units = data.units(self.budget)?;
        compiled.admit(units)?;
        let matched = compiled.test(units, self.config.match_limits, self.budget)?;
        self.record(entry, regress_engine());
        Ok(matched)
    }

    /// `data.replace(regex, template)` as new data; `data` itself only gains the forms the search reads.
    fn replace(&mut self, entry: &PlanEntry, compiled: &CompiledPattern, data: &mut Data, template: &[u16]) -> Result<Data, StageError> {
        if compiled.has_safe_plan() {
            if let Some((safe, view)) = compiled.safe_for(data.text(self.budget)?) {
                if let Some(output) = CompiledPattern::safe_replace(safe, view, template, self.budget)? {
                    let _output = self.budget.reserve_owned(output.capacity())?;
                    let text = Text::from_str(&output, self.budget)?;
                    self.record(entry, EngineClass::Rust);
                    return Ok(Data::from_text(text));
                }
            }
        }
        let units = data.units(self.budget)?;
        compiled.admit(units)?;
        let output = compiled.replace_units(units, template, self.config.match_limits, self.budget)?;
        self.record(entry, regress_engine());
        Ok(Data::from_units16(output))
    }

    /// `matchAll` for a global pattern, otherwise `[input.match(regex)]`, as `@@move_top`/`@@move_bottom` read it,
    /// with the charge that covers the copies.
    fn move_matches(&mut self, entry: &PlanEntry, compiled: &CompiledPattern, data: &mut Data) -> Result<(Vec<MoveMatch>, Held), StageError> {
        let budget = self.budget;
        let units = data.units(budget)?;
        compiled.admit(units)?;
        let names = compiled.names();
        let mut found = Vec::new();
        let mut held = Held::new(0, budget)?;
        let mut bytes = 0usize;
        compiled.each_match(units, self.config.match_limits, budget, |groups| {
            let view = UnitMatch { input: units, groups, names };
            let captured: usize = groups.iter().flatten().map(|range| range.len()).sum();
            let named: usize = names.iter().map(|(name, _)| name.len() + view.named(name).map_or(0, |range| range.len())).sum();
            budget.work(captured + named + 1)?;
            bytes = bytes.saturating_add(std::mem::size_of::<MoveMatch>() + groups.len() * std::mem::size_of::<Option<Vec<u16>>>()
                + names.len() * std::mem::size_of::<(Vec<u16>, Option<Vec<u16>>)>() + 2 * (captured + named));
            held.cover(bytes.saturating_add(found.capacity().max(1) * std::mem::size_of::<MoveMatch>()), budget)?;
            let values: Vec<Option<Vec<u16>>> = groups.iter().map(|group| group.clone().map(|range| units[range].to_vec())).collect();
            let named = (!names.is_empty())
                .then(|| names.iter().map(|(name, _)| (name.clone(), view.named(name).map(|range| units[range].to_vec()))).collect());
            found.push(MoveMatch { values, groups: named });
            Ok(())
        })?;
        self.record(entry, regress_engine());
        Ok((found, held))
    }

    /// `executeScript` for one plan entry.
    fn execute(&mut self, entry: &PlanEntry, data: &mut Data, chat_index: i64) -> Result<Outcome, StageError> {
        if entry.pattern.is_empty() { return Ok(Outcome::Keep); }
        let budget = self.budget;
        let pattern = if entry.dynamic_pattern {
            let mut pattern = self.parse(Data::from_units(entry.pattern.clone(), budget)?)?;
            let pattern = pattern.units(budget)?.to_vec();
            if self.config.foreign_unicode && unicode::depends_on_version(&pattern, &entry.flags) { return Err(unsupported("regex-unicode")); }
            Cow::Owned(pattern)
        } else { Cow::Borrowed(&entry.pattern[..]) };
        let (compiled, _) = self.patterns.get_or_compile(&pattern, &entry.flags, budget)?;
        let compiled: Arc<CompiledPattern> = match compiled {
            Ok(compiled) => compiled,
            Err(CompileError::Syntax) => return Err(invalid("regex-syntax")),
            Err(CompileError::Unsupported(name)) => return Err(unsupported(name)),
        };
        let replacement = &entry.replacement;
        if !(script::starts_with(replacement, "@@") || !entry.actions.is_empty()) {
            let replaced = self.replace(entry, &compiled, data, replacement)?;
            return self.parse(replaced).map(Outcome::Data);
        }
        if !self.test(entry, &compiled, data)? {
            if directives::repeats_back(entry, chat_index) { return Ok(Outcome::Read(compiled)); }
            return Ok(Outcome::Keep);
        }
        match directives::on_match(entry, chat_index) {
            Directive::Emotion => {
                let name = directives::emotion_name(entry, budget)?;
                let image = match &self.config.emotions {
                    Some(emotions) => emotions.iter().find(|(candidate, _)| candidate.equals(&name))
                        .map(|(_, image)| image.copy(budget)).transpose()?,
                    None => None,
                };
                self.state.emotion_changed |= image.is_some();
                self.report.emotions.push(EmotionEvent { name, image });
                Ok(Outcome::Keep)
            }
            // `chat.message[chatID].data = data` with the rule input, which throws for a message that does not exist;
            // a capture context writes nothing.
            Directive::Inject => {
                let replaced = self.replace(entry, &compiled, data, &[])?;
                if self.config.capture_context { return Ok(Outcome::Data(replaced)); }
                let Ok(index) = usize::try_from(chat_index) else { return Err(invalid("inject-message")) };
                let Some(length) = self.state.messages else { return Err(unsupported("@@inject")) };
                if index >= length { return Err(invalid("inject-message")); }
                let input = data.text(budget)?.copy(budget)?;
                self.state.effects.push(Effect::Message(MessageEdit::SetData { index: index as f64, data: Value::Text(input) }));
                Ok(Outcome::Data(replaced))
            }
            directive @ (Directive::MoveTop | Directive::MoveBottom) => {
                let (found, _held) = self.move_matches(entry, &compiled, data)?;
                let mut current = self.replace(entry, &compiled, data, &[])?.into_units(budget)?;
                for found in &found {
                    let output = directives::move_output(replacement, found);
                    let length = output.len() + current.data.len() + 1;
                    budget.work(length)?;
                    let mut next = Units16::with_capacity(length, budget)?;
                    let (first, second) = if directive == Directive::MoveTop { (&output, &current.data) } else { (&current.data, &output) };
                    next.data.extend_from_slice(first);
                    next.data.push(10);
                    next.data.extend_from_slice(second);
                    current = next;
                }
                Ok(Outcome::Data(Data::from_units16(current)))
            }
            Directive::Replace => {
                let replaced = self.replace(entry, &compiled, data, replacement)?;
                self.parse(replaced).map(Outcome::Data)
            }
        }
    }

    /// The rest of a `@@repeat_back` once the host has read `lastChat`:
    /// ```js
    /// const r = await regex('match', lastChat)
    /// if(!v){ data = data + r[0] }
    /// else if(r[0]){ switch(v){ case 'end': ... case 'start': ... case 'end_nl': ... case 'start_nl': ... } }
    /// ```
    /// A source that is not text, a failed read and a source without a match throw, which keeps the rule input.
    fn repeat_back(&mut self, entry: &PlanEntry, compiled: &CompiledPattern, data: &mut Data, source: Source) -> Result<Option<Data>, StageError> {
        let budget = self.budget;
        let source = match source {
            Source::Text(text) => Units16::of(&text, budget)?,
            Source::Other => return Err(invalid("repeat-back-source")),
            Source::Failed(message) => return Err(StageError::Host(message)),
        };
        compiled.admit(&source.data)?;
        let found = compiled.exec(&source.data, 0, self.config.match_limits, budget)?;
        self.record(entry, regress_engine());
        let Some(groups) = found else { return Err(invalid("repeat-back-match")) };
        let matched = &source.data[groups[0].clone().expect("whole match")];
        // `outScript.split(' ', 2)[1]`, where an empty position reads as none.
        let position = directives::repeat_back_position(entry).filter(|position| !position.is_empty()).map(String::from_utf16_lossy);
        let (at_start, newline) = match position.as_deref() {
            None => (false, false),
            Some(_) if matched.is_empty() => return Ok(None),
            Some("end") => (false, false),
            Some("start") => (true, false),
            Some("end_nl") => (false, true),
            Some("start_nl") => (true, true),
            Some(_) => return Ok(None),
        };
        let current = data.units(budget)?;
        let length = current.len() + matched.len() + usize::from(newline);
        budget.work(length)?;
        let mut next = Units16::with_capacity(length, budget)?;
        let (first, second) = if at_start { (matched, current) } else { (current, matched) };
        next.data.extend_from_slice(first);
        if newline { next.data.push(10); }
        next.data.extend_from_slice(second);
        Ok(Some(Data::from_units16(next)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::budget::{Limits, OwnedVec};
    use risunest_ir::chat::MessageRole;
    use risunest_ir::stage::TextOutput;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicBool;

    const CASES: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/regex/scripts.cases.json");
    const EXPECTED: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/regex/scripts.expected.json");

    fn budget() -> Budget {
        Budget::new(Limits { max_owned_bytes: 1 << 29, max_json_bytes: 1 << 24, max_text_units: 1 << 23, max_values: 1 << 22,
            max_depth: 64, max_work: 1 << 40 }, Arc::new(AtomicBool::new(false))).unwrap()
    }
    fn units(value: &str) -> Vec<u16> { value.encode_utf16().collect() }

    /// CBS is out of scope for these fixtures, so any CBS text fails the run.
    struct NoCbs;
    impl TextProcessor for NoCbs {
        fn process(&mut self, _: &Text, _: &TextContext, _: &Budget) -> Result<TextOutput, StageError> { Err(unsupported("cbs")) }
        fn apply(&mut self, _: &Effect, _: &Budget) -> Result<(), StageError> { Ok(()) }
    }

    fn field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
        value.object()?.iter().find(|(name, _)| name.equals_ascii(key.as_bytes())).map(|(_, value)| value)
    }
    fn text_units(value: &Value) -> Vec<u16> { match value { Value::Text(text) => text.units().collect(), other => panic!("not text: {other:?}") } }
    fn string(value: &Value) -> String { String::from_utf16_lossy(&text_units(value)) }
    fn number(value: &Value) -> f64 { match value { Value::Number(number) => *number, other => panic!("not a number: {other:?}") } }
    fn boolean(value: Option<&Value>) -> bool { matches!(value, Some(Value::Bool(true))) }
    fn list(value: Option<&Value>) -> &[Value] { value.and_then(Value::array).unwrap_or(&[]) }
    fn lossy(text: &Text) -> String { String::from_utf16_lossy(&text.units().collect::<Vec<_>>()) }

    fn sha256(units: &[u16]) -> String {
        let bytes: Vec<u8> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
        Sha256::digest(&bytes).iter().map(|byte| format!("{byte:02x}")).collect()
    }
    /// The oracle's `record`: the text itself up to 4096 units, otherwise its length and digest. Lone surrogates are
    /// compared through `{:?}` of the units so they cannot collapse into U+FFFD.
    fn recorded(units: &[u16]) -> String {
        if units.len() <= 4096 { format!("{units:?}") } else { format!("{}:{}", units.len(), sha256(units)) }
    }
    fn expected_recorded(value: &Value) -> String {
        match value {
            Value::Text(_) => recorded(&text_units(value)),
            _ => format!("{}:{}", number(field(value, "length").unwrap()), string(field(value, "sha256").unwrap())),
        }
    }

    fn input(value: &Value) -> Vec<u16> {
        match value {
            Value::Text(_) => text_units(value),
            _ => text_units(field(value, "repeat").unwrap()).repeat(number(field(value, "times").unwrap()) as usize),
        }
    }

    fn mode(value: &Value, budget: &Budget) -> ScriptMode { script::parse_mode(&Text::from_units(&text_units(value), budget).unwrap()).unwrap() }

    fn reply(value: Option<&Value>, budget: &Budget) -> HostReply {
        let Some(value) = value else { return HostReply::Value(Value::Undefined) };
        if let Some(text) = field(value, "value") { return HostReply::Value(Value::Text(Text::from_units(&text_units(text), budget).unwrap())); }
        if field(value, "null").is_some() { return HostReply::Value(Value::Null); }
        if let Some(message) = field(value, "throw") { return HostReply::Failed { message: Text::from_units(&text_units(message), budget).unwrap() }; }
        HostReply::Value(Value::Undefined)
    }

    fn text(value: &Value, budget: &Budget) -> Text { Text::from_units(&text_units(value), budget).unwrap() }

    /// The case's `luaEffects` as the Lua host reports them.
    fn lua_effect(value: &Value, budget: &Budget) -> Effect {
        let (name, args) = &value.object().unwrap()[0];
        let arg = |index: usize| &args.array().unwrap()[index];
        Effect::Message(match string(&Value::Text(name.copy(budget).unwrap())).as_str() {
            "setChat" => MessageEdit::SetData { index: number(arg(0)), data: Value::Text(text(arg(1), budget)) },
            "addChat" => MessageEdit::Append { role: if string(arg(0)) == "user" { MessageRole::User } else { MessageRole::Char }, data: Value::Text(text(arg(1), budget)),
                chat_id: Text::from_str("lua", budget).unwrap() },
            "removeChat" => MessageEdit::Remove { index: number(args) },
            "cutChat" => MessageEdit::Cut { start: number(arg(0)), end: args.array().unwrap().get(1).map(number) },
            other => panic!("unknown Lua effect {other}"),
        })
    }

    fn lua_reply(run: &Value, budget: &Budget) -> HostReply {
        let reply = reply(field(run, "lua"), budget);
        let Some(effects) = field(run, "luaEffects") else { return reply };
        let HostReply::Value(value) = reply else { panic!("a Lua reply that throws has no writes") };
        HostReply::Lua(LuaReply { value, effects: list(Some(effects)).iter().map(|effect| lua_effect(effect, budget)).collect(), stop_sending: false })
    }

    /// The selected chat's messages as `(role, data)`, which the replay edits the way the owner applies the effects.
    type Messages = Vec<(String, Vec<u16>)>;

    fn initial_messages(case: &Value) -> Messages {
        let chat = field(case, "chat");
        list(chat.and_then(|chat| field(chat, "messages"))).iter()
            .map(|message| (string(field(message, "role").unwrap()), text_units(field(message, "data").unwrap()))).collect()
    }

    fn slice_index(value: f64, length: usize) -> usize {
        if value.is_nan() { 0 } else if value < 0.0 { (length as f64 + value.trunc()).max(0.0) as usize } else { value.trunc().min(length as f64) as usize }
    }

    fn apply_message_effects(messages: &mut Messages, effects: &[Effect]) {
        for effect in effects {
            let Effect::Message(edit) = effect else { panic!("unexpected effect {effect:?}") };
            match edit {
                MessageEdit::SetData { index, data: Value::Text(data) } => {
                    let message = messages.get_mut(*index as usize).expect("an @@inject index the runner checked");
                    message.1 = data.units().collect();
                }
                MessageEdit::Append { role, data: Value::Text(data), .. } => {
                    messages.push((if *role == MessageRole::User { "user" } else { "char" }.into(), data.units().collect()));
                }
                MessageEdit::Remove { index } => {
                    let index = slice_index(*index, messages.len());
                    if index < messages.len() { messages.remove(index); }
                }
                MessageEdit::Cut { start, end } => {
                    let length = messages.len();
                    let (start, end) = (slice_index(*start, length), end.map_or(length, |end| slice_index(end, length)));
                    *messages = if start < end { messages[start..end].to_vec() } else { Vec::new() };
                }
                other => panic!("unexpected message edit {other:?}"),
            }
        }
    }

    fn recorded_messages(messages: &Messages) -> Vec<String> {
        messages.iter().map(|(role, data)| format!("{role}|{}", recorded(data))).collect()
    }

    fn expected_messages(result: &Value) -> Vec<String> {
        list(field(result, "messages")).iter()
            .map(|message| format!("{}|{}", string(field(message, "role").unwrap()), expected_recorded(field(message, "data").unwrap()))).collect()
    }

    /// The reply to the `n`th conversation read of a run, from what TS found there.
    fn read_reply(result: Option<&Value>, n: usize, budget: &Budget) -> HostReply {
        let Some(read) = result.and_then(|result| list(field(result, "reads")).get(n)) else {
            return HostReply::Unsupported(Cow::Borrowed("oracle-read-tape-exhausted"));
        };
        if let Some(source) = field(read, "source") { return HostReply::Value(Value::Text(text(source, budget))); }
        if let Some(message) = field(read, "error") { return HostReply::Failed { message: text(message, budget) }; }
        HostReply::Value(Value::Undefined)
    }

    type Emotions = Option<Vec<(String, String, f64)>>;

    /// The `CharEmotion` update `processScriptFull` makes for one matching `@@emo` script, with `Date.now()` at 1000.
    fn apply_emotion(state: &mut Emotions, event: &EmotionEvent) {
        let existed = state.is_some();
        let mut entries = state.clone().unwrap_or_default();
        if entries.len() > 4 { entries.remove(0); }
        match &event.image {
            Some(image) => {
                entries.push((lossy(&event.name), lossy(image), 1000.0));
                *state = Some(entries);
            }
            // The splice changed the stored array in place.
            None if existed => *state = Some(entries),
            None => {}
        }
    }

    fn expected_emotions(value: Option<&Value>) -> Emotions {
        match value {
            None | Some(Value::Null) => None,
            Some(value) => Some(list(Some(value)).iter().map(|entry| {
                let entry = entry.array().unwrap();
                (string(&entry[0]), string(&entry[1]), number(&entry[2]))
            }).collect()),
        }
    }

    struct Observed { outcome: Result<ScriptOutput, StageError>, lua: Vec<String>, hooks: Vec<String>, reads: Vec<i64> }

    fn drive(runner: &mut Runner<NoCbs>, run: &Value, result: Option<&Value>, input: Vec<u16>, budget: &Budget) -> Observed {
        let mode = mode(field(run, "mode").unwrap(), budget);
        let chat_index = field(run, "chatIndex").map_or(-1, |value| number(value) as i64);
        let role = field(run, "role").map(|role| Text::from_units(&text_units(role), budget).unwrap());
        let request = ScriptRequest { mode, input: Text::from_units(&input, budget).unwrap(), chat_index,
            conditions: CbsConditions { first_message: None, chat_role: role } };
        let (mut lua, mut hooks, mut reads) = (Vec::new(), Vec::new(), Vec::new());
        let mut hook_index: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut step = runner.run(request, budget);
        let outcome = loop {
            match step {
                Ok(Step::Done(output)) => break Ok(output),
                Err(error) => break Err(error),
                Ok(Step::NeedsHost(HostRequest::Lua { trigger: LuaTrigger::Edit { index }, mode: LuaMode::Script(mode), payload: Value::Text(payload) })) => {
                    let index = index.map_or("undefined".into(), |index| index.to_string());
                    lua.push(format!("{}|{}|{index}", mode_name(mode), recorded(&payload.units().collect::<Vec<_>>())));
                    step = runner.resume(lua_reply(run, budget), budget);
                }
                Ok(Step::NeedsHost(HostRequest::PluginHook { kind: PluginHookKind::Script(mode), payload: Value::Text(payload) })) => {
                    let index = hook_index.entry(mode_name(mode)).or_default();
                    hooks.push(format!("{}|{}|{}", mode_name(mode), *index, recorded(&payload.units().collect::<Vec<_>>())));
                    *index += 1;
                    let replies = list(field(run, "hooks"));
                    step = runner.resume(reply(replies.get(hooks.len() - 1), budget), budget);
                }
                Ok(Step::NeedsHost(HostRequest::Conversation(ConversationRead::RepeatBackSource { chat_index }))) => {
                    reads.push(chat_index);
                    step = runner.resume(read_reply(result, reads.len() - 1, budget), budget);
                }
                Ok(Step::NeedsHost(other)) => panic!("unexpected host request {other:?}"),
            }
        };
        Observed { outcome, lua, hooks, reads }
    }

    fn config(case: &Value, budget: &Budget) -> ScriptRunnerConfig {
        let scripts = list(field(case, "preset")).iter().chain(list(field(case, "scripts")))
            .map(|value| CustomScript::from_value(value, budget).unwrap()).collect();
        let mut config = ScriptRunnerConfig::new(scripts);
        config.capture_context = boolean(field(case, "capture"));
        config.lua_listeners = boolean(field(case, "lua"));
        for (name, count) in field(case, "hooks").and_then(Value::object).unwrap_or(&[]) {
            let count = number(count) as usize;
            match mode(&Value::Text(name.copy(budget).unwrap()), budget) {
                ScriptMode::EditInput => config.plugin_hooks.edit_input = count,
                ScriptMode::EditOutput => config.plugin_hooks.edit_output = count,
                ScriptMode::EditProcess => config.plugin_hooks.edit_process = count,
                ScriptMode::EditDisplay => config.plugin_hooks.edit_display = count,
            }
        }
        config.emotions = match field(case, "emotions") {
            Some(Value::Null) => None,
            value => Some(list(value).iter().map(|pair| {
                let pair = pair.array().unwrap();
                (Text::from_units(&text_units(&pair[0]), budget).unwrap(), Text::from_units(&text_units(&pair[1]), budget).unwrap())
            }).collect()),
        };
        config.result_cache = field(case, "cache").is_none();
        config
    }

    struct Tally { matched_cases: usize, matched_runs: usize, unsupported: BTreeMap<String, usize>, mismatches: Vec<String> }

    fn replay_run(runner: &mut Runner<NoCbs>, run: &Value, result: &Value, emotions: &mut Emotions, messages: &mut Messages, label: &str,
        tally: &mut Tally) -> bool {
        let budget = budget();
        assert!(runner.set_message_count(Some(messages.len())));
        if let Some(fill) = field(run, "fill") {
            let base = input(field(run, "input").unwrap());
            let (mut outputs, mut changed) = (Vec::new(), 0.0);
            for index in 0..number(fill) as usize {
                let observed = drive(runner, run, None, [base.clone(), units(&index.to_string())].concat(), &budget);
                let output = observed.outcome.unwrap_or_else(|error| panic!("{label}: fill run failed: {error:?}"));
                for event in &runner.report().emotions { apply_emotion(emotions, event); }
                let data: Vec<u16> = output.text.units().collect();
                outputs.push(if data.len() <= 4096 { String::from_utf16_lossy(&data) } else { sha256(&data) });
                if output.emotion_changed { changed += 1.0; }
            }
            let actual = (outputs.join("\u{1}"), changed);
            let wanted = (string(field(result, "filled").unwrap()), number(field(result, "emoChangedCount").unwrap()));
            if actual == wanted { tally.matched_runs += 1; return true; }
            tally.mismatches.push(format!("{label}: fill {actual:?} != {wanted:?}"));
            return false;
        }
        let observed = drive(runner, run, Some(result), input(field(run, "input").unwrap()), &budget);
        if let Some(name) = field(run, "unsupported") {
            let name = string(name);
            match &observed.outcome {
                Err(StageError::Unsupported(actual)) if *actual == name => *tally.unsupported.entry(name).or_default() += 1,
                other => tally.mismatches.push(format!("{label}: expected Unsupported({name}), got {:?}", other.as_ref().map(|output| lossy(&output.text)))),
            }
            return false;
        }
        for event in &runner.report().emotions { apply_emotion(emotions, event); }
        if let Ok(output) = &observed.outcome { apply_message_effects(messages, &output.effects); }
        let actual = match &observed.outcome {
            Ok(output) => format!("data={} emo={}", recorded(&output.text.units().collect::<Vec<_>>()), output.emotion_changed),
            // The report keeps the same message as the error.
            Err(StageError::Host(message)) if runner.report().host_failure.as_ref().is_some_and(|failure| failure.units().eq(message.iter().copied())) =>
                format!("error={}", String::from_utf16_lossy(message)),
            Err(error) => format!("stage-error={error:?}"),
        };
        let wanted = match field(result, "error") {
            Some(message) => format!("error={}", string(message)),
            None => format!("data={} emo={}", expected_recorded(field(result, "data").unwrap()), boolean(field(result, "emoChanged"))),
        };
        let wanted_lua: Vec<String> = list(field(result, "lua")).iter().map(|call| {
            let index = field(call, "index").map_or("undefined".into(), |index| (number(index) as i64).to_string());
            format!("{}|{}|{index}", string(field(call, "mode").unwrap()), expected_recorded(field(call, "data").unwrap()))
        }).collect();
        let wanted_hooks: Vec<String> = list(field(result, "hooks")).iter().map(|call| format!("{}|{}|{}", string(field(call, "mode").unwrap()),
            number(field(call, "hook").unwrap()), expected_recorded(field(call, "data").unwrap()))).collect();
        let wanted_emotions = expected_emotions(field(result, "emotions"));
        let mut problems = Vec::new();
        if actual != wanted { problems.push(format!("{actual} != {wanted}")); }
        if observed.lua != wanted_lua { problems.push(format!("lua {:?} != {wanted_lua:?}", observed.lua)); }
        if observed.hooks != wanted_hooks { problems.push(format!("hooks {:?} != {wanted_hooks:?}", observed.hooks)); }
        if *emotions != wanted_emotions { problems.push(format!("emotions {emotions:?} != {wanted_emotions:?}")); }
        let (actual_messages, wanted_messages) = (recorded_messages(messages), expected_messages(result));
        if actual_messages != wanted_messages { problems.push(format!("messages {actual_messages:?} != {wanted_messages:?}")); }
        let wanted_reads = list(field(result, "reads")).len();
        if observed.reads.len() != wanted_reads { problems.push(format!("reads {:?} != {wanted_reads} reads", observed.reads)); }
        if problems.is_empty() { tally.matched_runs += 1; return true; }
        tally.mismatches.push(format!("{label}: {}", problems.join("; ")));
        false
    }

    #[test]
    fn replays_process_script_full_oracle() {
        let setup = budget();
        let digest: String = Sha256::digest(CASES).iter().map(|byte| format!("{byte:02x}")).collect();
        let cases = risunest_ir::parse_json(std::str::from_utf8(CASES).unwrap(), &setup).unwrap();
        let expected = risunest_ir::parse_json(std::str::from_utf8(EXPECTED).unwrap(), &setup).unwrap();
        assert_eq!(string(field(&expected, "fixtureSha256").unwrap()), digest, "regenerate the expected file");
        let cases = list(field(&cases, "cases"));
        let expected = list(field(&expected, "cases"));
        assert_eq!(cases.len(), expected.len());
        let mut tally = Tally { matched_cases: 0, matched_runs: 0, unsupported: BTreeMap::new(), mismatches: Vec::new() };
        for (case, expected) in cases.iter().zip(expected) {
            let id = string(field(case, "id").unwrap());
            assert_eq!(string(field(expected, "id").unwrap()), id);
            let limits = if field(case, "profile").is_some() { ResultLimits::LOW_SPEC } else { ResultLimits::NORMAL };
            let mut runner = Runner::new(config(case, &setup), NoCbs, limits);
            let mut emotions = expected_emotions(field(case, "priorEmotions"));
            let mut messages = initial_messages(case);
            let (runs, results) = (list(field(case, "runs")), list(field(expected, "result")));
            assert_eq!(runs.len(), results.len(), "{id}");
            let mut case_ok = true;
            for (index, (run, result)) in runs.iter().zip(results).enumerate() {
                if field(run, "unsupported").is_some() { assert_eq!(index + 1, runs.len(), "{id}: an unsupported run must end its case"); }
                case_ok &= replay_run(&mut runner, run, result, &mut emotions, &mut messages, &format!("{id}#{index}"), &mut tally);
            }
            if case_ok { tally.matched_cases += 1; }
        }
        println!("regex oracle: {} cases, {} fully matched, {} runs matched, unsupported {:?}, {} mismatches",
            cases.len(), tally.matched_cases, tally.matched_runs, tally.unsupported, tally.mismatches.len());
        assert!(tally.mismatches.is_empty(), "{} mismatches:\n{}", tally.mismatches.len(), tally.mismatches.iter().take(40).cloned().collect::<Vec<_>>().join("\n"));
        assert!(tally.matched_cases >= 400, "{} matched cases", tally.matched_cases);
    }

    fn plain(pattern: &str, out: &str, mode: ScriptMode, flag: &str, budget: &Budget) -> CustomScript {
        let text = |value: &str| Text::from_str(value, budget).unwrap();
        CustomScript::new(text(pattern), text(out), Some(mode), text(flag), !flag.is_empty())
    }

    fn request(mode: ScriptMode, input: &str, budget: &Budget) -> ScriptRequest {
        ScriptRequest { mode, input: Text::from_str(input, budget).unwrap(), chat_index: -1, conditions: CbsConditions::default() }
    }

    fn finish(step: Result<Step<ScriptOutput>, StageError>) -> ScriptOutput {
        match step { Ok(Step::Done(output)) => output, other => panic!("not done: {other:?}") }
    }

    #[test]
    fn reports_the_engine_of_every_stage() {
        let budget = budget();
        let scripts = vec![plain("ab", "x", ScriptMode::EditOutput, "", &budget), plain("(?<=x)c", "y", ScriptMode::EditOutput, "", &budget)];
        let mut config = ScriptRunnerConfig::new(scripts);
        config.lua_listeners = true;
        config.plugin_hooks.edit_output = 1;
        let mut runner = Runner::new(config, NoCbs, ResultLimits::NORMAL);
        assert!(matches!(runner.run(request(ScriptMode::EditOutput, "abc", &budget), &budget), Ok(Step::NeedsHost(HostRequest::Lua { .. }))));
        assert!(matches!(runner.resume(HostReply::Value(Value::Undefined), &budget), Ok(Step::NeedsHost(HostRequest::PluginHook { .. }))));
        assert_eq!(lossy(&finish(runner.resume(HostReply::Value(Value::Null), &budget)).text), "xy");
        let engines: Vec<_> = runner.report().engines.iter().map(|used| (used.stage.clone(), used.engine.clone())).collect();
        assert_eq!(engines, [
            (EngineStage::LuaEdit, EngineClass::Host(HostKind::Lua)),
            (EngineStage::PluginHook(0), EngineClass::Host(HostKind::PluginHook)),
            (EngineStage::Script(0), EngineClass::Rust),
            (EngineStage::Script(1), regress_engine()),
        ]);
        // Ill-formed input keeps even a safe pattern on the general engine.
        let mut runner = Runner::new(ScriptRunnerConfig::new(vec![plain("ab", "x", ScriptMode::EditOutput, "", &budget)]), NoCbs, ResultLimits::NORMAL);
        let input = Text::from_units(&[0xd800, 97, 98], &budget).unwrap();
        let output = finish(runner.run(ScriptRequest { input, ..request(ScriptMode::EditOutput, "", &budget) }, &budget));
        assert_eq!(output.text.units().collect::<Vec<_>>(), [0xd800, 120]);
        assert_eq!(runner.report().engines[0].engine, regress_engine());
    }

    /// Replaces `{{x}}` with `X` and reports one variable write per call.
    struct MarkCbs { calls: usize }
    impl TextProcessor for MarkCbs {
        fn process(&mut self, input: &Text, context: &TextContext, budget: &Budget) -> Result<TextOutput, StageError> {
            self.calls += 1;
            let role = context.role.as_ref().map_or(String::from("none"), lossy);
            let text = Text::from_str(&lossy(input).replace("{{x}}", "X"), budget)?;
            let name = Text::from_str(&format!("call{}-{role}", self.calls), budget)?;
            Ok(TextOutput { text, effects: vec![Effect::ChatVariable { name, value: Value::Null }] })
        }
        fn apply(&mut self, _: &Effect, _: &Budget) -> Result<(), StageError> { Ok(()) }
    }

    #[test]
    fn forwards_cbs_effects_and_skips_cbs_free_text() {
        let budget = budget();
        let mut config = ScriptRunnerConfig::new(vec![plain("a", "{{x}}", ScriptMode::EditDisplay, "", &budget),
            plain("X", "b", ScriptMode::EditDisplay, "", &budget), plain("{{x}}", "c", ScriptMode::EditDisplay, "g<cbs>", &budget)]);
        config.capture_context = true;
        let mut runner = Runner::new(config, MarkCbs { calls: 0 }, ResultLimits::NORMAL);
        let mut run = request(ScriptMode::EditDisplay, "{{x}}a", &budget);
        run.conditions.chat_role = Some(Text::from_str("user", &budget).unwrap());
        let output = finish(runner.run(run, &budget));
        // The input, the `<cbs>` pattern for the cache key, the first replacement and the pattern again before its search.
        assert_eq!(lossy(&output.text), "bb");
        let names: Vec<String> = output.effects.iter().map(|effect| match effect { Effect::ChatVariable { name, .. } => lossy(name), _ => unreachable!() }).collect();
        assert_eq!(names, ["call1-user", "call2-user", "call3-user", "call4-user"]);
    }

    #[test]
    fn leaves_other_stages_to_the_owner() {
        let budget = budget();
        let scripts = || vec![plain("a", "b", ScriptMode::EditOutput, "", &budget), plain("a", "b", ScriptMode::EditDisplay, "", &budget)];
        let mut config = ScriptRunnerConfig::new(scripts());
        config.dynamic_assets = true;
        config.lua_listeners = true;
        config.plugin_hooks.edit_display = 2;
        let mut runner = Runner::new(config, NoCbs, ResultLimits::NORMAL);
        assert_eq!(runner.run(request(ScriptMode::EditDisplay, "a", &budget), &budget).unwrap_err(), unsupported("editdisplay-live"));
        assert!(matches!(runner.resume(HostReply::Cancelled, &budget), Err(StageError::Invalid(name)) if name == "script-runner-resume"));
        // Dynamic assets decline before the Lua listeners run.
        assert_eq!(runner.run(request(ScriptMode::EditOutput, "a", &budget), &budget).unwrap_err(), unsupported("dynamicAssets"));
        assert!(runner.report().engines.is_empty());
        assert!(matches!(runner.run(request(ScriptMode::EditInput, "a", &budget), &budget), Ok(Step::NeedsHost(_))));
        assert!(matches!(runner.run(request(ScriptMode::EditInput, "a", &budget), &budget), Err(StageError::Invalid(name)) if name == "script-runner-busy"));
        assert_eq!(lossy(&finish(runner.resume(HostReply::Value(Value::Undefined), &budget)).text), "a");
        assert_eq!(lossy(&finish(runner.run(request(ScriptMode::EditProcess, "a", &budget), &budget)).text), "a");
        // A display recovery run skips the host callbacks and every script.
        let mut config = ScriptRunnerConfig::new(scripts());
        config.display_recovery = true;
        config.lua_listeners = true;
        config.plugin_hooks.edit_display = 2;
        let mut runner = Runner::new(config, NoCbs, ResultLimits::NORMAL);
        assert_eq!(lossy(&finish(runner.run(request(ScriptMode::EditDisplay, "a", &budget), &budget)).text), "a");
        assert!(runner.report().engines.is_empty());
    }

    #[test]
    fn declines_come_before_the_first_host_request() {
        let budget = budget();
        let at = |input: &str, chat_index: i64| ScriptRequest { mode: ScriptMode::EditOutput, input: Text::from_str(input, &budget).unwrap(),
            chat_index, conditions: CbsConditions::default() };
        let one = |pattern: &str, out: &str, flag: &str| vec![plain(pattern, out, ScriptMode::EditOutput, flag, &budget)];
        let order = || (0..64).map(|index| plain("$", "x", ScriptMode::EditOutput, if index == 3 { "g<order x>" } else { "g" }, &budget)).collect::<Vec<_>>();
        // `@@inject` declines when the configuration does not give the message count.
        let cases: [(&dyn Fn() -> Vec<CustomScript>, i64, &str); 3] = [
            (&|| one("b", "@@inject", ""), 0, "@@inject"), (&|| one("a", "X", "gv"), -1, "unicode_sets"),
            (&order, -1, "script-order-nan")];
        for (scripts, chat_index, name) in cases {
            for (lua, hooks) in [(true, 0), (false, 1)] {
                let mut config = ScriptRunnerConfig::new(scripts());
                config.lua_listeners = lua;
                config.plugin_hooks.edit_output = hooks;
                let mut runner = Runner::new(config, NoCbs, ResultLimits::NORMAL);
                assert_eq!(runner.run(at("abc", chat_index), &budget).unwrap_err(), unsupported(name), "{name}");
                assert!(runner.report().engines.is_empty(), "{name}");
            }
        }
        // A run that yields nothing declines only where TS would need another stage.
        let scripts = vec![plain("a", "X", ScriptMode::EditOutput, "gi", &budget), plain("z", "@@inject", ScriptMode::EditOutput, "", &budget)];
        let mut config = ScriptRunnerConfig::new(scripts);
        config.message_count = Some(1);
        let mut runner = Runner::new(config, NoCbs, ResultLimits::NORMAL);
        assert_eq!(lossy(&finish(runner.run(at("abc", 0), &budget)).text), "Xbc");
        // Chat index -1 replaces `@@inject` literally and needs no message count.
        assert!(runner.set_message_count(None));
        assert_eq!(lossy(&finish(runner.run(at("abc", -1), &budget)).text), "Xbc");
    }

    #[test]
    fn a_decline_after_a_served_host_request_fails_the_run() {
        let budget = budget();
        let runner = || {
            // The `<cbs>` pattern needs CBS, which `NoCbs` declines.
            let mut config = ScriptRunnerConfig::new(vec![plain("{{x}}", "Y", ScriptMode::EditOutput, "g<cbs>", &budget)]);
            config.lua_listeners = true;
            config.plugin_hooks.edit_output = 1;
            let mut runner = Runner::new(config, NoCbs, ResultLimits::NORMAL);
            assert!(matches!(runner.run(request(ScriptMode::EditOutput, "a", &budget), &budget), Ok(Step::NeedsHost(HostRequest::Lua { .. }))));
            runner
        };
        let after = |name: &str| StageError::Invalid(Cow::Owned(format!("{name}-after-host")));
        // A host that cannot serve the first request leaves the call to TS.
        assert_eq!(runner().resume(HostReply::Unsupported(Cow::Borrowed("lua-x")), &budget).unwrap_err(), unsupported("lua-x"));
        let mut served = runner();
        assert!(matches!(served.resume(HostReply::Value(Value::Undefined), &budget), Ok(Step::NeedsHost(HostRequest::PluginHook { .. }))));
        assert_eq!(served.resume(HostReply::Unsupported(Cow::Borrowed("hook-x")), &budget).unwrap_err(), after("hook-x"));
        let mut served = runner();
        assert!(matches!(served.resume(HostReply::Failed { message: Text::from_str("e", &budget).unwrap() }, &budget), Ok(Step::NeedsHost(_))));
        assert_eq!(served.resume(HostReply::Value(Value::Undefined), &budget).unwrap_err(), after("cbs"));
    }

    fn describe(effect: &Effect) -> String {
        match effect {
            Effect::Message(MessageEdit::SetData { index, data: Value::Text(data) }) => format!("set {index} {}", lossy(data)),
            Effect::Message(MessageEdit::Replace { messages }) => format!("replace {:?}", messages.array().map(<[Value]>::len)),
            Effect::Message(MessageEdit::Cut { start, end }) => format!("cut {start} {end:?}"),
            Effect::ChatVariable { name, .. } => format!("var {}", lossy(name)),
            other => format!("{other:?}"),
        }
    }

    fn set(index: f64, data: &str, budget: &Budget) -> Effect { Effect::Message(MessageEdit::SetData { index, data: Value::Text(Text::from_str(data, budget).unwrap()) }) }

    /// A Lua reply that keeps the content, makes `effects` and stops the send.
    fn lua_writes(effects: Vec<Effect>) -> HostReply { HostReply::Lua(LuaReply { value: Value::Undefined, effects, stop_sending: true }) }

    /// [`MarkCbs`] that keeps every write it is given and refuses the message edits when `refuse` is set.
    struct ApplyCbs { mark: MarkCbs, applied: Vec<String>, refuse: bool }
    impl TextProcessor for ApplyCbs {
        fn process(&mut self, input: &Text, context: &TextContext, budget: &Budget) -> Result<TextOutput, StageError> {
            self.mark.process(input, context, budget)
        }
        fn apply(&mut self, effect: &Effect, _: &Budget) -> Result<(), StageError> {
            self.applied.push(describe(effect));
            if self.refuse && matches!(effect, Effect::Message(_)) { return Err(unsupported("effect")); }
            Ok(())
        }
    }

    #[test]
    fn inject_counts_the_lua_writes() {
        let budget = budget();
        let runner = |count: usize| {
            let mut config = ScriptRunnerConfig::new(vec![plain("b", "@@inject", ScriptMode::EditOutput, "", &budget)]);
            config.lua_listeners = true;
            config.message_count = Some(count);
            let mut runner = Runner::new(config, NoCbs, ResultLimits::NORMAL);
            let run = ScriptRequest { chat_index: 1, ..request(ScriptMode::EditOutput, "abc", &budget) };
            assert!(matches!(runner.run(run, &budget), Ok(Step::NeedsHost(HostRequest::Lua { .. }))));
            runner
        };
        let array = |length: usize| {
            let mut values = OwnedVec::new(&budget).unwrap();
            for _ in 0..length { values.push(Value::Null).unwrap(); }
            Value::Array(values)
        };
        // A full replacement with two messages makes index 1 exist.
        let output = finish(runner(1).resume(lua_writes(vec![Effect::Message(MessageEdit::Replace { messages: array(2) })]), &budget));
        assert_eq!(lossy(&output.text), "ac");
        assert_eq!(output.effects.iter().map(describe).collect::<Vec<_>>(), ["replace Some(2)", "set 1 abc"]);
        // A cut down to one message leaves index 1 past the end, which keeps the rule input.
        let mut cut = runner(2);
        let output = finish(cut.resume(lua_writes(vec![Effect::Message(MessageEdit::Cut { start: 1.0, end: None })]), &budget));
        assert_eq!(lossy(&output.text), "abc");
        assert_eq!(output.effects.iter().map(describe).collect::<Vec<_>>(), ["cut 1 None"]);
        assert_eq!(skipped(&cut), [(0, invalid("inject-message"))]);
        // After a write the runner cannot count, an `@@inject` fails the run: TS has already run the listeners.
        let unknown = vec![Effect::Message(MessageEdit::Replace { messages: Value::Null })];
        assert_eq!(runner(1).resume(lua_writes(unknown), &budget).unwrap_err(), StageError::Invalid(Cow::Borrowed("@@inject-after-host")));
    }

    #[test]
    fn lua_writes_reach_the_processor_and_lead_the_output() {
        let budget = budget();
        let runner = |scripts: Vec<CustomScript>, refuse: bool| {
            let mut config = ScriptRunnerConfig::new(scripts);
            config.lua_listeners = true;
            config.message_count = Some(1);
            let mut runner = Runner::new(config, ApplyCbs { mark: MarkCbs { calls: 0 }, applied: Vec::new(), refuse }, ResultLimits::NORMAL);
            let run = ScriptRequest { chat_index: 0, ..request(ScriptMode::EditOutput, "ab", &budget) };
            assert!(matches!(runner.run(run, &budget), Ok(Step::NeedsHost(HostRequest::Lua { .. }))));
            runner
        };
        let scripts = || vec![plain("a", "{{x}}", ScriptMode::EditOutput, "", &budget), plain("b", "@@inject", ScriptMode::EditOutput, "", &budget)];
        let variable = || Effect::ChatVariable { name: Text::from_str("lua", &budget).unwrap(), value: Value::Null };
        // The listeners' writes come first, then the CBS writes and the `@@inject` edits in rule order; a stop is ignored.
        let mut cbs = runner(scripts(), false);
        let output = finish(cbs.resume(lua_writes(vec![set(0.0, "L", &budget), variable()]), &budget));
        assert_eq!(lossy(&output.text), "X");
        assert_eq!(output.effects.iter().map(describe).collect::<Vec<_>>(), ["set 0 L", "var lua", "var call1-none", "set 0 Xb"]);
        assert_eq!(cbs.processor().applied, ["set 0 L", "var lua"]);
        // A write the processor refuses fails the run only where CBS would read after it.
        let mut cbs_free = runner(vec![plain("b", "@@inject", ScriptMode::EditOutput, "", &budget)], true);
        let output = finish(cbs_free.resume(lua_writes(vec![set(0.0, "L", &budget), variable()]), &budget));
        assert_eq!(lossy(&output.text), "a");
        assert_eq!(output.effects.iter().map(describe).collect::<Vec<_>>(), ["set 0 L", "var lua", "set 0 ab"]);
        let mut refused = runner(scripts(), true);
        assert_eq!(refused.resume(lua_writes(vec![set(0.0, "L", &budget)]), &budget).unwrap_err(),
            StageError::Invalid(Cow::Borrowed("effect-after-host")));
        assert_eq!(refused.processor().mark.calls, 0);
    }

    #[test]
    fn repeat_back_reads_the_source_from_the_host() {
        let budget = budget();
        let runner = |lua: bool, hooks: usize, scripts: Vec<CustomScript>| {
            let mut config = ScriptRunnerConfig::new(scripts);
            config.lua_listeners = lua;
            config.plugin_hooks.edit_output = hooks;
            Runner::new(config, NoCbs, ResultLimits::NORMAL)
        };
        let scripts = || vec![plain("q\\d", "@@repeat_back end", ScriptMode::EditOutput, "", &budget), plain("c", "C", ScriptMode::EditOutput, "", &budget)];
        let at = || ScriptRequest { chat_index: 3, ..request(ScriptMode::EditOutput, "abc", &budget) };
        let is_read = |step: Result<Step<ScriptOutput>, StageError>| {
            matches!(step, Ok(Step::NeedsHost(HostRequest::Conversation(ConversationRead::RepeatBackSource { chat_index: 3 }))))
        };
        let source = |value: &str| HostReply::Value(Value::Text(Text::from_str(value, &budget).unwrap()));
        // The read comes after the Lua listeners and the hooks, and holds the runner until the host answers.
        let mut waiting = runner(true, 1, scripts());
        assert!(matches!(waiting.run(at(), &budget), Ok(Step::NeedsHost(HostRequest::Lua { .. }))));
        assert!(matches!(waiting.resume(HostReply::Value(Value::Undefined), &budget), Ok(Step::NeedsHost(HostRequest::PluginHook { .. }))));
        assert!(is_read(waiting.resume(HostReply::Value(Value::Undefined), &budget)));
        assert!(waiting.config_mut().is_none());
        assert!(!waiting.set_message_count(Some(1)));
        assert_eq!(waiting.run(at(), &budget).unwrap_err(), invalid("script-runner-busy"));
        assert_eq!(lossy(&finish(waiting.resume(source("x q5 q6"), &budget)).text), "abCq5");
        let read = EngineUse { stage: EngineStage::Script(0), engine: EngineClass::Host(HostKind::Conversation) };
        assert!(waiting.report().engines.contains(&read));
        // A source that is not text, a failed read and a source without a match keep the rule input.
        for (reply, error) in [(HostReply::Value(Value::Undefined), invalid("repeat-back-source")),
            (HostReply::Failed { message: Text::from_str("gone", &budget).unwrap() }, StageError::Host(units("gone"))),
            (source("none"), invalid("repeat-back-match"))] {
            let mut failed = runner(false, 0, scripts());
            assert!(is_read(failed.run(at(), &budget)));
            assert_eq!(lossy(&finish(failed.resume(reply, &budget)).text), "abC");
            assert_eq!(skipped(&failed), [(0, error)]);
        }
        // An unsupported first read is a decline; after a served request it fails the run, as a later decline does.
        let mut first = runner(false, 0, scripts());
        assert!(is_read(first.run(at(), &budget)));
        assert_eq!(first.resume(HostReply::Unsupported(Cow::Borrowed("read-x")), &budget).unwrap_err(), unsupported("read-x"));
        let mut served = runner(true, 0, scripts());
        assert!(matches!(served.run(at(), &budget), Ok(Step::NeedsHost(HostRequest::Lua { .. }))));
        assert!(is_read(served.resume(HostReply::Value(Value::Undefined), &budget)));
        assert_eq!(served.resume(HostReply::Unsupported(Cow::Borrowed("read-x")), &budget).unwrap_err(),
            StageError::Invalid(Cow::Borrowed("read-x-after-host")));
        let mut later = runner(false, 0, vec![plain("q\\d", "@@repeat_back end", ScriptMode::EditOutput, "", &budget),
            plain("c", "{{x}}", ScriptMode::EditOutput, "", &budget)]);
        assert!(is_read(later.run(at(), &budget)));
        assert_eq!(later.resume(source("q1"), &budget).unwrap_err(), StageError::Invalid(Cow::Borrowed("cbs-after-host")));
        let mut wrong = runner(false, 0, scripts());
        assert!(is_read(wrong.run(at(), &budget)));
        assert_eq!(wrong.resume(HostReply::Tokens(Vec::new()), &budget).unwrap_err(), invalid("repeat-back-reply"));
        let mut cancelled = runner(false, 0, scripts());
        assert!(is_read(cancelled.run(at(), &budget)));
        assert_eq!(cancelled.resume(HostReply::Cancelled, &budget).unwrap_err(), StageError::Cancelled);
    }

    #[test]
    fn runner_is_send() {
        fn send<T: Send>() {}
        send::<Runner<NoCbs>>();
    }

    fn budget_with(max_work: usize, max_owned_bytes: usize, cancel: Arc<AtomicBool>) -> Budget {
        Budget::new(Limits { max_owned_bytes, max_json_bytes: 1 << 24, max_text_units: 1 << 23, max_values: 1 << 22, max_depth: 64, max_work },
            cancel).unwrap()
    }

    /// `(?:a+)+$` backtracks exponentially on this input.
    fn catastrophic() -> String { format!("{}!", "a".repeat(40)) }

    fn skipped(runner: &Runner<impl TextProcessor>) -> Vec<(usize, StageError)> {
        runner.report().skipped.iter().map(|skip| { assert_eq!(skip.mode, ScriptMode::EditOutput); (skip.rule, skip.error.clone()) }).collect()
    }

    #[test]
    fn a_search_limit_skips_its_rule_and_later_rules_run() {
        let budget = budget();
        let scripts = vec![plain("(?:a+)+$", "X", ScriptMode::EditOutput, "", &budget), plain("!", "?", ScriptMode::EditOutput, "", &budget),
            plain("(a|b)*c", "Y", ScriptMode::EditOutput, "", &budget)];
        let mut config = ScriptRunnerConfig::new(scripts);
        // Allocation is charged cumulatively as the backtrack stack grows; the first rule stays shallow and runs out of work.
        config.match_limits = MatchLimits { work_units: 100_000, allocation_bytes: 1 << 20, lookaround_depth: 16 };
        let mut runner = Runner::new(config, NoCbs, ResultLimits::NORMAL);
        let input = format!("{}{}", catastrophic(), "ab".repeat(2000));
        let output = finish(runner.run(request(ScriptMode::EditOutput, &input, &budget), &budget));
        assert_eq!(lossy(&output.text), input.replace('!', "?"));
        assert_eq!(skipped(&runner), [(0, StageError::Limit(Cow::Borrowed("regex-work"))), (2, StageError::Limit(Cow::Borrowed("regex-allocation")))]);
    }

    /// Replaces `{{x}}` with `X`, failing on the listed calls.
    struct FailingCbs { calls: usize, fail: Vec<(usize, StageError)> }
    impl TextProcessor for FailingCbs {
        fn process(&mut self, input: &Text, _: &TextContext, budget: &Budget) -> Result<TextOutput, StageError> {
            self.calls += 1;
            if let Some((_, error)) = self.fail.iter().find(|(call, _)| *call == self.calls) { return Err(error.clone()); }
            Ok(TextOutput { text: Text::from_str(&lossy(input).replace("{{x}}", "X"), budget)?, effects: Vec::new() })
        }
        fn apply(&mut self, _: &Effect, _: &Budget) -> Result<(), StageError> { Ok(()) }
    }

    #[test]
    fn a_cbs_failure_inside_a_rule_keeps_the_rule_input() {
        let budget = budget();
        let scripts = vec![plain("a", "{{x}}", ScriptMode::EditOutput, "", &budget), plain("{{x}}", "Y", ScriptMode::EditOutput, "g<cbs>", &budget),
            plain("b", "B", ScriptMode::EditOutput, "", &budget)];
        // The cache key parses the `<cbs>` pattern first; the replacement and the pattern before its search fail.
        let host = StageError::Host(units("boom"));
        let fail = vec![(2, host.clone()), (3, StageError::Invalid(Cow::Borrowed("cbs-test")))];
        let mut runner = Runner::new(ScriptRunnerConfig::new(scripts), FailingCbs { calls: 0, fail }, ResultLimits::NORMAL);
        assert_eq!(lossy(&finish(runner.run(request(ScriptMode::EditOutput, "ab", &budget), &budget)).text), "aB");
        assert_eq!(skipped(&runner), [(0, host), (1, StageError::Invalid(Cow::Borrowed("cbs-test")))]);
        // A decline from CBS still ends the run.
        let scripts = vec![plain("a", "{{x}}", ScriptMode::EditOutput, "", &budget)];
        let fail = vec![(1, unsupported("cbs"))];
        let mut runner = Runner::new(ScriptRunnerConfig::new(scripts), FailingCbs { calls: 0, fail }, ResultLimits::NORMAL);
        assert_eq!(runner.run(request(ScriptMode::EditOutput, "ab", &budget), &budget).unwrap_err(), unsupported("cbs"));
    }

    #[test]
    fn the_run_budget_reaches_into_a_search() {
        // The search would stop at its own limit and skip the rule; the run's work limit ends the run first.
        let budget = budget_with(200_000, 1 << 26, Arc::new(AtomicBool::new(false)));
        let mut runner = Runner::new(ScriptRunnerConfig::new(vec![plain("(?:a+)+$", "X", ScriptMode::EditOutput, "", &budget)]), NoCbs,
            ResultLimits::NORMAL);
        assert_eq!(runner.run(request(ScriptMode::EditOutput, &catastrophic(), &budget), &budget).unwrap_err(), StageError::Limit(Cow::Borrowed("work")));
        assert!(runner.report().skipped.is_empty());
        // Cancellation stops a search that would otherwise run to its own limit and skip the rule.
        let cancel = Arc::new(AtomicBool::new(false));
        let budget = budget_with(1 << 40, 1 << 26, cancel.clone());
        let mut config = ScriptRunnerConfig::new(vec![plain("(?:a+)+$", "X", ScriptMode::EditOutput, "", &budget)]);
        config.match_limits = MatchLimits { work_units: 1 << 26, ..DEFAULT_MATCH_LIMITS };
        let mut runner = Runner::new(config, NoCbs, ResultLimits::NORMAL);
        let outcome = std::thread::scope(|scope| {
            scope.spawn(|| { std::thread::sleep(std::time::Duration::from_millis(10)); cancel.store(true, std::sync::atomic::Ordering::Relaxed); });
            runner.run(request(ScriptMode::EditOutput, &catastrophic(), &budget), &budget)
        });
        assert_eq!(outcome.unwrap_err(), StageError::Cancelled);
    }

    #[test]
    fn a_compile_limit_declines_and_a_syntax_error_skips_its_rule() {
        let budget = budget();
        let words = (0..300).map(|index| format!("w{index}")).collect::<Vec<_>>().join("|");
        let scripts = |first: &str| vec![plain(first, "X", ScriptMode::EditOutput, "", &budget), plain("(", "X", ScriptMode::EditOutput, "", &budget),
            plain("w1", "Y", ScriptMode::EditOutput, "", &budget)];
        // JavaScript runs the long pattern, so the run declines before the Lua listeners.
        let mut config = ScriptRunnerConfig::new(scripts(&words));
        config.pattern_limits.compile.max_pattern_units = 64;
        config.lua_listeners = true;
        let mut runner = Runner::new(config, NoCbs, ResultLimits::NORMAL);
        assert_eq!(runner.run(request(ScriptMode::EditOutput, "w1 w2", &budget), &budget).unwrap_err(), unsupported("regex-pattern-units"));
        assert!(runner.report().engines.is_empty());
        let mut runner = Runner::new(ScriptRunnerConfig::new(scripts("w2")), NoCbs, ResultLimits::NORMAL);
        assert_eq!(lossy(&finish(runner.run(request(ScriptMode::EditOutput, "w1 w2", &budget), &budget)).text), "Y X");
        assert_eq!(skipped(&runner), [(1, invalid("regex-syntax"))]);
    }

    /// CBS that prints the same text for any input.
    struct Prints(&'static str);
    impl TextProcessor for Prints {
        fn process(&mut self, _: &Text, _: &TextContext, budget: &Budget) -> Result<TextOutput, StageError> {
            Ok(TextOutput { text: Text::from_str(self.0, budget)?, effects: Vec::new() })
        }
        fn apply(&mut self, _: &Effect, _: &Budget) -> Result<(), StageError> { Ok(()) }
    }

    #[test]
    fn a_foreign_unicode_engine_declines_the_patterns_that_follow_its_version() {
        let budget = budget();
        let run = |pattern: &str, flag: &str, foreign: bool| {
            let mut config = ScriptRunnerConfig::new(vec![plain(pattern, "X", ScriptMode::EditOutput, flag, &budget)]);
            config.foreign_unicode = foreign;
            config.lua_listeners = foreign;
            let mut runner = Runner::new(config, Prints("é"), ResultLimits::NORMAL);
            let step = runner.run(request(ScriptMode::EditOutput, "aé", &budget), &budget);
            let result = match step {
                Ok(Step::NeedsHost(_)) => finish(runner.resume(HostReply::Value(Value::Undefined), &budget)),
                other => finish(other),
            };
            lossy(&result.text)
        };
        let declined = |pattern: &str, flag: &str| {
            let mut config = ScriptRunnerConfig::new(vec![plain(pattern, "X", ScriptMode::EditOutput, flag, &budget)]);
            config.foreign_unicode = true;
            config.lua_listeners = true;
            let mut runner = Runner::new(config, Prints("é"), ResultLimits::NORMAL);
            let error = runner.run(request(ScriptMode::EditOutput, "aé", &budget), &budget).unwrap_err();
            (error, runner.report().engines.len())
        };
        // A stored pattern declines before the Lua listeners; a `<cbs>` pattern once CBS has printed it.
        assert_eq!(declined("\\p{L}", "gu"), (unsupported("regex-unicode"), 0));
        assert_eq!(declined("É", "gi"), (unsupported("regex-unicode"), 0));
        let mut config = ScriptRunnerConfig::new(vec![plain("{{x}}", "X", ScriptMode::EditOutput, "gi<cbs>", &budget)]);
        config.foreign_unicode = true;
        let mut runner = Runner::new(config, Prints("é"), ResultLimits::NORMAL);
        assert_eq!(runner.run(request(ScriptMode::EditOutput, "aé", &budget), &budget).unwrap_err(), unsupported("regex-unicode"));
        // The same patterns run on an engine at the tables' version, and ASCII ignore case runs on any engine.
        assert_eq!(run("\\p{L}", "gu", false), "XX");
        assert_eq!(run("É", "gi", false), "aX");
        assert_eq!(run("{{x}}", "gi<cbs>", false), "aX");
        assert_eq!(run("A", "gi", true), "Xé");
        assert_eq!(run("{{x}}", "g<cbs>", true), "aX");
    }

    #[test]
    fn static_admission_visits_only_executable_rules_without_running_cbs_or_hosts() {
        let budget = budget();
        let mut config = ScriptRunnerConfig::new(vec![
            plain("(", "invalid", ScriptMode::EditOutput, "", &budget),
            plain("", "empty", ScriptMode::EditOutput, "", &budget),
            plain("seed", "safe", ScriptMode::EditOutput, "", &budget),
        ]);
        config.lua_listeners = true;
        let mut runner = Runner::new(config, NoCbs, ResultLimits::NORMAL);
        let mut visited = Vec::new();
        runner.admit_static(ScriptMode::EditOutput, 4, &budget, |entry| { visited.push(entry.source_index); Ok(()) }).unwrap();
        assert_eq!(visited, [2]);
        assert!(runner.report().engines.is_empty());
        // Static inspection did not start a run: the first actual request can still yield the Lua listener.
        assert!(matches!(runner.run(request(ScriptMode::EditOutput, "seed", &budget), &budget).unwrap(), Step::NeedsHost(HostRequest::Lua { .. })));
    }

    #[test]
    fn plans_follow_script_changes() {
        let budget = budget();
        let mut runner = Runner::new(ScriptRunnerConfig::new(vec![plain("a", "b", ScriptMode::EditOutput, "u", &budget)]), NoCbs, ResultLimits::NORMAL);
        assert_eq!(lossy(&finish(runner.run(request(ScriptMode::EditOutput, "aa", &budget), &budget)).text), "ba");
        assert_eq!(lossy(&finish(runner.run(request(ScriptMode::EditOutput, "ab", &budget), &budget)).text), "bb");
        runner.config_mut().unwrap().scripts = vec![plain("a", "c", ScriptMode::EditOutput, "g", &budget)];
        assert_eq!(lossy(&finish(runner.run(request(ScriptMode::EditOutput, "ab", &budget), &budget)).text), "cb");
    }

    #[test]
    fn replace_output_is_charged_as_it_grows() {
        let budget = budget();
        let template = "x".repeat(30);
        let mut runner = Runner::new(ScriptRunnerConfig::new(vec![plain("[^b]", &template, ScriptMode::EditOutput, "g", &budget)]), NoCbs,
            ResultLimits::NORMAL);
        let before = budget.peak_bytes();
        let output = finish(runner.run(request(ScriptMode::EditOutput, &"a".repeat(1000), &budget), &budget));
        assert_eq!(output.text.len_units(), 30_000);
        // The UTF-16 output buffer, not only the text made from it.
        assert!(budget.peak_bytes() - before >= 2 * 30_000, "{}", budget.peak_bytes() - before);
    }

    #[test]
    fn move_captures_are_charged_while_they_live() {
        let size = 4000;
        let input = "a".repeat(size);
        let peak = |pattern: &str| {
            let budget = budget();
            let mut runner = Runner::new(ScriptRunnerConfig::new(vec![plain(pattern, "@@move_top", ScriptMode::EditOutput, "g", &budget)]),
                NoCbs, ResultLimits::NORMAL);
            let output = finish(runner.run(request(ScriptMode::EditOutput, &input, &budget), &budget));
            assert_eq!(lossy(&output.text), "@@move_top\n");
            budget.peak_bytes()
        };
        // The whole match and the group are both copied for each match.
        let (plain_peak, captured_peak) = (peak("a+"), peak("(a+)"));
        assert!(captured_peak >= plain_peak + 2 * size, "{plain_peak} {captured_peak}");
    }
}
