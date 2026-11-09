//! `runTrigger` (`src/ts/process/triggers.ts:1129-3007`) for the start, input, output and manual events.
//!
//! Admission compiles every trigger the run can reach, through `v2RunTrigger` included, and declines what this stage
//! does not run before anything else happens. The triggers then run with CBS through the scoped processor, regular
//! expressions through the pattern engine and `Math.random` through the runner's source. The host is reached for token
//! counts, a `v2RunLLM` or `runLLM` model request and a `v2CheckSimilarity` or `checkSimilarity` search, where the run
//! waits with its state held by the runner, and for the system prompt counts TS makes at the end of every invocation.
//! The model request and the search's
//! embeddings commit (`HostKind::commits`).
//!
//! [`admit`] is that admission alone, for an owner that has to know before its own first request: the output trigger
//! runs after the provider request, so its owner admits it over the records it holds before sending. `run` repeats
//! every check over the records it runs on.
//!
//! A decline is `StageError::Unsupported` and leaves nothing behind, so the owner runs TS on the original data. A
//! decline is allowed until a host request TS could not repeat has gone out (`HostKind::commits`), and the plugin
//! tokenizer counts this stage requests are not one. Effects leave the run only through `take_effects` while a request
//! is pending and through `Done`, so a condition the run itself reaches before then (a CBS result, a parsed variable, a
//! value the stage IR cannot carry, a count that declines) discards the pending effects and console output, and the
//! owner drops the ones it took. The processor saw those writes through `apply` and is discarded with the run. After a
//! request that commits the same condition is `StageError::Invalid` with the same name, a terminal failure. On a
//! terminal failure `take_effects` returns the variable and author note writes and drops the message edits, as TS
//! commits a failed run without its messages (`commitWithoutMessages`, `:3000-3004`).

use crate::input::{MessageIds, TriggerInput};
use crate::js::{
    array, case, code_points, concat, copy_value, count_text, equals, has_own, index_of, into_elements, join, literal,
    number_index, number_of, own_entries, parse, parse_float, property, relative, same_value_zero, split, strict, stringify,
    text, text_array, to_string, units, unsupported, Property,
};
use crate::script::{self, bracketed, Body, Condition, Left, Location, Op, Program, Selector, Src, VarType};
use crate::vars::{identical, Locals, Var, Variables};
use risunest_cbs_experimental::cbs::{self, calc_string, js_string};
use risunest_ir::budget::{Budget, Error, OwnedVec};
use risunest_ir::chat::{MessageRole, PromptMessage, Role};
use risunest_ir::chat_ml::parse_chat_ml;
use risunest_ir::num::{number_to_text, to_number};
use risunest_ir::provider::OutputShape;
use risunest_ir::rng::Rng;
use risunest_ir::stage::{
    Effect, HostReply, HostRequest, MessageEdit, PatternEngine, ScopedTextProcessor, StageError, Step, SubRequest, TextContext,
    TextScope, TextVariables, TokenCounter, TokenInput, TriggerCbsVariables, TriggerMode, TriggerOutput, TriggerRequest,
    TriggerRunner,
};
use risunest_ir::text::Text;
use risunest_ir::value::Value;
use std::borrow::Cow;
use std::collections::HashMap;

/// `new HypaProcesser()`, `addText(documents)` and `similaritySearch(query)` (`src/ts/process/memory/hypamemory.ts`) as
/// the owner configures them: the documents ordered by their similarity to `query`. Its embedding requests commit.
pub trait SimilaritySearch {
    fn search(&mut self, documents: Vec<Text>, query: Text, budget: &Budget) -> Result<Step<Vec<Text>>, StageError>;
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<Vec<Text>>, StageError>;
}

/// What a run's dependencies decide before it starts: the counter's own decline ([`TokenCounter::decline`]), whether
/// a [`SimilaritySearch`] is wired, whether the owner answers [`HostRequest::Llm`], and whether it acts on the
/// `stop_sending` and `send_ai_prompt` outcomes. TS reads `stopSending` after the start trigger
/// (`src/ts/process/index.svelte.ts:1901`) and `sendAIprompt` after the output trigger
/// (`src/ts/process/generationResponseApplication.ts:294`, `:545`). The default has no search and no model host and
/// acts on both outcomes.
#[derive(Debug, Clone, Copy)]
pub struct Dependencies {
    pub tokenizer: Option<&'static str>,
    pub similarity: bool,
    /// Without it a low-level `v2RunLLM` or `runLLM` declines (`trigger-effect-v2RunLLM`, `trigger-effect-runLLM`).
    pub llm: bool,
    /// Without it `v2StopPromptSending` or v1 `stop` declines (`trigger-stop-sending`).
    pub stop_sending: bool,
    /// Without it a low-level `v2SendAIprompt` or v1 `sendAIprompt` declines (`trigger-send-ai-prompt`).
    pub send_ai_prompt: bool,
}

impl Default for Dependencies {
    fn default() -> Self { Self { tokenizer: None, similarity: false, llm: false, stop_sending: true, send_ai_prompt: true } }
}

/// One `runTrigger` call over a [`TriggerInput`]: build it per run, call [`TriggerRunner::run`] once and drive it
/// to the end. `P` evaluates CBS against the trigger variables, `C` counts tokens, `E` compiles regular expressions,
/// `R` is `Math.random` and `I` draws the ids of appended messages. Without [`TriggerRun::with_similarity`] a run that
/// reaches `v2CheckSimilarity` declines, and without [`TriggerRun::with_llm`] one that reaches `v2RunLLM` does.
pub struct TriggerRun<P, C, E, R, I> {
    input: TriggerInput,
    processor: P,
    counter: C,
    patterns: E,
    rng: R,
    ids: I,
    phase: Phase,
    effects: Vec<Effect>,
    console: Vec<Value>,
    /// A request TS could not repeat has gone out (`risunest_ir::stage::HostKind::commits`).
    committed: bool,
    similarity: Option<Box<dyn SimilaritySearch + Send>>,
    llm: bool,
    stop_sending: bool,
    send_ai_prompt: bool,
    /// From the start of the effects to their end.
    running: Option<Running>,
    tokens: Option<Tokens>,
}

/// A run between its start and the end of its effects, kept by the runner while an effect waits for the host.
struct Running {
    mode: &'static str,
    program: Program,
    top: Selector,
    temp_vars: Option<Value>,
    /// Out of the runner while the interpreter holds it.
    state: Option<State>,
    waiting: Option<Waiting>,
}

/// What a waiting run does with the host's answer.
enum Waiting {
    /// A `v2Tokenize` count, for the variable TS parsed before it awaited `tokenize`.
    Count(Text),
    /// A `v2RunLLM` reply, stored under this `outputVar`, which TS parses once the reply is in; or, `legacy`, a
    /// `runLLM` reply stored under its `inputVar` as written.
    Llm { output: Text, legacy: bool },
    /// A `v2CheckSimilarity` or `checkSimilarity` search, stored the same way.
    Similarity { output: Text, legacy: bool },
}

#[derive(Clone, Copy)]
enum Wait { Count, Llm, Similarity }

impl Waiting {
    fn wait(&self) -> Wait {
        match self { Self::Count(_) => Wait::Count, Self::Llm { .. } => Wait::Llm, Self::Similarity { .. } => Wait::Similarity }
    }
}

/// The host's answer to a waiting effect.
enum Answer { Count(u32), Llm(HostReply), Similarity(Vec<Text>) }

/// What the interpreter carries from one call to the next.
struct State { stack: Vec<Invocation>, chat: Chat, vars: Variables, system: [Text; 3], current_id: Option<Text>, queue: Vec<Text> }

#[derive(Debug, Clone, Copy)]
enum Phase { Ready, Host, Done, Declined, Failed }

struct Tokens {
    /// The system prompt texts in the order TS tokenizes them: each nested invocation's, then the run's own.
    queue: Vec<Text>,
    next: usize,
    /// The run's own texts start here; nested counts are not added (`:1980-1985`).
    counted_from: usize,
    total: u32,
    output: Option<TriggerOutput>,
}

impl<P, C, E, R, I> TriggerRun<P, C, E, R, I>
where P: ScopedTextProcessor, C: TokenCounter, E: PatternEngine, R: Rng, I: MessageIds {
    pub fn new(input: TriggerInput, processor: P, counter: C, patterns: E, rng: R, ids: I) -> Self {
        Self { input, processor, counter, patterns, rng, ids, phase: Phase::Ready, effects: Vec::new(), console: Vec::new(),
            committed: false, similarity: None, llm: false, stop_sending: true, send_ai_prompt: true, running: None, tokens: None }
    }

    /// The search `v2CheckSimilarity` runs.
    pub fn with_similarity(mut self, search: Box<dyn SimilaritySearch + Send>) -> Self {
        self.similarity = Some(search);
        self
    }

    /// The owner answers the [`HostRequest::Llm`] a low-level `v2RunLLM` or `runLLM` makes.
    pub fn with_llm(mut self) -> Self {
        self.llm = true;
        self
    }

    /// The owner does not act on [`TriggerOutput::stop_sending`], so a run that can set it declines in admission.
    pub fn without_stop_sending(mut self) -> Self {
        self.stop_sending = false;
        self
    }

    /// The owner does not act on [`TriggerOutput::send_ai_prompt`], so a run that can set it declines in admission.
    pub fn without_send_ai_prompt(mut self) -> Self {
        self.send_ai_prompt = false;
        self
    }

    fn dependencies(&self) -> Dependencies {
        Dependencies { tokenizer: self.counter.decline(), similarity: self.similarity.is_some(), llm: self.llm,
            stop_sending: self.stop_sending, send_ai_prompt: self.send_ai_prompt }
    }

    /// The values `v2ConsoleLog` passes to `console.log`, in order. The stage IR has no log channel, so the owner prints
    /// them.
    pub fn console_log(&self) -> &[Value] { &self.console }

    pub fn into_parts(self) -> (P, C, E, R, I) { (self.processor, self.counter, self.patterns, self.rng, self.ids) }

    fn settle(&mut self, result: Result<Step<TriggerOutput>, StageError>) -> Result<Step<TriggerOutput>, StageError> {
        if !matches!(result, Ok(Step::NeedsHost(_))) { self.running = None; }
        match result {
            Ok(Step::NeedsHost(request)) => {
                self.phase = Phase::Host;
                self.committed |= request.kind().commits();
                Ok(Step::NeedsHost(request))
            }
            Ok(Step::Done(output)) => {
                self.phase = Phase::Done;
                Ok(Step::Done(output))
            }
            // Before a request that commits, the owner drops the effects it took and the pending ones are dropped here.
            Err(StageError::Unsupported(name)) if !self.committed => {
                self.phase = Phase::Declined;
                self.tokens = None;
                self.effects.clear();
                self.console.clear();
                Err(StageError::Unsupported(name))
            }
            Err(StageError::Unsupported(name)) => {
                self.phase = Phase::Failed;
                Err(StageError::Invalid(name))
            }
            Err(error) => {
                self.phase = Phase::Failed;
                Err(error)
            }
        }
    }

    fn start(&mut self, request: TriggerRequest, budget: &Budget) -> Result<Step<TriggerOutput>, StageError> {
        let admitted = admission(&self.input, &request, self.dependencies(), budget)?;
        let empty = || literal("", budget);
        let Some(Admitted { mode, program, top, vars, chat }) = admitted else {
            // `runTrigger` returns null before it sets the trigger ID.
            let previous = copy_text(self.input.previous_trigger_id.as_ref(), budget)?;
            return Ok(Step::Done(TriggerOutput { system_prompt_start: empty()?, system_prompt_history_end: empty()?,
                system_prompt_end: empty()?, effects: Vec::new(), tokens: 0, stop_sending: false, send_ai_prompt: false,
                display: None, temp_vars: request.temp_vars, current_trigger_id: previous }));
        };
        let current_id = copy_text(request.trigger_id.as_ref().filter(|id| id.len_units() != 0), budget)?;
        let state = State { stack: vec![Invocation::new(Target::Top, 0, false)], chat, vars, system: [empty()?, empty()?, empty()?],
            current_id, queue: Vec::new() };
        self.running = Some(Running { mode, program, top, temp_vars: request.temp_vars, state: Some(state), waiting: None });
        self.interpret(None, budget)
    }

    /// Runs the effects on from where the run stands, first storing `answer` for the effect that waited for it.
    fn interpret(&mut self, answer: Option<Answer>, budget: &Budget) -> Result<Step<TriggerOutput>, StageError> {
        let running = self.running.as_mut().ok_or_else(|| invalid("trigger-run"))?;
        let Running { program, top, state, waiting, .. } = running;
        let State { mut stack, chat, vars, system, current_id, queue } = state.take().ok_or_else(|| invalid("trigger-run"))?;
        // Every effect the runner still holds has reached the processor.
        let applied = self.effects.len();
        let mut run = Interp {
            program, top, input: &self.input, chat, vars, system, effects: &mut self.effects, applied, processor: &mut self.processor,
            counter: &mut self.counter, similarity: self.similarity.as_deref_mut(), patterns: &mut self.patterns, rng: &mut self.rng,
            ids: &mut self.ids, console: &mut self.console, current_id, queue, budget, called: false, waiting: None,
        };
        if let Some(answer) = answer {
            let waited = waiting.take().ok_or_else(|| invalid("trigger-waiting"))?;
            let frame = &mut stack.last_mut().ok_or_else(|| invalid("trigger-stack"))?.frame;
            run.answer(frame, waited, answer)?;
        }
        let flow = run.invoke(&mut stack)?;
        let Interp { chat, vars, system, current_id, queue, .. } = run;
        let invoked = match flow {
            Flow::Waiting { waiting: next, request } => {
                *state = Some(State { stack, chat, vars, system, current_id, queue });
                *waiting = Some(next);
                return Ok(Step::NeedsHost(request));
            }
            Flow::Done(invoked) => invoked,
        };
        let Running { mode, temp_vars, .. } = self.running.take().ok_or_else(|| invalid("trigger-run"))?;
        let [start, history_end, end] = system;
        let mut queue = queue;
        let counted_from = queue.len();
        for text in [&start, &history_end, &end] {
            if text.len_units() != 0 { queue.push(text.copy(budget)?); }
        }
        // Only a manual run keeps the ID it set (`:2985-2987`).
        let current_trigger_id = if mode == "manual" { current_id } else { copy_text(self.input.previous_trigger_id.as_ref(), budget)? };
        let output = TriggerOutput { system_prompt_start: start, system_prompt_history_end: history_end, system_prompt_end: end,
            effects: Vec::new(), tokens: 0, stop_sending: invoked.stop, send_ai_prompt: invoked.send_ai, display: None,
            temp_vars, current_trigger_id };
        self.tokens = Some(Tokens { queue, next: 0, counted_from, total: 0, output: Some(output) });
        self.count(budget)
    }

    fn count(&mut self, budget: &Budget) -> Result<Step<TriggerOutput>, StageError> {
        loop {
            let tokens = self.tokens.as_mut().ok_or(StageError::Invalid(Cow::Borrowed("trigger-tokens")))?;
            if tokens.next == tokens.queue.len() {
                let mut output = tokens.output.take().ok_or(StageError::Invalid(Cow::Borrowed("trigger-tokens")))?;
                output.tokens = tokens.total;
                output.effects = std::mem::take(&mut self.effects);
                self.tokens = None;
                return Ok(Step::Done(output));
            }
            match self.counter.count(TokenInput::Text(&tokens.queue[tokens.next]), budget)? {
                Step::Done(count) => self.counted(count),
                Step::NeedsHost(request) => return Ok(Step::NeedsHost(request)),
            }
        }
    }

    fn counted(&mut self, count: u32) {
        if let Some(tokens) = self.tokens.as_mut() {
            if tokens.next >= tokens.counted_from { tokens.total = tokens.total.saturating_add(count); }
            tokens.next += 1;
        }
    }
}

/// What admission hands the run: the event, the compiled program with its top selection, and the conversation state.
struct Admitted { mode: &'static str, program: Program, top: Selector, vars: Variables, chat: Chat }

/// The event, the compiled program and its top selection of one request, before any dependency is consulted.
struct Compiled { mode: &'static str, program: Program, top: Selector }

// The declines the request and the stored triggers decide; `None` when no trigger is stored.
fn compile(input: &TriggerInput, request: &TriggerRequest, budget: &Budget) -> Result<Option<Compiled>, StageError> {
    budget.check()?;
    let mode = match request.mode {
        TriggerMode::Start => "start",
        TriggerMode::Manual => "manual",
        TriggerMode::Output => "output",
        TriggerMode::Input => "input",
        TriggerMode::Display | TriggerMode::Request => return Err(unsupported("trigger-display-mode")),
    };
    if request.display_mode { return Err(unsupported("trigger-display-mode")); }
    if matches!(request.cbs_variables, TriggerCbsVariables::Conversation) { return Err(unsupported("trigger-cbs-variables")); }
    let top = match request.manual_name.as_ref().filter(|name| name.len_units() != 0) {
        Some(name) => Selector::Name(name.copy(budget)?),
        None => Selector::Mode(mode),
    };
    Ok(script::admit(input, top, budget)?.map(|(program, top)| Compiled { mode, program, top }))
}

/// Whether an effect of a trigger the run can reach matches; only reached triggers are compiled.
fn reachable(program: &Program, matches: fn(&Op) -> bool) -> bool {
    program.triggers.iter().filter_map(|trigger| trigger.body.as_ref()).any(|body| body.effects.iter().any(|effect| matches(&effect.op)))
}

// `v2Tokenize` counts with the counter, and so do the system prompts at the end of every invocation that sets one.
fn counts(op: &Op) -> bool { matches!(op, Op::Tokenize { .. } | Op::SystemPrompt { .. }) }

// Every decline the records and the run's dependencies decide before the first effect; `None` when no trigger is
// stored, where `runTrigger` returns null.
fn admission(input: &TriggerInput, request: &TriggerRequest, dependencies: Dependencies, budget: &Budget)
    -> Result<Option<Admitted>, StageError> {
    let Some(Compiled { mode, program, top }) = compile(input, request, budget)? else { return Ok(None) };
    let reaches = |matches: fn(&Op) -> bool| reachable(&program, matches);
    if reaches(|op| matches!(op, Op::FirstMessage { .. })) { greeting_shape(input)?; }
    if let Some(name) = dependencies.tokenizer {
        if reaches(counts) { return Err(StageError::Unsupported(Cow::Borrowed(name))); }
    }
    if !dependencies.similarity {
        if reaches(|op| matches!(op, Op::CheckSimilarity { legacy: false, .. })) { return Err(unsupported("trigger-effect-v2CheckSimilarity")); }
        if reaches(|op| matches!(op, Op::CheckSimilarity { legacy: true, .. })) { return Err(unsupported("trigger-effect-checkSimilarity")); }
    }
    // Only a low-level trigger compiles these; without low-level access each is a no-op.
    if !dependencies.llm {
        if reaches(|op| matches!(op, Op::RunLlm { legacy: false, .. })) { return Err(unsupported("trigger-effect-v2RunLLM")); }
        if reaches(|op| matches!(op, Op::RunLlm { legacy: true, .. })) { return Err(unsupported("trigger-effect-runLLM")); }
    }
    if !dependencies.stop_sending && reaches(|op| matches!(op, Op::StopSending)) { return Err(unsupported("trigger-stop-sending")); }
    if !dependencies.send_ai_prompt && reaches(|op| matches!(op, Op::SendAiPrompt)) { return Err(unsupported("trigger-send-ai-prompt")); }
    let mut vars = Variables::new(&input.scriptstate, &input.default_variables, &input.template_default_variables, budget)?;
    let chat = Chat::new(input, &mut vars, budget)?;
    Ok(Some(Admitted { mode, program, top, vars, chat }))
}

/// The stored shapes `v2GetFirstMessage` declines, for a program that reaches one: `alternateGreetings` neither an
/// array nor nullish, or an array read with an `fmIndex` object or array, unless `fmIndex` is -1.
fn greeting_shape(input: &TriggerInput) -> Result<(), StageError> {
    if matches!(input.first_message_index, Value::Number(value) if value == -1.0) { return Ok(()); }
    match (&input.alternate_greetings, &input.first_message_index) {
        // `undefined[fmIndex]` throws while running, as in TS.
        (Value::Undefined | Value::Null, _) => Ok(()),
        (Value::Array(_), Value::Object(_) | Value::Array(_)) => Err(unsupported("trigger-conversation-shape")),
        (Value::Array(_), _) => Ok(()),
        _ => Err(unsupported("trigger-character-shape")),
    }
}

/// The admission of `runTrigger(char, mode, arg)` over `input` without running it: the `StageError::Unsupported` that
/// [`TriggerRunner::run`] would return before its first effect or host request with `dependencies`, or `Ok` when it
/// would start. Every trigger the request can reach is compiled, through `v2RunTrigger` and module triggers included.
/// A decline the run meets while running, such as a CBS result the stage cannot reproduce, is not among them.
pub fn admit(input: &TriggerInput, request: &TriggerRequest, dependencies: Dependencies, budget: &Budget) -> Result<(), StageError> {
    admission(input, request, dependencies, budget).map(|_| ())
}

/// Whether `runTrigger(char, mode, arg)` over `input` can count tokens: a trigger it can reach, through `v2RunTrigger`
/// and module triggers included, has a `v2Tokenize` or sets a system prompt, which TS counts at the end of the
/// invocation. It reads the program [`admit`] compiles, so an owner that counts only approximately in admission can
/// count exactly when the run would branch on a count. `Ok(false)` when no trigger is stored; an error is a decline
/// admission gives from the request and the stored triggers alone.
pub fn reaches_counts(input: &TriggerInput, request: &TriggerRequest, budget: &Budget) -> Result<bool, StageError> {
    Ok(compile(input, request, budget)?.is_some_and(|compiled| reachable(&compiled.program, counts)))
}

impl<P, C, E, R, I> TriggerRunner for TriggerRun<P, C, E, R, I>
where P: ScopedTextProcessor, C: TokenCounter, E: PatternEngine, R: Rng, I: MessageIds {
    fn admit(&self, request: &TriggerRequest, budget: &Budget) -> Result<(), StageError> {
        admit(&self.input, request, self.dependencies(), budget)
    }

    /// Before the run, a change another stage made: the run starts from the input with it applied, and the processor
    /// reads it too.
    fn apply(&mut self, effect: &Effect, budget: &Budget) -> Result<(), StageError> {
        if !matches!(self.phase, Phase::Ready) { return Err(invalid("trigger-apply")); }
        self.processor.apply(effect, budget)?;
        self.input.apply(effect, budget)
    }

    fn run(&mut self, request: TriggerRequest, budget: &Budget) -> Result<Step<TriggerOutput>, StageError> {
        if !matches!(self.phase, Phase::Ready) { return Err(StageError::Invalid(Cow::Borrowed("trigger-run"))); }
        let result = self.start(request, budget);
        self.settle(result)
    }

    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<TriggerOutput>, StageError> {
        if !matches!(self.phase, Phase::Host) { return Err(StageError::Invalid(Cow::Borrowed("resume"))); }
        let wait = self.running.as_ref().and_then(|running| running.waiting.as_ref()).map(Waiting::wait);
        let result = match wait {
            Some(Wait::Count) => self.counter.resume(reply, budget).and_then(|step| match step {
                Step::NeedsHost(request) => Ok(Step::NeedsHost(request)),
                Step::Done(count) => self.interpret(Some(Answer::Count(count)), budget),
            }),
            Some(Wait::Llm) => self.interpret(Some(Answer::Llm(reply)), budget),
            Some(Wait::Similarity) => {
                let step = match self.similarity.as_mut() {
                    Some(search) => search.resume(reply, budget),
                    None => Err(invalid("trigger-similarity")),
                };
                step.and_then(|step| match step {
                    Step::NeedsHost(request) => Ok(Step::NeedsHost(request)),
                    Step::Done(found) => self.interpret(Some(Answer::Similarity(found)), budget),
                })
            }
            None => self.counter.resume(reply, budget).and_then(|step| match step {
                Step::NeedsHost(request) => Ok(Step::NeedsHost(request)),
                Step::Done(count) => {
                    self.counted(count);
                    self.count(budget)
                }
            }),
        };
        self.settle(result)
    }

    /// Before a host request, the variable and author note writes; message edits stay until `Done` because a run
    /// that fails later still drops them. After a terminal failure, the remaining variable and note writes.
    fn take_effects(&mut self) -> Vec<Effect> {
        match self.phase {
            Phase::Host => kept(&mut self.effects),
            Phase::Failed => {
                let kept = kept(&mut self.effects);
                self.effects.clear();
                kept
            }
            Phase::Ready | Phase::Done | Phase::Declined => Vec::new(),
        }
    }
}

fn kept(effects: &mut Vec<Effect>) -> Vec<Effect> {
    let (kept, held) = std::mem::take(effects).into_iter().partition(|effect| !matches!(effect, Effect::Message(_)));
    *effects = held;
    kept
}

fn copy_text(text: Option<&Text>, budget: &Budget) -> Result<Option<Text>, StageError> {
    Ok(match text { Some(text) => Some(text.copy(budget)?), None => None })
}

fn invalid(name: &'static str) -> StageError { StageError::Invalid(Cow::Borrowed(name)) }

/// The `requestChatData` arguments besides `formated` that `v2RunLLM` passes.
fn llm_options(streaming: bool, budget: &Budget) -> Result<Value, StageError> {
    let mut options = Value::Object(OwnedVec::new(budget)?);
    options.define(literal("bias", budget)?, Value::Object(OwnedVec::new(budget)?), budget)?;
    options.define(literal("useStreaming", budget)?, Value::Bool(streaming), budget)?;
    options.define(literal("noMultiGen", budget)?, Value::Bool(true), budget)?;
    Ok(options)
}

/// What `v2RunLLM` stores for `requestChatData`'s result: the text of a success, a streamed one as the host collected
/// its last chunk, and `'null'` for a failure or a multiline result, one pair included. A thrown request throws.
fn llm_text(reply: HostReply, budget: &Budget) -> Result<Text, StageError> {
    match reply {
        HostReply::Llm(Ok(output)) if matches!(output.shape, OutputShape::Multiline(_)) => Ok(literal("null", budget)?),
        HostReply::Llm(Ok(mut output)) if output.texts.len() == 1 => Ok(output.texts.pop().expect("one text")),
        HostReply::Llm(Err(_)) => Ok(literal("null", budget)?),
        reply => thrown(reply),
    }
}

/// What `runLLM` stores: the text of a success, otherwise `'Error: ' + result.result`, where a stream is the
/// `ReadableStream` as `String` names it and the multiline pairs read as `Array.prototype.toString` joins them. A
/// failure's text is its `sdk_message`, which the owner sets to the `result` TS shows.
fn legacy_llm_text(reply: HostReply, budget: &Budget) -> Result<Text, StageError> {
    let error = |result: &[u16]| -> Result<Text, StageError> {
        let mut text = literal("Error: ", budget)?;
        text.append(&Text::from_units(result, budget)?)?;
        Ok(text)
    };
    match reply {
        HostReply::Llm(Ok(mut output)) => match &output.shape {
            OutputShape::Text if output.texts.len() == 1 => Ok(output.texts.pop().expect("one text")),
            OutputShape::Streamed if output.texts.len() == 1 => error(&"[object ReadableStream]".encode_utf16().collect::<Vec<_>>()),
            OutputShape::Multiline(roles) if roles.len() == output.texts.len() => {
                let mut pairs = Vec::new();
                for (index, (role, text)) in roles.iter().zip(&output.texts).enumerate() {
                    budget.work(text.len_units().saturating_add(6))?;
                    if index != 0 { pairs.push(0x2C); }
                    pairs.extend(if *role == MessageRole::User { "user" } else { "char" }.encode_utf16());
                    pairs.push(0x2C);
                    pairs.extend(text.units());
                }
                error(&pairs)
            }
            _ => Err(invalid("trigger-llm-reply")),
        },
        HostReply::Llm(Err(failure)) => match failure.sdk_message {
            Some(message) => error(&message.units().collect::<Vec<_>>()),
            None => Err(invalid("trigger-llm-failure-text")),
        },
        reply => thrown(reply),
    }
}

/// A request that threw, was cancelled or that the host does not serve.
fn thrown<T>(reply: HostReply) -> Result<T, StageError> {
    match reply {
        HostReply::Failed { message } => Err(StageError::Host(message.units().collect())),
        HostReply::Cancelled => Err(StageError::Cancelled),
        HostReply::Unsupported(name) => Err(StageError::Unsupported(name)),
        _ => Err(invalid("trigger-llm-reply")),
    }
}

// Conversation state.

/// `data` as written: a trigger can store any variable value there.
struct Message { role: Text, data: Var }

struct Chat { messages: Vec<Message>, note: Value }

impl Chat {
    // A stored array or object is the same reference on every read.
    fn new(input: &TriggerInput, vars: &mut Variables, budget: &Budget) -> Result<Self, StageError> {
        let Value::Array(rows) = &input.messages else { return Err(unsupported("trigger-conversation-shape")) };
        let mut messages = Vec::new();
        for row in rows.as_slice() {
            budget.work(1)?;
            match (row.get(b"role", budget)?, row.get(b"data", budget)?) {
                (Some(Value::Text(role)), Some(data)) => {
                    messages.push(Message { role: role.copy(budget)?, data: vars.fresh(copy_value(data, budget)?) });
                }
                _ => return Err(unsupported("trigger-conversation-shape")),
            }
        }
        Ok(Self { messages, note: copy_value(&input.note, budget)? })
    }
}

// Interpretation.

struct Frame { locals: Locals, indent: u32, recursive: u32, stop: bool, send_ai: bool }

struct Invoked { stop: bool, send_ai: bool }

/// The trigger an invocation is running, whose body is its invocation's `trigger`: its effect index and its
/// `v2LoopNTimes` counters by header index.
struct Active { low_level: bool, index: i64, counts: HashMap<usize, f64> }

/// The selection an invocation runs: the run's own, or the target of the `v2RunTrigger` at `effect` of `trigger`.
#[derive(Debug, Clone, Copy)]
enum Target { Top, Call { trigger: usize, effect: usize } }

fn selector<'a>(program: &'a Program, top: &'a Selector, target: Target) -> Result<&'a Selector, StageError> {
    let Target::Call { trigger, effect } = target else { return Ok(top) };
    let body = program.triggers.get(trigger).and_then(|trigger| trigger.body.as_ref());
    match body.and_then(|body| body.effects.get(effect)).map(|effect| &effect.op) {
        Some(Op::RunTrigger { target }) => Ok(target),
        _ => Err(invalid("trigger-stack")),
    }
}

/// One `runTrigger` call: the top-level run, or a nested manual run of `v2RunTrigger`.
struct Invocation { target: Target, frame: Frame, trigger: usize, active: Option<Active> }

impl Invocation {
    fn new(target: Target, recursive: u32, stop: bool) -> Self {
        Self { target, frame: Frame { locals: Locals::default(), indent: 0, recursive, stop, send_ai: false }, trigger: 0, active: None }
    }
}

/// How a call of the interpreter ends: the run's effects are done, or an effect waits for the host.
enum Flow { Done(Invoked), Waiting { waiting: Waiting, request: HostRequest } }

struct Interp<'a, P, C, E, R, I> {
    program: &'a Program,
    top: &'a Selector,
    input: &'a TriggerInput,
    chat: Chat,
    vars: Variables,
    /// `additonalSysPrompt.start`, `.historyend` and `.promptend`, shared by every invocation.
    system: [Text; 3],
    effects: &'a mut Vec<Effect>,
    /// The effects already fed to the processor.
    applied: usize,
    processor: &'a mut P,
    counter: &'a mut C,
    similarity: Option<&'a mut (dyn SimilaritySearch + Send + 'static)>,
    patterns: &'a mut E,
    rng: &'a mut R,
    ids: &'a mut I,
    console: &'a mut Vec<Value>,
    /// `CurrentTriggerIdStore` during the run.
    current_id: Option<Text>,
    queue: Vec<Text>,
    budget: &'a Budget,
    /// The last effect is a `v2RunTrigger` that calls its target.
    called: bool,
    /// The effect that waits for the host and its request.
    waiting: Option<(Waiting, HostRequest)>,
}

/// `getVar(key)`: a local of the invocation, else the access read.
fn read_var(vars: &Variables, frame: &Frame, key: &Text, budget: &Budget) -> Result<Var, StageError> {
    let key = units(key);
    if let Some(local) = frame.locals.get(&key, frame.indent, budget)? { return Ok(local.copy(budget)?); }
    vars.get(&key, budget)
}

/// `setVar(key, value)`: a local of the invocation at `currentIndent`, else the access write, emitted when it changes.
fn write_var(vars: &mut Variables, frame: &mut Frame, effects: &mut Vec<Effect>, key: &Text, value: Var, budget: &Budget)
    -> Result<(), StageError> {
    let units = units(key);
    if frame.locals.get(&units, frame.indent, budget)?.is_some() {
        frame.locals.set(&units, value, frame.indent, budget)?;
        return Ok(());
    }
    let effect = copy_value(&value.value, budget)?;
    if vars.set(&units, value, budget)? { effects.push(Effect::ChatVariable { name: key.copy(budget)?, value: effect }); }
    Ok(())
}

/// The trigger variables CBS reads and writes (`operationGetChatVar`, `:1338-1341`).
struct Scope<'s> { vars: &'s mut Variables, frame: &'s mut Frame, effects: &'s mut Vec<Effect> }

impl TextVariables for Scope<'_> {
    // The CBS processor reads through `get_value`, which keeps a value as written. A processor that reads only
    // strings cannot apply the CBS rules to anything else.
    fn get(&mut self, key: Option<&Text>, budget: &Budget) -> Result<Text, StageError> {
        match self.get_value(key, budget)? {
            Value::Text(text) => Ok(text),
            _ => Err(unsupported("trigger-text-only-read")),
        }
    }
    fn get_value(&mut self, key: Option<&Text>, budget: &Budget) -> Result<Value, StageError> {
        Ok(read_var(self.vars, self.frame, key.ok_or_else(|| unsupported("trigger-variable-key"))?, budget)?.value)
    }
    fn set(&mut self, key: Option<&Text>, value: Option<&Text>, budget: &Budget) -> Result<(), StageError> {
        let key = key.ok_or_else(|| unsupported("trigger-variable-key"))?;
        let value = value.ok_or_else(|| unsupported("trigger-variable-value"))?;
        write_var(self.vars, self.frame, self.effects, key, Var::text(value.copy(budget)?), budget)
    }
}

/// `JSON.parse` in a `try`: `Ok(None)` is the caught `SyntaxError` or `TypeError`.
type Caught<T> = Result<Option<T>, StageError>;

fn number_key(value: f64, budget: &Budget) -> Result<Vec<u16>, StageError> { Ok(units(&number_to_text(value, budget)?)) }

fn variable_value() -> StageError { unsupported("trigger-variable-value") }

/// The `TypeError` TS throws, such as a string method called on another value.
fn type_error(name: &'static str) -> StageError { StageError::Invalid(Cow::Borrowed(name)) }

/// `String(value)`, the conversion every implicit string coercion here makes.
fn string_of(value: Var, budget: &Budget) -> Result<Text, StageError> {
    match value.value {
        Value::Text(text) => Ok(text),
        other => Ok(other.to_text(budget)?),
    }
}

/// A `try` around part of an effect: `None` where TS throws the `TypeError` or `SyntaxError` its `catch` handles.
fn caught<T>(result: Result<T, StageError>) -> Caught<T> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(StageError::Invalid(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

/// A `try` that ends in `setVar(risuChatParser(outputVar), value)`: what throws while it reads its arguments is `Before`.
fn before(result: Result<Tried<Var>, StageError>) -> Result<Tried<Var>, StageError> { Ok(caught(result)?.unwrap_or(Tried::Before)) }

/// The string conversion inside a `try`: `None` where it throws.
fn caught_string(value: Var, budget: &Budget) -> Caught<Text> { caught(string_of(value, budget)) }

/// The receiver of a string method: any other value has no such method.
fn receiver(value: Var) -> Result<Text, StageError> {
    match value.value {
        Value::Text(text) => Ok(text),
        _ => Err(type_error("trigger-string-method")),
    }
}

/// `JSON.parse(value)`, which parses the string of the value, in a `try`.
fn parse_value(value: Var, budget: &Budget) -> Caught<Value> {
    let Some(text) = caught_string(value, budget)? else { return Ok(None) };
    Ok(parse(&text, budget)?)
}

/// `left + right`: two strings after `ToPrimitive` when either is a string, else the sum of their numbers.
fn add(left: Var, right: Var, budget: &Budget) -> Result<Var, StageError> {
    let primitive = |value: Value| -> Result<Value, Error> {
        if value.is_object() { Ok(Value::Text(value.to_text(budget)?)) } else { Ok(value) }
    };
    let (left, right) = (primitive(left.value)?, primitive(right.value)?);
    if matches!(left, Value::Text(_)) || matches!(right, Value::Text(_)) {
        return Ok(Var::text(concat(&left.to_text(budget)?, &right.to_text(budget)?, budget)?));
    }
    Ok(Var { value: Value::Number(number_of(&left, budget)? + number_of(&right, budget)?), id: 0 })
}

/// What the `try` of `setVar(risuChatParser(outputVar), value)` stores: `Before` when it threw before that call,
/// `After` when evaluating `value` threw once the output name was parsed. Either way the `catch` stores its fallback.
enum Tried<T> { Value(T), Before, After }

impl<'a, P, C, E, R, I> Interp<'a, P, C, E, R, I>
where P: ScopedTextProcessor, C: TokenCounter, E: PatternEngine, R: Rng, I: MessageIds {
    fn sync(&mut self) -> Result<(), StageError> {
        while self.applied < self.effects.len() {
            self.processor.apply(&self.effects[self.applied], self.budget)?;
            self.applied += 1;
        }
        Ok(())
    }

    /// Feeds `effect` to the processor before recording it, so an effect the processor refuses leaves nothing behind.
    fn emit(&mut self, effect: Effect) -> Result<(), StageError> {
        self.sync()?;
        self.processor.apply(&effect, self.budget)?;
        self.effects.push(effect);
        self.applied += 1;
        Ok(())
    }

    /// `risuChatParser(text, { chara: char })` against the trigger variables.
    fn parse(&mut self, frame: &mut Frame, input: &Text) -> Result<Text, StageError> {
        let mut scope = Scope { vars: &mut self.vars, frame, effects: &mut *self.effects };
        let output = self.processor.process_scoped(input, &TextContext::default(),
            TextScope { variables: &mut scope, trigger_id: self.current_id.as_ref() }, self.budget)?;
        self.sync()?;
        Ok(output)
    }

    fn get_var(&mut self, frame: &mut Frame, key: &Text) -> Result<Var, StageError> { read_var(&self.vars, frame, key, self.budget) }

    fn set_var(&mut self, frame: &mut Frame, key: &Text, value: Var) -> Result<(), StageError> {
        write_var(&mut self.vars, frame, self.effects, key, value, self.budget)?;
        self.sync()
    }

    fn set_text(&mut self, frame: &mut Frame, key: &Text, value: Text) -> Result<(), StageError> { self.set_var(frame, key, Var::text(value)) }

    /// Stores what the host answered for the effect the run waited on.
    fn answer(&mut self, frame: &mut Frame, waiting: Waiting, answer: Answer) -> Result<(), StageError> {
        match (waiting, answer) {
            (Waiting::Count(key), Answer::Count(count)) => self.set_text(frame, &key, count_text(count as usize, self.budget)?),
            (Waiting::Llm { output, legacy: false }, Answer::Llm(reply)) => {
                let value = llm_text(reply, self.budget)?;
                let key = self.parse(frame, &output)?;
                self.set_text(frame, &key, value)
            }
            (Waiting::Llm { output, legacy: true }, Answer::Llm(reply)) => {
                let value = legacy_llm_text(reply, self.budget)?;
                self.set_text(frame, &output, value)
            }
            (Waiting::Similarity { output, legacy }, Answer::Similarity(found)) => self.store_similar(frame, &output, legacy, found),
            _ => Err(invalid("trigger-waiting")),
        }
    }

    /// `setVar(risuChatParser(outputVar), found.join('§'))`.
    fn store_similar(&mut self, frame: &mut Frame, output: &Text, legacy: bool, found: Vec<Text>) -> Result<(), StageError> {
        let key = if legacy { output.copy(self.budget)? } else { self.parse(frame, output)? };
        let mut joined = literal("", self.budget)?;
        for (index, document) in found.iter().enumerate() {
            self.budget.work(1)?;
            if index != 0 { joined.append(&text(&[0xA7], self.budget)?)?; }
            joined.append(document)?;
        }
        self.set_text(frame, &key, joined)
    }

    /// `parseChatML(value)`, with its `risuChatParser` pass over each content in order, else
    /// `[{ role: 'user', content: value }]`.
    fn chat_ml(&mut self, frame: &mut Frame, value: &Text) -> Result<Vec<PromptMessage>, StageError> {
        let units: Vec<u16> = value.units().collect();
        let Some(parts) = parse_chat_ml(&units, self.budget)? else {
            return Ok(vec![PromptMessage::new(Role::User, value.copy(self.budget)?)]);
        };
        let mut messages = Vec::new();
        for part in parts {
            let content = self.parse(frame, &text(&part.content, self.budget)?)?;
            let mut thoughts = Vec::new();
            for thought in &part.thoughts { thoughts.push(text(thought, self.budget)?); }
            let mut message = PromptMessage::new(part.role, content);
            message.thoughts = Some(thoughts);
            messages.push(message);
        }
        Ok(messages)
    }

    fn set_literal(&mut self, frame: &mut Frame, key: &Text, value: &str) -> Result<(), StageError> {
        let value = literal(value, self.budget)?;
        self.set_text(frame, key, value)
    }

    /// `setVar(risuChatParser(outputVar), value)`.
    fn output(&mut self, frame: &mut Frame, var: &Text, value: Var) -> Result<(), StageError> {
        let key = self.parse(frame, var)?;
        self.set_var(frame, &key, value)
    }

    fn output_text(&mut self, frame: &mut Frame, var: &Text, value: Text) -> Result<(), StageError> { self.output(frame, var, Var::text(value)) }

    fn output_literal(&mut self, frame: &mut Frame, var: &Text, value: &str) -> Result<(), StageError> {
        let value = literal(value, self.budget)?;
        self.output_text(frame, var, value)
    }

    fn store(&mut self, frame: &mut Frame, output: &Text, tried: Tried<Var>, fallback: &str) -> Result<(), StageError> {
        match tried {
            Tried::Value(value) => self.output(frame, output, value),
            Tried::Before => self.output_literal(frame, output, fallback),
            Tried::After => {
                self.parse(frame, output)?;
                self.output_literal(frame, output, fallback)
            }
        }
    }

    /// `value ?? 'null'` for a property a `setVar` stores. A function read through a prototype declines.
    fn property_value(&mut self, property: Property<'_>) -> Result<Tried<Var>, StageError> {
        let budget = self.budget;
        Ok(Tried::Value(match property {
            Property::Throws => return Ok(Tried::After),
            Property::Missing | Property::Value(Value::Null | Value::Undefined) => Var::text(literal("null", budget)?),
            Property::Value(value) => self.vars.fresh(copy_value(value, budget)?),
            Property::Unit(unit) => Var::text(text(&[unit], budget)?),
            Property::Length(length) => Var { value: Value::Number(length as f64), id: 0 },
            Property::Other => return Err(variable_value()),
        }))
    }

    /// The value of `xType === 'value' ? parse(x) : getVar(parse(x))`.
    fn src(&mut self, frame: &mut Frame, src: &Src) -> Result<Var, StageError> {
        match src {
            Src::Value(text) => Ok(Var::text(self.parse(frame, text)?)),
            Src::Var(text) => {
                let key = self.parse(frame, text)?;
                self.get_var(frame, &key)
            }
        }
    }

    /// `String(src)`.
    fn src_string(&mut self, frame: &mut Frame, src: &Src) -> Result<Text, StageError> {
        let value = self.src(frame, src)?;
        string_of(value, self.budget)
    }

    /// `Number(src)`.
    fn number(&mut self, frame: &mut Frame, src: &Src) -> Result<f64, StageError> {
        let value = self.src(frame, src)?;
        Ok(number_of(&value.value, self.budget)?)
    }

    /// `JSON.parse(getVar(name))` in a `try`.
    fn parsed_var(&mut self, frame: &mut Frame, name: &Text) -> Caught<Value> {
        let value = self.get_var(frame, name)?;
        parse_value(value, self.budget)
    }

    fn src_json(&mut self, frame: &mut Frame, src: &Src) -> Caught<Value> {
        let value = self.src(frame, src)?;
        parse_value(value, self.budget)
    }

    /// A property key from `ToPropertyKey`, in a `try`: `None` where it throws.
    fn key(&mut self, frame: &mut Frame, src: &Src) -> Caught<Vec<u16>> {
        let value = self.src(frame, src)?;
        Ok(caught_string(value, self.budget)?.map(|key| units(&key)))
    }

    /// `regexOperation('exec', source, flags, input)`, which converts each argument to a string; `None` for no match.
    /// A pattern JS rejects is `Invalid`.
    fn exec(&mut self, source: Var, flags: Var, input: Var) -> Result<Option<Vec<Option<Vec<u16>>>>, StageError> {
        let source = string_of(source, self.budget)?;
        let flags = string_of(flags, self.budget)?;
        let pattern = self.patterns.compile(&source, &flags, self.budget)?;
        let input = string_of(input, self.budget)?;
        let Some(found) = pattern.exec(&input, 0, self.budget)? else { return Ok(None) };
        let input = units(&input);
        Ok(Some(found.groups.into_iter().map(|range| range.map(|range| input[range].to_vec())).collect()))
    }

    /// `replaceRegexMatches(source, matches, callback)` over the matches `input.replace(regex, ...)` visits.
    fn replace(&mut self, source: Var, regex: Var, flags: Var, format: &Var, replacement: &Var) -> Result<Text, StageError> {
        let budget = self.budget;
        let regex = string_of(regex, budget)?;
        let flags = string_of(flags, budget)?;
        let pattern = self.patterns.compile(&regex, &flags, budget)?;
        // `input.replace`: any other value has no such method.
        let input = receiver(source)?;
        let flags = units(&flags);
        let global = flags.contains(&u16::from(b'g'));
        let unicode = flags.contains(&u16::from(b'u')) || flags.contains(&u16::from(b'v'));
        let named = !pattern.group_names().is_empty();
        let source = units(&input);
        let mut parts = Vec::new();
        let (mut end, mut last) = (0, 0);
        while let Some(found) = pattern.exec(&input, last, budget)? {
            budget.work(1)?;
            let range = found.groups.first().cloned().flatten().ok_or(Error::Invariant("pattern-match"))?;
            // `lastIndex` steps past an empty match.
            last = if range.is_empty() { advance(&source, range.end, unicode) } else { range.end };
            parts.push(Value::Text(text(&source[end.min(range.start)..range.start], budget)?));
            let mut groups = Vec::with_capacity(found.groups.len() + 1);
            groups.push(Some(source[range.clone()].to_vec()));
            for group in &found.groups[1..] { groups.push(group.clone().map(|group| source[group].to_vec())); }
            if named { groups.push(if range.start == 0 { None } else { Some(units(&count_text(range.start, budget)?)) }); }
            parts.push(piece(&groups, format, replacement, budget)?);
            end = range.end;
            if !global || last > source.len() { break; }
        }
        parts.push(Value::Text(text(&source[end.min(source.len())..], budget)?));
        // `parts.join('')`.
        let empty = literal("", budget)?;
        join(&parts, &empty, budget)?.ok_or_else(|| type_error("trigger-join"))
    }

    /// `chat.message.slice(start).map(v => v.data).join(' ')`.
    fn joined(&self, start: f64) -> Result<Text, StageError> {
        let start = relative(start, self.chat.messages.len());
        let space = literal(" ", self.budget)?;
        let data = self.chat.messages[start..].iter().map(|message| &message.data.value);
        join(data, &space, self.budget)?.ok_or_else(|| type_error("trigger-join"))
    }

    /// The `strict`, `loose` and `regex` searches of `exists` and `v2QuickSearchChat`; `None` for any other kind.
    fn search(&mut self, kind: Option<&Text>, haystack: &Text, needle: Var) -> Result<Option<bool>, StageError> {
        let Some(kind) = kind else { return Ok(None) };
        if equals(kind, "strict") {
            // `includes` finds only a string among the words.
            let words = split(&units(haystack), &[u16::from(b' ')], self.budget)?;
            Ok(Some(words.into_iter().any(|word| strict(&Value::Text(word), &needle.value))))
        } else if equals(kind, "loose") {
            let tables = self.input.case_tables;
            let haystack = units(&case(&units(haystack), false, tables, self.budget)?);
            let needle = units(&case(&units(&receiver(needle)?), false, tables, self.budget)?);
            Ok(Some(index_of(&haystack, &needle).is_some()))
        } else if equals(kind, "regex") {
            let flags = Var::text(literal("", self.budget)?);
            let haystack = Var::text(haystack.copy(self.budget)?);
            Ok(Some(self.exec(needle, flags, haystack)?.is_some()))
        } else {
            Ok(None)
        }
    }

    /// Runs the invocations on `stack`, the run's own at the bottom and each nested `v2RunTrigger` invocation above
    /// it, bounded by `Limits::max_depth`, until they end or an effect waits for the host.
    fn invoke(&mut self, stack: &mut Vec<Invocation>) -> Result<Flow, StageError> {
        let program = self.program;
        loop {
            self.budget.check()?;
            let current = stack.last_mut().ok_or_else(|| invalid("trigger-stack"))?;
            if let Some(active) = current.active.as_mut() {
                let body = program.triggers.get(current.trigger).and_then(|trigger| trigger.body.as_ref())
                    .ok_or_else(|| invalid("trigger-admission"))?;
                if (active.index as usize) < body.effects.len() {
                    self.budget.work(1)?;
                    let at = active.index as usize;
                    current.frame.indent = body.effects[at].indent.unwrap_or(0);
                    active.index = self.effect(&mut current.frame, active.low_level, body, at, &mut active.counts)? + 1;
                    if let Some((waiting, request)) = self.waiting.take() { return Ok(Flow::Waiting { waiting, request }); }
                    if std::mem::take(&mut self.called) {
                        let target = Target::Call { trigger: current.trigger, effect: at };
                        let (recursive, stop) = (current.frame.recursive, current.frame.stop);
                        if stack.len() > self.budget.limits().max_depth { return Err(StageError::Limit(Cow::Borrowed("depth"))); }
                        // A nested run sets `CurrentTriggerIdStore` to null and, being manual, leaves it there
                        // (`:1204-1207`, `:2985-2987`).
                        self.current_id = None;
                        stack.push(Invocation::new(target, recursive, stop));
                    }
                    continue;
                }
                current.active = None;
                current.trigger += 1;
                continue;
            }
            if let Some(trigger) = program.triggers.get(current.trigger) {
                if !trigger.selected(selector(program, self.top, current.target)?) {
                    current.trigger += 1;
                    continue;
                }
                let body = trigger.body.as_ref().ok_or_else(|| invalid("trigger-admission"))?;
                let mut passes = true;
                for condition in &body.conditions {
                    if !self.condition(&mut current.frame, condition)? { passes = false; break; }
                }
                if passes {
                    current.active = Some(Active { low_level: trigger.low_level, index: 0, counts: HashMap::new() });
                } else {
                    current.trigger += 1;
                }
                continue;
            }
            let done = stack.pop().ok_or_else(|| invalid("trigger-stack"))?;
            let Some(parent) = stack.last_mut() else {
                return Ok(Flow::Done(Invoked { stop: done.frame.stop, send_ai: done.frame.send_ai }));
            };
            // The nested run tokenizes the shared system prompts as they stand, and its caller adopts its `stopSending`.
            for text in &self.system {
                if text.len_units() != 0 { self.queue.push(text.copy(self.budget)?); }
            }
            parent.frame.stop = done.frame.stop;
        }
    }

    fn condition(&mut self, frame: &mut Frame, condition: &Condition) -> Result<bool, StageError> {
        let budget = self.budget;
        match condition {
            Condition::Pass => Ok(true),
            Condition::Compare { left, operator, value } => {
                let left = match left {
                    // `getVar(condition.var) ?? 'null'`.
                    Left::Var(key) => match self.get_var(frame, key)?.value {
                        Value::Null => Var::text(literal("null", budget)?),
                        other => Var { value: other, id: 0 },
                    },
                    Left::Literal(text) => Var::text(text.copy(budget)?),
                    Left::ChatIndex => Var::text(count_text(self.chat.messages.len(), budget)?),
                };
                let right = self.parse(frame, value)?;
                // `risuChatParser` calls `replace` on what it is given.
                let left = receiver(left)?;
                let left = self.parse(frame, &left)?;
                let Some(operator) = operator else { return Ok(true) };
                let numbers = || -> Result<(f64, f64), StageError> { Ok((to_number(&left, budget)?, to_number(&right, budget)?)) };
                Ok(if equals(operator, "true") {
                    equals(&left, "true") || equals(&left, "1")
                } else if equals(operator, "=") {
                    left.equals(&right)
                } else if equals(operator, "!=") {
                    !left.equals(&right)
                } else if equals(operator, ">") {
                    let (left, right) = numbers()?;
                    !(left <= right)
                } else if equals(operator, "<") {
                    let (left, right) = numbers()?;
                    !(left >= right)
                } else if equals(operator, ">=") {
                    let (left, right) = numbers()?;
                    !(left < right)
                } else if equals(operator, "<=") {
                    let (left, right) = numbers()?;
                    !(left > right)
                } else if equals(operator, "null") {
                    equals(&left, "null")
                } else {
                    true
                })
            }
            Condition::Exists { value, kind, start } => {
                let value = self.parse(frame, value)?;
                let value = self.parse(frame, &value)?;
                let haystack = self.joined(*start)?;
                Ok(self.search(kind.as_ref(), &haystack, Var::text(value))?.unwrap_or(true))
            }
        }
    }

    /// The `v2If` / `v2IfAdvanced` comparison (`:1800-1888`).
    fn compare(&mut self, source: Var, target: Var, condition: Option<&Text>) -> Result<bool, StageError> {
        let budget = self.budget;
        let Some(condition) = condition else { return Ok(false) };
        let numbers = || -> Result<(f64, f64), StageError> { Ok((number_of(&source.value, budget)?, number_of(&target.value, budget)?)) };
        // `JSON.parse(list).includes(item)`; `None` where it throws.
        let includes = |list: &Var, item: &Var| -> Caught<bool> {
            let Some(parsed) = parse_value(list.copy(budget)?, budget)? else { return Ok(None) };
            Ok(match parsed {
                Value::Array(values) => Some(values.as_slice().iter().any(|value| same_value_zero(value, &item.value))),
                Value::Text(text) => match caught_string(item.copy(budget)?, budget)? {
                    Some(item) => Some(index_of(&units(&text), &units(&item)).is_some()),
                    None => None,
                },
                _ => None,
            })
        };
        let is = |value: &Var, literal: &str| matches!(&value.value, Value::Text(text) if equals(text, literal));
        Ok(if equals(condition, "=") || equals(condition, "!=") {
            let (left, right) = numbers()?;
            let same = if !left.is_nan() && !right.is_nan() { left == right } else { identical(&source, &target) };
            same == equals(condition, "=")
        } else if equals(condition, ">") {
            let (left, right) = numbers()?;
            left > right
        } else if equals(condition, "<") {
            let (left, right) = numbers()?;
            left < right
        } else if equals(condition, ">=") {
            let (left, right) = numbers()?;
            left >= right
        } else if equals(condition, "<=") {
            let (left, right) = numbers()?;
            left <= right
        } else if equals(condition, "∈") {
            includes(&target, &source)?.unwrap_or(false)
        } else if equals(condition, "∋") {
            includes(&source, &target)?.unwrap_or(false)
        } else if equals(condition, "∉") {
            includes(&target, &source)?.map_or(true, |found| !found)
        } else if equals(condition, "∌") {
            includes(&source, &target)?.map_or(true, |found| !found)
        } else if equals(condition, "≒") {
            let (left, right) = numbers()?;
            if left.is_nan() || right.is_nan() {
                let (source, target) = (receiver(source)?, receiver(target)?);
                // `toLocaleLowerCase()` maps differently under a tailored default locale.
                if self.input.tailored_case { return Err(unsupported("case-locale")); }
                let tables = self.input.case_tables;
                let squash = |text: &Text| -> Result<Vec<u16>, StageError> {
                    Ok(units(&case(&units(text), false, tables, budget)?).into_iter().filter(|unit| *unit != u16::from(b' ')).collect())
                };
                squash(&source)? == squash(&target)?
            } else {
                (left - right).abs() < 0.0001
            }
        } else if equals(condition, "≡") {
            let truthy = is(&source, "true") || is(&source, "1");
            if is(&target, "true") { truthy } else if is(&target, "false") { !truthy } else { identical(&source, &target) }
        } else {
            false
        })
    }

    /// Runs the effect at `index` and returns the index the loop's `index++` continues from.
    fn effect(&mut self, frame: &mut Frame, low_level: bool, body: &'a Body, index: usize, counts: &mut HashMap<usize, f64>)
        -> Result<i64, StageError> {
        let budget = self.budget;
        let effects = &body.effects;
        let here = index as i64;
        match &effects[index].op {
            Op::Noop => {}
            Op::SetVar { var, value, operator, modulo } => {
                let value = self.src(frame, value)?;
                let key = self.parse(frame, var)?;
                let current = self.get_var(frame, &key)?;
                let mut original = number_of(&current.value, budget)?;
                if original.is_nan() { original = 0.0; }
                let operand = |value: &Var| number_of(&value.value, budget);
                let result = match operator.as_ref() {
                    Some(operator) if equals(operator, "=") => value,
                    Some(operator) if equals(operator, "+=") => Var::text(number_to_text(original + operand(&value)?, budget)?),
                    Some(operator) if equals(operator, "-=") => Var::text(number_to_text(original - operand(&value)?, budget)?),
                    Some(operator) if equals(operator, "*=") => Var::text(number_to_text(original * operand(&value)?, budget)?),
                    Some(operator) if equals(operator, "/=") => Var::text(number_to_text(original / operand(&value)?, budget)?),
                    Some(operator) if *modulo && equals(operator, "%=") => Var::text(number_to_text(original % operand(&value)?, budget)?),
                    _ => Var::text(literal("", budget)?),
                };
                self.set_var(frame, &key, result)?;
            }
            Op::DeclareLocal { var, value, indent } => {
                let value = self.src(frame, value)?;
                let key = self.parse(frame, var)?;
                frame.locals.set(&units(&key), value, *indent, budget)?;
            }
            Op::If { source, target, condition, indent } => {
                let source = self.src(frame, source)?;
                let target = self.src(frame, target)?;
                if !self.compare(source, target, condition.as_ref())? {
                    let wanted = u64::from(*indent) + 1;
                    let mut at = index;
                    while at < effects.len() {
                        if matches!(effects[at].op, Op::EndIndent { .. }) && effects[at].indent.map(u64::from) == Some(wanted) {
                            if effects.get(at + 1).is_some_and(|next| matches!(next.op, Op::Else { .. }) && next.indent == Some(*indent)) {
                                at += 1;
                            }
                            break;
                        }
                        at += 1;
                    }
                    return Ok(at as i64);
                }
            }
            Op::Else { indent } => {
                let wanted = u64::from(*indent) + 1;
                let at = (index..effects.len())
                    .find(|at| matches!(effects[*at].op, Op::EndIndent { .. }) && effects[*at].indent.map(u64::from) == Some(wanted))
                    .unwrap_or(effects.len());
                return Ok(at as i64);
            }
            Op::EndIndent { indent, end_of_loop } => {
                let mut at = here;
                if *end_of_loop {
                    let wanted = i64::from(*indent) - 1;
                    while at >= 0 {
                        let header = &effects[at as usize];
                        if matches!(header.op, Op::Loop | Op::LoopTimes { .. }) && header.indent.map(i64::from) == Some(wanted) {
                            if let Op::LoopTimes { value, .. } = &header.op {
                                let mut times = self.number(frame, value)?;
                                if times.is_nan() { times = 0.0; }
                                let count = counts.entry(at as usize).or_insert(0.0);
                                *count += 1.0;
                                if *count >= times { at = here; }
                            }
                            break;
                        }
                        at -= 1;
                    }
                }
                frame.locals.clear_from(*indent);
                return Ok(at);
            }
            Op::Loop | Op::LoopTimes { .. } => {}
            Op::BreakLoop => {
                let at = (index..effects.len())
                    .find(|at| matches!(effects[*at].op, Op::EndIndent { end_of_loop: true, .. }))
                    .unwrap_or(effects.len());
                return Ok(at as i64);
            }
            Op::RunTrigger { .. } => {
                if frame.recursive < 10 || low_level {
                    frame.recursive += 1;
                    self.called = true;
                }
            }
            Op::ConsoleLog { source } => {
                let source = self.src(frame, source)?;
                self.console.push(source.value);
            }
            // `setVar(risuChatParser(outputVar), (await tokenize(value)).toString())`.
            Op::Tokenize { value, output } => {
                let value = self.src(frame, value)?;
                let key = self.parse(frame, output)?;
                // `tokenize` hands the value to the tokenizer as it is; only a string is text to every tokenizer.
                let Value::Text(value) = value.value else { return Err(unsupported("trigger-tokenize-value")) };
                match self.counter.count(TokenInput::Text(&value), budget)? {
                    Step::Done(count) => self.set_text(frame, &key, count_text(count as usize, budget)?)?,
                    Step::NeedsHost(request) => self.waiting = Some((Waiting::Count(key), request)),
                }
            }
            // `requestChatData` over `parseChatML(value)`, or the value as one user message, after the variables are
            // flushed (`:2090-2117`, and v1 `:1648-1674`).
            Op::RunLlm { value, model, streaming, output, legacy } => {
                let value = self.src(frame, value)?;
                // `parseChatML` trims the value, which only a string has.
                let Value::Text(value) = value.value else { return Err(unsupported("trigger-llm-value")) };
                let messages = self.chat_ml(frame, &value)?;
                let request = SubRequest { mode: model.copy(budget)?, messages, options: llm_options(*streaming, budget)? };
                self.waiting = Some((Waiting::Llm { output: output.copy(budget)?, legacy: *legacy }, HostRequest::Llm(request)));
            }
            // `addText(value.split('§'))` and `similaritySearch(source)` on a new `HypaProcesser` (`:2077-2088`); v1
            // (`:1676-1689`) parses `value` first.
            Op::CheckSimilarity { source, value, output, legacy } => {
                let (source, value) = if *legacy {
                    let value = self.src(frame, value)?;
                    (self.src(frame, source)?, value)
                } else {
                    let source = self.src(frame, source)?;
                    (source, self.src(frame, value)?)
                };
                // The search embeds a string and `split` takes one.
                let (Value::Text(source), Value::Text(value)) = (source.value, value.value) else {
                    return Err(unsupported("trigger-similarity-value"));
                };
                let units: Vec<u16> = value.units().collect();
                let documents = split(&units, &[0xA7], budget)?;
                let declined = if *legacy { "trigger-effect-checkSimilarity" } else { "trigger-effect-v2CheckSimilarity" };
                let search = self.similarity.as_deref_mut().ok_or_else(|| unsupported(declined))?;
                match search.search(documents, source, budget)? {
                    Step::Done(found) => self.store_similar(frame, output, *legacy, found)?,
                    Step::NeedsHost(request) => self.waiting = Some((Waiting::Similarity { output: output.copy(budget)?, legacy: *legacy }, request)),
                }
            }
            Op::StopTrigger => return Ok(effects.len() as i64),
            Op::CutChat { start, end, defaults } => {
                let mut start = self.number(frame, start)?;
                let mut end = self.number(frame, end)?;
                if *defaults && start.is_nan() { start = 0.0; }
                if *defaults && end.is_nan() { end = self.chat.messages.len() as f64; }
                let length = self.chat.messages.len();
                let (from, to) = (relative(start, length), relative(end, length));
                let kept: Vec<Message> = self.chat.messages.drain(..).skip(from).take(to.saturating_sub(from)).collect();
                self.chat.messages = kept;
                self.emit(Effect::Message(MessageEdit::Cut { start, end: Some(end) }))?;
            }
            Op::ModifyChat { index, value } => {
                let position = self.number(frame, index)?;
                let value = self.src(frame, value)?;
                if let Some(at) = number_index(position).filter(|at| *at < self.chat.messages.len()) {
                    let data = copy_value(&value.value, budget)?;
                    self.chat.messages[at].data = value;
                    self.emit(Effect::Message(MessageEdit::SetData { index: at as f64, data }))?;
                }
            }
            Op::SystemPrompt { location, value } => {
                // `value + "\n\n"`.
                let mut value = self.src_string(frame, value)?;
                value.append(&literal("\n\n", budget)?)?;
                match location {
                    Location::Start => self.system[0].append(&value)?,
                    Location::HistoryEnd => self.system[1].append(&value)?,
                    Location::PromptEnd => self.system[2].append(&value)?,
                    // Another property of the prompt object, which nothing reads.
                    Location::Other => {}
                }
            }
            Op::Impersonate { role, value } => {
                let value = self.src(frame, value)?;
                if let Some(role) = role {
                    let chat_id = self.ids.next_id(budget)?;
                    let name = match role { MessageRole::User => "user", MessageRole::Char => "char" };
                    let data = copy_value(&value.value, budget)?;
                    self.chat.messages.push(Message { role: literal(name, budget)?, data: value });
                    self.emit(Effect::Message(MessageEdit::Append { role: *role, data, chat_id }))?;
                }
            }
            Op::StopSending => frame.stop = true,
            Op::SendAiPrompt => frame.send_ai = true,
            Op::ExtractRegex { value, regex, flags, result, output } => {
                let value = self.src(frame, value)?;
                let regex = self.src(frame, regex)?;
                let flags = self.src(frame, flags)?;
                let found = self.exec(regex, flags, value)?;
                // `resultValue.replace(...)`.
                let format = receiver(self.src(frame, result)?)?;
                let extracted = text(&extract(&units(&format), found.as_deref(), &[], budget)?, budget)?;
                self.output_text(frame, output, extracted)?;
            }
            Op::LegacyExtract { value, regex, flags, result, output } => {
                let value = self.parse(frame, value)?;
                let found = self.exec(Var::text(regex.copy(budget)?), Var::text(flags.copy(budget)?), Var::text(value))?;
                // `regexResult[0]` of a failed match throws, and a missing group is the text `undefined`.
                if found.is_none() { return Err(type_error("trigger-extract-null")); }
                let undefined: Vec<u16> = "undefined".encode_utf16().collect();
                let extracted = text(&extract(&units(result), found.as_deref(), &undefined, budget)?, budget)?;
                self.set_text(frame, output, extracted)?;
            }
            Op::LastMessage { output } => {
                let value = stored_data(self.chat.messages.last(), budget)?;
                self.output(frame, output, value)?;
            }
            Op::MessageAt { index, output } => {
                let position = self.number(frame, index)?;
                let value = stored_data(number_index(position).and_then(|at| self.chat.messages.get(at)), budget)?;
                self.output(frame, output, value)?;
            }
            Op::MessageCount { output } => {
                let count = count_text(self.chat.messages.len(), budget)?;
                self.output_text(frame, output, count)?;
            }
            Op::Random { min, max, output } => {
                let min = self.number(frame, min)?;
                let max = self.number(frame, max)?;
                let drawn = self.rng.random()?;
                let value = number_to_text((drawn * (max - min + 1.0) + min).floor(), budget)?;
                self.output_text(frame, output, value)?;
            }
            Op::CharAt { source, index, output } => {
                let source = self.src(frame, source)?;
                let position = self.number(frame, index)?;
                // `source[index] ?? 'null'`.
                let key = number_key(position, budget)?;
                let value = match property(&source.value, &key, budget)? {
                    Property::Throws => return Err(type_error("trigger-property-of-null")),
                    found => match self.property_value(found)? { Tried::Value(value) => value, _ => return Err(variable_value()) },
                };
                self.output(frame, output, value)?;
            }
            Op::CharCount { source, output } => {
                let source = self.src(frame, source)?;
                // `source.length.toString()`.
                let length = [b'l', b'e', b'n', b'g', b't', b'h'].map(u16::from);
                let count = match property(&source.value, &length, budget)? {
                    Property::Length(length) => count_text(length, budget)?,
                    Property::Value(value) => to_string(value, budget)?.ok_or_else(|| type_error("trigger-length"))?,
                    Property::Missing | Property::Throws => return Err(type_error("trigger-length")),
                    Property::Unit(_) | Property::Other => return Err(variable_value()),
                };
                self.output_text(frame, output, count)?;
            }
            Op::Case { source, upper, output } => {
                let source = receiver(self.src(frame, source)?)?;
                let mapped = case(&units(&source), *upper, self.input.case_tables, budget)?;
                self.output_text(frame, output, mapped)?;
            }
            Op::SetCharAt { source, index, value, output } => {
                let source = self.src(frame, source)?;
                let position = self.number(frame, index)?;
                let value = self.src(frame, value)?;
                // `[...source]`: a string's code points or an array's elements; anything else is not iterable.
                let mut parts: Vec<Value> = match source.value {
                    Value::Text(source) => {
                        let mut parts = Vec::new();
                        for point in code_points(&units(&source)) { parts.push(Value::Text(text(point, budget)?)); }
                        parts
                    }
                    list @ Value::Array(_) => into_elements(list),
                    _ => return Err(type_error("trigger-not-iterable")),
                };
                if let Some(at) = number_index(position) {
                    budget.work(parts.len())?;
                    // Holes past the end join as empty strings.
                    if at < parts.len() { parts[at] = value.value; } else { parts.push(value.value); }
                }
                let empty = literal("", budget)?;
                let joined = join(&parts, &empty, budget)?.ok_or_else(|| type_error("trigger-join"))?;
                self.output_text(frame, output, joined)?;
            }
            Op::Split { source, delimiter, output } => {
                let source = self.src(frame, source)?;
                let delimiter = self.src(frame, delimiter)?;
                let source = receiver(source)?;
                let delimiter = string_of(delimiter, budget)?;
                let parts = split(&units(&source), &units(&delimiter), budget)?;
                let json = stringify(&text_array(parts, budget)?, budget)?;
                self.output_text(frame, output, json)?;
            }
            Op::Join { var, delimiter, output } => {
                let joined = before(self.join(frame, var, delimiter))?;
                self.store(frame, output, joined, "")?;
            }
            Op::MakeArray { var } => {
                let name = self.parse(frame, var)?;
                if bracketed(&name, b'[', b']') { return Err(unsupported("trigger-early-return")); }
                self.set_literal(frame, &name, "[]")?;
            }
            Op::ArrayLength { var, output } => {
                let length = before(self.array_length(frame, var))?;
                self.store(frame, output, length, "0")?;
            }
            Op::ArrayGet { var, index, output } => {
                let value = before(self.array_get(frame, var, index))?;
                self.store(frame, output, value, "null")?;
            }
            Op::ArrayPush { var, value, front } => {
                let pushed = caught(self.array_edit(frame, var, |run, frame, list| {
                    let value = run.src(frame, value)?;
                    if !matches!(list, Value::Array(_)) { return Ok(None); }
                    let mut values = into_elements(list);
                    if *front { values.insert(0, value.value); } else { values.push(value.value); }
                    Ok(Some(values))
                }))?;
                self.array_write(frame, var, pushed.flatten())?;
            }
            Op::ArrayTake { var, output, front } => {
                if caught(self.array_take(frame, var, output, *front))? != Some(true) {
                    let name = self.parse(frame, var)?;
                    self.set_literal(frame, &name, "[]")?;
                    self.output_literal(frame, output, "null")?;
                }
            }
            Op::ArraySplice { var, start, item } => {
                let spliced = caught(self.array_edit(frame, var, |run, frame, list| {
                    let start = run.number(frame, start)?;
                    let item = run.src(frame, item)?;
                    if !matches!(list, Value::Array(_)) { return Ok(None); }
                    let mut values = into_elements(list);
                    let at = relative(start, values.len());
                    run.budget.work(values.len())?;
                    values.insert(at, item.value);
                    Ok(Some(values))
                }))?;
                self.array_write(frame, var, spliced.flatten())?;
            }
            Op::ArraySlice { var, start, end, output } => {
                let sliced = before(self.array_slice(frame, var, start, end))?;
                self.store(frame, output, sliced, "[]")?;
            }
            Op::ArrayIndexOf { var, value, output } => {
                let found = before(self.array_index_of(frame, var, value))?;
                self.store(frame, output, found, "-1")?;
            }
            Op::ArrayRemove { var, index } => {
                let removed = caught(self.array_edit(frame, var, |run, frame, list| {
                    let position = run.number(frame, index)?;
                    if !matches!(list, Value::Array(_)) { return Ok(None); }
                    let mut values = into_elements(list);
                    let at = relative(position, values.len());
                    if at < values.len() { values.remove(at); }
                    Ok(Some(values))
                }))?;
                self.array_write(frame, var, removed.flatten())?;
            }
            Op::ArraySet { var, index, value } => {
                let value = self.src(frame, value)?;
                let position = self.number(frame, index)?;
                // The `catch` is empty.
                if !position.is_nan() {
                    if let Some((name, json)) = caught(self.array_set(frame, var, position, value))?.flatten() { self.set_text(frame, &name, json)?; }
                }
            }
            Op::Concat { left, right, output } => {
                let left = self.src(frame, left)?;
                let right = self.src(frame, right)?;
                let joined = add(left, right, budget)?;
                self.output(frame, output, joined)?;
            }
            Op::LastByRole { role, output } => {
                let value = stored_data(self.chat.messages.iter().rev().find(|message| equals(&message.role, role)), budget)?;
                self.output(frame, output, value)?;
            }
            Op::FirstMessage { output } => {
                let value = self.first_message()?;
                self.output(frame, output, value)?;
            }
            Op::QuickSearch { value, depth, condition, output } => {
                let value = self.src(frame, value)?;
                let depth = self.number(frame, depth)?;
                if depth.is_nan() {
                    self.output_literal(frame, output, "0")?;
                } else {
                    let haystack = self.joined(0.0 - depth)?;
                    let found = self.search(condition.as_ref(), &haystack, value)?.unwrap_or(false);
                    self.output_literal(frame, output, if found { "1" } else { "0" })?;
                }
            }
            Op::RegexTest { value, regex, flags, output } => {
                // The `try` also catches the `SyntaxError` of a pattern JS rejects and the `TypeError` of a conversion.
                let found = caught(self.regex_test(frame, value, regex, flags))?.unwrap_or(false);
                self.output_literal(frame, output, if found { "1" } else { "0" })?;
            }
            Op::Calculate { expression, output } => {
                let result = before(self.calculate(frame, expression))?;
                self.store(frame, output, result, "0")?;
            }
            Op::ReplaceString { source, regex, result, replacement, flags, output } => {
                match caught(self.replace_string(frame, [source, regex, result, replacement, flags], output))? {
                    Some((key, replaced)) => self.set_text(frame, &key, replaced)?,
                    // The `catch` reads the source again and stores it as it is.
                    None => {
                        let value = self.src(frame, source)?;
                        self.output(frame, output, value)?;
                    }
                }
            }
            Op::GetNote { output } => {
                // `chat.note ?? ''`.
                let note = match &self.chat.note {
                    Value::Undefined | Value::Null => Var::text(literal("", budget)?),
                    note => {
                        let note = copy_value(note, budget)?;
                        self.vars.fresh(note)
                    }
                };
                self.output(frame, output, note)?;
            }
            Op::SetNote { value } => {
                let value = self.src(frame, value)?;
                self.chat.note = copy_value(&value.value, budget)?;
                self.emit(Effect::ChatField { field: literal("note", budget)?, value: value.value })?;
            }
            Op::ResetDict { var } => {
                let name = self.parse(frame, var)?;
                self.set_literal(frame, &name, "{}")?;
            }
            Op::DictGet { var, key, output } => {
                let value = before(self.dict_get(frame, var, key))?;
                self.store(frame, output, value, "null")?;
            }
            Op::DictSet { var, var_type, key, value } => self.dict_set(frame, var, *var_type, key, value)?,
            Op::DictDelete { var, var_type, key } => self.dict_delete(frame, var, *var_type, key)?,
            Op::DictHas { var, key, output } => {
                let has = before(self.dict_has(frame, var, key))?;
                self.store(frame, output, has, "0")?;
            }
            Op::DictSize { var, output } => {
                let size = before(self.dict_size(frame, var))?;
                self.store(frame, output, size, "0")?;
            }
            Op::DictKeys { var, values, output } => {
                match caught(self.dict_list(frame, var, *values))?.flatten() {
                    Some(json) => self.output_text(frame, output, json)?,
                    None => self.output_literal(frame, output, "[]")?,
                }
            }
        }
        Ok(here)
    }

    /// `arr.join(delimiter)` in `v2JoinArrayVar`.
    fn join(&mut self, frame: &mut Frame, var: &Src, delimiter: &Src) -> Result<Tried<Var>, StageError> {
        let Some(list) = self.src_json(frame, var)? else { return Ok(Tried::Before) };
        let delimiter = self.src(frame, delimiter)?;
        Ok(match list {
            // `join` converts the delimiter, then each element.
            Value::Array(values) => match caught_string(delimiter, self.budget)? {
                Some(delimiter) => match join(values.as_slice(), &delimiter, self.budget)? {
                    Some(joined) => Tried::Value(Var::text(joined)),
                    None => Tried::After,
                },
                None => Tried::After,
            },
            _ => Tried::After,
        })
    }

    /// `arr.length.toString()` in `v2GetArrayVarLength`.
    fn array_length(&mut self, frame: &mut Frame, var: &Text) -> Result<Tried<Var>, StageError> {
        let budget = self.budget;
        let name = self.parse(frame, var)?;
        let Some(list) = self.parsed_var(frame, &name)? else { return Ok(Tried::Before) };
        let length = [b'l', b'e', b'n', b'g', b't', b'h'].map(u16::from);
        Ok(match property(&list, &length, budget)? {
            Property::Length(length) => Tried::Value(Var::text(count_text(length, budget)?)),
            Property::Value(value) => to_string(value, budget)?.map_or(Tried::After, |text| Tried::Value(Var::text(text))),
            Property::Missing | Property::Throws => Tried::After,
            Property::Unit(_) | Property::Other => return Err(variable_value()),
        })
    }

    /// `arr[index] ?? 'null'` in `v2GetArrayVar`.
    fn array_get(&mut self, frame: &mut Frame, var: &Text, index: &Src) -> Result<Tried<Var>, StageError> {
        let name = self.parse(frame, var)?;
        let Some(list) = self.parsed_var(frame, &name)? else { return Ok(Tried::Before) };
        let position = self.number(frame, index)?;
        let key = number_key(position, self.budget)?;
        let found = property(&list, &key, self.budget)?;
        self.property_value(found)
    }

    /// The `try` of the array effects that write the array back: `varName`, `JSON.parse(getVar(varName))` and `edit`, or
    /// `None` where the `catch` runs.
    fn array_edit(&mut self, frame: &mut Frame, var: &Text, edit: impl FnOnce(&mut Self, &mut Frame, Value) -> Caught<Vec<Value>>)
        -> Caught<(Text, Text)> {
        let name = self.parse(frame, var)?;
        let Some(list) = self.parsed_var(frame, &name)? else { return Ok(None) };
        let Some(values) = edit(self, frame, list)? else { return Ok(None) };
        let json = stringify(&array(values, self.budget)?, self.budget)?;
        Ok(Some((name, json)))
    }

    /// `setVar(varName, JSON.stringify(arr))` at the end of the `try`, or the `catch`'s `setVar(varName, '[]')`.
    fn array_write(&mut self, frame: &mut Frame, var: &Text, written: Option<(Text, Text)>) -> Result<(), StageError> {
        match written {
            Some((name, json)) => self.set_text(frame, &name, json),
            None => {
                let name = self.parse(frame, var)?;
                self.set_literal(frame, &name, "[]")
            }
        }
    }

    /// The `try` of `v2PopArrayVar` and `v2ShiftArrayVar`; `false` where the `catch` runs.
    fn array_take(&mut self, frame: &mut Frame, var: &Text, output: &Text, front: bool) -> Result<bool, StageError> {
        let budget = self.budget;
        let name = self.parse(frame, var)?;
        let Some(list) = self.parsed_var(frame, &name)? else { return Ok(false) };
        // `setVar(risuChatParser(outputVar), arr.pop() ...)` parses the name before the call throws.
        let key = self.parse(frame, output)?;
        if !matches!(list, Value::Array(_)) { return Ok(false); }
        let mut values = into_elements(list);
        let item = if front { (!values.is_empty()).then(|| values.remove(0)) } else { values.pop() };
        // `arr.pop() ?? 'null'`.
        let item = match item {
            None | Some(Value::Null) => Var::text(literal("null", budget)?),
            Some(item) => self.vars.fresh(item),
        };
        self.set_var(frame, &key, item)?;
        let json = stringify(&array(values, budget)?, budget)?;
        self.set_text(frame, &name, json)?;
        Ok(true)
    }

    /// `arr.slice(start, end)` in `v2SliceArrayVar`.
    fn array_slice(&mut self, frame: &mut Frame, var: &Text, start: &Src, end: &Src) -> Result<Tried<Var>, StageError> {
        let budget = self.budget;
        let name = self.parse(frame, var)?;
        let Some(list) = self.parsed_var(frame, &name)? else { return Ok(Tried::Before) };
        let start = self.number(frame, start)?;
        let end = self.number(frame, end)?;
        Ok(match list {
            Value::Array(_) => {
                let values = into_elements(list);
                let (from, to) = (relative(start, values.len()), relative(end, values.len()));
                let values = values.into_iter().skip(from).take(to.saturating_sub(from)).collect();
                Tried::Value(Var::text(stringify(&array(values, budget)?, budget)?))
            }
            Value::Text(source) => {
                let source = units(&source);
                let (from, to) = (relative(start, source.len()), relative(end, source.len()));
                let part = text(&source[from..to.max(from)], budget)?;
                Tried::Value(Var::text(stringify(&Value::Text(part), budget)?))
            }
            _ => Tried::After,
        })
    }

    /// `arr.indexOf(value).toString()` in `v2GetIndexOfValueInArrayVar`.
    fn array_index_of(&mut self, frame: &mut Frame, var: &Text, value: &Src) -> Result<Tried<Var>, StageError> {
        let budget = self.budget;
        let name = self.parse(frame, var)?;
        let Some(list) = self.parsed_var(frame, &name)? else { return Ok(Tried::Before) };
        let value = self.src(frame, value)?;
        let at = match &list {
            Value::Array(values) => values.as_slice().iter().position(|item| strict(item, &value.value)),
            // `String.prototype.indexOf` converts its argument.
            Value::Text(source) => match caught_string(value, budget)? {
                Some(value) => index_of(&units(source), &units(&value)),
                None => return Ok(Tried::After),
            },
            _ => return Ok(Tried::After),
        };
        Ok(Tried::Value(Var::text(match at { Some(at) => count_text(at, budget)?, None => literal("-1", budget)? })))
    }

    /// The `try` of `v2SetArrayVar`: `arr[index] = value`, or `None` where it throws.
    fn array_set(&mut self, frame: &mut Frame, var: &Text, position: f64, value: Var) -> Caught<(Text, Text)> {
        let budget = self.budget;
        let name = self.parse(frame, var)?;
        let json = match self.parsed_var(frame, &name)? {
            Some(list @ Value::Array(_)) => {
                let mut values = into_elements(list);
                // Any other key is a property `JSON.stringify` leaves out.
                if let Some(at) = number_index(position) {
                    budget.work(at.saturating_sub(values.len()))?;
                    while values.len() <= at { values.push(Value::Null); }
                    values[at] = value.value;
                }
                stringify(&array(values, budget)?, budget)?
            }
            Some(mut object @ Value::Object(_)) => {
                let key = text(&number_key(position, budget)?, budget)?;
                object.define(key, value.value, budget)?;
                stringify(&object, budget)?
            }
            // A property write on null or a primitive throws in strict mode.
            _ => return Ok(None),
        };
        Ok(Some((name, json)))
    }

    /// The `try` of `v2RegexTest`, which also reads its arguments.
    fn regex_test(&mut self, frame: &mut Frame, value: &Src, regex: &Src, flags: &Src) -> Result<bool, StageError> {
        let value = self.src(frame, value)?;
        let regex = self.src(frame, regex)?;
        let flags = self.src(frame, flags)?;
        Ok(self.exec(regex, flags, value)?.is_some())
    }

    /// The `try` of `v2ReplaceString`: its source, regex, result, replacement and flags in that order, the replacement,
    /// then the parsed output name.
    fn replace_string(&mut self, frame: &mut Frame, sources: [&Src; 5], output: &Text) -> Result<(Text, Text), StageError> {
        let [source, regex, result, replacement, flags] = sources;
        let value = self.src(frame, source)?;
        let regex = self.src(frame, regex)?;
        let format = self.src(frame, result)?;
        let replacement = self.src(frame, replacement)?;
        let flags = self.src(frame, flags)?;
        let replaced = self.replace(value, regex, flags, &format, &replacement)?;
        Ok((self.parse(frame, output)?, replaced))
    }

    /// `dict[key] ?? 'null'` in `v2GetDictVar`.
    fn dict_get(&mut self, frame: &mut Frame, var: &Src, key: &Src) -> Result<Tried<Var>, StageError> {
        let Some(dict) = self.src_json(frame, var)? else { return Ok(Tried::Before) };
        // `dict[key]` converts the key after the output name is parsed.
        let Some(key) = self.key(frame, key)? else { return Ok(Tried::After) };
        let found = property(&dict, &key, self.budget)?;
        self.property_value(found)
    }

    /// `Object.hasOwn(dict, key)` in `v2HasDictKey`.
    fn dict_has(&mut self, frame: &mut Frame, var: &Src, key: &Src) -> Result<Tried<Var>, StageError> {
        let budget = self.budget;
        let Some(dict) = self.src_json(frame, var)? else { return Ok(Tried::Before) };
        let Some(key) = self.key(frame, key)? else { return Ok(Tried::After) };
        Ok(match has_own(&dict, &key, budget)? {
            Some(has) => Tried::Value(Var::text(literal(if has { "1" } else { "0" }, budget)?)),
            None => Tried::After,
        })
    }

    /// `Object.keys(dict).length` in `v2GetDictSize`.
    fn dict_size(&mut self, frame: &mut Frame, var: &Src) -> Result<Tried<Var>, StageError> {
        let budget = self.budget;
        let Some(dict) = self.src_json(frame, var)? else { return Ok(Tried::Before) };
        Ok(match own_entries(&dict, budget)? {
            Some(entries) => Tried::Value(Var::text(count_text(entries.len(), budget)?)),
            None => Tried::After,
        })
    }

    /// `Object.keys(dict)` or `Object.values(dict)` as JSON, or `None` where the `catch` runs.
    fn dict_list(&mut self, frame: &mut Frame, var: &Src, values: bool) -> Caught<Text> {
        let budget = self.budget;
        let Some(dict) = self.src_json(frame, var)? else { return Ok(None) };
        let Some(entries) = own_entries(&dict, budget)? else { return Ok(None) };
        let items = entries.into_iter().map(|(key, value)| if values { value } else { Value::Text(key) }).collect();
        Ok(Some(stringify(&array(items, budget)?, budget)?))
    }

    /// `calcString(expression).toString()` in `v2Calculate`, after each `$name` became `parseFloat(getVar(name))`;
    /// `After` where `toString` throws on an undefined result.
    fn calculate(&mut self, frame: &mut Frame, expression: &Src) -> Result<Tried<Var>, StageError> {
        let budget = self.budget;
        // `expression.replace(...)`: any other value has no such method.
        let expression = units(&receiver(self.src(frame, expression)?)?);
        let word = |unit: u16| unit == u16::from(b'_') || (unit < 128 && (unit as u8).is_ascii_alphanumeric());
        let mut replaced = Vec::with_capacity(expression.len());
        let mut at = 0;
        while at < expression.len() {
            budget.work(1)?;
            if expression[at] == u16::from(b'$') && expression.get(at + 1).copied().is_some_and(word) {
                let end = (at + 1..expression.len()).find(|end| !word(expression[*end])).unwrap_or(expression.len());
                let value = self.get_var(frame, &text(&expression[at + 1..end], budget)?)?;
                let value = units(&string_of(value, budget)?);
                budget.work(value.len())?;
                let parsed = parse_float(&value);
                replaced.extend(units(&if parsed.is_nan() { literal("0", budget)? } else { number_to_text(parsed, budget)? }));
                at = end;
            } else {
                replaced.push(expression[at]);
                at += 1;
            }
        }
        match calc_string(&replaced, &mut cbs::Budget::shared(budget)) {
            Ok(Some(result)) => Ok(Tried::Value(Var::text(text(&js_string(result), budget)?))),
            Ok(None) => Ok(Tried::After),
            Err(reason) => Err(calc_error(reason)),
        }
    }

    /// `char.firstMessage` or `char.alternateGreetings[chat.fmIndex]`, as stored. Admission has declined the shapes this
    /// does not read ([`greeting_shape`]).
    fn first_message(&mut self) -> Result<Var, StageError> {
        let budget = self.budget;
        let index = &self.input.first_message_index;
        if matches!(index, Value::Number(value) if *value == -1.0) {
            let message = copy_value(&self.input.first_message, budget)?;
            return Ok(self.vars.fresh(message));
        }
        match &self.input.alternate_greetings {
            Value::Array(_) => {}
            // `undefined[fmIndex]` throws.
            Value::Undefined | Value::Null => return Err(invalid("alternate-greetings")),
            _ => return Err(unsupported("trigger-character-shape")),
        }
        // `String(fmIndex)` as the property key.
        let key = match index {
            Value::Number(value) => number_key(*value, budget)?,
            Value::Text(key) => units(key),
            Value::Undefined => units(&literal("undefined", budget)?),
            Value::Null => units(&literal("null", budget)?),
            Value::Bool(value) => units(&literal(if *value { "true" } else { "false" }, budget)?),
            _ => return Err(unsupported("trigger-conversation-shape")),
        };
        Ok(match property(&self.input.alternate_greetings, &key, budget)? {
            Property::Missing => Var { value: Value::Undefined, id: 0 },
            Property::Value(value) => {
                let value = copy_value(value, budget)?;
                self.vars.fresh(value)
            }
            Property::Length(length) => Var { value: Value::Number(length as f64), id: 0 },
            // The array methods.
            Property::Unit(_) | Property::Other | Property::Throws => return Err(variable_value()),
        })
    }

    fn dict_set(&mut self, frame: &mut Frame, var: &Text, var_type: VarType, key_src: &Src, value_src: &Src) -> Result<(), StageError> {
        let budget = self.budget;
        if caught(self.dict_set_try(frame, var, var_type, key_src, value_src))? == Some(true) || var_type != VarType::Var { return Ok(()); }
        // The `catch` reads both again, and its own key conversion is not caught.
        let value = self.src(frame, value_src)?;
        let key = self.src_string(frame, key_src)?;
        if proto_key(&units(&key)) { return Err(unsupported("trigger-json-shape")); }
        let mut dict = Value::Object(OwnedVec::new(budget)?);
        dict.define(key, value.value, budget)?;
        let json = stringify(&dict, budget)?;
        let name = self.parse(frame, var)?;
        self.set_text(frame, &name, json)
    }

    /// The `try` of `v2SetDictVar`: `true` once it wrote the variable or returned for a `value` one.
    fn dict_set_try(&mut self, frame: &mut Frame, var: &Text, var_type: VarType, key_src: &Src, value_src: &Src) -> Result<bool, StageError> {
        let budget = self.budget;
        let value = self.src(frame, value_src)?;
        let key = self.src(frame, key_src)?;
        if var_type == VarType::Value { return Ok(true); }
        let name = self.parse(frame, var)?;
        let mut dict = match self.parsed_var(frame, &name)? {
            Some(dict @ Value::Object(_)) => dict,
            Some(Value::Array(_)) => return Err(unsupported("trigger-json-shape")),
            // A property write on null or a primitive throws in strict mode.
            Some(_) | None => return Ok(false),
        };
        // `dict[key] = value` converts the key first.
        let Some(key) = caught_string(key, budget)? else { return Ok(false) };
        let key_units = units(&key);
        if proto_key(&key_units) && !matches!(has_own(&dict, &key_units, budget)?, Some(true)) { return Err(unsupported("trigger-json-shape")); }
        dict.define(key, value.value, budget)?;
        let json = stringify(&dict, budget)?;
        let name = self.parse(frame, var)?;
        self.set_text(frame, &name, json)?;
        Ok(true)
    }

    fn dict_delete(&mut self, frame: &mut Frame, var: &Text, var_type: VarType, key: &Src) -> Result<(), StageError> {
        if var_type == VarType::Value { return Ok(()); }
        match caught(self.dict_delete_try(frame, var, key))?.flatten() {
            Some(dict) => {
                let json = stringify(&dict, self.budget)?;
                let name = self.parse(frame, var)?;
                self.set_text(frame, &name, json)
            }
            None if var_type == VarType::Var => {
                let name = self.parse(frame, var)?;
                self.set_literal(frame, &name, "{}")
            }
            None => Ok(()),
        }
    }

    /// The `try` of `v2DeleteDictKey` up to `delete dict[key]`, or `None` where the `catch` runs.
    fn dict_delete_try(&mut self, frame: &mut Frame, var: &Text, key: &Src) -> Caught<Value> {
        let budget = self.budget;
        let name = self.parse(frame, var)?;
        match self.parsed_var(frame, &name)? {
            Some(Value::Object(mut entries)) => {
                let Some(key) = self.key(frame, key)? else { return Ok(None) };
                let mut kept = OwnedVec::new(budget)?;
                for (name, value) in entries.as_mut_slice().iter_mut() {
                    budget.work(1)?;
                    if name.len_units() == key.len() && name.units().eq(key.iter().copied()) { continue; }
                    kept.push((name.copy(budget)?, std::mem::replace(value, Value::Null)))?;
                }
                Ok(Some(Value::Object(kept)))
            }
            // `delete null[key]` throws once the key is read.
            Some(Value::Null) => {
                self.src(frame, key)?;
                Ok(None)
            }
            Some(_) => Err(unsupported("trigger-json-shape")),
            None => Ok(None),
        }
    }
}

/// `__proto__`, the key a plain object assignment does not define as an own property.
fn proto_key(key: &[u16]) -> bool { key.iter().copied().eq("__proto__".encode_utf16()) }

/// The calc engine's reason: limits and cancellation keep their meaning, and any other reason, such as
/// `calc-variable` for an `@name` the engine would read from the chat, declines.
fn calc_error(reason: String) -> StageError {
    match reason.as_str() {
        "cancelled" => StageError::Cancelled,
        "budget" => StageError::Limit(Cow::Borrowed("work")),
        _ => match reason.strip_prefix("limit:") {
            Some(name) => StageError::Limit(Cow::Owned(name.to_owned())),
            None => StageError::Unsupported(Cow::Owned(reason)),
        },
    }
}

/// `message?.data ?? 'null'`, as written.
fn stored_data(message: Option<&Message>, budget: &Budget) -> Result<Var, StageError> {
    Ok(match message {
        Some(message) if !matches!(message.data.value, Value::Undefined | Value::Null) => message.data.copy(budget)?,
        _ => Var::text(literal("null", budget)?),
    })
}

/// The `v2ExtractRegex` result: `$n` from the groups, then `$&` with the match as a replacement pattern, then `$$`
/// (`:2137-2150`). `groups` is `None` when nothing matched; a group that did not match is `missing`, which `extractRegex`
/// writes as `undefined` (`:1700-1703`).
fn extract(format: &[u16], groups: Option<&[Option<Vec<u16>>]>, missing: &[u16], budget: &Budget) -> Result<Vec<u16>, StageError> {
    budget.work(format.len().saturating_mul(3))?;
    let dollar = u16::from(b'$');
    let digit = |unit: u16| (u16::from(b'0')..=u16::from(b'9')).contains(&unit);
    // `.replace(/\$[0-9]+/g, m => res[Number(m.slice(1))] || '')`.
    let mut first = Vec::with_capacity(format.len());
    let mut at = 0;
    while at < format.len() {
        if format[at] == dollar && format.get(at + 1).copied().is_some_and(digit) {
            let mut end = at + 1;
            let mut number: u64 = 0;
            while end < format.len() && digit(format[end]) {
                number = number.saturating_mul(10).saturating_add(u64::from(format[end] - u16::from(b'0')));
                end += 1;
            }
            match groups.and_then(|groups| usize::try_from(number).ok().and_then(|number| groups.get(number))) {
                Some(Some(group)) => first.extend_from_slice(group),
                _ => first.extend_from_slice(missing),
            }
            at = end;
        } else {
            first.push(format[at]);
            at += 1;
        }
    }
    // `.replace(/\$&/g, res[0] || '')`: the replacement is a pattern, so its `$$`, `$&`, `` $` `` and `$'` expand.
    let whole: &[u16] = match groups { Some(groups) => groups.first().and_then(Option::as_deref).unwrap_or(&[]), None => &[] };
    let ampersand = u16::from(b'&');
    let mut second = Vec::with_capacity(first.len());
    let mut at = 0;
    while at < first.len() {
        if first[at] == dollar && first.get(at + 1) == Some(&ampersand) {
            let expanded = substitute(whole, &[dollar, ampersand], &first, at, budget)?;
            if second.len().saturating_add(expanded.len()) > budget.limits().max_text_units { return Err(Error::Limit("text-units").into()); }
            second.extend(expanded);
            at += 2;
        } else {
            second.push(first[at]);
            at += 1;
        }
    }
    // `.replace(/\$\$/g, '$')`.
    let mut third = Vec::with_capacity(second.len());
    let mut at = 0;
    while at < second.len() {
        third.push(second[at]);
        at += if second[at] == dollar && second.get(at + 1) == Some(&dollar) { 2 } else { 1 };
    }
    Ok(third)
}

/// The callback `v2ReplaceString` gives `replaceRegexMatches` (`:2939-2958`). `groups` is the match, then its captures
/// and, when the pattern names a group, the offset `args.slice(1, -2)` keeps; each is `None` where `|| ''` gives up.
fn piece(groups: &[Option<Vec<u16>>], format: &Var, replacement: &Var, budget: &Budget) -> Result<Value, StageError> {
    // `resultFormat.match(...)`: any other value has no such method.
    let Value::Text(format) = &format.value else { return Err(type_error("trigger-string-method")) };
    let format = units(format);
    let digit = |unit: &u16| (u16::from(b'0')..=u16::from(b'9')).contains(unit);
    let whole: &[u16] = groups.first().and_then(Option::as_deref).unwrap_or(&[]);
    // `/^\$(\d+)$/`: `$0` is the replacement as it is, and a matched group is replaced inside the match.
    if format.len() > 1 && format[0] == u16::from(b'$') && format[1..].iter().all(digit) {
        let index = format[1..].iter().fold(0u64, |number, unit| number.saturating_mul(10).saturating_add(u64::from(unit - u16::from(b'0'))));
        if index == 0 { return Ok(copy_value(&replacement.value, budget)?); }
        let target = usize::try_from(index).ok().and_then(|index| groups.get(index)).and_then(Option::as_deref).filter(|target| !target.is_empty());
        if let Some(target) = target {
            // `match.replace(targetGroup, replacement)` converts the replacement and expands its `$` patterns.
            let template = units(&replacement.value.to_text(budget)?);
            let replaced = match index_of(whole, target) {
                Some(at) => {
                    let mut replaced = whole[..at].to_vec();
                    replaced.extend(substitute(&template, target, whole, at, budget)?);
                    replaced.extend_from_slice(&whole[at + target.len()..]);
                    replaced
                }
                None => whole.to_vec(),
            };
            return Ok(Value::Text(text(&replaced, budget)?));
        }
    }
    Ok(Value::Text(text(&extract(&format, Some(groups), &[], budget)?, budget)?))
}

/// `GetSubstitution` without captures: `$$`, `$&`, `` $` `` and `$'` expand, and any other `$` stays.
fn substitute(template: &[u16], matched: &[u16], string: &[u16], position: usize, budget: &Budget) -> Result<Vec<u16>, StageError> {
    let dollar = u16::from(b'$');
    let mut output = Vec::with_capacity(template.len());
    let mut at = 0;
    while at < template.len() {
        let next = if template[at] == dollar { template.get(at + 1).copied() } else { None };
        let part: Option<&[u16]> = match next {
            Some(unit) if unit == dollar => Some(&template[at..at + 1]),
            Some(unit) if unit == u16::from(b'&') => Some(matched),
            Some(unit) if unit == u16::from(b'`') => Some(&string[..position]),
            Some(unit) if unit == u16::from(b'\'') => Some(&string[(position + matched.len()).min(string.len())..]),
            _ => None,
        };
        match part {
            Some(part) => {
                budget.work(part.len())?;
                if output.len().saturating_add(part.len()) > budget.limits().max_text_units { return Err(Error::Limit("text-units").into()); }
                output.extend_from_slice(part);
                at += 2;
            }
            None => {
                output.push(template[at]);
                at += 1;
            }
        }
    }
    Ok(output)
}

/// `AdvanceStringIndex`: one code unit, or a whole surrogate pair under the `u` and `v` flags.
fn advance(source: &[u16], index: usize, unicode: bool) -> usize {
    let pair = unicode && source.get(index).is_some_and(|unit| (0xD800..0xDC00).contains(unit))
        && source.get(index + 1).is_some_and(|unit| (0xDC00..0xE000).contains(unit));
    index + if pair { 2 } else { 1 }
}
