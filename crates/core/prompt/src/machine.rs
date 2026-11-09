//! The sans-IO preparation machine.
//!
//! [`PromptMachine`] builds the prompt array `sendChatInternal` builds (`src/ts/process/index.svelte.ts`) from stored
//! records and the stages of `ir::stage`, up to the array `requestChatData` receives: CBS escapes stay in place, as
//! they do until `risuUnescape` runs in the request stage. Every host callback a stage reaches comes out of
//! [`PromptMachine::start`] or [`PromptMachine::resume`] as [`Step::NeedsHost`], and the owner answers it with
//! [`PromptMachine::resume`]; the Lua `editRequest` listeners are the machine's own host request.
//!
//! The conversation is the complete stored conversation (no history window). Inputs this stage does not prepare
//! natively yet are refused as `StageError::Unsupported(name)`: `groupChat`, `presetChain`, `promptInfoInsideChat`,
//! `modules`, `hypaV3`, `additionalText`, `postHistoryInstructions`, `messageIdNormalization`, `historyRewrite`,
//! `prebuiltAssetCommand`, `inlay`, `assetPrompt`, and `lorebook` for stored lore entries.

use crate::{accounting, examples, history, items, notes, placement::Placement};
use risunest_ir::chat::{AssetRef, Multimodal, MultimodalData, PromptMessage, Role};
use risunest_ir::stage::{
    Effect, HostReply, HostRequest, LoreOutput, LoreSelector, LuaMode, LuaReply, LuaTrigger, MemoryInput, MemoryOutput,
    MemoryStage, ScriptRequest, ScriptRunner, StageError, Step, TextContext, TextProcessor, TokenCounter, TokenInput, TriggerMode,
    TriggerOutput, TriggerRequest, TriggerRunner,
};
use risunest_ir::{Budget, OwnedVec, Text, Value};
pub use risunest_jinja::CaseTables;
use risunest_records::character::Character;
use risunest_records::conversation::Conversation;
use risunest_records::preset::Preset;
use risunest_records::modules::SelectedModules;
use risunest_records::root::{copy, Fields, Root};
use risunest_records::settings::Settings;
use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

/// The stored records one send reads, and the WebView engine's case mapping.
#[derive(Debug)]
pub struct PromptInput {
    /// The stored root record.
    pub root: Value,
    /// The preset the send uses, as stored; `None` when the root selects none.
    pub preset: Option<Value>,
    /// The selected character.
    pub character: Value,
    /// The selected conversation with its complete `message` array, every message with its `chatId`.
    pub conversation: Value,
    /// The `toLocaleLowerCase` the example messages are read with.
    pub engine_case: EngineCase,
}

/// The WebView engine's `toLocaleLowerCase`, as the host probed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineCase {
    /// The engine's case tables.
    pub tables: CaseTables,
    /// The default locale tailors case mapping (`tr`, `az`, `lt`).
    pub tailored: bool,
}

/// The stages a send runs through, each built over the same records.
pub struct PromptStages {
    /// CBS (`risuChatParser`) for the selected character and conversation.
    pub text: Box<dyn TextProcessor + Send>,
    /// `processScriptFull` in `editprocess` mode: the regex scripts and the plugin `editprocess` hooks.
    pub scripts: Box<dyn ScriptRunner + Send>,
    pub lore: Box<dyn LoreSelector + Send>,
    /// The character triggers; the machine runs the `start` trigger.
    pub triggers: Box<dyn TriggerRunner + Send>,
    /// The memory mode the settings select; called only when the character uses memory.
    pub memory: Box<dyn MemoryStage + Send>,
    /// `tokenizeChat` under the active tokenizer, with the accounting of [`accounting::message_accounting`].
    pub tokens: Box<dyn TokenCounter + Send>,
}

/// A prepared prompt.
#[derive(Debug)]
pub struct PromptOutput {
    /// The final prompt array, with CBS escapes still in place.
    pub messages: Vec<PromptMessage>,
    /// `generationInfo.inputTokens`: the rechecked total.
    pub input_tokens: f64,
    /// `generationInfo.outputTokens`: `maxResponse`, lowered to what the context leaves.
    pub output_tokens: f64,
    /// `maxTokens` of the request: `maxResponse` as set.
    pub max_tokens: f64,
    pub max_context: f64,
    /// `biases`: the stored and character biases with their CBS evaluated.
    pub biases: Vec<(Text, f64)>,
    /// Conversation and variable changes in the order TS makes them, `lastMemory` among them.
    pub effects: Vec<Effect>,
}

/// Why a send stops before its request.
#[derive(Debug, PartialEq)]
pub enum PromptFailure {
    /// `language.errors.toomuchtoken` with `Required Tokens: {required_tokens}`, raised while trimming or, with
    /// `recheck`, at the final token recheck.
    ContextLimit { required_tokens: f64, recheck: bool },
    /// The memory stage's error, which TS raises through `throwError`.
    Memory(Text),
    /// The `start` trigger stopped the send.
    StopSending,
    /// An input the machine does not prepare, produced by a stage or found after a host request TS would make again
    /// (see [`PromptMachine::start`]). The owner cannot retry it through TS, so the send ends here.
    Unsupported(Cow<'static, str>),
}

#[derive(Debug)]
pub enum PromptOutcome {
    Ready(PromptOutput),
    /// The send stops. `effects` are what TS has stored when it stops: the variable and chat field changes of the
    /// stages that ran (the start trigger, scripts, the memory stage) in order. `lastMemory` is not among them: TS sets
    /// it on its working copy of the chat, which only a completed send stores. (One TS path differs: after a start
    /// trigger ran, a failed token recheck leaves `lastMemory` on the stored chat and the store flush then fails.) A
    /// memory stage that reports an error stores the effects it returns with the error.
    Failed { reason: PromptFailure, effects: Vec<Effect> },
}

type Run = Pin<Box<dyn Future<Output = Result<PromptOutcome, StageError>> + Send>>;

enum State {
    Idle(Box<(PromptInput, PromptStages, Option<SelectedModules>)>),
    Running { run: Run, shared: Shared, waiting: bool },
    Finished,
}

/// Whether unsupported inputs can leave preparation to TS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptPolicy {
    /// An unsupported input before a committing host request may fall back to TS.
    FallbackAllowed,
    /// The owner has committed to native execution; unsupported inputs are terminal.
    Owned,
}

/// One send's preparation: `start`, then `resume` with the reply to each host request. Every call should pass the
/// budget of the send; stage calls use the budget of the call that reaches them.
pub struct PromptMachine { state: State, stored: Vec<Effect>, policy: PromptPolicy }

impl PromptMachine {
    pub fn new(input: PromptInput, stages: PromptStages) -> Self {
        Self::with_policy(input, stages, PromptPolicy::FallbackAllowed)
    }

    /// Selects whether a late unsupported input can fall back or must retain its completed effects.
    pub fn with_policy(input: PromptInput, stages: PromptStages, policy: PromptPolicy) -> Self {
        Self { state: State::Idle(Box::new((input, stages, None))), stored: Vec::new(), policy }
    }

    /// Installs the canonical capture used by module-capable lore and script stages.
    pub fn with_captured_modules(input: PromptInput, stages: PromptStages, policy: PromptPolicy, modules: SelectedModules) -> Self {
        Self { state: State::Idle(Box::new((input, stages, Some(modules)))), stored: Vec::new(), policy }
    }

    /// After `start` or `resume` returned an error, the changes TS has stored when the send throws there, as
    /// [`PromptOutcome::Failed`] carries them; none after an `Unsupported` error the owner can still send through TS.
    /// Later calls return none.
    pub fn take_effects(&mut self) -> Vec<Effect> { std::mem::take(&mut self.stored) }

    /// Starts the preparation. `Err(StageError::Unsupported(name))`, from `start` or `resume`, leaves the send to TS:
    /// it comes only while every host request so far is one TS can make again without a visible change, a plugin
    /// tokenizer count or a conversation read, and the changes collected by then are dropped, as TS makes them again.
    /// After any other host request (a Lua listener, a plugin hook, a model, embedding or network call) an unsupported
    /// input ends the send as [`PromptFailure::Unsupported`]. [`PromptPolicy::Owned`] always uses that terminal outcome.
    pub fn start(&mut self, budget: &Budget) -> Result<Step<PromptOutcome>, StageError> {
        if !matches!(self.state, State::Idle(_)) { return Err(invalid("start")); }
        let State::Idle(parts) = std::mem::replace(&mut self.state, State::Finished) else { unreachable!() };
        let (input, stages, modules) = *parts;
        let shared = Arc::new(Mutex::new(Exchange::default()));
        self.state = State::Running { run: Box::pin(prepare(input, stages, shared.clone(), self.policy, modules)), shared, waiting: false };
        self.poll(budget)
    }

    pub fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<PromptOutcome>, StageError> {
        let State::Running { shared, waiting, .. } = &mut self.state else { return Err(invalid("resume")) };
        if !*waiting { return Err(invalid("resume")); }
        lock(shared).reply = Some(reply);
        *waiting = false;
        self.poll(budget)
    }

    fn poll(&mut self, budget: &Budget) -> Result<Step<PromptOutcome>, StageError> {
        let State::Running { run, shared, waiting } = &mut self.state else { return Err(invalid("poll")) };
        lock(shared).budget = Some(budget.clone());
        let polled = run.as_mut().poll(&mut Context::from_waker(Waker::noop()));
        let result = match polled {
            Poll::Ready(result) => {
                if result.is_err() { self.stored = std::mem::take(&mut lock(shared).stored); }
                result.map(Step::Done)
            }
            Poll::Pending => match lock(shared).request.take() {
                Some(request) => { *waiting = true; return Ok(Step::NeedsHost(request)); }
                None => Err(invalid("stalled")),
            },
        };
        self.state = State::Finished;
        result
    }
}

/// Refuses selected contributions the module-capable preparation stages do not execute.
pub fn admit_selected_modules(modules: &SelectedModules, budget: &Budget) -> Result<(), StageError> {
    notes::admit_selected_modules(modules, budget)
}

pub(crate) fn unsupported(name: &'static str) -> StageError { StageError::Unsupported(Cow::Borrowed(name)) }
pub(crate) fn invalid(name: &'static str) -> StageError { StageError::Invalid(Cow::Borrowed(name)) }

// Host plumbing: the preparation is an async function polled with a no-op waker. A host request parks it in
// `HostCall` until `resume` stores the reply.

#[derive(Default)]
struct Exchange { request: Option<HostRequest>, reply: Option<HostReply>, budget: Option<Budget>, stored: Vec<Effect> }
type Shared = Arc<Mutex<Exchange>>;

fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Exchange> { shared.lock().unwrap_or_else(|error| error.into_inner()) }

struct HostCall { shared: Shared, request: Option<HostRequest> }
impl Future for HostCall {
    type Output = HostReply;
    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<HostReply> {
        let this = &mut *self;
        let mut exchange = lock(&this.shared);
        if let Some(request) = this.request.take() {
            exchange.request = Some(request);
            return Poll::Pending;
        }
        match exchange.reply.take() { Some(reply) => Poll::Ready(reply), None => Poll::Pending }
    }
}

// Runs a stage call to completion, forwarding each host request it makes.
macro_rules! drive {
    ($ctx:expr, $stage:ident, $first:expr) => {{
        let mut step = $first;
        loop {
            match step {
                Step::Done(value) => break value,
                Step::NeedsHost(request) => {
                    let reply = $ctx.host(request).await;
                    let budget = $ctx.budget();
                    step = $ctx.stages.$stage.resume(reply, &budget)?;
                }
            }
        }
    }};
}

/// The stages and the effects collected so far.
pub(crate) struct Ctx {
    shared: Shared,
    pub(crate) stages: PromptStages,
    pub(crate) effects: Vec<Effect>,
    /// A host request TS would make again on a fallback has gone out ([`risunest_ir::stage::HostKind::commits`]: not a
    /// plugin tokenizer count, which TS memoizes only under `db.useTokenizerCaching`, nor a conversation read).
    committed: bool,
    /// The start trigger has begun; until then every change also reaches the trigger stage.
    trigger_started: bool,
}

impl Ctx {
    pub(crate) fn budget(&self) -> Budget { lock(&self.shared).budget.clone().expect("a budget before every poll") }

    // Not an `async fn`: the future must not borrow the stages, which are `Send` but not `Sync`.
    pub(crate) fn host(&mut self, request: HostRequest) -> impl Future<Output = HostReply> + Send + 'static {
        self.committed |= request.kind().commits();
        HostCall { shared: self.shared.clone(), request: Some(request) }
    }

    // CBS reads the variables other stages write; its own writes it already holds. The start trigger reads the chat as
    // it stands when it runs (`getVar` through the access's session, `src/ts/process/triggers.ts:1339-1345`,
    // `scriptConversationAccess.ts:93-104`), so every change made before it also goes to the trigger stage, whose CBS
    // and variables are its own.
    fn absorb(&mut self, effects: Vec<Effect>, from_text: bool) -> Result<(), StageError> {
        let budget = self.budget();
        for effect in effects {
            if !from_text { self.stages.text.apply(&effect, &budget)?; }
            if !self.trigger_started { self.stages.triggers.apply(&effect, &budget)?; }
            self.effects.push(effect);
        }
        Ok(())
    }

    /// `risuChatParser(input, context)`.
    pub(crate) fn parse(&mut self, input: &[u16], context: TextContext) -> Result<js::S, StageError> {
        let budget = self.budget();
        let output = self.stages.text.process(&js::text(input, &budget)?, &context, &budget)?;
        let text = js::units(&output.text, &budget)?;
        self.absorb(output.effects, true)?;
        Ok(text)
    }

    pub(crate) fn parse_plain(&mut self, input: &[u16]) -> Result<js::S, StageError> { self.parse(input, TextContext::default()) }

    /// `tokenizer.tokenizeChat(message)`.
    pub(crate) async fn count(&mut self, message: &PromptMessage) -> Result<f64, StageError> {
        let budget = self.budget();
        let first = self.stages.tokens.count(TokenInput::Message(message), &budget)?;
        Ok(f64::from(drive!(self, tokens, first)))
    }

    /// `processScriptFull(...).data`.
    pub(crate) async fn script(&mut self, request: ScriptRequest) -> Result<js::S, StageError> {
        let budget = self.budget();
        let first = self.stages.scripts.run(request, &budget)?;
        let output = drive!(self, scripts, first);
        let text = js::units(&output.text, &self.budget())?;
        self.absorb(output.effects, false)?;
        Ok(text)
    }

    pub(crate) async fn lore(&mut self) -> Result<LoreOutput, StageError> {
        let budget = self.budget();
        let mut step = self.stages.lore.select(&budget);
        loop {
            match step {
                Ok(Step::Done(mut output)) => {
                    let effects = std::mem::take(&mut output.effects);
                    self.absorb(effects, false)?;
                    return Ok(output);
                }
                Ok(Step::NeedsHost(request)) => {
                    let reply = self.host(request).await;
                    let budget = self.budget();
                    step = self.stages.lore.resume(reply, &budget);
                }
                Err(error) => {
                    self.effects.extend(self.stages.lore.take_effects());
                    return Err(error);
                }
            }
        }
    }

    // `drive!` with `TriggerRunner::take_effects`: the completed prefix leaves the run before each host request, and
    // a failed run still hands over the writes TS stores after a failure.
    pub(crate) async fn trigger(&mut self, request: TriggerRequest) -> Result<TriggerOutput, StageError> {
        let budget = self.budget();
        self.trigger_started = true;
        let first = self.stages.triggers.run(request, &budget);
        let mut step = self.trigger_step(first)?;
        loop {
            match step {
                Step::Done(mut output) => {
                    let effects = std::mem::take(&mut output.effects);
                    self.absorb(effects, false)?;
                    return Ok(output);
                }
                Step::NeedsHost(request) => {
                    let effects = self.stages.triggers.take_effects();
                    self.absorb(effects, false)?;
                    let reply = self.host(request).await;
                    let budget = self.budget();
                    let next = self.stages.triggers.resume(reply, &budget);
                    step = self.trigger_step(next)?;
                }
            }
        }
    }

    fn trigger_step(&mut self, step: Result<Step<TriggerOutput>, StageError>) -> Result<Step<TriggerOutput>, StageError> {
        if step.is_err() {
            let effects = self.stages.triggers.take_effects();
            self.effects.extend(effects);
        }
        step
    }

    pub(crate) async fn memory(&mut self, input: MemoryInput) -> Result<MemoryOutput, StageError> {
        let budget = self.budget();
        let first = self.stages.memory.run(input, &budget)?;
        let mut output: MemoryOutput = drive!(self, memory, first);
        let effects = std::mem::take(&mut output.effects);
        self.absorb(effects, false)?;
        Ok(output)
    }

    pub(crate) fn record(&mut self, effect: Effect) -> Result<(), StageError> { self.absorb(vec![effect], false) }
}

/// The messages `sendChatInternal` holds, by object identity: TS pushes one object into several places and mutates it
/// in place (system merging, `systemizeChat`, cache points, the final trim, the token recheck), so the groups and the
/// final array hold indices into `arena`.
#[derive(Debug, Default)]
pub(crate) struct Prompt {
    pub(crate) arena: Vec<PromptMessage>,
    pub(crate) groups: Groups,
}

impl Prompt {
    pub(crate) fn add(&mut self, message: PromptMessage) -> usize { self.arena.push(message); self.arena.len() - 1 }
}

/// `unformated`, in its key order.
#[derive(Debug, Default)]
pub(crate) struct Groups {
    pub(crate) main: Vec<usize>,
    pub(crate) jailbreak: Vec<usize>,
    pub(crate) chats: Vec<usize>,
    pub(crate) lorebook: Vec<usize>,
    pub(crate) global_note: Vec<usize>,
    pub(crate) author_note: Vec<usize>,
    pub(crate) last_chat: Vec<usize>,
    pub(crate) description: Vec<usize>,
    pub(crate) post_everything: Vec<usize>,
    pub(crate) persona_prompt: Vec<usize>,
}

impl Groups {
    pub(crate) fn in_order(&self) -> [&Vec<usize>; 10] {
        [&self.main, &self.jailbreak, &self.chats, &self.lorebook, &self.global_note, &self.author_note, &self.last_chat,
            &self.description, &self.post_everything, &self.persona_prompt]
    }
    /// `unformated[key]`; `None` for a key `unformated` lacks.
    pub(crate) fn by_key(&self, key: &[u16]) -> Option<&Vec<usize>> {
        const KEYS: [&str; 10] = ["main", "jailbreak", "chats", "lorebook", "globalNote", "authorNote", "lastChat", "description",
            "postEverything", "personaPrompt"];
        let index = KEYS.iter().position(|name| js::equals(key, name))?;
        Some(self.in_order()[index])
    }
}

/// `{ role, content }`.
pub(crate) fn message(role: Role, content: &[u16], budget: &Budget) -> Result<PromptMessage, StageError> {
    Ok(PromptMessage::new(role, js::text(content, budget)?))
}

fn copy_texts(values: &Option<Vec<Text>>, budget: &Budget) -> Result<Option<Vec<Text>>, StageError> {
    let Some(values) = values else { return Ok(None) };
    let mut output = Vec::with_capacity(values.len());
    for value in values { output.push(value.copy(budget)?); }
    Ok(Some(output))
}

fn copy_text(value: &Option<Text>, budget: &Budget) -> Result<Option<Text>, StageError> {
    Ok(value.as_ref().map(|value| value.copy(budget)).transpose()?)
}

fn copy_extra(extra: &[(Text, Value)], budget: &Budget) -> Result<Vec<(Text, Value)>, StageError> {
    extra.iter().map(|(key, value)| Ok((key.copy(budget)?, copy(value, budget)?))).collect()
}

/// `safeStructuredClone(message)`.
pub(crate) fn clone_message(message: &PromptMessage, budget: &Budget) -> Result<PromptMessage, StageError> {
    let multimodals = match &message.multimodals {
        None => None,
        Some(values) => {
            let mut output = Vec::with_capacity(values.len());
            for value in values {
                let data = match &value.data {
                    MultimodalData::Inline(text) => MultimodalData::Inline(text.copy(budget)?),
                    MultimodalData::Asset(AssetRef(text)) => MultimodalData::Asset(AssetRef(text.copy(budget)?)),
                };
                output.push(Multimodal { kind: value.kind, data, width: value.width, height: value.height, extra: copy_extra(&value.extra, budget)? });
            }
            Some(output)
        }
    };
    Ok(PromptMessage {
        role: message.role,
        content: message.content.copy(budget)?,
        memo: copy_text(&message.memo, budget)?,
        name: copy_text(&message.name, budget)?,
        removable: message.removable,
        attr: copy_texts(&message.attr, budget)?,
        multimodals,
        thoughts: copy_texts(&message.thoughts, budget)?,
        cache_point: message.cache_point,
        extra: copy_extra(&message.extra, budget)?,
    })
}

/// `chat.content.trim() !== '' || chat.multimodals?.length > 0`.
pub(crate) fn sendable(message: &PromptMessage) -> bool {
    message.content.units().any(|unit| !js::is_space(unit)) || message.multimodals.as_ref().is_some_and(|values| !values.is_empty())
}

pub(crate) fn role_name(role: Role) -> &'static str {
    match role { Role::System => "system", Role::User => "user", Role::Assistant => "assistant", Role::Function => "function" }
}

/// The checks `sendChatInternal` makes on `aiModel`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Model {
    /// `aiModel.startsWith('gpt')`.
    pub(crate) gpt: bool,
    /// `pushPrompts` merges consecutive system messages for these models.
    pub(crate) merges_system: bool,
    pub(crate) novelai: bool,
}

impl Model {
    pub(crate) fn read(settings: &Settings<'_>, budget: &Budget) -> Result<Self, StageError> {
        let model = js::units(settings.text("aiModel", budget)?.ok_or_else(|| invalid("settings.aiModel"))?, budget)?;
        let gpt = model.starts_with(&js::s("gpt"));
        let merges_system = gpt || model.starts_with(&js::s("claude")) || js::equals(&model, "openrouter") || js::equals(&model, "reverse_proxy");
        Ok(Self { gpt, merges_system, novelai: model.starts_with(&js::s("novelai")) })
    }
}

async fn prepare(input: PromptInput, stages: PromptStages, shared: Shared, policy: PromptPolicy, modules: Option<SelectedModules>) -> Result<PromptOutcome, StageError> {
    let mut ctx = Ctx { shared, stages, effects: Vec::new(), committed: false, trigger_started: false };
    match run(&mut ctx, &input, modules.as_ref()).await {
        // TS sends from the start and makes every change again, so the collected ones are dropped.
        Err(StageError::Unsupported(name)) if !ctx.committed && policy == PromptPolicy::FallbackAllowed => Err(StageError::Unsupported(name)),
        // TS would repeat a host call that has run.
        Err(StageError::Unsupported(name)) => Ok(failed(&mut ctx, PromptFailure::Unsupported(name))),
        // TS throws with these stored; the owner takes them through `PromptMachine::take_effects`.
        Err(error) => {
            let effects = stored(&mut ctx);
            lock(&ctx.shared).stored = effects;
            Err(error)
        }
        outcome => outcome,
    }
}

// The outcome of a send that stops before its request, with what TS has stored by then.
fn failed(ctx: &mut Ctx, reason: PromptFailure) -> PromptOutcome { PromptOutcome::Failed { reason, effects: stored(ctx) } }

fn stored(ctx: &mut Ctx) -> Vec<Effect> {
    let mut effects = std::mem::take(&mut ctx.effects);
    effects.retain(|effect| !matches!(effect, Effect::ChatField { field, .. } if field.equals_ascii(b"lastMemory")));
    effects
}

/// The stored `message` array with every message change the stages before the history made, in order, as
/// `{ message }`; `None` when they made none.
fn adopted_messages(ctx: &Ctx, conversation: Conversation<'_>, budget: &Budget) -> Result<Option<Value>, StageError> {
    let changes = |effect: &&Effect| matches!(effect, Effect::Message(_))
        || matches!(effect, Effect::ChatField { field, .. } if field.equals_ascii(b"message"));
    if !ctx.effects.iter().any(|effect| changes(&effect)) { return Ok(None); }
    let messages = conversation.get("message", budget)?.ok_or_else(|| invalid("conversation.message"))?;
    let mut adopted = Value::Object(OwnedVec::new(budget)?);
    adopted.define(Text::from_str("message", budget)?, copy(messages, budget)?, budget)?;
    for effect in ctx.effects.iter().filter(changes) { crate::response::apply_conversation_effect(&mut adopted, effect, budget)?; }
    Ok(Some(adopted))
}

/// `runTrigger(currentChar, 'start', { chat })`.
fn start_trigger() -> TriggerRequest {
    TriggerRequest { mode: TriggerMode::Start, display: None, manual_name: None, display_mode: false, temp_vars: None,
        trigger_id: None, cbs_variables: risunest_ir::stage::TriggerCbsVariables::TriggerScope }
}

async fn run(ctx: &mut Ctx, input: &PromptInput, modules: Option<&SelectedModules>) -> Result<PromptOutcome, StageError> {
    let budget = ctx.budget();
    let preset = input.preset.as_ref().map(Preset::new).transpose()?;
    let db = Settings::derive(Root::new(&input.root)?, preset, &budget)?;
    let character = Character::new(&input.character)?;
    let conversation = Conversation::new(&input.conversation)?;
    let rows = conversation.messages(&budget)?;
    let selection = history::select(&rows, &budget)?;
    // Every decline up to the lorebook selector comes before the first host request; one found after a host request TS
    // would make again ends the send (see `prepare`).
    notes::admit(&db, character, conversation, &rows, &selection, modules, &budget)?;
    // The start trigger runs after the greeting's editprocess hooks and Lua listeners, so the declines its records
    // decide come here.
    ctx.stages.triggers.admit(&start_trigger(), &budget)?;
    let model = Model::read(&db, &budget)?;
    let max_context = db.number("maxContext", &budget)?.ok_or_else(|| invalid("settings.maxContext"))?;
    let max_response = db.number("maxResponse", &budget)?.ok_or_else(|| invalid("settings.maxResponse"))?;
    let empty = Value::Object(OwnedVec::new(&budget)?);
    let prompt_settings = match db.get("promptSettings", &budget)? {
        Some(value @ Value::Object(_)) => Fields::new(value, "promptSettings")?,
        // TS reads it optional-chained without a template; with one, `promptSettings.x` throws.
        None | Some(Value::Null) if !db.truthy("promptTemplate", &budget)? => Fields::new(&empty, "promptSettings")?,
        _ => return Err(unsupported("promptSettings")),
    };
    let max_thought_depth = history::max_thought_depth(prompt_settings, &budget)?;
    let greeting = if selection.reset_by_all_before { None } else { Some(history::greeting_source(character, conversation, &budget)?) };
    history::parse_current_chat(ctx, &rows)?;

    let mut prompt = Prompt::default();
    let template = items::Template::read(&db, character, prompt_settings, &budget)?;
    template.admit(&db, character, &budget)?;
    if template.cards.is_none() { notes::main_prompts(ctx, &mut prompt, &db, character)?; }
    notes::author_note(ctx, &mut prompt, &db, conversation)?;
    if db.truthy("chainOfThought", &budget)? && !(template.using && prompt_settings.truthy("customChainOfThought", &budget)?) {
        let instruction = js::s("<instruction> - before respond everything, Think step by step as a ai assistant how would you respond inside <Thoughts> xml tag. this must be less than 5 paragraphs.</instruction>");
        let index = prompt.add(message(Role::System, &instruction, &budget)?);
        prompt.groups.post_everything.push(index);
    }
    let description = notes::description(ctx, &mut prompt, &db, character)?;
    let lore = Placement::new(ctx.lore().await?.fragments);
    lore.groups(ctx, &mut prompt)?;
    notes::persona(ctx, &mut prompt, &db, conversation)?;
    notes::inlay_instructions(&mut prompt, character, &budget)?;
    lore.depth_zero(ctx, &mut prompt)?;

    let mut tokens = max_response + 50.0;
    let mut state = items::CardState::default();
    tokens += items::count_items(ctx, &prompt, &lore, &template, description, &db, character, prompt_settings, &mut state).await?;
    let budget = ctx.budget();
    let mut chats: Vec<usize> = Vec::new();
    for example in examples::read(character, input.engine_case, &budget)? {
        let content = ctx.parse_plain(&example.content)?;
        let mut entry = message(example.role, &content, &budget)?;
        entry.name = example.name.map(|name| Text::from_str(name, &budget)).transpose()?;
        entry.memo = example.memo.map(|memo| Text::from_str(memo, &budget)).transpose()?;
        chats.push(prompt.add(entry));
    }
    for index in chats.clone() { tokens += ctx.count(&prompt.arena[index]).await?; }
    let budget = ctx.budget();
    if !model.novelai && !prompt_settings.truthy("trimStartNewChat", &budget)? {
        let mut start = message(Role::System, &js::s("[Start a new chat]"), &budget)?;
        start.memo = Some(Text::from_str("NewChat", &budget)?);
        chats.push(prompt.add(start));
    }
    if let Some(greeting) = greeting {
        let greeting = history::greeting(ctx, greeting, character, &template, prompt_settings).await?;
        tokens += ctx.count(&greeting).await?;
        chats.push(prompt.add(greeting));
    }
    let trigger = ctx.trigger(start_trigger()).await?;
    tokens += f64::from(trigger.tokens);
    if trigger.stop_sending { return Ok(failed(ctx, PromptFailure::StopSending)); }
    // TS adopts the trigger's chat (`currentChat = triggerResult.chat`, `index.svelte.ts:1861-1882`) and selects the
    // history again over it (`beginPromptHistoryOperation` and `selectPromptHistory`, `:1910-1921`).
    let budget = ctx.budget();
    let adopted = adopted_messages(ctx, conversation, &budget)?;
    let (rows, selection) = match &adopted {
        Some(adopted) => {
            let rows = Conversation::new(adopted)?.messages(&budget)?;
            let selection = history::select(&rows, &budget)?;
            (rows, selection)
        }
        None => (rows, selection),
    };
    tokens += history::messages(ctx, &mut prompt, &mut chats, &rows, &selection, character, &db, &template, prompt_settings,
        max_thought_depth).await?;
    tokens += lore.count_depth(ctx).await?;

    let budget = ctx.budget();
    if notes::uses_memory(&db, character, &budget)? {
        let input_chats = chats.iter().map(|index| clone_message(&prompt.arena[*index], &budget)).collect::<Result<_, _>>()?;
        let output = ctx.memory(MemoryInput { chats: input_chats, current_tokens: tokens, max_context_tokens: max_context }).await?;
        if let Some(error) = output.error {
            return Ok(failed(ctx, PromptFailure::Memory(error)));
        }
        chats = output.chats.into_iter().map(|entry| prompt.add(entry)).collect();
    } else {
        if let Err(required_tokens) = accounting::trim(ctx, &prompt, &mut chats, tokens, max_context).await? {
            return Ok(failed(ctx, PromptFailure::ContextLimit { required_tokens, recheck: false }));
        }
        // `chats[0].memo` with no chat left throws.
        let first = chats.first().ok_or_else(|| invalid("chats"))?;
        let budget = ctx.budget();
        let memo = match &prompt.arena[*first].memo { Some(memo) => Value::Text(memo.copy(&budget)?), None => Value::Undefined };
        ctx.record(Effect::ChatField { field: Text::from_str("lastMemory", &budget)?, value: memo })?;
    }
    let biases = notes::biases(ctx, &db, character)?;

    let budget = ctx.budget();
    let mut memories = Vec::new();
    if template.cards.is_none() {
        let last = chats.pop().ok_or_else(|| invalid("chats"))?;
        prompt.groups.last_chat.push(last);
    }
    for index in chats {
        let memo = prompt.arena[index].memo.as_ref();
        let memory = memo.is_some_and(|memo| memo.equals_ascii(b"supaMemory") || memo.equals_ascii(b"hypaMemory"));
        if !memory {
            prompt.arena[index].removable = Some(true);
        } else if state.memory_card_used {
            // The memory card places it; its slot in the chats is an empty system message, which the filter drops.
            memories.push(index);
            continue;
        } else {
            let content = js::units(&prompt.arena[index].content, &budget)?;
            let wrapped = js::concat(&[&js::s("<Previous Conversation>"), &content, &js::s("</Previous Conversation>")]);
            prompt.arena[index].content = js::text(&wrapped, &budget)?;
        }
        if sendable(&prompt.arena[index]) { prompt.groups.chats.push(index); }
    }
    lore.insert_depth(ctx, &mut prompt)?;
    if trigger.system_prompt_end.len_units() != 0 {
        let index = prompt.add(PromptMessage::new(Role::System, trigger.system_prompt_end.copy(&budget)?));
        prompt.groups.post_everything.push(index);
    }
    if trigger.system_prompt_history_end.len_units() != 0 {
        let index = prompt.add(PromptMessage::new(Role::System, trigger.system_prompt_history_end.copy(&budget)?));
        prompt.groups.last_chat.push(index);
    }
    if trigger.system_prompt_start.len_units() != 0 {
        let index = prompt.add(PromptMessage::new(Role::System, trigger.system_prompt_start.copy(&budget)?));
        prompt.groups.last_chat.insert(0, index);
    }

    let mut formated = items::assemble(ctx, &mut prompt, &lore, &template, description, &memories, &db, character, prompt_settings,
        model, &state).await?;
    let budget = ctx.budget();
    for &index in &formated { prompt.arena[index].content = prompt.arena[index].content.trim(&budget)?; }
    notes::depth_prompt(ctx, &mut prompt, &mut formated, character)?;
    if character.has_lua_trigger(&budget)? { formated = edit_request(ctx, &mut prompt, formated).await?; }
    let input_tokens = match accounting::recheck(ctx, &mut prompt, &mut formated, max_context).await? {
        Ok(tokens) => tokens,
        Err(required_tokens) => return Ok(failed(ctx, PromptFailure::ContextLimit { required_tokens, recheck: true })),
    };
    let output_tokens = if input_tokens + max_response > max_context { max_context - input_tokens } else { max_response };
    let budget = ctx.budget();
    let messages = formated.iter().map(|index| clone_message(&prompt.arena[*index], &budget)).collect::<Result<_, _>>()?;
    Ok(PromptOutcome::Ready(PromptOutput { messages, input_tokens, output_tokens, max_tokens: max_response, max_context, biases,
        effects: std::mem::take(&mut ctx.effects) }))
}

// `runLuaEditTrigger(currentChar, 'editRequest', formated)`: the host runs every Lua listener in order and answers with
// the final array. TS keeps the array it passed when a listener throws or nothing is returned.
async fn edit_request(ctx: &mut Ctx, prompt: &mut Prompt, formated: Vec<usize>) -> Result<Vec<usize>, StageError> {
    let budget = ctx.budget();
    let mut payload = OwnedVec::new(&budget)?;
    for index in &formated { payload.push(prompt.arena[*index].to_value(&budget)?)?; }
    let request = HostRequest::Lua { trigger: LuaTrigger::Edit { index: None }, mode: LuaMode::EditRequest, payload: Value::Array(payload) };
    let (value, effects) = match ctx.host(request).await {
        // `runLuaEditTriggerBatch` reads only `res`; it commits edits but does not read `stopSending`.
        HostReply::Lua(LuaReply { value, effects, .. }) => (value, effects),
        HostReply::Value(value) => (value, Vec::new()),
        HostReply::Failed { .. } => (Value::Undefined, Vec::new()),
        HostReply::Unsupported(name) => return Err(StageError::Unsupported(name)),
        HostReply::Cancelled => return Err(StageError::Cancelled),
        _ => return Err(invalid("editRequest")),
    };
    ctx.absorb(effects, false)?;
    match value {
        Value::Undefined | Value::Null => Ok(formated),
        Value::Array(values) => {
            let budget = ctx.budget();
            let mut output = Vec::with_capacity(values.as_slice().len());
            for value in values.as_slice() { output.push(prompt.add(PromptMessage::from_value(copy(value, &budget)?, &budget)?)); }
            Ok(output)
        }
        _ => Err(invalid("editRequest")),
    }
}

/// UTF-16 string operations with the JS semantics the preparation relies on.
pub(crate) mod js {
    use super::unsupported;
    use risunest_ir::stage::StageError;
    use risunest_ir::{Budget, Text, Value};

    pub(crate) type S = Vec<u16>;

    pub(crate) fn s(text: &str) -> S { text.encode_utf16().collect() }
    pub(crate) fn units(text: &Text, budget: &Budget) -> Result<S, StageError> {
        budget.work(text.len_units())?;
        Ok(text.units().collect())
    }
    pub(crate) fn text(units: &[u16], budget: &Budget) -> Result<Text, StageError> { Ok(Text::from_units(units, budget)?) }
    pub(crate) fn equals(units: &[u16], text: &str) -> bool { units.iter().copied().eq(text.encode_utf16()) }
    pub(crate) fn is_space(unit: u16) -> bool { risunest_ir::text::is_whitespace(unit) }
    /// What `.` without the `s` flag does not match.
    pub(crate) fn is_line_terminator(unit: u16) -> bool { matches!(unit, 0x0a | 0x0d | 0x2028 | 0x2029) }
    pub(crate) fn trim(units: &[u16], budget: &Budget) -> Result<S, StageError> { self::units(&text(units, budget)?.trim(budget)?, budget) }
    pub(crate) fn trim_start(units: &[u16], budget: &Budget) -> Result<S, StageError> {
        self::units(&text(units, budget)?.trim_start(budget)?, budget)
    }
    /// `String(value)`; `None` is `undefined`.
    pub(crate) fn string(value: Option<&Value>, name: &'static str, budget: &Budget) -> Result<S, StageError> {
        // An object with its own `toString` would run code.
        let text = value.unwrap_or(&Value::Undefined).to_text(budget).map_err(|_| unsupported(name))?;
        units(&text, budget)
    }
    /// `Number(value)` for the operand of an arithmetic operator; `None` is `undefined`.
    pub(crate) fn number(value: Option<&Value>, name: &'static str, budget: &Budget) -> Result<f64, StageError> {
        Ok(match value {
            None | Some(Value::Undefined) => f64::NAN,
            Some(Value::Null) => 0.0,
            Some(Value::Bool(value)) => f64::from(u8::from(*value)),
            Some(Value::Number(value)) => *value,
            Some(Value::Text(text)) => risunest_ir::num::to_number(text, budget)?,
            Some(value) => risunest_ir::num::to_number(&value.to_text(budget).map_err(|_| unsupported(name))?, budget)?,
        })
    }
    pub(crate) fn find(units: &[u16], needle: &[u16], from: usize) -> Option<usize> {
        if needle.is_empty() { return (from <= units.len()).then_some(from); }
        if needle.len() > units.len() { return None; }
        (from..=units.len() - needle.len()).find(|index| units[*index..].starts_with(needle))
    }
    pub(crate) fn concat(parts: &[&[u16]]) -> S { parts.iter().flat_map(|part| part.iter().copied()).collect() }

    // GetSubstitution for a string pattern, which has no capture groups: `$1` and `$<` stay literal.
    fn substitute(output: &mut S, replacement: &[u16], units: &[u16], at: usize, length: usize) {
        let mut index = 0;
        while index < replacement.len() {
            if replacement[index] == 0x24 && index + 1 < replacement.len() {
                let insert: Option<&[u16]> = match replacement[index + 1] {
                    0x24 => Some(&[0x24]),
                    0x26 => Some(&units[at..at + length]),
                    0x60 => Some(&units[..at]),
                    0x27 => Some(&units[at + length..]),
                    _ => None,
                };
                if let Some(insert) = insert {
                    output.extend_from_slice(insert);
                    index += 2;
                    continue;
                }
            }
            output.push(replacement[index]);
            index += 1;
        }
    }

    /// `units.replace(pattern, replacement)`, or `replaceAll` with `all`, for a string pattern.
    pub(crate) fn replace(units: &[u16], pattern: &[u16], replacement: &[u16], all: bool, budget: &Budget) -> Result<S, StageError> {
        budget.work(units.len() + replacement.len())?;
        let mut output = Vec::with_capacity(units.len());
        if pattern.is_empty() {
            for at in 0..=units.len() {
                if all || at == 0 {
                    budget.work(replacement.len())?;
                    substitute(&mut output, replacement, units, at, 0);
                }
                if let Some(unit) = units.get(at) { output.push(*unit); }
            }
            return Ok(output);
        }
        let mut last = 0;
        while let Some(at) = find(units, pattern, last) {
            budget.work(replacement.len())?;
            output.extend_from_slice(&units[last..at]);
            substitute(&mut output, replacement, units, at, pattern.len());
            last = at + pattern.len();
            if !all { break; }
        }
        output.extend_from_slice(&units[last..]);
        Ok(output)
    }

    /// `String.prototype.toLocaleLowerCase()` under a locale without special casing rules. An isolated surrogate is
    /// neither cased nor case-ignorable, so it ends the context Final_Sigma reads: each well-formed run is lowered on
    /// its own and the surrogate is kept.
    pub(crate) fn lower(units: &[u16], budget: &Budget) -> Result<S, StageError> {
        budget.work(units.len())?;
        let mut output = Vec::with_capacity(units.len());
        let mut run = String::new();
        for unit in char::decode_utf16(units.iter().copied()) {
            match unit {
                Ok(character) => run.push(character),
                Err(error) => {
                    output.extend(run.to_lowercase().encode_utf16());
                    run.clear();
                    output.push(error.unpaired_surrogate());
                }
            }
        }
        output.extend(run.to_lowercase().encode_utf16());
        Ok(output)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn string_replacement_follows_get_substitution() {
            let budget = crate::machine::tests::budget();
            let replace = |units: &str, pattern: &str, replacement: &str, all: bool| {
                String::from_utf16(&replace(&s(units), &s(pattern), &s(replacement), all, &budget).unwrap()).unwrap()
            };
            assert_eq!(replace("a{{slot}}b{{slot}}", "{{slot}}", "[$&|$`|$'|$$|$1]", false), "a[{{slot}}|a|b{{slot}}|$|$1]b{{slot}}");
            assert_eq!(replace("xox", "o", "$", true), "x$x");
            assert_eq!(replace("aaa", "a", "b", true), "bbb");
        }

        #[test]
        fn lowering_keeps_final_sigma_around_isolated_surrogates() {
            let budget = crate::machine::tests::budget();
            let lowered = |units: &[u16]| lower(units, &budget).unwrap();
            assert_eq!(lowered(&s("ΑΣ ΑΣΑ")), s("ας ασα"));
            // The surrogate ends the word before it and starts the context after it.
            let mut input = s("ΑΣ");
            input.push(0xd800);
            input.extend(s("ΣΑ"));
            let mut expected = s("ας");
            expected.push(0xd800);
            expected.extend(s("σα"));
            assert_eq!(lowered(&input), expected);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use risunest_ir::stage::{PluginHookKind, ScriptMode, ScriptOutput, TextOutput};
    use risunest_ir::{parse_json, Limits};
    use risunest_records::root::is_text;
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::fmt::Write as _;
    use std::sync::atomic::AtomicBool;

    const SEND_CASES: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/prompt/send.cases.json");
    const SEND_EXPECTED: &str = include_str!("../../../../tests/fixtures/rust-core/prompt/send.expected.json");
    const OLLAMA_CASES: &[u8] = include_bytes!("../../../../tests/fixtures/native-generation/ollama-text.json");
    const OLLAMA_EXPECTED: &str = include_str!("../../../../tests/fixtures/rust-core/prompt/ollama-text.expected.json");
    /// `language.errors.toomuchtoken` in `src/lang/en.ts`.
    const TOO_MUCH_TOKEN: &str = "Error: The minimum required token is greater than the Max Context Size.";

    pub(crate) fn budget() -> Budget {
        let limits = Limits { max_owned_bytes: 1 << 34, max_json_bytes: 1 << 26, max_text_units: 1 << 26, max_values: 1 << 26,
            max_depth: 128, max_work: usize::MAX / 4 };
        Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap()
    }

    // `JSON.stringify` for the comparisons; `undefined` prints as a marker JSON cannot hold.
    fn stringify(value: &Value, budget: &Budget) -> String {
        risunest_ir::json::stringify(value, budget).unwrap()
            .map_or_else(|| "<undefined>".into(), |text| String::from_utf16(&text.units().collect::<Vec<_>>()).unwrap())
    }
    fn number(value: f64, budget: &Budget) -> String { stringify(&Value::Number(value), budget) }
    fn units_value(units: &[u16], budget: &Budget) -> Value { Value::Text(Text::from_units(units, budget).unwrap()) }

    // `risuUnescape`, which `requestChatData` applies before the provider sees the array.
    fn unescape(text: &Text) -> Vec<u16> {
        const REPLACEMENTS: [&str; 8] = ["{", "}", "(", ")", "&lt;", "&gt;", ":", ";"];
        let mut output = Vec::with_capacity(text.len_units());
        for unit in text.units() {
            match unit { 0xe9b8..=0xe9bf => output.extend(REPLACEMENTS[usize::from(unit - 0xe9b8)].encode_utf16()), _ => output.push(unit) }
        }
        output
    }

    // The request view of the prompt array: each `OpenAIChat` in the canonical key order, after `risuUnescape`.
    fn request_view(messages: &[PromptMessage], ollama: bool, budget: &Budget) -> String {
        let key = |name: &str| Text::from_str(name, budget).unwrap();
        let texts = |values: &[Text]| {
            let mut array = OwnedVec::new(budget).unwrap();
            for value in values { array.push(Value::Text(value.copy(budget).unwrap())).unwrap(); }
            Value::Array(array)
        };
        let mut array = OwnedVec::new(budget).unwrap();
        for message in messages {
            let mut entry = Value::Object(OwnedVec::new(budget).unwrap());
            entry.define(key("role"), units_value(&js::s(role_name(message.role)), budget), budget).unwrap();
            entry.define(key("content"), units_value(&unescape(&message.content), budget), budget).unwrap();
            if !ollama {
                for (name, value) in [("name", &message.name), ("memo", &message.memo)] {
                    if let Some(value) = value { entry.define(key(name), Value::Text(value.copy(budget).unwrap()), budget).unwrap(); }
                }
                if let Some(attr) = &message.attr { entry.define(key("attr"), texts(attr), budget).unwrap(); }
                assert!(message.multimodals.as_ref().is_none_or(Vec::is_empty) && message.extra.is_empty());
                if let Some(thoughts) = &message.thoughts { entry.define(key("thoughts"), texts(thoughts), budget).unwrap(); }
                for (name, value) in [("removable", message.removable), ("cachePoint", message.cache_point)] {
                    if let Some(value) = value { entry.define(key(name), Value::Bool(value), budget).unwrap(); }
                }
            }
            array.push(entry).unwrap();
        }
        stringify(&Value::Array(array), budget)
    }

    fn get<'a>(value: &'a Value, key: &str, budget: &Budget) -> Option<&'a Value> {
        value.get(key.as_bytes(), budget).unwrap().filter(|value| !matches!(value, Value::Undefined))
    }
    fn at<'a>(value: &'a Value, key: &str, budget: &Budget) -> &'a Value {
        get(value, key, budget).unwrap_or_else(|| panic!("missing {key}"))
    }
    fn string(value: &Value) -> String {
        match value { Value::Text(text) => String::from_utf16(&text.units().collect::<Vec<_>>()).unwrap(), _ => panic!("not a string") }
    }
    fn hex(bytes: &[u8]) -> String { bytes.iter().fold(String::new(), |mut text, byte| { write!(text, "{byte:02x}").unwrap(); text }) }

    // Later cases store only their differences from the first case's root.
    fn case_root(base: &Value, database: &Value, budget: &Budget) -> Value {
        if let Some(root) = get(database, "root", budget) { return copy(root, budget).unwrap(); }
        let changes = at(database, "rootChanges", budget);
        let mut root = copy(base, budget).unwrap();
        for (key, value) in at(changes, "set", budget).object().unwrap() {
            root.define(key.copy(budget).unwrap(), copy(value, budget).unwrap(), budget).unwrap();
        }
        let removed = at(changes, "removed", budget).array().unwrap();
        let Value::Object(entries) = &root else { panic!("root") };
        let mut next = OwnedVec::new(budget).unwrap();
        for (name, value) in entries.as_slice() {
            if removed.iter().any(|key| matches!(key, Value::Text(key) if key.equals(name))) { continue; }
            next.push((name.copy(budget).unwrap(), copy(value, budget).unwrap())).unwrap();
        }
        Value::Object(next)
    }

    /// CBS for inputs without CBS: `{{char}}`, the `{{slot}}` placeholder TS fills after parsing, and a literal
    /// `{{setvar::name::value}}` under `runVar`; anything else that starts CBS syntax is refused.
    struct LiteralText { name: Vec<u16> }
    impl TextProcessor for LiteralText {
        fn process(&mut self, input: &Text, context: &TextContext, budget: &Budget) -> Result<TextOutput, StageError> {
            let units: Vec<u16> = input.units().collect();
            let mut output = Vec::with_capacity(units.len());
            let mut effects = Vec::new();
            let mut index = 0;
            while index < units.len() {
                let rest = &units[index..];
                if rest.starts_with(&js::s("{{char}}")) { output.extend_from_slice(&self.name); index += 8; continue; }
                if rest.starts_with(&js::s("{{slot}}")) { output.extend_from_slice(&rest[..8]); index += 8; continue; }
                if context.run_var && rest.starts_with(&js::s("{{setvar::")) {
                    let args = &rest[10..];
                    let end = js::find(args, &js::s("}}"), 0).ok_or_else(|| unsupported("cbs"))?;
                    let split = js::find(&args[..end], &js::s("::"), 0).ok_or_else(|| unsupported("cbs"))?;
                    let (name, value) = (&args[..split], &args[split + 2..end]);
                    if [name, value].iter().any(|part| js::find(part, &js::s("{{"), 0).is_some() || js::find(part, &js::s("::"), 0).is_some()) {
                        return Err(unsupported("cbs"));
                    }
                    effects.push(Effect::ChatVariable { name: Text::from_units(name, budget)?, value: Value::Text(Text::from_units(value, budget)?) });
                    index += 10 + end + 2;
                    continue;
                }
                let tag = |tag: &str| rest.len() > tag.len() && tag.bytes().zip(&rest[1..]).all(|(tag, unit)| (*unit | 0x20) == u16::from(tag));
                if matches!(rest, [0x7b, 0x7b | 0x23, ..]) || (rest[0] == 0x3c && (tag("user>") || tag("char>") || tag("bot>"))) {
                    return Err(StageError::Unsupported(Cow::Borrowed("cbs")));
                }
                output.push(units[index]);
                index += 1;
            }
            Ok(TextOutput { text: Text::from_units(&output, budget)?, effects })
        }
        fn apply(&mut self, _: &Effect, _: &Budget) -> Result<(), StageError> { Ok(()) }
    }

    /// The regex scripts of the `regex-*` Ollama cases, which the route runs through its regex runner.
    struct NoRegex;
    impl ScriptRunner for NoRegex {
        fn run(&mut self, _: ScriptRequest, _: &Budget) -> Result<Step<ScriptOutput>, StageError> { Err(unsupported("regex")) }
        fn resume(&mut self, _: HostReply, _: &Budget) -> Result<Step<ScriptOutput>, StageError> { Err(invalid("resume")) }
    }

    /// `processScriptFull` with no scripts and `hooks` plugin `editprocess` hooks: a string reply replaces the text,
    /// a nullish one keeps it.
    struct HookScripts { hooks: usize, pending: Option<(usize, Text)> }
    impl HookScripts {
        fn next(&mut self, index: usize, text: Text, budget: &Budget) -> Result<Step<ScriptOutput>, StageError> {
            if index == self.hooks { return Ok(Step::Done(ScriptOutput { text, emotion_changed: false, effects: Vec::new() })); }
            let payload = Value::Text(text.copy(budget)?);
            self.pending = Some((index, text));
            Ok(Step::NeedsHost(HostRequest::PluginHook { kind: PluginHookKind::Script(ScriptMode::EditProcess), payload }))
        }
    }
    impl ScriptRunner for HookScripts {
        fn run(&mut self, request: ScriptRequest, budget: &Budget) -> Result<Step<ScriptOutput>, StageError> {
            assert_eq!(request.mode, ScriptMode::EditProcess);
            self.next(0, request.input, budget)
        }
        fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<ScriptOutput>, StageError> {
            let (index, text) = self.pending.take().ok_or_else(|| invalid("resume"))?;
            let text = match reply {
                HostReply::Value(Value::Text(next)) => next,
                HostReply::Value(Value::Null | Value::Undefined) => text,
                _ => return Err(invalid("hook reply")),
            };
            self.next(index + 1, text, budget)
        }
    }

    struct NoLore;
    impl LoreSelector for NoLore {
        fn select(&mut self, _: &Budget) -> Result<Step<LoreOutput>, StageError> {
            Ok(Step::Done(LoreOutput { fragments: Vec::new(), effects: Vec::new(), match_log: Vec::new() }))
        }
        fn resume(&mut self, _: HostReply, _: &Budget) -> Result<Step<LoreOutput>, StageError> { Err(invalid("resume")) }
    }

    /// The `chatId` of an appended message; the `send.cases.json` trigger rows append none.
    struct NoIds;
    impl risunest_triggers::input::MessageIds for NoIds {
        fn next_id(&mut self, _: &Budget) -> Result<Text, StageError> { Err(unsupported("message-id")) }
    }

    /// `runTrigger(char, 'start')` through the real `risunest_triggers::runner::TriggerRun`: the scoped CBS processor
    /// over the case's records (as `risuChatParser` reads them), the regex pattern engine, the plugin counter and an
    /// empty `Math.random` tape, since no trigger row draws. The runner reads the records the machine holds and gets
    /// every earlier change through `TriggerRunner::apply`, as the machine feeds it.
    fn start_triggers(input: &PromptInput, budget: &Budget) -> Box<dyn TriggerRunner + Send> {
        use risunest_tokenizer::counter::{ChatAccounting, Counter};
        use risunest_tokenizer::id::TokenizerId;
        start_triggers_with(input, budget, Counter::new(TokenizerId::Plugin, ChatAccounting::default()), NoIds)
    }

    /// [`start_triggers`] counting with `counter` and drawing appended message IDs from `ids`.
    fn start_triggers_with<C, I>(input: &PromptInput, budget: &Budget, counter: C, ids: I) -> Box<dyn TriggerRunner + Send>
    where C: TokenCounter + Send + 'static, I: risunest_triggers::input::MessageIds + Send + 'static {
        use risunest_cbs_experimental::cbs::processor::{CaseTables, Character as CbsCharacter, Chat, Database, Processor, Snapshot, Source};
        use risunest_triggers::input::TriggerInput;
        use risunest_triggers::runner::TriggerRun;
        let text = |record: &Value, key: &str| match get(record, key, budget) {
            Some(Value::Text(text)) => Some(text.units().collect::<Vec<u16>>()), _ => None,
        };
        let (character, conversation) = (&input.character, &input.conversation);
        // The owner reads the effective preset over the stored root mirrors: its `templateDefaultVariables` and the
        // `moduleIntergration` that selects the modules.
        let settings = Settings::derive(Root::new(&input.root).unwrap(), input.preset.as_ref().map(|preset| Preset::new(preset).unwrap()),
            budget).unwrap();
        let mut root = copy(&input.root, budget).unwrap();
        if let Some(template) = settings.get("templateDefaultVariables", budget).unwrap() {
            root.define(Text::from_str("templateDefaultVariables", budget).unwrap(), copy(template, budget).unwrap(), budget).unwrap();
        }
        let sources = risunest_records::modules::ModuleSources::for_conversation(&input.root, character, conversation, budget).unwrap();
        let modules = SelectedModules::capture(&sources, settings.get("moduleIntergration", budget).unwrap(), budget).unwrap();
        let mut variables = HashMap::new();
        for (key, value) in get(conversation, "scriptstate", budget).and_then(Value::object).unwrap_or(&[]) {
            let key: Vec<u16> = key.units().collect();
            if key.first() != Some(&u16::from(b'$')) || matches!(value, Value::Null | Value::Undefined) { continue; }
            variables.insert(key[1..].to_vec(), value.to_text(budget).unwrap().units().collect());
        }
        let snapshot = Database {
            character: Some(CbsCharacter {
                id: text(character, "chaId").unwrap_or_default(),
                name: text(character, "name").unwrap_or_default(),
                first_message: text(character, "firstMessage"),
                alternate_greetings: get(character, "alternateGreetings", budget).and_then(Value::array).unwrap_or_default().iter()
                    .filter_map(|greeting| if let Value::Text(text) = greeting { Some(text.units().collect()) } else { None }).collect(),
                default_variables: text(character, "defaultVariables").unwrap_or_default(),
                chat: Chat {
                    id: text(conversation, "id"),
                    first_message_index: match get(conversation, "fmIndex", budget) { Some(Value::Number(index)) => *index as i64, _ => -1 },
                    note: get(conversation, "note", budget).map(|note| copy(note, budget).unwrap()),
                    variables,
                    messages: get(conversation, "message", budget).and_then(Value::array).unwrap_or_default().iter()
                        .map(|row| copy(row, budget).unwrap()).collect(),
                    ..Chat::default()
                },
                ..CbsCharacter::default()
            }),
            template_default_variables: text(&root, "templateDefaultVariables").unwrap_or_default(),
            ..Database::default()
        };
        let processor = Processor::new(Snapshot { user: text(&input.root, "username").unwrap_or_default(),
            source: Source::Database(Box::new(snapshot)), case_tables: CaseTables::Unicode16, ..Snapshot::default() }, risunest_ir::rng::Tape::new(Vec::new()));
        // The send rows were captured on Node 24.12 (Unicode 16).
        let mut triggers = TriggerInput::from_records_with_modules(&root, character, conversation, &modules, budget).unwrap();
        triggers.case_tables = CaseTables::Unicode16;
        Box::new(TriggerRun::new(triggers, processor, counter, risunest_regex::engine::JsPatternEngine::default(),
            risunest_ir::rng::Tape::new(Vec::new()), ids))
    }

    struct NoMemory;
    impl MemoryStage for NoMemory {
        fn run(&mut self, _: MemoryInput, _: &Budget) -> Result<Step<MemoryOutput>, StageError> { Err(unsupported("memory")) }
        fn resume(&mut self, _: HostReply, _: &Budget) -> Result<Step<MemoryOutput>, StageError> { Err(invalid("resume")) }
    }

    /// `tokenizeChat` over recorded counts: the built-in tokenizer's counts from the oracle, or the plugin tokenizer
    /// through the host, remembered per text when `useTokenizerCaching` is on.
    struct RecordedTokens {
        builtin: Option<HashMap<Vec<u16>, u32>>,
        memo: Option<HashMap<Vec<u16>, u32>>,
        accounting: accounting::MessageAccounting,
        pending: Option<(Vec<Vec<u16>>, usize, u32)>,
    }
    impl RecordedTokens {
        fn advance(&mut self, texts: Vec<Vec<u16>>, mut next: usize, mut total: u32, budget: &Budget) -> Result<Step<u32>, StageError> {
            while let Some(text) = texts.get(next) {
                let known = match (&self.builtin, &self.memo) {
                    (Some(builtin), _) => Some(builtin.get(text).copied().or(text.is_empty().then_some(0)).ok_or_else(|| {
                        StageError::Invalid(Cow::Owned(format!("no recorded count for {:?}", String::from_utf16_lossy(text))))
                    })?),
                    (None, Some(memo)) => memo.get(text).copied(),
                    (None, None) => None,
                };
                let Some(count) = known else {
                    let request = HostRequest::PluginTokenizer { text: Text::from_units(text, budget)? };
                    self.pending = Some((texts, next, total));
                    return Ok(Step::NeedsHost(request));
                };
                total += count;
                next += 1;
            }
            Ok(Step::Done(total))
        }
    }
    impl TokenCounter for RecordedTokens {
        fn count(&mut self, input: TokenInput<'_>, budget: &Budget) -> Result<Step<u32>, StageError> {
            let message = match input {
                // `tokenize(text)`, which counts a trigger's system prompts and its `v2Tokenize` texts.
                TokenInput::Text(text) => return self.advance(vec![text.units().collect()], 0, 0, budget),
                TokenInput::Message(message) => message,
                _ => return Err(invalid("token input")),
            };
            let mut texts = vec![message.content.units().collect::<Vec<_>>()];
            let mut total = self.accounting.additional_tokens;
            if let Some(name) = message.name.as_ref().filter(|name| self.accounting.use_name && name.len_units() != 0) {
                texts.push(name.units().collect());
                total += 1;
            }
            self.advance(texts, 0, total, budget)
        }
        fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<u32>, StageError> {
            let (texts, next, total) = self.pending.take().ok_or_else(|| invalid("resume"))?;
            let HostReply::Tokens(ids) = reply else { return Err(invalid("tokenizer reply")) };
            let count = ids.len() as u32;
            if let Some(memo) = &mut self.memo { memo.insert(texts[next].clone(), count); }
            self.advance(texts, next + 1, total + count, budget)
        }
    }

    #[derive(Clone)]
    struct PlacementRng(Arc<Mutex<risunest_ir::rng::Tape>>);
    impl risunest_ir::rng::Rng for PlacementRng {
        fn random(&mut self) -> Result<f64, risunest_ir::Error> {
            risunest_ir::rng::Rng::random(&mut *self.0.lock().unwrap())
        }
    }

    #[derive(Clone)]
    struct PlacementText(Arc<Mutex<Box<dyn TextProcessor + Send>>>);
    impl TextProcessor for PlacementText {
        fn process(&mut self, input: &Text, context: &TextContext, budget: &Budget) -> Result<TextOutput, StageError> {
            self.0.lock().unwrap().process(input, context, budget)
        }
        fn apply(&mut self, effect: &Effect, budget: &Budget) -> Result<(), StageError> {
            self.0.lock().unwrap().apply(effect, budget)
        }
    }

    // Synthetic placement cases use literal fields, char/user, and random. This captures those CBS reads while the
    // real processor, selector and tokenizer execute together; it is not an application snapshot adapter.
    fn placement_stages(replay: &mut Replay, params: &Value, budget: &Budget) {
        use risunest_cbs_experimental::cbs::processor::{Character as CbsCharacter, Chat, Database, Processor, Snapshot, Source};
        use risunest_tokenizer::counter::{ChatAccounting, Counter};
        use risunest_tokenizer::id::TokenizerId;
        assert!(is_text(get(params, "tokenizer", budget), "cl100k_base"));
        let values = get(params, "random", budget).and_then(Value::array).unwrap_or_default().iter().map(|value| {
            let Value::Number(value) = value else { panic!("random draw") };
            *value
        }).collect();
        let rng = PlacementRng(Arc::new(Mutex::new(risunest_ir::rng::Tape::new(values))));
        let text_field = |record: &Value, key| match get(record, key, budget) {
            Some(Value::Text(text)) => text.units().collect(), _ => Vec::new(),
        };
        let character = CbsCharacter {
            id: text_field(&replay.input.character, "chaId"), name: text_field(&replay.input.character, "name"),
            nickname: text_field(&replay.input.character, "nickname"),
            chat: Chat { id: Some(text_field(&replay.input.conversation, "id")), ..Chat::default() },
            ..CbsCharacter::default()
        };
        let snapshot = Snapshot { user: text_field(&replay.input.root, "username"),
            source: Source::Database(Box::new(Database { character: Some(character), ..Database::default() })),
            ..Snapshot::default() };
        let text = PlacementText(Arc::new(Mutex::new(Box::new(Processor::new(snapshot, rng.clone())))));
        let input = risunest_lorebook::selector::LoreInput::from_records(&replay.input.root, &replay.input.character,
            &replay.input.conversation, budget).unwrap();
        replay.stages.lore = Box::new(risunest_lorebook::selector::LoreSelection::new(input, text.clone(),
            Counter::new(TokenizerId::Cl100kBase, ChatAccounting::default()), risunest_regex::engine::JsPatternEngine::default(), rng));
        replay.stages.text = Box::new(text);
        let settings = Settings::derive(Root::new(&replay.input.root).unwrap(),
            replay.input.preset.as_ref().map(|preset| Preset::new(preset).unwrap()), budget).unwrap();
        let accounting = accounting::message_accounting(&settings, budget).unwrap();
        replay.stages.tokens = Box::new(Counter::new(TokenizerId::Cl100kBase, ChatAccounting {
            additional_tokens: accounting.additional_tokens, use_name: accounting.use_name, ..ChatAccounting::default()
        }));
        // The selector owns the original lore input; production PromptInput admission remains closed.
        set(&mut replay.input.character, "globalLore", "[]", budget);
        set(&mut replay.input.conversation, "localLore", "[]", budget);
    }

    /// One recorded case as the machine reads it.
    struct Replay {
        input: PromptInput,
        stages: PromptStages,
        stored_last_memory: String,
        stored_scriptstate: Option<Value>,
        stored_messages: Value,
    }

    fn replay_case(result: &Value, base_root: &Value, params: &Value, ollama: bool, budget: &Budget) -> Replay {
        let database = at(result, "database", budget);
        let root = case_root(base_root, database, budget);
        let preset = at(database, "preset", budget);
        // The recorded preset is the only stored preset.
        let found = risunest_records::preset::selected(Root::new(&root).unwrap(), std::slice::from_ref(preset), budget).unwrap().is_some();
        let preset = found.then(|| copy(preset, budget).unwrap());
        let character = copy(at(database, "character", budget), budget).unwrap();
        let conversation = copy(at(database, "conversation", budget), budget).unwrap();
        let settings = Settings::derive(Root::new(&root).unwrap(), preset.as_ref().map(|preset| Preset::new(preset).unwrap()), budget).unwrap();
        let tokenizer = get(params, "tokenizer", budget);
        let plugin = !ollama && is_text(settings.get("aiModel", budget).unwrap(), "custom") && (tokenizer.is_none() || is_text(tokenizer, "plugin"));
        let mut recorded = HashMap::new();
        for entry in at(result, "tokens", budget).array().unwrap() {
            let pair = entry.array().unwrap();
            let Value::Number(count) = pair[1] else { panic!("count") };
            recorded.insert(js::units(match &pair[0] { Value::Text(text) => text, _ => panic!("text") }, budget).unwrap(), count as u32);
        }
        // An Ollama trigger counts its texts with the built-in tokenizer the oracle recorded.
        let trigger_tokens = ollama.then(|| RecordedTokens { builtin: Some(recorded.clone()), memo: None,
            accounting: accounting::MessageAccounting { additional_tokens: 0, use_name: false }, pending: None });
        let tokens = RecordedTokens {
            builtin: (!plugin).then_some(recorded),
            memo: (plugin && settings.truthy("useTokenizerCaching", budget).unwrap()).then(HashMap::new),
            accounting: accounting::message_accounting(&settings, budget).unwrap(),
            pending: None,
        };
        let name = js::units(Character::new(&character).unwrap().display_name(budget).unwrap(), budget).unwrap();
        let hooks = get(params, "hooks", budget).and_then(Value::array).map_or(0, <[Value]>::len);
        let stored_last_memory = get(&conversation, "lastMemory", budget).map_or("null".into(), |value| stringify(value, budget));
        let stored_scriptstate = get(&conversation, "scriptstate", budget).filter(|value| !matches!(value, Value::Null))
            .map(|value| copy(value, budget).unwrap());
        let stored_messages = copy(at(&conversation, "message", budget), budget).unwrap();
        drop(settings);
        let scripts: Box<dyn ScriptRunner + Send> = if is_text(get(params, "decline", budget), "regex") { Box::new(NoRegex) }
            else { Box::new(HookScripts { hooks, pending: None }) };
        // The recording's Node 24.12 lowers with Unicode 16 tables under an untailored locale.
        let engine_case = EngineCase { tables: CaseTables::Unicode16, tailored: false };
        let input = PromptInput { root, preset, character, conversation, engine_case };
        let triggers = match trigger_tokens {
            Some(counter) => start_triggers_with(&input, budget, counter, risunest_triggers::input::RandomIds(risunest_ir::rng::OsEntropy)),
            None => start_triggers(&input, budget),
        };
        let stages = PromptStages {
            text: Box::new(LiteralText { name }), scripts, lore: Box::new(NoLore), triggers, memory: Box::new(NoMemory),
            tokens: Box::new(tokens),
        };
        let mut replay = Replay { input, stages, stored_last_memory, stored_scriptstate, stored_messages };
        if matches!(get(params, "lorePlacement", budget), Some(Value::Bool(true))) { placement_stages(&mut replay, params, budget); }
        replay
    }

    // Runs the machine, answering each host request from the recorded host tape in order.
    fn run(replay: Replay, tape: &[Value], budget: &Budget, problems: &mut Vec<String>) -> Result<PromptOutcome, StageError> {
        let mut machine = PromptMachine::new(replay.input, replay.stages);
        let mut step = machine.start(budget)?;
        let mut used = 0;
        let outcome = loop {
            let request = match step { Step::Done(outcome) => break outcome, Step::NeedsHost(request) => request };
            let entry = tape.get(used);
            used += 1;
            let reply = match (&request, entry) {
                (HostRequest::PluginTokenizer { text }, Some(entry))
                    if is_text(get(entry, "kind", budget), "tokenizer") && at(entry, "text", budget).to_text(budget).unwrap().equals(text) => {
                    let Value::Number(count) = at(entry, "count", budget) else { panic!("count") };
                    HostReply::Tokens(vec![0.0; *count as usize])
                }
                (HostRequest::PluginHook { kind: PluginHookKind::Script(ScriptMode::EditProcess), payload: Value::Text(input) }, Some(entry))
                    if is_text(get(entry, "kind", budget), "editprocess") && at(entry, "input", budget).to_text(budget).unwrap().equals(input) => {
                    HostReply::Value(copy(at(entry, "output", budget), budget).unwrap())
                }
                _ => {
                    problems.push(format!("host request {request:?} where the tape has {}", entry.map_or("nothing".into(), |entry| stringify(entry, budget))));
                    return Err(invalid("tape"));
                }
            };
            step = machine.resume(reply, budget)?;
        };
        if used != tape.len() { problems.push(format!("{} of {} host tape entries used", used, tape.len())); }
        Ok(outcome)
    }

    fn last_memory(effects: &[Effect], stored: &str, budget: &Budget) -> String {
        effects.iter().rev().find_map(|effect| match effect {
            Effect::ChatField { field, value } if field.equals_ascii(b"lastMemory") => {
                Some(if matches!(value, Value::Undefined) { "null".into() } else { stringify(value, budget) })
            }
            _ => None,
        }).unwrap_or_else(|| stored.to_string())
    }

    // The stored `scriptstate` with the chat variable effects applied as `setChatVar` stores them.
    fn scriptstate(effects: &[Effect], stored: Option<&Value>, budget: &Budget) -> String {
        let mut state = stored.map(|value| copy(value, budget).unwrap());
        for effect in effects {
            let Effect::ChatVariable { name, value } = effect else { continue };
            let state = state.get_or_insert_with(|| Value::Object(OwnedVec::new(budget).unwrap()));
            let key = js::concat(&[&js::s("$"), &js::units(name, budget).unwrap()]);
            state.define(Text::from_units(&key, budget).unwrap(), copy(value, budget).unwrap(), budget).unwrap();
        }
        state.map_or("null".into(), |state| stringify(&state, budget))
    }

    // The data TS leaves in the stored message slots: the stored messages with the message effects applied in order,
    // then the provider's `Synthetic response.` when the send went out, read over the stored length; `null` when unchanged.
    fn rewritten(effects: &[Effect], stored: &Value, sent: bool, budget: &Budget) -> String {
        let data = |messages: &[Value]| messages.iter().map(|message| stringify(at(message, "data", budget), budget)).collect::<Vec<_>>();
        let before = data(stored.array().unwrap());
        let mut adopted = Value::Object(OwnedVec::new(budget).unwrap());
        adopted.define(Text::from_str("message", budget).unwrap(), copy(stored, budget).unwrap(), budget).unwrap();
        for effect in effects {
            let message = matches!(effect, Effect::Message(_)) || matches!(effect, Effect::ChatField { field, .. } if field.equals_ascii(b"message"));
            if message { crate::response::apply_conversation_effect(&mut adopted, effect, budget).unwrap(); }
        }
        let mut after = data(at(&adopted, "message", budget).array().unwrap());
        if sent { after.push("\"Synthetic response.\"".into()); }
        after.truncate(before.len());
        if after == before { "null".into() } else { format!("[{}]", after.join(",")) }
    }

    fn biases(output: &PromptOutput, budget: &Budget) -> String {
        let mut array = OwnedVec::new(budget).unwrap();
        for (text, weight) in &output.biases {
            let mut pair = OwnedVec::new(budget).unwrap();
            pair.push(Value::Text(text.copy(budget).unwrap())).unwrap();
            pair.push(Value::Number(*weight)).unwrap();
            array.push(Value::Array(pair)).unwrap();
        }
        stringify(&Value::Array(array), budget)
    }

    /// Recorded sends whose store flush TS could not complete; their stored `lastMemory` is not compared.
    const FLUSH_FAILURES: [&str; 1] = ["failure-trigger-setvar-recheck"];

    // Replays every recorded case and returns the case count, the declined count and the mismatches.
    fn replay(expected: &str, cases: &[u8], ollama: bool) -> (usize, usize, Vec<String>) {
        let budget = budget();
        let expected = parse_json(expected, &budget).unwrap();
        // The Ollama fixture may be checked out with CRLF; its digest covers the LF text.
        let source = String::from_utf8(cases.to_vec()).unwrap();
        let source = if ollama { source.replace("\r\n", "\n") } else { source };
        assert_eq!(string(at(&expected, "fixtureSha256", &budget)), hex(&Sha256::digest(source.as_bytes())));
        let params = parse_json(&source, &budget).unwrap();
        let params = at(&params, "cases", &budget).array().unwrap();
        let cases = at(&expected, "cases", &budget).array().unwrap();
        assert_eq!(params.len(), cases.len());
        let base_root = at(at(at(&cases[0], "result", &budget), "database", &budget), "root", &budget);
        let app_composition = ["lore-global-local-state", "lore-template-composition", "lore-inactive-nonempty",
            "lore-all-budget-excluded", "lore-exact-fit-budget", "lore-injection-only", "lore-shared-random-stages", "lore-depth-count-trim",
            "module-selected-order", "module-regex-excluded", "module-only-no-fragments"];
        let composed: Vec<_> = params.iter().filter(|params| matches!(get(params, "observeLore", &budget), Some(Value::Bool(true))))
            .map(|params| string(at(params, "id", &budget))).collect();
        // The trigger rows record TS sends with character or module triggers; their `decline` names the route's refusal,
        // not a decline of this literal replay, which runs them through the real trigger runner.
        let route_triggers = ["trigger-setvar-string", "trigger-setvar-values", "trigger-setvar-conditions", "trigger-locals-loops",
            "trigger-system-prompts", "trigger-author-note", "trigger-nested-runtrigger", "trigger-random-cbs-order", "trigger-tokenize-branch",
            "trigger-template-default-preset", "trigger-lore-then-trigger-var", "trigger-low-level-off", "trigger-triggercode-any-mode",
            "trigger-modifychat", "trigger-cutchat", "trigger-cutchat-allbefore", "trigger-impersonate", "trigger-decline-lua",
            "trigger-decline-command", "trigger-decline-run-llm", "trigger-decline-similarity", "trigger-decline-request-type",
            "trigger-decline-output-type", "trigger-decline-trigger-id", "trigger-decline-module-trigger"];
        let triggered: Vec<_> = params.iter().filter(|params| matches!(get(params, "observeTriggers", &budget), Some(Value::Bool(true))))
            .map(|params| string(at(params, "id", &budget))).collect();
        // Trigger rows these literal stages cannot reproduce: a `Math.random()` draw the literal CBS does not take, a
        // variable the lore stage writes, and a request trigger, which runs at the request this replay does not reach.
        let unreplayed = ["trigger-random-cbs-order", "trigger-lore-then-trigger-var", "trigger-decline-request-type"];
        // Trigger rows TS sends that the machine refuses at start: effects the trigger runner does not run, and a selected
        // module with triggers.
        let trigger_declines = [("trigger-decline-lua", "trigger-effect-triggerlua"), ("trigger-decline-command", "trigger-effect-command"),
            ("trigger-decline-run-llm", "trigger-effect-v2RunLLM"), ("trigger-decline-similarity", "trigger-effect-v2CheckSimilarity"),
            ("trigger-decline-module-trigger", "modules")];
        if ollama {
            // These exact cases require the application's CBS, shared entropy and L05 composition; its comparator
            // consumes all fixture rows. This literal-stage replay retains the original cases and decline controls.
            assert_eq!(composed, app_composition);
            assert_eq!(triggered, route_triggers);
            assert_eq!(cases.len(), 50 + app_composition.len() + route_triggers.len());
        } else { assert!(composed.is_empty() && triggered.is_empty()); }
        let mut mismatches = Vec::new();
        let mut declined = 0;
        for (case, params) in cases.iter().zip(params) {
            let id = string(at(case, "id", &budget));
            assert_eq!(string(at(params, "id", &budget)), id);
            if ollama && (composed.contains(&id) || unreplayed.contains(&id.as_str())) { continue; }
            let result = at(case, "result", &budget);
            let replay = replay_case(result, base_root, params, ollama, &budget);
            let mut problems = Vec::new();
            let decline = match triggered.contains(&id) {
                true => trigger_declines.iter().find(|(row, _)| *row == id).map(|(_, name)| (*name).to_owned()),
                false => get(params, "decline", &budget).map(string),
            };
            if let Some(decline) = decline {
                // `start` itself refuses: no host request goes out before the decline.
                declined += 1;
                match PromptMachine::new(replay.input, replay.stages).start(&budget) {
                    Err(StageError::Unsupported(name)) if name == decline => {}
                    other => problems.push(format!("want a decline of {decline:?} at start, got {other:?}")),
                }
                if !problems.is_empty() { mismatches.push(format!("{id}:\n    {}", problems.join("\n    "))); }
                continue;
            }
            let stored = replay.stored_last_memory.clone();
            let stored_state = replay.stored_scriptstate.as_ref().map(|value| copy(value, &budget).unwrap());
            let stored_messages = copy(&replay.stored_messages, &budget).unwrap();
            let tape = get(result, "host", &budget).and_then(Value::array).unwrap_or_default();
            let outcome = run(replay, tape, &budget, &mut problems);
            let mut check = |name: &str, want: String, got: String| {
                if want != got { problems.push(format!("{name}: want {want}\n      got  {got}")); }
            };
            let want = |key: &str| stringify(at(result, key, &budget), &budget);
            let flush_failed = !ollama && !matches!(at(result, "flushError", &budget), Value::Null);
            if !ollama {
                check("flushError", FLUSH_FAILURES.contains(&id.as_str()).to_string(), flush_failed.to_string());
            }
            let stored_after = |effects: &[Effect], sent: bool, check: &mut dyn FnMut(&str, String, String)| {
                if !flush_failed { check("lastMemory", want("lastMemory"), last_memory(effects, &stored, &budget)); }
                if !ollama {
                    check("scriptstate", want("scriptstate"), scriptstate(effects, stored_state.as_ref(), &budget));
                    check("rewritten", want("rewritten"), rewritten(effects, &stored_messages, sent, &budget));
                }
            };
            let not_sent = |check: &mut dyn FnMut(&str, String, String)| {
                check("sent", want("sent"), "false".into());
                check(if ollama { "messages" } else { "request" }, want(if ollama { "messages" } else { "request" }), "null".into());
                check("generation", want("generation"), "null".into());
                if !ollama { check("biases", want("biases"), "null".into()); }
            };
            match outcome {
                Ok(PromptOutcome::Ready(output)) => {
                    check("sent", want("sent"), "true".into());
                    if ollama {
                        check("messages", want("messages"), request_view(&output.messages, true, &budget));
                    } else {
                        let request = at(result, "request", &budget);
                        check("prompt_chat", stringify(at(request, "prompt_chat", &budget), &budget), request_view(&output.messages, false, &budget));
                        let tokens = if is_text(get(params, "capture", &budget), "replacer") { "null".into() } else { number(output.max_tokens, &budget) };
                        check("max_tokens", stringify(at(request, "max_tokens", &budget), &budget), tokens);
                        check("biases", want("biases"), biases(&output, &budget));
                    }
                    let generation = format!("{{\"inputTokens\":{},\"outputTokens\":{},\"maxContext\":{}}}", number(output.input_tokens, &budget),
                        number(output.output_tokens, &budget), number(output.max_context, &budget));
                    check("generation", want("generation"), generation);
                    check("errors", want("errors"), "[]".into());
                    stored_after(&output.effects, true, &mut check);
                }
                Ok(PromptOutcome::Failed { reason: PromptFailure::ContextLimit { required_tokens, recheck }, effects }) => {
                    let prefix = if recheck { "\n\nAt token rechecking. Required Tokens: " } else { "\n\nRequired Tokens: " };
                    let message = format!("{TOO_MUCH_TOKEN}{prefix}{}", number(required_tokens, &budget));
                    check("errors", want("errors"), format!("[{}]", stringify(&units_value(&js::s(&message), &budget), &budget)));
                    not_sent(&mut check);
                    stored_after(&effects, false, &mut check);
                }
                Ok(PromptOutcome::Failed { reason: PromptFailure::StopSending, effects }) => {
                    check("errors", want("errors"), "[]".into());
                    not_sent(&mut check);
                    stored_after(&effects, false, &mut check);
                }
                Ok(PromptOutcome::Failed { reason, .. }) => problems.push(format!("failed with {reason:?}")),
                Err(error) => problems.push(format!("refused with {error:?}")),
            }
            if !problems.is_empty() { mismatches.push(format!("{id}:\n    {}", problems.join("\n    "))); }
        }
        let skipped = if ollama { composed.len() + unreplayed.len() } else { 0 };
        (cases.len() - skipped, declined, mismatches)
    }

    #[test]
    fn prompt_preparation_matches_the_send_oracle() {
        let (cases, declined, mismatches) = replay(SEND_EXPECTED, SEND_CASES, false);
        assert_eq!((cases, declined), (106, 1));
        assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
    }

    #[test]
    fn the_ollama_literal_cases_reproduce_through_the_machine() {
        let (cases, declined, mismatches) = replay(OLLAMA_EXPECTED, OLLAMA_CASES, true);
        assert_eq!((cases, declined), (72, 35));
        assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
    }

    // The first recorded send case, with `edit` applied to its character and conversation.
    fn base_replay(edit: impl FnOnce(&mut Value, &mut Value, &Budget)) -> (Replay, Budget) {
        let budget = budget();
        let expected = parse_json(SEND_EXPECTED, &budget).unwrap();
        let case = &at(&expected, "cases", &budget).array().unwrap()[0];
        assert_eq!(string(at(case, "id", &budget)), "base-order");
        let result = at(case, "result", &budget);
        let base_root = at(at(result, "database", &budget), "root", &budget);
        let params = parse_json("{\"tokenizer\":\"cl100k_base\"}", &budget).unwrap();
        let mut replay = replay_case(result, base_root, &params, false, &budget);
        // The base case counts with the plugin tokenizer; one count per text keeps these tests off the host.
        replay.stages.tokens = Box::new(RecordedTokens {
            builtin: None, memo: Some(HashMap::new()),
            accounting: accounting::MessageAccounting { additional_tokens: 3, use_name: true }, pending: None,
        });
        edit(&mut replay.input.character, &mut replay.input.conversation, &budget);
        (replay, budget)
    }
    pub(crate) fn text_context() -> (Ctx, Budget) {
        let (replay, budget) = base_replay(|_, _, _| {});
        let shared = Arc::new(Mutex::new(Exchange { budget: Some(budget.clone()), ..Exchange::default() }));
        (Ctx { shared, stages: replay.stages, effects: Vec::new(), committed: false, trigger_started: false }, budget)
    }

    fn set(object: &mut Value, key: &str, json: &str, budget: &Budget) {
        object.define(Text::from_str(key, budget).unwrap(), parse_json(json, budget).unwrap(), budget).unwrap();
    }

    // Drives the machine, answering plugin token counts with one token per text and passing other requests to `reply`.
    fn drive(replay: Replay, budget: &Budget, mut reply: impl FnMut(HostRequest) -> HostReply) -> Result<PromptOutcome, StageError> {
        let mut machine = PromptMachine::new(replay.input, replay.stages);
        let mut step = machine.start(budget)?;
        loop {
            step = match step {
                Step::Done(outcome) => return Ok(outcome),
                Step::NeedsHost(HostRequest::PluginTokenizer { .. }) => machine.resume(HostReply::Tokens(vec![0.0]), budget)?,
                Step::NeedsHost(request) => machine.resume(reply(request), budget)?,
            };
        }
    }
    fn contents(outcome: &PromptOutcome) -> Vec<String> {
        let PromptOutcome::Ready(output) = outcome else { panic!("not ready: {outcome:?}") };
        output.messages.iter().map(|message| format!("{}:{}", role_name(message.role), String::from_utf16(&message.content.units().collect::<Vec<_>>()).unwrap())).collect()
    }

    #[test]
    fn a_lua_edit_request_yields_the_final_array_and_takes_the_listener_result() {
        let lua = |character: &mut Value, _: &mut Value, budget: &Budget| {
            set(character, "triggerscript", r#"[{"comment":"","type":"manual","conditions":[],"effect":[{"type":"triggerlua","code":""}]}]"#, budget);
        };
        let (replay, budget) = base_replay(lua);
        let mut payload = None;
        let outcome = drive(replay, &budget, |request| {
            let HostRequest::Lua { trigger: LuaTrigger::Edit { index: None }, mode: LuaMode::EditRequest, payload: sent } = request else { panic!("{request:?}") };
            payload = Some(stringify(&sent, &crate::machine::tests::budget()));
            HostReply::Value(parse_json(r#"[{"role":"user","content":"Edited."}]"#, &crate::machine::tests::budget()).unwrap())
        }).unwrap();
        let payload = payload.expect("an editRequest yield");
        assert!(payload.starts_with(r#"[{"role":"system","content":"Synthetic main."}"#), "{payload}");
        assert!(payload.contains(r#""memo":"message-b","attr":[],"thoughts":[],"removable":true}"#), "{payload}");
        assert!(payload.contains(r#""memo":"message-c","attr":[],"thoughts":[]}"#), "{payload}");
        assert_eq!(contents(&outcome), vec!["user:Edited."]);
        // A listener that throws leaves the array as it was.
        let (replay, budget) = base_replay(lua);
        let failed = drive(replay, &budget, |_| HostReply::Failed { message: Text::from_str("boom", &crate::machine::tests::budget()).unwrap() }).unwrap();
        let (replay, budget) = base_replay(|_, _, _| {});
        assert_eq!(contents(&failed), contents(&drive(replay, &budget, |request| panic!("{request:?}")).unwrap()));
        // The batch result and writes are independent: throws keep prior writes, and edit listeners ignore stop.
        for result in [r#"[{"role":"user","content":"Lua array."}]"#, "null", "undefined"] {
            let (replay, budget) = base_replay(lua);
            let outcome = drive(replay, &budget, |_| {
                let budget = crate::machine::tests::budget();
                let value = if result == "undefined" { Value::Undefined } else { parse_json(result, &budget).unwrap() };
                let effects = ["first", "second"].into_iter().map(|value| Effect::ChatVariable {
                    name: Text::from_str("lua", &budget).unwrap(), value: Value::Text(Text::from_str(value, &budget).unwrap()),
                }).collect();
                HostReply::Lua(LuaReply { value, effects, stop_sending: true })
            }).unwrap();
            assert_eq!(contents(&outcome), if result.starts_with('[') { vec!["user:Lua array.".to_string()] } else { contents(&failed) });
            let PromptOutcome::Ready(output) = outcome else { unreachable!() };
            assert!(matches!(output.effects.first(), Some(Effect::ChatField { field, .. }) if field.equals_ascii(b"lastMemory")));
            let writes: Vec<_> = output.effects.iter().filter_map(|effect| match effect {
                Effect::ChatVariable { name, value } if name.equals_ascii(b"lua") => Some(string(value)),
                _ => None,
            }).collect();
            assert_eq!(writes, vec!["first", "second"]);
        }
        let (replay, budget) = base_replay(lua);
        let outcome = drive(replay, &budget, |_| HostReply::Unsupported(Cow::Borrowed("lua-host"))).unwrap();
        assert!(matches!(outcome, PromptOutcome::Failed { reason: PromptFailure::Unsupported(name), effects } if name == "lua-host" && effects.is_empty()));
    }

    /// A memory mode that keeps the last chat and puts a summary before it. A contract stage: the memory modes are not
    /// ported, so the test below checks the machine's side of `MemoryStage`, not recorded TS output.
    struct Summary { error: Option<&'static str> }
    impl MemoryStage for Summary {
        fn run(&mut self, mut input: MemoryInput, budget: &Budget) -> Result<Step<MemoryOutput>, StageError> {
            let last = input.chats.pop().ok_or_else(|| invalid("chats"))?;
            let mut summary = PromptMessage::new(Role::System, Text::from_str("Summary.", budget)?);
            summary.memo = Some(Text::from_str("supaMemory", budget)?);
            Ok(Step::Done(MemoryOutput {
                chats: vec![summary, last], current_tokens: input.current_tokens,
                effects: vec![Effect::ChatField { field: Text::from_str("supaMemoryData", budget)?, value: Value::Text(Text::from_str("kept", budget)?) }],
                error: self.error.map(|error| Text::from_str(error, budget)).transpose()?,
            }))
        }
        fn resume(&mut self, _: HostReply, _: &Budget) -> Result<Step<MemoryOutput>, StageError> { Err(invalid("resume")) }
    }

    #[test]
    fn a_memory_stage_replaces_trimming_and_its_summary_is_wrapped() {
        let (mut replay, budget) = base_replay(|character, _, budget| set(character, "supaMemory", "true", budget));
        set(&mut replay.input.root, "hypav2", "true", &budget);
        replay.stages.memory = Box::new(Summary { error: None });
        let outcome = drive(replay, &budget, |request| panic!("{request:?}")).unwrap();
        let PromptOutcome::Ready(output) = &outcome else { panic!("{outcome:?}") };
        assert!(matches!(output.effects.as_slice(), [Effect::ChatField { field, .. }] if field.equals_ascii(b"supaMemoryData")));
        let contents = contents(&outcome);
        assert!(contents.contains(&"system:<Previous Conversation>Summary.</Previous Conversation>".to_string()), "{contents:?}");
        assert!(!contents.iter().any(|content| content.contains("First question")), "{contents:?}");
        assert!(contents.iter().any(|content| content == "user:Second question."), "{contents:?}");
        // The memory updates a failing stage returns are stored with its error.
        let (mut replay, budget) = base_replay(|character, _, budget| set(character, "supaMemory", "true", budget));
        set(&mut replay.input.root, "hypav2", "true", &budget);
        replay.stages.memory = Box::new(Summary { error: Some("Memory failed.") });
        let outcome = drive(replay, &budget, |request| panic!("{request:?}")).unwrap();
        assert!(matches!(&outcome, PromptOutcome::Failed { reason: PromptFailure::Memory(error), effects: stored }
            if error.equals_ascii(b"Memory failed.")
                && matches!(stored.as_slice(), [Effect::ChatField { field, .. }] if field.equals_ascii(b"supaMemoryData"))), "{outcome:?}");
    }

    // `start` itself refuses, so no host request went out before the decline.
    fn declined_at_start(replay: Replay, budget: &Budget) -> String {
        match PromptMachine::new(replay.input, replay.stages).start(budget) {
            Err(StageError::Unsupported(name)) => name.into_owned(),
            Ok(Step::NeedsHost(request)) => panic!("{request:?} before a decline"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn inputs_the_machine_does_not_prepare_are_declined_before_any_host_request() {
        // CBS that changes nothing, so only the admission checks decline.
        let declined = |edit: &dyn Fn(&mut Replay, &Budget)| {
            let (mut replay, budget) = base_replay(|_, _, _| {});
            replay.stages.text = Box::new(Passthrough);
            replay.stages.lore = Box::new(UnreachedLore);
            edit(&mut replay, &budget);
            declined_at_start(replay, &budget)
        };
        let character = |key: &'static str, json: &'static str| move |replay: &mut Replay, budget: &Budget| set(&mut replay.input.character, key, json, budget);
        let conversation = |key: &'static str, json: &'static str| move |replay: &mut Replay, budget: &Budget| set(&mut replay.input.conversation, key, json, budget);
        let message = |json: &'static str| conversation("message", json);
        let template = |cards: &'static str| move |replay: &mut Replay, budget: &Budget| {
            set(replay.input.preset.as_mut().expect("a selected preset"), "promptTemplate", cards, budget);
        };
        assert_eq!(declined(&character("type", "\"group\"")), "groupChat");
        assert_eq!(declined(&character("additionalText", "\"Extra.\"")), "additionalText");
        assert_eq!(declined(&message(r#"[{"role":"user","data":"No id."}]"#)), "messageIdNormalization");
        assert_eq!(declined(&message(r#"[{"role":"char","data":"A {{inlayeddata::a}}","chatId":"m"}]"#)), "inlay");
        assert_eq!(declined(&message(r#"[{"role":"user","data":"A {{inlay::a}}","chatId":"m"}]"#)), "inlay");
        assert_eq!(declined(&message(r#"[{"role":"user","data":"{{asset_prompt::icon}}","chatId":"m"}]"#)), "assetPrompt");
        assert_eq!(declined(&|replay: &mut Replay, budget: &Budget| {
            set(&mut replay.input.character, "additionalAssets", r#"[["pic","pic.png","png"]]"#, budget);
            set(&mut replay.input.conversation, "message", r#"[{"role":"char","data":"{{AssetPrompt::pic}}","chatId":"m"}]"#, budget);
        }), "assetPrompt");
        assert_eq!(declined(&character("globalLore", "{}")), "lorebook");
        assert_eq!(declined(&conversation("localLore", "{}")), "lorebook");
        // The utility bot template has a global note card.
        assert_eq!(declined(&|replay: &mut Replay, budget: &Budget| {
            set(&mut replay.input.character, "utilityBot", "true", budget);
            set(&mut replay.input.character, "prebuiltAssetCommand", "true", budget);
        }), "prebuiltAssetCommand");
        // The load keeps an array `promptSettings` in the root; only a selected preset replaces it.
        assert_eq!(declined(&|replay: &mut Replay, budget: &Budget| {
            replay.input.preset = None;
            set(&mut replay.input.root, "promptTemplate", r#"[{"type":"chat","rangeStart":0,"rangeEnd":"end"}]"#, budget);
            set(&mut replay.input.root, "promptSettings", "[]", budget);
        }), "promptSettings");
        assert_eq!(declined(&template(r#"[{"type":"chat","rangeStart":"0","rangeEnd":"end"}]"#)), "promptTemplate.range");
        assert_eq!(declined(&template(r#"[{"type":"cache","depth":"1","role":"all"}]"#)), "promptTemplate.depth");
        assert_eq!(declined(&conversation("fmIndex", "5")), "fmIndex");
        assert_eq!(declined(&conversation("fmIndex", "\"0\"")), "fmIndex");
        assert_eq!(declined(&conversation("fmIndex", "null")), "fmIndex");
        // The current chat parser pass would store a rewritten message.
        assert_eq!(declined(&|replay: &mut Replay, _: &Budget| replay.stages.text = Box::new(Rewrite)), "historyRewrite");
    }

    struct UnreachedLore;
    impl LoreSelector for UnreachedLore {
        fn select(&mut self, _: &Budget) -> Result<Step<LoreOutput>, StageError> { panic!("selector before admission") }
        fn resume(&mut self, _: HostReply, _: &Budget) -> Result<Step<LoreOutput>, StageError> { panic!("selector before admission") }
    }

    const LORE_LEAD: &str = r#"{"key":"","content":"@@keep_activate_after_match\nLead.","alwaysActive":true,"insertorder":100,"comment":"Lead","id":"lead"}"#;
    const LORE_SECOND: &str = r#"{"key":"","content":"@@keep_activate_after_match\nSecond.","alwaysActive":true,"insertorder":100,"comment":"Second","id":"second"}"#;
    const LORE_MALFORMED: &str = r#"{"key":"","content":"@@position\nBroken.","alwaysActive":true,"insertorder":100}"#;

    fn lore_counter(id: risunest_tokenizer::id::TokenizerId) -> risunest_tokenizer::counter::Counter {
        risunest_tokenizer::counter::Counter::new(id, risunest_tokenizer::counter::ChatAccounting::default())
    }

    fn real_lore_replay(entries: &str, counter: risunest_tokenizer::counter::Counter) -> (Replay, Budget) {
        let (mut replay, budget) = base_replay(|_, _, _| {});
        let root = parse_json(r#"{"username":"User","loreBookDepth":10,"loreBookToken":1000}"#, &budget).unwrap();
        let character = parse_json(&format!(r#"{{"name":"Character","globalLore":[{entries}],"loreSettings":{{"recursiveScanning":false}}}}"#), &budget).unwrap();
        let conversation = parse_json(r#"{"message":[]}"#, &budget).unwrap();
        let input = risunest_lorebook::selector::LoreInput::from_records(&root, &character, &conversation, &budget).unwrap();
        replay.stages.lore = Box::new(risunest_lorebook::selector::LoreSelection::new(input, Passthrough, counter,
            risunest_regex::engine::JsPatternEngine::default(), risunest_ir::rng::Tape::new(Vec::new())));
        (replay, budget)
    }

    fn real_lore_machine(entries: &str, counter: risunest_tokenizer::counter::Counter) -> (PromptMachine, Budget) {
        let (replay, budget) = real_lore_replay(entries, counter);
        (PromptMachine::new(replay.input, replay.stages), budget)
    }

    fn assert_lore_prefix(effects: &[Effect], expected: &[&str]) {
        let writes: Vec<_> = effects.iter().map(|effect| match effect {
            Effect::ChatVariable { name, value } => {
                assert!(matches!(value, Value::Text(text) if text.equals_ascii(b"true")));
                String::from_utf16_lossy(&name.units().collect::<Vec<_>>())
            }
            other => panic!("unexpected effect {other:?}"),
        }).collect();
        assert_eq!(writes, expected);
    }

    #[test]
    fn real_lore_select_failure_retains_only_the_completed_prefix_once() {
        let (mut machine, budget) = real_lore_machine(&format!("{LORE_LEAD},{LORE_MALFORMED}"),
            lore_counter(risunest_tokenizer::id::TokenizerId::Cl100kBase));
        assert_eq!(machine.start(&budget).unwrap_err(), invalid("lore-position-argument"));
        assert_lore_prefix(&machine.take_effects(), &["__internal_ka_lead"]);
        assert!(machine.take_effects().is_empty());
    }

    #[test]
    fn real_lore_resume_failure_keeps_the_original_host_error_and_prefix() {
        let (mut machine, budget) = real_lore_machine(&format!("{LORE_LEAD},{LORE_SECOND}"),
            lore_counter(risunest_tokenizer::id::TokenizerId::Plugin));
        assert!(matches!(machine.start(&budget).unwrap(), Step::NeedsHost(HostRequest::PluginTokenizer { .. })));
        assert!(machine.take_effects().is_empty());
        assert!(matches!(machine.resume(HostReply::Tokens(vec![0.0; 2]), &budget).unwrap(),
            Step::NeedsHost(HostRequest::PluginTokenizer { .. })));
        assert!(machine.take_effects().is_empty());
        let message = Text::from_str("Synthetic tokenizer failure.", &budget).unwrap();
        let expected = StageError::Host(message.units().collect());
        assert_eq!(machine.resume(HostReply::Failed { message }, &budget).unwrap_err(), expected);
        assert_lore_prefix(&machine.take_effects(), &["__internal_ka_lead"]);
        assert!(machine.take_effects().is_empty());
    }

    #[test]
    fn real_lore_plugin_refusal_discards_the_prefix_for_fallback() {
        let (mut machine, budget) = real_lore_machine(&format!("{LORE_LEAD},{LORE_SECOND}"),
            lore_counter(risunest_tokenizer::id::TokenizerId::Plugin));
        assert!(matches!(machine.start(&budget).unwrap(), Step::NeedsHost(HostRequest::PluginTokenizer { .. })));
        assert!(matches!(machine.resume(HostReply::Tokens(vec![0.0; 2]), &budget).unwrap(), Step::NeedsHost(_)));
        assert_eq!(machine.resume(HostReply::Unsupported(Cow::Borrowed("synthetic-tokenizer")), &budget).unwrap_err(),
            unsupported("synthetic-tokenizer"));
        assert!(machine.take_effects().is_empty());
    }

    #[test]
    fn real_lore_committing_counter_refusal_is_invalid_with_the_completed_prefix() {
        let gemma = risunest_tokenizer::id::TokenizerId::Gemma;
        let path = gemma.artifact().unwrap().path;
        let bytes = std::fs::read(format!("{}/../../../public{path}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        risunest_tokenizer::id::model::register(gemma, &bytes).unwrap();
        let budget = budget();
        let counter = lore_counter(risunest_tokenizer::id::TokenizerId::GoogleCloud)
            .with_google_cloud(risunest_tokenizer::counter::google::GoogleCloud {
                internal_id: Text::from_str("synthetic-model", &budget).unwrap(),
                access_token: None, memo: Default::default(),
            });
        let (mut machine, budget) = real_lore_machine(&format!("{LORE_LEAD},{LORE_SECOND}"), counter);
        assert!(matches!(machine.start(&budget).unwrap(), Step::NeedsHost(HostRequest::Http(_))));
        assert!(machine.take_effects().is_empty());
        assert!(matches!(machine.resume(HostReply::Http { status: 200, body: br#"{"totalTokens":2}"#.to_vec() }, &budget).unwrap(),
            Step::NeedsHost(HostRequest::Http(_))));
        assert!(machine.take_effects().is_empty());
        assert_eq!(machine.resume(HostReply::Unsupported(Cow::Borrowed("synthetic-http")), &budget).unwrap_err(),
            invalid("synthetic-http"));
        assert_lore_prefix(&machine.take_effects(), &["__internal_ka_lead"]);
        assert!(machine.take_effects().is_empty());
    }

    #[test]
    fn real_lore_success_places_text_and_transfers_effects_once() {
        let (mut machine, budget) = real_lore_machine(LORE_LEAD,
            lore_counter(risunest_tokenizer::id::TokenizerId::Cl100kBase));
        let mut step = machine.start(&budget).unwrap();
        while let Step::NeedsHost(HostRequest::PluginTokenizer { .. }) = step {
            step = machine.resume(HostReply::Tokens(vec![0.0]), &budget).unwrap();
        }
        let Step::Done(outcome @ PromptOutcome::Ready(_)) = step else { panic!("{step:?}") };
        assert!(contents(&outcome).contains(&"system:Lead.".to_string()), "{outcome:?}");
        let PromptOutcome::Ready(output) = outcome else { unreachable!() };
        let variables: Vec<_> = output.effects.into_iter().filter(|effect| matches!(effect, Effect::ChatVariable { .. })).collect();
        assert_lore_prefix(&variables, &["__internal_ka_lead"]);
        assert!(machine.take_effects().is_empty());
    }

    #[test]
    fn owned_late_unsupported_keeps_real_lore_effects_without_a_committing_host() {
        for policy in [PromptPolicy::FallbackAllowed, PromptPolicy::Owned] {
            let (mut replay, budget) = real_lore_replay(LORE_LEAD,
                lore_counter(risunest_tokenizer::id::TokenizerId::Cl100kBase));
            replay.stages.triggers = Box::new(DeclinedWhileRunning);
            let mut machine = PromptMachine::with_policy(replay.input, replay.stages, policy);
            let mut step = machine.start(&budget);
            while let Ok(Step::NeedsHost(HostRequest::PluginTokenizer { .. })) = step {
                step = machine.resume(HostReply::Tokens(vec![0.0]), &budget);
            }
            match policy {
                PromptPolicy::FallbackAllowed => assert!(matches!(step, Err(StageError::Unsupported(name)) if name == "trigger-early-return")),
                PromptPolicy::Owned => {
                    let Ok(Step::Done(PromptOutcome::Failed { reason: PromptFailure::Unsupported(name), effects })) = step else { panic!("{step:?}") };
                    assert_eq!(name, "trigger-early-return");
                    assert_lore_prefix(&effects, &["__internal_ka_lead"]);
                }
            }
            assert!(machine.take_effects().is_empty());
            assert!(machine.take_effects().is_empty());
        }
    }

    #[test]
    fn depth_placement_uses_current_chat_length_and_js_splice_bounds() {
        let (replay, budget) = base_replay(|_, _, _| {});
        let shared = Arc::new(Mutex::new(Exchange { budget: Some(budget.clone()), ..Exchange::default() }));
        let mut ctx = Ctx { shared, stages: replay.stages, effects: Vec::new(), committed: false, trigger_started: false };
        let fragment = |position, depth, role, content| risunest_ir::stage::LoreFragment {
            position: Text::from_str(position, &budget).unwrap(), prompt: Text::from_str(content, &budget).unwrap(),
            role, depth, order: 0.0, priority: 0.0, tokens: 0, source: Text::from_str("synthetic", &budget).unwrap(), inject: None,
        };
        let mut prompt = Prompt::default();
        for content in ["A", "B", "C"] {
            let index = prompt.add(message(Role::User, &js::s(content), &budget).unwrap());
            prompt.groups.chats.push(index);
        }
        let lore = Placement::new(vec![
            fragment("depth", -1.0, Role::System, "excluded negative"),
            fragment("depth", f64::NAN, Role::System, "excluded NaN"),
            fragment("depth", 1.9, Role::Assistant, "D"),
            fragment("reverse_depth", 1.5, Role::System, "R"),
            fragment("reverse_depth", -2.0, Role::User, "N"),
            fragment("reverse_depth", 9.0, Role::System, "O"),
            fragment("reverse_depth", f64::INFINITY, Role::User, "I"),
            fragment("reverse_depth", f64::NAN, Role::Assistant, "Q"),
            fragment("depth", f64::INFINITY, Role::User, "Z"),
            fragment("reverse_depth", 0.0, Role::Assistant, "E"),
        ]);
        lore.insert_depth(&mut ctx, &mut prompt).unwrap();
        let contents: Vec<_> = prompt.groups.chats.iter().map(|index| {
            let entry = &prompt.arena[*index];
            (entry.role, String::from_utf16_lossy(&entry.content.units().collect::<Vec<_>>()))
        }).collect();
        assert_eq!(contents, vec![(Role::Assistant, "Q".into()), (Role::User, "I".into()), (Role::User, "A".into()),
            (Role::Assistant, "D".into()), (Role::System, "R".into()), (Role::System, "O".into()), (Role::User, "B".into()),
            (Role::User, "C".into()), (Role::User, "N".into()), (Role::User, "Z".into()), (Role::Assistant, "E".into())]);

        let index = prompt.add(message(Role::System, &js::s("inlay instruction"), &budget).unwrap());
        prompt.groups.post_everything.push(index);
        Placement::new(vec![
            fragment("depth", 0.0, Role::Assistant, "assistant one"),
            fragment("depth", -0.0, Role::System, "system"),
            fragment("depth", 0.0, Role::Assistant, "assistant two"),
            fragment("depth", 0.0, Role::User, "user"),
        ]).depth_zero(&mut ctx, &mut prompt).unwrap();
        let contents: Vec<_> = prompt.groups.post_everything.iter().map(|index| {
            String::from_utf16_lossy(&prompt.arena[*index].content.units().collect::<Vec<_>>())
        }).collect();
        assert_eq!(contents, ["inlay instruction", "system", "user", "assistant one", "assistant two"]);
    }

    #[test]
    fn owned_prompt_reads_stored_lore_but_refuses_selected_modules_before_selection() {
        for field in ["globalLore", "localLore"] {
            let (mut replay, budget) = base_replay(|_, _, _| {});
            set(&mut replay.input.root, "loreBookDepth", "10", &budget);
            set(&mut replay.input.root, "loreBookToken", "1000", &budget);
            set(&mut replay.input.character, "loreSettings", r#"{"recursiveScanning":false}"#, &budget);
            let record = if field == "globalLore" { &mut replay.input.character } else { &mut replay.input.conversation };
            set(record, field, &format!("[{LORE_LEAD}]"), &budget);
            let input = risunest_lorebook::selector::LoreInput::from_records(&replay.input.root,
                &replay.input.character, &replay.input.conversation, &budget).unwrap();
            replay.stages.lore = Box::new(risunest_lorebook::selector::LoreSelection::with_policy(input, Passthrough,
                lore_counter(risunest_tokenizer::id::TokenizerId::Cl100kBase), risunest_regex::engine::JsPatternEngine::default(),
                risunest_ir::rng::Tape::new(Vec::new()), risunest_lorebook::selector::LorePolicy::Owned));
            let mut machine = PromptMachine::with_policy(replay.input, replay.stages, PromptPolicy::Owned);
            let mut step = machine.start(&budget).unwrap();
            while let Step::NeedsHost(HostRequest::PluginTokenizer { .. }) = step {
                step = machine.resume(HostReply::Tokens(vec![0.0]), &budget).unwrap();
            }
            let Step::Done(PromptOutcome::Ready(output)) = step else { panic!("{step:?}") };
            assert!(output.messages.iter().any(|message| message.content.units().collect::<Vec<_>>() == js::s("Lead.")), "{field}");
            let effects: Vec<_> = output.effects.into_iter().filter(|effect| matches!(effect, Effect::ChatVariable { .. })).collect();
            assert_lore_prefix(&effects, &["__internal_ka_lead"]);
            assert!(machine.take_effects().is_empty());
        }
        let (mut replay, budget) = base_replay(|_, _, _| {});
        replay.stages.lore = Box::new(UnreachedLore);
        set(&mut replay.input.root, "modules", r#"[{"id":"selected"}]"#, &budget);
        set(&mut replay.input.character, "modules", r#"["selected"]"#, &budget);
        let mut machine = PromptMachine::with_policy(replay.input, replay.stages, PromptPolicy::Owned);
        let step = machine.start(&budget).unwrap();
        let Step::Done(PromptOutcome::Failed { reason: PromptFailure::Unsupported(name), effects }) = step else { panic!("{step:?}") };
        assert_eq!(name, "modules");
        assert!(effects.is_empty());
        assert!(machine.take_effects().is_empty());
    }

    #[test]
    fn captured_modules_admit_only_supported_contributions_before_lore() {
        use risunest_records::modules::ModuleSources;
        for contribution in [r#""trigger":[{}]"#, r#""trigger":{}"#, r#""assets":[["name","path","ext"]]"#,
            r#""assets":{}"#, r#""customModuleToggle":"toggle""#, r#""mcp":{"url":"https://example.invalid"}"#] {
            let (mut replay, budget) = base_replay(|_, _, _| {});
            set(&mut replay.input.root, "modules", &format!(r#"[{{"id":"selected",{contribution}}}]"#), &budget);
            set(&mut replay.input.character, "modules", r#"["selected"]"#, &budget);
            let sources = ModuleSources::for_conversation(&replay.input.root, &replay.input.character, &replay.input.conversation, &budget).unwrap();
            let modules = SelectedModules::capture(&sources, None, &budget).unwrap();
            assert!(matches!(admit_selected_modules(&modules, &budget), Err(StageError::Unsupported(name)) if name == "modules"));
            replay.stages.lore = Box::new(UnreachedLore);
            let mut machine = PromptMachine::with_captured_modules(replay.input, replay.stages, PromptPolicy::Owned, modules);
            assert!(matches!(machine.start(&budget).unwrap(), Step::Done(PromptOutcome::Failed { reason: PromptFailure::Unsupported(name), effects }) if name == "modules" && effects.is_empty()));
        }
        let (mut replay, budget) = base_replay(|_, _, _| {});
        set(&mut replay.input.root, "modules", r#"[{"id":7,"trigger":[],"assets":[],"customModuleToggle":"","mcp":{},"cjs":"unused","lowLevelAccess":true,"hideIcon":true,"backgroundEmbedding":"unused","regex":"excluded"}]"#, &budget);
        set(&mut replay.input.character, "modules", "7", &budget);
        let sources = ModuleSources::for_conversation(&replay.input.root, &replay.input.character, &replay.input.conversation, &budget).unwrap();
        let modules = SelectedModules::capture(&sources, None, &budget).unwrap();
        admit_selected_modules(&modules, &budget).unwrap();
        let mut machine = PromptMachine::with_captured_modules(replay.input, replay.stages, PromptPolicy::Owned, modules);
        let mut step = machine.start(&budget).unwrap();
        while let Step::NeedsHost(HostRequest::PluginTokenizer { .. }) = step {
            step = machine.resume(HostReply::Tokens(vec![0.0]), &budget).unwrap();
        }
        assert!(matches!(step, Step::Done(PromptOutcome::Ready(_))), "{step:?}");
    }

    #[test]
    fn captured_module_lore_uses_the_same_records_after_the_source_changes() {
        use risunest_records::modules::ModuleSources;
        let (mut replay, budget) = base_replay(|_, _, _| {});
        set(&mut replay.input.root, "loreBookDepth", "10", &budget);
        set(&mut replay.input.root, "loreBookToken", "1000", &budget);
        set(&mut replay.input.character, "loreSettings", r#"{"recursiveScanning":false}"#, &budget);
        set(&mut replay.input.root, "modules", &format!(r#"[{{"id":"selected","lorebook":[{LORE_LEAD}]}}]"#), &budget);
        set(&mut replay.input.character, "modules", r#"["selected"]"#, &budget);
        let sources = ModuleSources::for_conversation(&replay.input.root, &replay.input.character, &replay.input.conversation, &budget).unwrap();
        let modules = SelectedModules::capture(&sources, None, &budget).unwrap();
        set(&mut replay.input.root, "modules", "[]", &budget);
        let input = risunest_lorebook::selector::LoreInput::from_records_with_modules(&replay.input.root,
            &replay.input.character, &replay.input.conversation, &modules, &budget).unwrap();
        replay.stages.lore = Box::new(risunest_lorebook::selector::LoreSelection::with_policy(input, Passthrough,
            lore_counter(risunest_tokenizer::id::TokenizerId::Cl100kBase), risunest_regex::engine::JsPatternEngine::default(),
            risunest_ir::rng::Tape::new(Vec::new()), risunest_lorebook::selector::LorePolicy::Owned));
        let mut machine = PromptMachine::with_captured_modules(replay.input, replay.stages, PromptPolicy::Owned, modules);
        let mut step = machine.start(&budget).unwrap();
        while let Step::NeedsHost(HostRequest::PluginTokenizer { .. }) = step {
            step = machine.resume(HostReply::Tokens(vec![0.0]), &budget).unwrap();
        }
        let Step::Done(PromptOutcome::Ready(output)) = step else { panic!("{step:?}") };
        assert!(output.messages.iter().any(|message| message.content.units().collect::<Vec<_>>() == js::s("Lead.")));
        let effects: Vec<_> = output.effects.into_iter().filter(|effect| matches!(effect, Effect::ChatVariable { .. })).collect();
        assert_lore_prefix(&effects, &["__internal_ka_lead"]);
    }

    #[test]
    fn default_machine_uses_effective_preset_integration_instead_of_root_mirror() {
        for integration in [None, Some("null"), Some(r#""""#)] {
            let (mut replay, budget) = base_replay(|_, _, _| {});
            set(&mut replay.input.root, "modules", r#"[{"id":"stale"}]"#, &budget);
            set(&mut replay.input.root, "moduleIntergration", r#""stale""#, &budget);
            let preset = replay.input.preset.get_or_insert_with(|| parse_json("{}", &budget).unwrap());
            preset.take(b"moduleIntergration", &budget).unwrap();
            if let Some(integration) = integration { set(preset, "moduleIntergration", integration, &budget); }
            let outcome = drive(replay, &budget, |request| panic!("unexpected host request: {request:?}")).unwrap();
            assert!(matches!(outcome, PromptOutcome::Ready(_)), "{outcome:?}");
        }
        let (mut replay, budget) = base_replay(|_, _, _| {});
        set(&mut replay.input.root, "modules", r#"[{"id":"selected"}]"#, &budget);
        let preset = replay.input.preset.get_or_insert_with(|| parse_json("{}", &budget).unwrap());
        set(preset, "moduleIntergration", r#""selected""#, &budget);
        assert_eq!(declined_at_start(replay, &budget), "modules");
    }

    #[test]
    fn default_machine_refuses_canonical_numeric_module_selection() {
        let (mut replay, budget) = base_replay(|_, _, _| {});
        set(&mut replay.input.root, "modules", r#"[{"id":7}]"#, &budget);
        set(&mut replay.input.character, "modules", "7", &budget);
        assert_eq!(declined_at_start(replay, &budget), "modules");
    }

    #[test]
    fn an_unsupported_input_found_after_a_host_request_ends_the_send() {
        // A hook that adds an inlay to each text it gets; the token counts and the hook calls went to the host first.
        let (mut replay, budget) = base_replay(|_, _, _| {});
        replay.stages.scripts = Box::new(HookScripts { hooks: 1, pending: None });
        let outcome = drive(replay, &budget, |request| {
            let HostRequest::PluginHook { payload: Value::Text(text), .. } = request else { panic!("{request:?}") };
            let units = js::concat(&[&text.units().collect::<Vec<_>>(), &js::s("{{inlay::x}}")]);
            HostReply::Value(Value::Text(Text::from_units(&units, &crate::machine::tests::budget()).unwrap()))
        }).unwrap();
        assert!(matches!(&outcome, PromptOutcome::Failed { reason: PromptFailure::Unsupported(name), effects } if name == "inlay" && effects.is_empty()),
            "{outcome:?}");
    }

    /// A start trigger that writes `drained`, asks for a token count, then writes `after` and fails with the reply's
    /// message. Its writes leave only through `take_effects`, as a `TriggerRun`'s do.
    struct FailingCount { effects: Vec<Effect> }
    impl TriggerRunner for FailingCount {
        fn run(&mut self, _: TriggerRequest, budget: &Budget) -> Result<Step<TriggerOutput>, StageError> {
            self.effects.push(Effect::ChatVariable { name: Text::from_str("drained", budget)?, value: Value::Text(Text::from_str("1", budget)?) });
            Ok(Step::NeedsHost(HostRequest::PluginTokenizer { text: Text::from_str("trigger prompt", budget)? }))
        }
        fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<TriggerOutput>, StageError> {
            self.effects.push(Effect::ChatVariable { name: Text::from_str("after", budget)?, value: Value::Text(Text::from_str("2", budget)?) });
            let HostReply::Failed { message } = reply else { return Err(invalid("reply")) };
            Err(StageError::Host(message.units().collect()))
        }
        fn take_effects(&mut self) -> Vec<Effect> { std::mem::take(&mut self.effects) }
    }

    /// A start trigger whose admission declines. Its run declines the same way, as a `TriggerRun`'s does.
    struct DeclinedStart;
    impl TriggerRunner for DeclinedStart {
        fn run(&mut self, _: TriggerRequest, _: &Budget) -> Result<Step<TriggerOutput>, StageError> { Err(unsupported("trigger-effect-v2UpdateGUI")) }
        fn resume(&mut self, _: HostReply, _: &Budget) -> Result<Step<TriggerOutput>, StageError> { Err(invalid("resume")) }
        fn admit(&self, request: &TriggerRequest, _: &Budget) -> Result<(), StageError> {
            assert_eq!(request.mode, TriggerMode::Start);
            Err(unsupported("trigger-effect-v2UpdateGUI"))
        }
    }

    #[test]
    fn a_start_trigger_that_declines_in_admission_declines_the_send_before_any_host_request() {
        // The base replay counts tokens through the host before the start trigger runs.
        let (mut replay, budget) = base_replay(|_, _, _| {});
        replay.stages.triggers = Box::new(DeclinedStart);
        let mut machine = PromptMachine::new(replay.input, replay.stages);
        let result = machine.start(&budget);
        assert!(matches!(&result, Err(StageError::Unsupported(name)) if name == "trigger-effect-v2UpdateGUI"), "{result:?}");
        assert!(machine.take_effects().is_empty());
    }

    /// A start trigger that admits and then meets an input it does not run, as a `TriggerRun` does with a variable name
    /// CBS produced.
    struct DeclinedWhileRunning;
    impl TriggerRunner for DeclinedWhileRunning {
        fn run(&mut self, _: TriggerRequest, _: &Budget) -> Result<Step<TriggerOutput>, StageError> { Err(unsupported("trigger-early-return")) }
        fn resume(&mut self, _: HostReply, _: &Budget) -> Result<Step<TriggerOutput>, StageError> { Err(invalid("resume")) }
    }

    /// CBS whose first prompt parse outside a message also writes a variable; a write in the current-chat pass would
    /// refuse the send as `historyRewrite` before any token count.
    struct FirstParseWrites { inner: Box<dyn TextProcessor + Send>, written: Arc<std::sync::atomic::AtomicBool> }
    impl TextProcessor for FirstParseWrites {
        fn process(&mut self, input: &Text, context: &TextContext, budget: &Budget) -> Result<TextOutput, StageError> {
            let mut output = self.inner.process(input, context, budget)?;
            if !context.run_var && context.chat_index == -1 && !self.written.swap(true, std::sync::atomic::Ordering::SeqCst) {
                output.effects.push(Effect::ChatVariable { name: Text::from_str("parsed", budget)?, value: Value::Text(Text::from_str("1", budget)?) });
            }
            Ok(output)
        }
        fn apply(&mut self, effect: &Effect, budget: &Budget) -> Result<(), StageError> { self.inner.apply(effect, budget) }
    }

    #[test]
    fn a_decline_after_only_token_counts_leaves_the_send_to_ts() {
        // The base replay counts tokens through the plugin tokenizer before the start trigger runs, and CBS has written
        // a variable by then. TS can count again and makes the write again, so the decline still leaves the send to TS.
        let (mut replay, budget) = base_replay(|_, _, _| {});
        replay.stages.triggers = Box::new(DeclinedWhileRunning);
        let inner = std::mem::replace(&mut replay.stages.text, Box::new(Passthrough));
        let written = Arc::new(std::sync::atomic::AtomicBool::new(false));
        replay.stages.text = Box::new(FirstParseWrites { inner, written: written.clone() });
        let mut machine = PromptMachine::new(replay.input, replay.stages);
        let mut counts = 0;
        let mut step = machine.start(&budget);
        while let Ok(Step::NeedsHost(HostRequest::PluginTokenizer { .. })) = step {
            counts += 1;
            step = machine.resume(HostReply::Tokens(vec![0.0]), &budget);
        }
        assert!(counts > 0, "no token count went out before the trigger: {step:?}");
        assert!(written.load(std::sync::atomic::Ordering::SeqCst), "CBS wrote nothing before the trigger");
        assert!(matches!(&step, Err(StageError::Unsupported(name)) if name == "trigger-early-return"), "{step:?}");
        assert!(machine.take_effects().is_empty());
        // After a plugin hook, which TS would call again, the same decline ends the send with the write CBS made.
        let (mut replay, budget) = base_replay(|_, _, _| {});
        replay.stages.triggers = Box::new(DeclinedWhileRunning);
        replay.stages.scripts = Box::new(HookScripts { hooks: 1, pending: None });
        let inner = std::mem::replace(&mut replay.stages.text, Box::new(Passthrough));
        replay.stages.text = Box::new(FirstParseWrites { inner, written: Default::default() });
        let outcome = drive(replay, &budget, |request| match request {
            HostRequest::PluginHook { payload, .. } => HostReply::Value(payload),
            other => panic!("{other:?}"),
        }).unwrap();
        let PromptOutcome::Failed { reason: PromptFailure::Unsupported(name), effects } = &outcome else { panic!("{outcome:?}") };
        assert_eq!(name, "trigger-early-return");
        let written: Vec<(String, String)> = effects.iter().filter_map(|effect| match effect {
            Effect::ChatVariable { name, value: Value::Text(value) } => Some((String::from_utf16_lossy(&name.units().collect::<Vec<_>>()),
                String::from_utf16_lossy(&value.units().collect::<Vec<_>>()))),
            _ => None,
        }).collect();
        assert_eq!(written, [("parsed".to_string(), "1".to_string())], "{effects:?}");
    }

    /// The variables CBS has been given, by name.
    struct AppliedText { inner: Box<dyn TextProcessor + Send>, applied: Arc<Mutex<Vec<String>>> }
    impl TextProcessor for AppliedText {
        fn process(&mut self, input: &Text, context: &TextContext, budget: &Budget) -> Result<TextOutput, StageError> {
            self.inner.process(input, context, budget)
        }
        fn apply(&mut self, effect: &Effect, budget: &Budget) -> Result<(), StageError> {
            if let Effect::ChatVariable { name, .. } = effect { self.applied.lock().unwrap().push(String::from_utf16_lossy(&name.units().collect::<Vec<_>>())); }
            self.inner.apply(effect, budget)
        }
    }

    /// A start trigger that reads what it was given before it ran: its `start` system prompt names each change, and it
    /// writes `own` when it runs.
    struct ReadsApplied { applied: Vec<String>, ran: bool }
    impl TriggerRunner for ReadsApplied {
        fn run(&mut self, request: TriggerRequest, budget: &Budget) -> Result<Step<TriggerOutput>, StageError> {
            self.ran = true;
            let empty = || Text::from_str("", budget);
            let start = Text::from_str(&format!("applied: {}", self.applied.join(",")), budget)?;
            let own = Effect::ChatVariable { name: Text::from_str("own", budget)?, value: Value::Text(Text::from_str("1", budget)?) };
            Ok(Step::Done(TriggerOutput { system_prompt_start: start, system_prompt_history_end: empty()?, system_prompt_end: empty()?,
                effects: vec![own], tokens: 0, stop_sending: false, send_ai_prompt: false, display: None, temp_vars: request.temp_vars,
                current_trigger_id: None }))
        }
        fn resume(&mut self, _: HostReply, _: &Budget) -> Result<Step<TriggerOutput>, StageError> { Err(invalid("resume")) }
        fn apply(&mut self, effect: &Effect, _: &Budget) -> Result<(), StageError> {
            assert!(!self.ran, "a change after the run started: {effect:?}");
            self.applied.push(match effect {
                Effect::ChatVariable { name, .. } => String::from_utf16_lossy(&name.units().collect::<Vec<_>>()),
                other => format!("{other:?}"),
            });
            Ok(())
        }
    }

    #[test]
    fn the_start_trigger_reads_the_writes_made_before_it() {
        // TS's current-chat pass stores its CBS writes before the start trigger runs (`runCurrentChatFunction`,
        // `src/ts/process/index.svelte.ts:1322`; `runTrigger` at `:1863`), and the trigger reads them through the session
        // (`getVar`, `src/ts/process/triggers.ts:1339-1345`; `scriptConversationAccess.ts:93-104`). The trigger's own
        // writes and `lastMemory`, recorded after it ran, are not handed back.
        let (mut replay, budget) = base_replay(|_, _, _| {});
        replay.stages.triggers = Box::new(ReadsApplied { applied: Vec::new(), ran: false });
        let inner = std::mem::replace(&mut replay.stages.text, Box::new(Passthrough));
        replay.stages.text = Box::new(FirstParseWrites { inner, written: Default::default() });
        let outcome = drive(replay, &budget, |request| panic!("{request:?}")).unwrap();
        let contents = contents(&outcome);
        assert!(contents.contains(&"system:applied: parsed".to_string()), "{contents:?}");
    }

    #[test]
    fn the_real_start_trigger_reads_the_writes_made_before_it() {
        // The same hand-off as above, through `start_triggers`: CBS writes `parsed` in the prompt parse, the machine
        // absorbs it before the trigger starts, and the real runner's `var` condition sees it as `scriptstate.$parsed`.
        let trigger = r#"[{"comment":"","type":"start","conditions":[{"type":"var","var":"parsed","operator":"=","value":"1"}],"effect":[{"type":"setvar","operator":"=","var":"seen","value":"yes"}]}]"#;
        let seen = |writes_first: bool| {
            let (mut replay, budget) = base_replay(|character, _, budget| set(character, "triggerscript", trigger, budget));
            replay.stages.triggers = start_triggers(&replay.input, &budget);
            if writes_first {
                let inner = std::mem::replace(&mut replay.stages.text, Box::new(Passthrough));
                replay.stages.text = Box::new(FirstParseWrites { inner, written: Default::default() });
            } else {
                replay.stages.text = Box::new(Passthrough);
            }
            let PromptOutcome::Ready(output) = drive(replay, &budget, |request| panic!("{request:?}")).unwrap() else { panic!("not ready") };
            output.effects.iter().any(|effect| matches!(effect, Effect::ChatVariable { name, value: Value::Text(value) }
                if name.equals_ascii(b"seen") && value.equals_ascii(b"yes")))
        };
        assert!(seen(true), "the trigger did not read the write made before it");
        assert!(!seen(false), "the trigger ran without a write before it");
    }

    #[test]
    fn a_trigger_that_fails_after_a_host_request_keeps_the_writes_ts_stores() {
        let (mut replay, budget) = base_replay(|_, _, _| {});
        let applied = Arc::new(Mutex::new(Vec::new()));
        let inner = std::mem::replace(&mut replay.stages.text, Box::new(Passthrough));
        replay.stages.text = Box::new(AppliedText { inner, applied: applied.clone() });
        replay.stages.triggers = Box::new(FailingCount { effects: Vec::new() });
        let mut machine = PromptMachine::new(replay.input, replay.stages);
        let mut step = machine.start(&budget);
        let result = loop {
            step = match step {
                Ok(Step::NeedsHost(HostRequest::PluginTokenizer { text })) if text.equals_ascii(b"trigger prompt") => {
                    // The write made before the request reaches CBS before the host answers.
                    assert!(applied.lock().unwrap().iter().any(|name| name == "drained"), "{applied:?}");
                    machine.resume(HostReply::Failed { message: Text::from_str("tokenizer down", &budget).unwrap() }, &budget)
                }
                Ok(Step::NeedsHost(HostRequest::PluginTokenizer { .. })) => machine.resume(HostReply::Tokens(vec![0.0]), &budget),
                other => break other,
            };
        };
        assert!(matches!(&result, Err(StageError::Host(message)) if *message == js::s("tokenizer down")), "{result:?}");
        let written: Vec<String> = machine.take_effects().iter().filter_map(|effect| match effect {
            Effect::ChatVariable { name, .. } => Some(String::from_utf16_lossy(&name.units().collect::<Vec<_>>())),
            _ => None,
        }).collect();
        assert_eq!(written, ["drained", "after"]);
        assert!(machine.take_effects().is_empty());
    }

    #[test]
    fn start_while_a_host_request_is_pending_keeps_the_run() {
        let (fresh, budget) = base_replay(|_, _, _| {});
        let expected = contents(&drive(fresh, &budget, |request| panic!("{request:?}")).unwrap());
        let (replay, _) = base_replay(|_, _, _| {});
        let mut machine = PromptMachine::new(replay.input, replay.stages);
        let Ok(Step::NeedsHost(HostRequest::PluginTokenizer { .. })) = machine.start(&budget) else { panic!("a token count first") };
        assert!(matches!(machine.start(&budget), Err(StageError::Invalid(_))));
        let mut step = machine.resume(HostReply::Tokens(vec![0.0]), &budget).unwrap();
        while let Step::NeedsHost(request) = step {
            assert!(matches!(request, HostRequest::PluginTokenizer { .. }), "{request:?}");
            step = machine.resume(HostReply::Tokens(vec![0.0]), &budget).unwrap();
        }
        let Step::Done(outcome) = step else { unreachable!() };
        assert_eq!(contents(&outcome), expected);
    }

    // Contract tests: TS reads these as `String(value)` or optional-chained, but its normalization never stores such
    // values, so no recorded send reaches them.
    #[test]
    fn non_string_prompt_fields_read_as_their_string_form() {
        let (mut replay, budget) = base_replay(|_, _, _| {});
        // Without a selected preset the stored root fields are read as they are.
        replay.input.preset = None;
        for (key, json) in [("additionalPrompt", "null"), ("descriptionPrefix", "7"), ("promptPreprocess", "true"), ("mainPrompt", "[1,2]"),
            ("promptTemplate", "null"), ("promptSettings", "null")] {
            set(&mut replay.input.root, key, json, &budget);
        }
        let contents = contents(&drive(replay, &budget, |request| panic!("{request:?}")).unwrap());
        assert_eq!(contents[0], "system:1,2\nnull");
        assert!(contents.contains(&"system:7Synthetic description.".to_string()), "{contents:?}");
        assert!(contents.contains(&"system:[Start a new chat]".to_string()), "{contents:?}");
    }

    /// CBS that leaves every text as it is.
    struct Passthrough;
    impl TextProcessor for Passthrough {
        fn process(&mut self, input: &Text, _: &TextContext, budget: &Budget) -> Result<TextOutput, StageError> {
            Ok(TextOutput { text: input.copy(budget)?, effects: Vec::new() })
        }
        fn apply(&mut self, _: &Effect, _: &Budget) -> Result<(), StageError> { Ok(()) }
    }
    /// CBS that changes the texts it parses with `runVar`.
    struct Rewrite;
    impl TextProcessor for Rewrite {
        fn process(&mut self, input: &Text, context: &TextContext, budget: &Budget) -> Result<TextOutput, StageError> {
            let mut units: Vec<u16> = input.units().collect();
            if context.run_var { units.push(0x21); }
            Ok(TextOutput { text: Text::from_units(&units, budget)?, effects: Vec::new() })
        }
        fn apply(&mut self, _: &Effect, _: &Budget) -> Result<(), StageError> { Ok(()) }
    }
}
