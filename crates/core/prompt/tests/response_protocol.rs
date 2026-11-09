//! The response stage's host protocol: declines before any host request or effect, misuse, cancellation, effect
//! delivery, the output trigger seam, and the effect applier.

mod response_support;

use response_support::*;
use risunest_ir::stage::{
    Effect, HostReply, HostRequest, MessageEdit, ScriptOutput, ScriptRequest, ScriptRunner, StageError, Step, TriggerOutput,
    TriggerRequest, TriggerRunner,
};
use risunest_ir::{Budget, Value};
use risunest_prompt::response::{
    apply_conversation_effect, Emotion, OutputScripts, OutputTriggers, ResponseEffect, ResponseInput, ResponseOutcome,
    ResponseSettings, ResponseStage,
};
use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const CHARACTER: &str = r#"{"chaId":"character","name":"Synthetic character","type":"character","firstMessage":"Hello.","alternateGreetings":[],"customscript":[],"triggerscript":[]}"#;
const CONVERSATION: &str = r#"{"id":"conversation","fmIndex":-1,"scriptstate":{"$mood":"calm"},"message":[{"role":"user","data":"Question?","chatId":"message-a"}]}"#;

fn entry(character: &str, reply: &str, settings: &str, hooks: usize, budget: &Budget) -> Value {
    parse(&format!(r#"{{"response":{reply},"continue":false,"generationId":"generation","saying":"character","generationInfo":{{"generationId":"generation"}},
        "promptInfo":{{}},"character":{character},"presetRegex":[],"settings":{settings},"plugins":{{"editoutput":{hooks},"chatOutput":0}},
        "conversation":{CONVERSATION}}}"#), budget)
}

const PLAIN: &str = r#"{"removeIncompleteResponse":false,"ttsAutoSpeech":false}"#;
const ONE: &str = r#"{"type":"success","result":"Answer."}"#;
const TWO: &str = r#"{"type":"multiline","result":[["char","First."],["char","Second."]]}"#;

fn stage(entry: &Value, adjust: impl FnOnce(&mut ResponseInput), triggers: Option<Box<dyn OutputTriggers + Send>>, budget: &Budget) -> ResponseStage {
    let mut inputs = inputs(entry, budget);
    adjust(&mut inputs.input);
    let stages = stages(&mut inputs, triggers, budget);
    ResponseStage::new(inputs.input, stages)
}

fn hooked(_: usize, request: &HostRequest, budget: &Budget) -> HostReply {
    match request {
        HostRequest::PluginHook { payload: Value::Text(data), .. } => {
            let mut data = data.copy(budget).unwrap();
            data.append(&text(" [h]", budget)).unwrap();
            HostReply::Value(Value::Text(data))
        }
        other => panic!("unexpected request {other:?}"),
    }
}

fn declined(result: &Result<Step<ResponseOutcome>, StageError>, name: &str) -> bool {
    matches!(result, Err(StageError::Unsupported(declined)) if declined == name)
}

const OUTPUT_TRIGGER: &str = r#"{"chaId":"character","name":"Synthetic character","type":"character","firstMessage":"Hello.","alternateGreetings":[],"customscript":[],
    "triggerscript":[{"comment":"","type":"output","conditions":[],"effect":[{"type":"setvar","operator":"=","var":"seen","value":"yes"}]}]}"#;

#[test]
fn declines_come_before_any_host_request_or_effect() {
    let budget = budget();
    let with = |settings: ResponseSettings| move |input: &mut ResponseInput| input.settings = settings;
    let imggen = CHARACTER.replace(r#""customscript":[]"#, r#""customscript":[],"inlayViewScreen":true,"viewScreen":"imggen""#);
    let lua = CHARACTER.replace(r#""triggerscript":[]"#, r#""triggerscript":[{"comment":"","type":"start","conditions":[],"effect":[{"type":"triggerlua","code":""}]}]"#);
    let cases: Vec<(&str, ResponseStage)> = vec![
        ("groupChat", stage(&entry(CHARACTER, ONE, PLAIN, 1, &budget), with(ResponseSettings { group_chat: true, ..Default::default() }), None, &budget)),
        ("ttsAutoSpeech", stage(&entry(CHARACTER, ONE, PLAIN, 1, &budget), with(ResponseSettings { tts_auto_speech: true, ..Default::default() }), None, &budget)),
        ("chatOutput", stage(&entry(CHARACTER, ONE, PLAIN, 1, &budget), with(ResponseSettings { output_listeners: 1, ..Default::default() }), None, &budget)),
        ("inlayImageGeneration", stage(&entry(&imggen, ONE, PLAIN, 1, &budget), |_| {}, None, &budget)),
        ("outputTrigger", stage(&entry(OUTPUT_TRIGGER, ONE, PLAIN, 1, &budget), |_| {}, None, &budget)),
        // A `triggerlua` first effect runs in every mode, output included.
        ("outputTrigger", stage(&entry(&lua, ONE, PLAIN, 1, &budget), |_| {}, None, &budget)),
        ("multilineEmpty", stage(&entry(CHARACTER, r#"{"type":"multiline","result":[]}"#, PLAIN, 1, &budget), |_| {}, None, &budget)),
        ("triggerEffect", stage(&entry(OUTPUT_TRIGGER, ONE, PLAIN, 1, &budget), |_| {},
            Some(Box::new(Triggers { decline: Some("triggerEffect"), ..Triggers::default() })), &budget)),
        // TS continues with `data + reply`, and the current-chat pass parses every stored message.
        ("messageData", stage(&entry(CHARACTER, ONE, PLAIN, 1, &budget), |input| {
            input.continue_generation = true;
            input.conversation = parse(&CONVERSATION.replace(r#""data":"Question?""#, r#""data":["Question?"]"#), &budget);
        }, None, &budget)),
        ("messageData", stage(&entry(CHARACTER, ONE, PLAIN, 1, &budget), |input| {
            input.conversation = parse(&CONVERSATION.replace(r#""message":["#, r#""message":[{"role":"char","data":7,"chatId":"message-0"},"#), &budget);
        }, None, &budget)),
    ];
    for (name, mut stage) in cases {
        let result = stage.start(&budget);
        assert!(declined(&result, name), "{name}: {result:?}");
        assert!(stage.take_effects().is_empty(), "{name}: effects before the decline");
        assert!(matches!(stage.resume(HostReply::Value(Value::Null), &budget), Err(StageError::Invalid(name)) if name == "response-resume"));
    }

    // A decline of the script runner's own admission is forwarded before its hooks run.
    let entry = entry(CHARACTER, ONE, PLAIN, 1, &budget);
    let mut inputs = inputs(&entry, &budget);
    let mut stages = stages(&mut inputs, None, &budget);
    let mut config = risunest_regex::runner::ScriptRunnerConfig::new(vec![risunest_regex::script::CustomScript::from_value(
        &parse(r#"{"comment":"","in":"a","out":"b","type":"editoutput"}"#, &budget), &budget).unwrap()]);
    config.dynamic_assets = true;
    config.plugin_hooks.edit_output = 1;
    stages.scripts = Box::new(Scripts(risunest_regex::runner::Runner::new(config,
        LiteralCbs::new(&text("Synthetic character", &budget), None, &budget), risunest_regex::cache::ResultLimits::NORMAL)));
    let mut stage = ResponseStage::new(inputs.input, stages);
    let result = stage.start(&budget);
    assert!(declined(&result, "dynamicAssets"), "{result:?}");
    assert!(stage.take_effects().is_empty());
}

#[test]
fn start_and_resume_follow_the_protocol() {
    let budget = budget();
    let entry = entry(CHARACTER, ONE, PLAIN, 1, &budget);
    let mut stage = stage(&entry, |_| {}, None, &budget);
    assert!(matches!(stage.resume(HostReply::Value(Value::Null), &budget), Err(StageError::Invalid(name)) if name == "response-resume"));
    assert!(matches!(stage.start(&budget), Ok(Step::NeedsHost(HostRequest::PluginHook { .. }))));
    assert!(matches!(stage.start(&budget), Err(StageError::Invalid(name)) if name == "response-started"));
    let done = stage.resume(HostReply::Value(Value::Undefined), &budget);
    let Ok(Step::Done(ResponseOutcome::Applied(applied))) = done else { panic!("{done:?}") };
    assert_eq!(string(&applied.result), "Answer.");
    assert!(stage.take_effects().is_empty(), "Done carries every undelivered effect");
    assert!(matches!(stage.resume(HostReply::Value(Value::Null), &budget), Err(StageError::Invalid(name)) if name == "response-resume"));

    // Scripts still waiting for the host are the adapter's misuse, not a `processOutput` throw: no fallback commit.
    let mut inputs = inputs(&entry, &budget);
    let mut stages = stages(&mut inputs, None, &budget);
    stages.scripts = Box::new(Busy);
    let mut stage = ResponseStage::new(inputs.input, stages);
    let busy = stage.start(&budget);
    assert!(matches!(&busy, Err(StageError::Invalid(name)) if name == "response-scripts-busy"), "{busy:?}");
    assert!(stage.take_effects().is_empty(), "a fallback was committed");
}

/// Scripts whose earlier run still waits for the host.
struct Busy;

impl ScriptRunner for Busy {
    fn run(&mut self, _: ScriptRequest, _: &Budget) -> Result<Step<ScriptOutput>, StageError> { panic!("a busy runner is not run") }
    fn resume(&mut self, _: HostReply, _: &Budget) -> Result<Step<ScriptOutput>, StageError> { panic!("a busy runner is not resumed") }
}

impl OutputScripts for Busy {
    fn set_message_count(&mut self, _: usize) -> bool { false }
    fn take_emotions(&mut self, _: &Budget) -> Result<Vec<Emotion>, StageError> { Ok(Vec::new()) }
    fn apply(&mut self, _: &Effect, _: &Budget) -> Result<(), StageError> { Ok(()) }
}

#[test]
fn effects_are_delivered_once_whether_drained_or_not() {
    let budget = budget();
    let entry = entry(CHARACTER, TWO, PLAIN, 1, &budget);
    let run = |drain: bool| {
        let mut stage = stage(&entry, |_| {}, None, &budget);
        let conversation = copy(&stage_conversation(&entry, &budget), &budget);
        let (owner, result) = drive(&mut stage, conversation, drain, &budget, hooked);
        assert!(matches!(result, Ok(ResponseOutcome::Applied(_))), "{result:?}");
        (json(&owner.conversation, &budget), owner.effects.len(), owner.drained, json(stage.conversation(), &budget))
    };
    let (whole, count, none, stage_whole) = run(false);
    let (drained, drained_count, taken, stage_drained) = run(true);
    assert_eq!(none, 0);
    // The second output's hook is asked after the first output was appended.
    assert!(taken >= 1, "nothing was delivered before the second hook");
    assert_eq!((whole.as_str(), count), (drained.as_str(), drained_count));
    assert_eq!(whole, stage_whole);
    assert_eq!(drained, stage_drained);
    assert!(whole.contains(r#""data":"First. [h]""#) && whole.contains(r#""responseVariants""#), "{whole}");
}

fn stage_conversation(entry: &Value, budget: &Budget) -> Value { copy(field(entry, "conversation", budget).unwrap(), budget) }

#[test]
fn cancellation_keeps_the_committed_prefix_without_a_fallback() {
    let cancel = Arc::new(AtomicBool::new(false));
    let budget = budget_with(cancel.clone());
    let entry = entry(CHARACTER, TWO, PLAIN, 1, &budget);
    let mut stage = stage(&entry, |_| {}, None, &budget);
    let mut owner = budget_owner(&entry);
    assert!(matches!(stage.start(&budget), Ok(Step::NeedsHost(_))));
    let second = stage.resume(HostReply::Value(Value::Undefined), &budget);
    assert!(matches!(second, Ok(Step::NeedsHost(HostRequest::PluginHook { .. }))), "{second:?}");
    owner.deliver(stage.take_effects());
    cancel.store(true, Ordering::Relaxed);
    let result = stage.resume(HostReply::Value(Value::Text(text("Second. [h]", &owner.budget))), &budget);
    assert!(matches!(result, Err(StageError::Cancelled)), "{result:?}");
    owner.deliver(stage.take_effects());
    let stored = json(&owner.conversation, &owner.budget);
    // The first output stays as appended: no fallback replaces its data and no variants are added.
    assert!(stored.contains(r#""data":"First.""#) && !stored.contains("Second") && !stored.contains("responseVariants"), "{stored}");

    // A host reply of `Cancelled` ends the run the same way.
    let budget = response_support::budget();
    let mut stage = stage_for(&entry, &budget);
    assert!(matches!(stage.start(&budget), Ok(Step::NeedsHost(_))));
    assert!(matches!(stage.resume(HostReply::Cancelled, &budget), Err(StageError::Cancelled)));
    assert!(stage.take_effects().is_empty());
}

fn stage_for(entry: &Value, budget: &Budget) -> ResponseStage { stage(entry, |_| {}, None, budget) }

/// An owner with its own budget, which a cancelled send leaves usable.
struct BudgetOwner { budget: Budget, conversation: Value }
fn budget_owner(entry: &Value) -> BudgetOwner {
    let budget = response_support::budget();
    let conversation = stage_conversation(entry, &budget);
    BudgetOwner { budget, conversation }
}
impl BudgetOwner {
    fn deliver(&mut self, effects: Vec<ResponseEffect>) {
        for effect in effects { risunest_prompt::response::apply_effect(&mut self.conversation, &effect, &self.budget).unwrap(); }
    }
}

/// The trigger seam stand-in: records the conversation it was built over, delivers `prefix` before one host request,
/// then finishes with `suffix`.
#[derive(Default)]
struct Triggers { decline: Option<&'static str>, run_error: Option<&'static str>, built: Arc<Mutex<Vec<String>>>, suffix: Option<String> }

struct FakeRunner { prefix: Vec<Effect>, suffix: Option<String>, run_error: Option<&'static str>, requested: Option<TriggerRequest> }

impl OutputTriggers for Triggers {
    fn admit(&mut self, _: &Value, _: &[Value], _: &Budget) -> Result<(), StageError> {
        match self.decline { Some(name) => Err(StageError::Unsupported(Cow::Borrowed(name))), None => Ok(()) }
    }
    fn runner(&mut self, conversation: &Value, budget: &Budget) -> Result<Box<dyn TriggerRunner + Send>, StageError> {
        self.built.lock().unwrap().push(json(conversation, budget));
        let prefix = vec![Effect::ChatVariable { name: text("seen", budget), value: Value::Text(text("yes", budget)) }];
        Ok(Box::new(FakeRunner { prefix, suffix: self.suffix.clone(), run_error: self.run_error, requested: None }))
    }
}

fn output(effects: Vec<Effect>, budget: &Budget) -> TriggerOutput {
    TriggerOutput { system_prompt_start: text("", budget), system_prompt_history_end: text("", budget), system_prompt_end: text("", budget),
        effects, tokens: 0, stop_sending: false, send_ai_prompt: true, display: None, temp_vars: None, current_trigger_id: None }
}

impl TriggerRunner for FakeRunner {
    fn run(&mut self, request: TriggerRequest, budget: &Budget) -> Result<Step<TriggerOutput>, StageError> {
        if let Some(name) = self.run_error { return Err(StageError::Unsupported(Cow::Borrowed(name))); }
        self.requested = Some(request);
        Ok(Step::NeedsHost(HostRequest::PluginTokenizer { text: text("trigger", budget) }))
    }
    fn resume(&mut self, _: HostReply, budget: &Budget) -> Result<Step<TriggerOutput>, StageError> {
        let suffix = match self.suffix.as_deref() {
            Some("cut") => vec![Effect::Message(MessageEdit::Cut { start: 0.0, end: Some(1.0) })],
            _ => vec![Effect::Message(MessageEdit::SetData { index: 1.0, data: Value::Text(text("Triggered.", budget)) })],
        };
        Ok(Step::Done(output(suffix, budget)))
    }
    fn take_effects(&mut self) -> Vec<Effect> { std::mem::take(&mut self.prefix) }
}

#[test]
fn the_output_trigger_runs_over_the_parsed_conversation() {
    let budget = budget();
    let reply = r#"{"type":"success","result":"Hi{{setvar::mood::happy}} {{char}}."}"#;
    let entry = entry(OUTPUT_TRIGGER, reply, PLAIN, 0, &budget);
    let built = Arc::new(Mutex::new(Vec::new()));
    let mut stage = stage(&entry, |_| {}, Some(Box::new(Triggers { built: built.clone(), ..Triggers::default() })), &budget);
    let step = stage.start(&budget);
    assert!(matches!(step, Ok(Step::NeedsHost(HostRequest::PluginTokenizer { .. }))), "{step:?}");
    let before: Vec<String> = stage.take_effects().iter().map(|effect| format!("{effect:?}")).collect();
    // The trigger's completed prefix is delivered before its request goes out.
    assert!(before.last().is_some_and(|last| last.contains("ChatVariable") && last.contains("seen")), "{before:?}");
    let conversation = built.lock().unwrap().clone();
    assert_eq!(conversation.len(), 1);
    assert!(conversation[0].contains(r#""data":"Hi Synthetic character.""#) && conversation[0].contains(r#""$mood":"happy""#), "{}", conversation[0]);
    let done = stage.resume(HostReply::Value(Value::Number(1.0)), &budget);
    let Ok(Step::Done(ResponseOutcome::Applied(applied))) = done else { panic!("{done:?}") };
    assert!(applied.resend_chat);
    assert_eq!(applied.output, 1);
    assert_eq!(string(&applied.result), "Hi{{setvar::mood::happy}} Synthetic character.");
    let stored = json(stage.conversation(), &budget);
    assert!(stored.contains(r#""data":"Triggered.""#) && stored.contains(r#""$seen":"yes""#), "{stored}");
}

#[test]
fn a_trigger_that_removes_the_output_leaves_the_response_stale() {
    let budget = budget();
    let entry = entry(OUTPUT_TRIGGER, ONE, PLAIN, 0, &budget);
    let mut stage = stage(&entry, |_| {}, Some(Box::new(Triggers { suffix: Some("cut".into()), ..Triggers::default() })), &budget);
    assert!(matches!(stage.start(&budget), Ok(Step::NeedsHost(_))));
    let done = stage.resume(HostReply::Value(Value::Null), &budget);
    let Ok(Step::Done(ResponseOutcome::Stale { effects })) = done else { panic!("{done:?}") };
    assert!(effects.iter().any(|effect| matches!(effect, ResponseEffect::Conversation(Effect::Message(MessageEdit::Cut { .. })))));

    // A trigger decline once the output is committed can no longer fall back to TS.
    let mut stage = stage_with_run_error(&entry, &budget);
    let result = stage.start(&budget);
    assert!(matches!(&result, Err(StageError::Invalid(name)) if name == "triggerEffect-after-host"), "{result:?}");
    assert!(stage.take_effects().iter().any(|effect| matches!(effect, ResponseEffect::AppendMessage(_))));
}

fn stage_with_run_error(entry: &Value, budget: &Budget) -> ResponseStage {
    stage(entry, |_| {}, Some(Box::new(Triggers { run_error: Some("triggerEffect"), ..Triggers::default() })), budget)
}

#[test]
fn effects_apply_as_typescript_stores_them() {
    let budget = budget();
    let base = r#"{"message":[{"role":"user","data":"a","chatId":"1"},{"role":"char","data":"b","chatId":"2"},{"role":"user","data":"c","chatId":"3"}]}"#;
    let apply = |edit: MessageEdit| {
        let mut conversation = parse(base, &budget);
        apply_conversation_effect(&mut conversation, &Effect::Message(edit), &budget).unwrap();
        values(field(&conversation, "message", &budget).unwrap()).iter()
            .map(|message| match field(message, "data", &budget) { Some(Value::Text(data)) => string(data), _ => "?".into() }).collect::<Vec<_>>().join("")
    };
    let data = |value: &str| Value::Text(text(value, &budget));
    // `Array.prototype.at`: from the end when negative, truncated, nothing past the end.
    assert_eq!(apply(MessageEdit::SetData { index: -1.0, data: data("z") }), "abz");
    assert_eq!(apply(MessageEdit::SetData { index: 1.7, data: data("z") }), "azc");
    assert_eq!(apply(MessageEdit::SetData { index: f64::NAN, data: data("z") }), "zbc");
    assert_eq!(apply(MessageEdit::SetData { index: 3.0, data: data("z") }), "abc");
    // `slice`: an undefined end keeps the rest, a `NaN` end keeps nothing.
    assert_eq!(apply(MessageEdit::Cut { start: 1.0, end: None }), "bc");
    assert_eq!(apply(MessageEdit::Cut { start: 1.0, end: Some(f64::NAN) }), "");
    assert_eq!(apply(MessageEdit::Cut { start: -2.0, end: Some(-1.0) }), "b");
    // `splice` starts.
    assert_eq!(apply(MessageEdit::Remove { index: -1.0 }), "ab");
    assert_eq!(apply(MessageEdit::Remove { index: 9.0 }), "abc");
    let insert = |index: f64| MessageEdit::Insert { index, role: risunest_ir::chat::MessageRole::Char, data: data("z"), chat_id: text("4", &budget) };
    assert_eq!(apply(insert(1.0)), "azbc");
    assert_eq!(apply(insert(9.0)), "abcz");
    assert_eq!(apply(insert(-1.0)), "abzc");
    // The data is stored as the edit carries it, a non-string included.
    let mut conversation = parse(r#"{"message":[{"role":"user","data":"a","chatId":"1"}]}"#, &budget);
    for edit in [MessageEdit::SetData { index: 0.0, data: Value::Number(5.0) },
        MessageEdit::Append { role: risunest_ir::chat::MessageRole::Char, data: Value::Bool(true), chat_id: text("2", &budget) }] {
        apply_conversation_effect(&mut conversation, &Effect::Message(edit), &budget).unwrap();
    }
    assert_eq!(json(&conversation, &budget), r#"{"message":[{"role":"user","data":5,"chatId":"1"},{"role":"char","data":true,"chatId":"2"}]}"#);

    // `setChatVar` creates `scriptstate` and stores under `$name`; an unchanged value keeps its place.
    let mut conversation = parse(r#"{"message":[]}"#, &budget);
    let write = |conversation: &mut Value, name: &str, value: &str| apply_conversation_effect(conversation,
        &Effect::ChatVariable { name: text(name, &budget), value: data(value) }, &budget).unwrap();
    write(&mut conversation, "a", "1");
    write(&mut conversation, "b", "2");
    write(&mut conversation, "a", "1");
    assert_eq!(json(&conversation, &budget), r#"{"message":[],"scriptstate":{"$a":"1","$b":"2"}}"#);
}
