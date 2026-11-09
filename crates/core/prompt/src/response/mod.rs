//! Response application: trimming, thoughts, multiple outputs, incomplete removal, emotion and inlay policy.
//!
//! [`ResponseStage`] is what `applyGenerationResponse` (`src/ts/process/generationResponseApplication.ts:336-579`) does
//! with a buffered provider reply, with the callbacks `sendChat` wires (`src/ts/process/index.svelte.ts:2861-2950`),
//! until it hands `{ result, emoChanged, resendChat }` back. For each output, in order:
//!
//! 1. `processOutput`: the `editoutput` scripts ([`OutputScripts`], `processScriptFull` over the trimmed text, which
//!    yields the Lua edit listeners and the plugin hooks as host requests) at `chat.message.length`. The first output of
//!    a continue runs them twice: on the reply alone, whose result is discarded, then on the continued message's data
//!    plus the reply at that message's index.
//! 2. `trimUntilPunctuation` when `removeIncompleteResponse` is on, then the emotion inlay.
//! 3. The commit: the first output is appended (or replaces the continued message); with two or more outputs each one
//!    becomes a response variant of it (`precomputedResponseVariants`).
//!
//! Then the current-chat pass parses every stored message with `runVar` ([`ResponseStages::text`]), the output trigger
//! runs ([`OutputTriggers`], `runTrigger(char, 'output')`), and the output message is found again by its `chatId`.
//!
//! The stage answers the `@@repeat_back` conversation reads itself from the conversation as it stands. Every other
//! host request comes out of [`ResponseStage::start`] or [`ResponseStage::resume`]. Effects are delivered as a
//! `TriggerRunner` delivers them: [`ResponseStage::take_effects`] moves the undelivered prefix out (before answering a
//! host request, or after an error), and the outcome carries the rest. The owner applies them in order with
//! [`apply_effect`] before it answers a request, as TS commits each change before the next call.
//!
//! When `processOutput` throws (a plugin hook that throws, a failed host call, a limit), TS commits the trimmed raw text
//! as the output (`fallbackData`) and rethrows: the stage records that commit and returns the error. Once cancelled it
//! commits nothing more. This includes a script run's `Unsupported` after a host request or an effect: the trimmed raw
//! text is stored as the output before the stage returns `Invalid("<name>-after-host")`, so the owner has stored a
//! message when it gets that error. A misuse of the stage or of its adapters is returned without that commit.
//!
//! Declines, checked over the stored data before the first host request and the first effect: `multilineEmpty` (zero
//! outputs, where TS keeps the tail message as the output target), `groupChat`, `ttsAutoSpeech` (TS speaks each output
//! through `sayTTS`), `chatOutput` (registered `pluginV2.chatOutput` listeners, which run after the output trigger),
//! `inlayImageGeneration` (`viewScreen: 'imggen'` starts image generation and commits its result later) and
//! `outputTrigger` (a character or module trigger that runs in output mode, without [`OutputTriggers`]),
//! `messageData` (a stored message whose `data` is not a string, which the continue's `data + reply` and the
//! current-chat pass coerce), plus the declines of [`OutputScripts`] runs and of [`OutputTriggers::admit`].

mod conversation;
mod text;

pub use conversation::{apply_conversation_effect, apply_effect};
pub use text::{inlay_emotion, trim_until_punctuation};

use crate::machine::{invalid, unsupported};
use conversation::{message_id, messages, Commit};
use risunest_ir::chat::MessageRole;
use risunest_ir::clock::Clock;
use risunest_ir::stage::{
    CbsConditions, ConversationRead, Effect, HostReply, HostRequest, MessageEdit, ScriptMode, ScriptOutput, ScriptRequest,
    ScriptRunner, StageError, Step, TextContext, TextProcessor, TriggerCbsVariables, TriggerMode, TriggerOutput,
    TriggerRequest, TriggerRunner,
};
use risunest_ir::{Budget, Text, Value};
use risunest_records::root::is_text;
use std::borrow::Cow;

/// A buffered provider reply (`requestDataResponse`).
#[derive(Debug)]
pub enum Reply {
    /// `{ type: 'success', result }`: one output with the role `char`. Only a string `result` is a reply: the owner
    /// declines any other so that TS applies it, since `reformatContent` throws on it and TS commits the coerced
    /// `'' + result` as the output before rethrowing.
    Success(Text),
    /// `{ type: 'multiline', result }`: `[role, text]` pairs. Only the first is stored, and with two or more each one
    /// becomes a response variant.
    Multiline(Vec<(MessageRole, Text)>),
}

/// What the stage reads from the settings and the plugin registry.
#[derive(Debug, Default)]
pub struct ResponseSettings {
    pub remove_incomplete_response: bool,
    pub tts_auto_speech: bool,
    /// `pluginV2.chatOutput.size`.
    pub output_listeners: usize,
    /// The send's chatroom is a group (`nowChatroom.type === 'group'`), whose speaking member CBS cannot name.
    pub group_chat: bool,
}

#[derive(Debug)]
pub struct ResponseInput {
    /// The speaking character as stored (`currentChar`): the stage reads `inlayViewScreen`, `viewScreen`,
    /// `triggerscript`, `firstMessage` and `alternateGreetings`; the owner builds the stages from the rest.
    pub character: Value,
    /// `getModuleTriggers()`.
    pub module_triggers: Vec<Value>,
    /// The selected chat as stored, with its complete `message` array.
    pub conversation: Value,
    pub reply: Reply,
    pub continue_generation: bool,
    /// The send's `generationId`, the output message's `chatId`.
    pub generation_id: Text,
    /// `sayingCharacterId`.
    pub saying: Text,
    pub generation_info: Value,
    pub prompt_info: Value,
    pub settings: ResponseSettings,
}

/// One `@@emo` match of a script run. The owner updates `CharEmotion[char.chaId]` from it: it drops the oldest of more
/// than four stored entries, then appends `[name, image, Date.now()]` when `image` is set
/// (`src/ts/process/scripts.ts:950-968`).
#[derive(Debug)]
pub struct Emotion {
    pub name: Text,
    /// `None` for a name the character has no image for.
    pub image: Option<Text>,
}

/// The `editoutput` runs of `processScriptFull` (`risunest_regex::Runner` through an owner adapter).
pub trait OutputScripts: ScriptRunner {
    /// `chat.message.length` for the next run, which `@@inject` indexes; `false` while a run waits for the host.
    fn set_message_count(&mut self, count: usize) -> bool;
    /// The `@@emo` matches of the run that finished last, in order.
    fn take_emotions(&mut self, budget: &Budget) -> Result<Vec<Emotion>, StageError>;
    /// Feeds an output commit, which the runs did not make, to the CBS the scripts parse.
    fn apply(&mut self, effect: &Effect, budget: &Budget) -> Result<(), StageError>;
}

/// The output trigger (`runTrigger(char, 'output', { chat })`, `src/ts/process/triggers.ts:1153-1184`): the seam for
/// the trigger interpreter.
pub trait OutputTriggers {
    /// The declines the triggers show before any host request. The stage calls it in its admission when a trigger runs
    /// in output mode.
    fn admit(&mut self, character: &Value, module_triggers: &[Value], budget: &Budget) -> Result<(), StageError>;
    /// The run over `conversation`: the conversation as the current-chat pass left it, every earlier effect applied.
    fn runner(&mut self, conversation: &Value, budget: &Budget) -> Result<Box<dyn TriggerRunner + Send>, StageError>;
}

pub struct ResponseStages {
    /// `risuChatParser` for the current-chat pass. The stage feeds it every chat variable and message change it did not
    /// make, as `Effect::ChatVariable` and `MessageEdit::{SetData, SetRole, Append}` (and the edits of the scripts).
    pub text: Box<dyn TextProcessor + Send>,
    pub scripts: Box<dyn OutputScripts + Send>,
    pub triggers: Option<Box<dyn OutputTriggers + Send>>,
    /// `Date.now()` for each commit's `time`.
    pub clock: Box<dyn Clock + Send>,
}

/// A change the owner stores, in order.
#[derive(Debug)]
pub enum ResponseEffect {
    /// A change from the scripts, the current-chat pass, the output trigger, or the reroll recovery
    /// (`Effect::ChatField` of `rerollRecovery`, which records each output commit while a reroll is generating).
    Conversation(Effect),
    /// `chat.message.push(message)`: the output message.
    AppendMessage(Value),
    /// `chat.message[index] = message`: the continued message, the output with its response variants, or a fallback.
    SetMessage { index: usize, message: Value },
    /// An `@@emo` match; it changes `CharEmotion`, not the conversation.
    Emotion(Emotion),
}

#[derive(Debug)]
pub struct ResponseApplied {
    /// `result`: the last output after the incomplete-sentence removal and the inlay.
    pub result: Text,
    /// `emoChanged` of the last output's script run.
    pub emotion_changed: bool,
    /// `resendChat`: the output trigger asked to send again.
    pub resend_chat: bool,
    /// The output message's index after the trigger.
    pub output: usize,
    pub effects: Vec<ResponseEffect>,
}

#[derive(Debug)]
pub enum ResponseOutcome {
    Applied(ResponseApplied),
    /// TS returns `null`: the output message was gone when the stage committed to it or after the output trigger.
    Stale { effects: Vec<ResponseEffect> },
}

/// The output message the stage commits to, followed as `GenerationConversationOperation` follows it: owned while the
/// message at its index has its `chatId`, and found again by that id after an edit that moves messages.
#[derive(Debug)]
struct Target { index: usize, id: Option<Text> }

enum Phase {
    Ready,
    /// A script run of `output` waits for the host; `second` is the continue's run on the continued message.
    Script { output: usize, second: bool },
    Trigger,
    Finished,
}

enum Event {
    Output,
    Script(Result<Step<ScriptOutput>, StageError>),
    Trigger(Result<Step<TriggerOutput>, StageError>),
    Finish,
    Stale,
}

pub struct ResponseStage {
    character: Value,
    module_triggers: Vec<Value>,
    conversation: Value,
    outputs: Vec<(MessageRole, Text)>,
    multiline: bool,
    continue_generation: bool,
    generation_id: Text,
    saying: Text,
    generation_info: Value,
    prompt_info: Value,
    settings: ResponseSettings,
    stages: ResponseStages,
    trigger: Option<Box<dyn TriggerRunner + Send>>,
    phase: Phase,
    pending: Vec<ResponseEffect>,
    /// A host request was answered or an effect recorded, after which a decline can no longer fall back to TS.
    progressed: bool,
    next: usize,
    target: Option<Target>,
    /// `continueBaseData`.
    base: Option<Text>,
    /// `multilineRerolls`.
    variants: Vec<Text>,
    result: Option<Text>,
    emotion_changed: bool,
    resend_chat: bool,
}

/// `canRunTrigger` in output mode: the first effect is `triggercode` or `triggerlua`, or the type is `output`.
fn output_mode_trigger(trigger: &Value, budget: &Budget) -> Result<bool, StageError> {
    let first = match trigger.get(b"effect", budget)? { Some(Value::Array(effects)) => effects.as_slice().first(), _ => None };
    let kind = match first { Some(effect) => effect.get(b"type", budget)?, None => None };
    Ok(is_text(kind, "triggercode") || is_text(kind, "triggerlua") || is_text(trigger.get(b"type", budget)?, "output"))
}

fn concat(left: &Text, right: &Text, budget: &Budget) -> Result<Text, StageError> {
    let mut text = left.copy(budget)?;
    text.append(right)?;
    Ok(text)
}

impl ResponseStage {
    pub fn new(input: ResponseInput, stages: ResponseStages) -> Self {
        let ResponseInput { character, module_triggers, conversation, reply, continue_generation, generation_id, saying,
            generation_info, prompt_info, settings } = input;
        let (outputs, multiline) = match reply {
            Reply::Success(text) => (vec![(MessageRole::Char, text)], false),
            Reply::Multiline(outputs) => (outputs, true),
        };
        Self { character, module_triggers, conversation, outputs, multiline, continue_generation, generation_id, saying,
            generation_info, prompt_info, settings, stages, trigger: None, phase: Phase::Ready, pending: Vec::new(),
            progressed: false, next: 0, target: None, base: None, variants: Vec::new(), result: None, emotion_changed: false,
            resend_chat: false }
    }

    pub fn start(&mut self, budget: &Budget) -> Result<Step<ResponseOutcome>, StageError> {
        if !matches!(self.phase, Phase::Ready) { return Err(invalid("response-started")); }
        self.phase = Phase::Finished;
        self.admit(budget)?;
        self.drive(Event::Output, budget)
    }

    pub fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<ResponseOutcome>, StageError> {
        let served = !matches!(reply, HostReply::Unsupported(_) | HostReply::Cancelled);
        let event = match self.phase {
            Phase::Script { .. } => Event::Script(self.stages.scripts.resume(reply, budget)),
            Phase::Trigger => Event::Trigger(self.trigger.as_mut().expect("trigger").resume(reply, budget)),
            Phase::Ready | Phase::Finished => return Err(invalid("response-resume")),
        };
        self.progressed |= served;
        self.drive(event, budget)
    }

    /// Moves the recorded, undelivered effects out.
    pub fn take_effects(&mut self) -> Vec<ResponseEffect> { std::mem::take(&mut self.pending) }

    /// The conversation with every effect recorded so far applied.
    pub fn conversation(&self) -> &Value { &self.conversation }

    fn admit(&mut self, budget: &Budget) -> Result<(), StageError> {
        if !matches!(self.character, Value::Object(_)) { return Err(invalid("character")); }
        let length = messages(&self.conversation, budget)?.len();
        if self.multiline && self.outputs.is_empty() { return Err(unsupported("multilineEmpty")); }
        if self.settings.group_chat { return Err(unsupported("groupChat")); }
        if self.settings.tts_auto_speech { return Err(unsupported("ttsAutoSpeech")); }
        if self.settings.output_listeners > 0 { return Err(unsupported("chatOutput")); }
        if self.inlay(budget)? == Some(false) { return Err(unsupported("inlayImageGeneration")); }
        if self.output_trigger(budget)? {
            match self.stages.triggers.as_mut() {
                None => return Err(unsupported("outputTrigger")),
                Some(triggers) => triggers.admit(&self.character, &self.module_triggers, budget)?,
            }
        }
        // `captureGenerationConversationOperation({ continueLast: true })` throws before any script runs.
        if self.continue_generation && length == 0 { return Err(invalid("continue-empty-conversation")); }
        // The continue's `data + reply` and the current-chat pass's `risuChatParser(message.data)` coerce the rest.
        for message in messages(&self.conversation, budget)? {
            budget.check()?;
            if !matches!(message.get(b"data", budget)?, Some(Value::Text(_))) { return Err(unsupported("messageData")); }
        }
        Ok(())
    }

    /// `runInlayScreen`'s branch: `Some(true)` replaces emotion tags, `Some(false)` is image generation.
    fn inlay(&self, budget: &Budget) -> Result<Option<bool>, StageError> {
        if !self.character.get(b"inlayViewScreen", budget)?.is_some_and(Value::truthy) { return Ok(None); }
        let screen = self.character.get(b"viewScreen", budget)?;
        Ok(if is_text(screen, "emotion") { Some(true) } else if is_text(screen, "imggen") { Some(false) } else { None })
    }

    fn output_trigger(&self, budget: &Budget) -> Result<bool, StageError> {
        let character = match self.character.get(b"triggerscript", budget)? { Some(Value::Array(triggers)) => triggers.as_slice(), _ => &[] };
        for trigger in character.iter().chain(&self.module_triggers) {
            if output_mode_trigger(trigger, budget)? { return Ok(true); }
        }
        Ok(false)
    }

    fn drive(&mut self, event: Event, budget: &Budget) -> Result<Step<ResponseOutcome>, StageError> {
        match self.events(event, budget) {
            Ok(Step::NeedsHost(request)) => Ok(Step::NeedsHost(request)),
            Ok(Step::Done(outcome)) => { self.phase = Phase::Finished; Ok(Step::Done(outcome)) }
            Err(error) => {
                self.phase = Phase::Finished;
                Err(match error {
                    StageError::Unsupported(name) if self.progressed => StageError::Invalid(Cow::Owned(format!("{name}-after-host"))),
                    error => error,
                })
            }
        }
    }

    fn events(&mut self, mut event: Event, budget: &Budget) -> Result<Step<ResponseOutcome>, StageError> {
        loop {
            event = match event {
                Event::Output => {
                    budget.check()?;
                    if self.next == self.outputs.len() { self.after_outputs(budget)? } else { self.begin_output(budget)? }
                }
                Event::Script(step) => match step {
                    Ok(Step::NeedsHost(HostRequest::Conversation(ConversationRead::RepeatBackSource { chat_index }))) => {
                        let reply = conversation::repeat_back_source(&self.conversation, &self.character, chat_index, budget)?;
                        Event::Script(self.stages.scripts.resume(reply, budget))
                    }
                    Ok(Step::NeedsHost(request)) => return Ok(Step::NeedsHost(request)),
                    Ok(Step::Done(output)) => self.script_done(output, budget)?,
                    Err(error) => return Err(self.script_failed(error, budget)),
                },
                Event::Trigger(step) => {
                    let step = step.and_then(|step| { self.drain_trigger(budget)?; Ok(step) });
                    match step {
                        Ok(Step::NeedsHost(request)) => return Ok(Step::NeedsHost(request)),
                        Ok(Step::Done(output)) => {
                            for effect in output.effects { self.record(ResponseEffect::Conversation(effect), budget)?; }
                            self.resend_chat = output.send_ai_prompt;
                            Event::Finish
                        }
                        Err(error) => {
                            let _ = self.drain_trigger(budget);
                            return Err(error);
                        }
                    }
                }
                Event::Finish => return self.finish(budget),
                Event::Stale => return Ok(Step::Done(ResponseOutcome::Stale { effects: self.take_effects() })),
            };
        }
    }

    fn begin_output(&mut self, budget: &Budget) -> Result<Event, StageError> {
        let output = self.next;
        let length = messages(&self.conversation, budget)?.len();
        if output == 0 && self.continue_generation {
            let index = length - 1;
            let message = &messages(&self.conversation, budget)?[index];
            let Some(Value::Text(data)) = message.get(b"data", budget)? else { return Err(invalid("message.data")) };
            let base = data.copy(budget)?;
            let id = message_id(message, budget)?.map(|id| id.copy(budget)).transpose()?;
            self.base = Some(base);
            self.target = Some(Target { index, id });
        }
        let input = self.outputs[output].1.trim(budget)?;
        self.run_script(output, false, input, length as i64, budget)
    }

    /// Only the run's own errors are `processOutput` throws: the stage's are returned without the fallback commit.
    fn run_script(&mut self, output: usize, second: bool, input: Text, chat_index: i64, budget: &Budget) -> Result<Event, StageError> {
        let count = messages(&self.conversation, budget)?.len();
        if !self.stages.scripts.set_message_count(count) { return Err(invalid("response-scripts-busy")); }
        self.phase = Phase::Script { output, second };
        let request = ScriptRequest { mode: ScriptMode::EditOutput, input, chat_index, conditions: CbsConditions::default() };
        Ok(Event::Script(self.stages.scripts.run(request, budget)))
    }

    /// A finished script run: its effects, then the continue's second run, or the output's commit.
    fn script_done(&mut self, output: ScriptOutput, budget: &Budget) -> Result<Event, StageError> {
        let Phase::Script { output: index, second } = self.phase else { return Err(invalid("response-script")) };
        let ScriptOutput { text, emotion_changed, effects } = output;
        for effect in effects {
            self.stages.text.apply(&effect, budget)?;
            let moves = conversation::moves_messages(&effect);
            self.record(ResponseEffect::Conversation(effect), budget)?;
            if moves { self.retarget(budget)?; }
        }
        for emotion in self.stages.scripts.take_emotions(budget)? { self.record(ResponseEffect::Emotion(emotion), budget)?; }
        let continuing = index == 0 && self.continue_generation;
        if continuing && !second {
            let target = self.target.as_ref().expect("continued message").index;
            let input = concat(self.base.as_ref().expect("base"), &self.outputs[0].1, budget)?.trim(budget)?;
            return self.run_script(0, true, input, target as i64, budget);
        }
        let mut data = text;
        if self.settings.remove_incomplete_response { data = trim_until_punctuation(&data, budget)?; }
        let result = if self.inlay(budget)? == Some(true) { inlay_emotion(&data, budget)? } else { data };
        self.emotion_changed = emotion_changed;
        if continuing {
            if !self.owned(budget)? { return Ok(Event::Stale); }
            let message = self.commit_fields().continued(self.target_message(budget)?, &result, budget)?;
            self.commit_target(message, budget)?;
        } else if index == 0 {
            let role = self.outputs[0].0;
            self.append_output(role, &result, budget)?;
        }
        self.variants.push(result.copy(budget)?);
        self.result = Some(result);
        self.next = index + 1;
        Ok(Event::Output)
    }

    /// `processOutput` threw: TS commits the trimmed raw text and rethrows, or returns `null` once aborted.
    fn script_failed(&mut self, error: StageError, budget: &Budget) -> StageError {
        let Phase::Script { output, .. } = self.phase else { return error };
        match error {
            StageError::Cancelled => error,
            StageError::Unsupported(_) if !self.progressed => error,
            error => {
                // The commit sits in its own `try {} catch {}`.
                let _ = self.commit_fallback(output, budget);
                error
            }
        }
    }

    fn commit_fallback(&mut self, output: usize, budget: &Budget) -> Result<(), StageError> {
        let continuing = output == 0 && self.continue_generation;
        let raw = &self.outputs[output].1;
        let fallback = match (continuing, self.base.as_ref()) {
            (true, Some(base)) => concat(base, raw, budget)?.trim(budget)?,
            _ => raw.trim(budget)?,
        };
        if continuing {
            if self.owned(budget)? {
                let message = self.commit_fields().continued(self.target_message(budget)?, &fallback, budget)?;
                self.commit_target(message, budget)?;
            }
        } else if output == 0 {
            let role = self.outputs[0].0;
            self.append_output(role, &fallback, budget)?;
        } else if self.owned(budget)? {
            // `outputTarget.commitData(fallbackData)` on the first output's message.
            let message = conversation::with_data(self.target_message(budget)?, &fallback, budget)?;
            self.commit_target(message, budget)?;
        }
        Ok(())
    }

    /// The response variants, then the current-chat pass and the output trigger.
    fn after_outputs(&mut self, budget: &Budget) -> Result<Event, StageError> {
        if self.variants.len() > 1 {
            if !self.owned(budget)? { return Ok(Event::Stale); }
            let message = conversation::with_variants(self.target_message(budget)?, &self.variants, budget)?;
            self.commit_target(message, budget)?;
        }
        if self.target.is_some() && !self.owned(budget)? { return Ok(Event::Stale); }
        self.current_chat_pass(budget)?;
        if !self.output_trigger(budget)? { return Ok(Event::Finish); }
        let Some(triggers) = self.stages.triggers.as_mut() else { return Err(unsupported("outputTrigger")) };
        let mut runner = triggers.runner(&self.conversation, budget)?;
        self.phase = Phase::Trigger;
        let step = runner.run(TriggerRequest { mode: TriggerMode::Output, display: None, manual_name: None, display_mode: false,
            temp_vars: None, trigger_id: None, cbs_variables: TriggerCbsVariables::TriggerScope }, budget);
        self.trigger = Some(runner);
        Ok(Event::Trigger(step))
    }

    /// `runCurrentChatParserPass` (`src/ts/process/currentChatParserPass.ts:42-140`): `risuChatParser(message.data,
    /// { chara, runVar: true })` over every message in order, writing back the data that changed.
    fn current_chat_pass(&mut self, budget: &Budget) -> Result<(), StageError> {
        let length = messages(&self.conversation, budget)?.len();
        for index in 0..length {
            budget.check()?;
            let data = match messages(&self.conversation, budget)?[index].get(b"data", budget)? {
                Some(Value::Text(data)) => data.copy(budget)?,
                // Only a message the scripts stored gets here: the stage has stored the output already.
                _ => return Err(unsupported("messageData")),
            };
            let context = TextContext { run_var: true, ..TextContext::default() };
            let output = self.stages.text.process(&data, &context, budget)?;
            for effect in output.effects { self.record(ResponseEffect::Conversation(effect), budget)?; }
            if !output.text.equals(&data) {
                let edit = Effect::Message(MessageEdit::SetData { index: index as f64, data: Value::Text(output.text) });
                self.stages.text.apply(&edit, budget)?;
                self.record(ResponseEffect::Conversation(edit), budget)?;
            }
        }
        Ok(())
    }

    fn drain_trigger(&mut self, budget: &Budget) -> Result<(), StageError> {
        let effects = self.trigger.as_mut().map(|runner| runner.take_effects()).unwrap_or_default();
        for effect in effects { self.record(ResponseEffect::Conversation(effect), budget)?; }
        Ok(())
    }

    /// The output message found again by its id (`recaptureGenerationConversationOperation`).
    fn finish(&mut self, budget: &Budget) -> Result<Step<ResponseOutcome>, StageError> {
        let mut output = None;
        for (index, message) in messages(&self.conversation, budget)?.iter().enumerate() {
            if message_id(message, budget)?.is_some_and(|id| id.equals(&self.generation_id)) { output = Some(index); break; }
        }
        let Some(output) = output else { return Ok(Step::Done(ResponseOutcome::Stale { effects: self.take_effects() })) };
        let result = match self.result.take() { Some(result) => result, None => Text::from_str("", budget)? };
        Ok(Step::Done(ResponseOutcome::Applied(ResponseApplied {
            result, emotion_changed: self.emotion_changed, resend_chat: self.resend_chat, output, effects: self.take_effects(),
        })))
    }

    fn commit_fields(&self) -> Commit<'_> {
        Commit { saying: &self.saying, time: self.stages.clock.now_ms(), generation_info: &self.generation_info,
            prompt_info: &self.prompt_info, chat_id: &self.generation_id }
    }

    fn target_message(&self, budget: &Budget) -> Result<&Value, StageError> {
        let index = self.target.as_ref().ok_or_else(|| invalid("response-target"))?.index;
        messages(&self.conversation, budget)?.get(index).ok_or_else(|| invalid("response-target"))
    }

    /// `isOwned()`: the message at the target's index still has the target's `chatId`.
    fn owned(&self, budget: &Budget) -> Result<bool, StageError> {
        let Some(target) = &self.target else { return Ok(false) };
        let Some(message) = messages(&self.conversation, budget)?.get(target.index) else { return Ok(false) };
        Ok(match (message_id(message, budget)?, &target.id) {
            (Some(stored), Some(id)) => stored.equals(id),
            (None, None) => true,
            _ => false,
        })
    }

    /// After an edit that moves messages, the target is the first message with its id.
    fn retarget(&mut self, budget: &Budget) -> Result<(), StageError> {
        let Some(Target { id: Some(id), .. }) = &self.target else { return Ok(()) };
        let mut found = None;
        for (index, message) in messages(&self.conversation, budget)?.iter().enumerate() {
            if message_id(message, budget)?.is_some_and(|stored| stored.equals(id)) { found = Some(index); break; }
        }
        if let (Some(index), Some(target)) = (found, self.target.as_mut()) { target.index = index; }
        Ok(())
    }

    fn append_output(&mut self, role: MessageRole, data: &Text, budget: &Budget) -> Result<(), StageError> {
        let message = self.commit_fields().appended(role, data, budget)?;
        self.track_reroll(&message, budget)?;
        let index = messages(&self.conversation, budget)?.len();
        let edit = Effect::Message(MessageEdit::Append { role, data: Value::Text(data.copy(budget)?), chat_id: self.generation_id.copy(budget)? });
        self.share(&edit, budget)?;
        self.record(ResponseEffect::AppendMessage(message), budget)?;
        self.target = Some(Target { index, id: Some(self.generation_id.copy(budget)?) });
        Ok(())
    }

    /// `commitMessage(message)` on the owned target.
    fn commit_target(&mut self, message: Value, budget: &Budget) -> Result<(), StageError> {
        self.track_reroll(&message, budget)?;
        let index = self.target.as_ref().ok_or_else(|| invalid("response-target"))?.index;
        let current = &messages(&self.conversation, budget)?[index];
        let mut edits = Vec::new();
        if let Some(Value::Text(data)) = message.get(b"data", budget)? {
            if !matches!(current.get(b"data", budget)?, Some(Value::Text(stored)) if stored.equals(data)) {
                edits.push(Effect::Message(MessageEdit::SetData { index: index as f64, data: Value::Text(data.copy(budget)?) }));
            }
        }
        let role = message.get(b"role", budget)?;
        let same_role = matches!((role, current.get(b"role", budget)?), (Some(Value::Text(next)), Some(Value::Text(stored))) if next.equals(stored));
        if role.is_some() && !same_role {
            let role = if is_text(role, "user") { MessageRole::User } else { MessageRole::Char };
            edits.push(Effect::Message(MessageEdit::SetRole { index: index as f64, role }));
        }
        for edit in &edits { self.share(edit, budget)?; }
        let id = message_id(&message, budget)?.map(|id| id.copy(budget)).transpose()?;
        self.record(ResponseEffect::SetMessage { index, message }, budget)?;
        if let Some(target) = self.target.as_mut() { target.id = id; }
        Ok(())
    }

    fn track_reroll(&mut self, message: &Value, budget: &Budget) -> Result<(), StageError> {
        if let Some(recovery) = conversation::tracked_reroll(&self.conversation, message, budget)? {
            let effect = Effect::ChatField { field: Text::from_str("rerollRecovery", budget)?, value: recovery };
            self.record(ResponseEffect::Conversation(effect), budget)?;
        }
        Ok(())
    }

    /// Feeds an output commit to the CBS of the current-chat pass and of later script runs.
    fn share(&mut self, effect: &Effect, budget: &Budget) -> Result<(), StageError> {
        self.stages.text.apply(effect, budget)?;
        self.stages.scripts.apply(effect, budget)
    }

    fn record(&mut self, effect: ResponseEffect, budget: &Budget) -> Result<(), StageError> {
        apply_effect(&mut self.conversation, &effect, budget)?;
        self.pending.push(effect);
        self.progressed = true;
        Ok(())
    }
}
