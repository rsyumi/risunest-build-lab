use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::stage::{Effect, EmbedRequest, HostReply, HostRequest, MemoryInput, MemoryOutput, MemoryStage, StageError, Step, SubRequest, TokenCounter, TokenInput};
use risunest_ir::{Budget, Limits, OwnedVec, Text, Value};
use risunest_memory::hanurai::{Retrieval, RetrievalOutput, ScoredDocument};
use risunest_memory::hypa_v2::{SummaryOutput, SummaryService};
use risunest_memory::supa::{SupaMemory, SupaMemoryConfig};
use std::borrow::Cow;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};

fn limited(cancel: Arc<AtomicBool>) -> Budget {
    Budget::new(Limits { max_owned_bytes: 16 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
        max_values: 100_000, max_depth: 128, max_work: 10_000_000 }, cancel).unwrap()
}
fn budget() -> Budget { limited(Arc::new(AtomicBool::new(false))) }
fn text(value: &str, budget: &Budget) -> Text { Text::from_str(value, budget).unwrap() }

/// Every dependency call and every resume it receives, in order.
type Log = Arc<Mutex<Vec<&'static str>>>;
fn leaf(kind: &str, budget: &Budget) -> Result<HostRequest, StageError> {
    Ok(match kind {
        "token" => HostRequest::PluginTokenizer { text: Text::from_str("probe", budget)? },
        "retrieval" => HostRequest::Embed(EmbedRequest { model: Text::from_str("probe", budget)?, texts: Vec::new() }),
        _ => HostRequest::Llm(SubRequest { mode: Text::from_str("memory", budget)?, messages: Vec::new(), options: Value::Undefined }),
    })
}
fn failed(reply: HostReply, name: &'static str) -> StageError {
    match reply {
        HostReply::Failed { message } => StageError::Host(message.units().collect()),
        HostReply::Cancelled => StageError::Cancelled,
        _ => StageError::Invalid(Cow::Borrowed(name)),
    }
}

/// Counts every message and text as three tokens. `fail_on` makes the n-th call (1-based) fail.
struct Tokens { log: Log, suspend: bool, calls: usize, fail_on: Option<(usize, StageError)> }
impl TokenCounter for Tokens {
    fn count(&mut self, input: TokenInput<'_>, budget: &Budget) -> Result<Step<u32>, StageError> {
        assert!(!matches!(input, TokenInput::Messages(_)), "SupaMemory counts one message or text at a time");
        self.log.lock().unwrap().push("token");
        self.calls += 1;
        if self.fail_on.as_ref().is_some_and(|(at, _)| *at == self.calls) { return Err(self.fail_on.take().unwrap().1); }
        if self.suspend { Ok(Step::NeedsHost(leaf("token", budget)?)) } else { Ok(Step::Done(3)) }
    }
    fn resume(&mut self, reply: HostReply, _: &Budget) -> Result<Step<u32>, StageError> {
        self.log.lock().unwrap().push("token-resume");
        match reply { HostReply::Tokens(ids) => Ok(Step::Done(ids.len() as u32)), other => Err(failed(other, "probe-token-reply")) }
    }
}
/// Summarizes everything as `S`; `first` replaces the first call's result.
struct Summaries { log: Log, suspend: bool, first: Option<Result<SummaryOutput, StageError>> }
impl SummaryService for Summaries {
    fn summarize(&mut self, _: Text, budget: &Budget) -> Result<Step<SummaryOutput>, StageError> {
        self.log.lock().unwrap().push("summary");
        if let Some(result) = self.first.take() { return result.map(Step::Done); }
        if self.suspend { Ok(Step::NeedsHost(leaf("summary", budget)?)) } else { Ok(Step::Done(SummaryOutput::Success(text("S", budget)))) }
    }
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<SummaryOutput>, StageError> {
        self.log.lock().unwrap().push("summary-resume");
        match reply { HostReply::Value(_) => Ok(Step::Done(SummaryOutput::Success(text("S", budget)))), other => Err(failed(other, "probe-summary-reply")) }
    }
}
/// Ranks the prepared documents in their prepared order.
struct Search { log: Log, suspend: bool, documents: Vec<Text>, preparing: bool }
impl Search {
    fn scored(&self, budget: &Budget) -> RetrievalOutput {
        RetrievalOutput::Scored(self.documents.iter().map(|document| ScoredDocument { document: Some(document.copy(budget).unwrap()), score: 1.0 }).collect())
    }
}
impl Retrieval for Search {
    fn prepare(&mut self, documents: Vec<Text>, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        self.log.lock().unwrap().push("prepare");
        self.documents = documents;
        self.preparing = true;
        if self.suspend { Ok(Step::NeedsHost(leaf("retrieval", budget)?)) } else { Ok(Step::Done(RetrievalOutput::Prepared)) }
    }
    fn query(&mut self, _: Text, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        self.log.lock().unwrap().push("query");
        self.preparing = false;
        if self.suspend { Ok(Step::NeedsHost(leaf("retrieval", budget)?)) } else { Ok(Step::Done(self.scored(budget))) }
    }
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        self.log.lock().unwrap().push("retrieval-resume");
        match reply {
            HostReply::Embeddings(_) if self.preparing => Ok(Step::Done(RetrievalOutput::Prepared)),
            HostReply::Embeddings(_) => Ok(Step::Done(self.scored(budget))),
            other => Err(failed(other, "probe-retrieval-reply")),
        }
    }
}

fn config(budget: &Budget) -> SupaMemoryConfig {
    SupaMemoryConfig { as_hyper: false, remove_punctuation: false, max_chunk_size: 1200.0, char_name: text("Kai", budget), user_name: text("User", budget) }
}
fn hyper(budget: &Budget) -> SupaMemoryConfig { SupaMemoryConfig { as_hyper: true, ..config(budget) } }
type Stage = SupaMemory<Tokens, Search, Summaries>;
fn failing(config: SupaMemoryConfig, stored: Option<Value>, suspend: bool, log: &Log, tokens: Option<(usize, StageError)>,
    summary: Option<Result<SummaryOutput, StageError>>) -> Stage {
    SupaMemory::new(config, stored, Tokens { log: log.clone(), suspend, calls: 0, fail_on: tokens },
        Search { log: log.clone(), suspend, documents: Vec::new(), preparing: false }, Summaries { log: log.clone(), suspend, first: summary })
}
fn build(config: SupaMemoryConfig, stored: Option<Value>, suspend: bool, log: &Log) -> Stage { failing(config, stored, suspend, log, None, None) }
fn message(role: Role, memo: &str, budget: &Budget) -> PromptMessage {
    let mut message = PromptMessage::new(role, text(memo, budget));
    message.memo = Some(text(memo, budget));
    message
}
fn input(memos: &[&str], current_tokens: f64, max_context_tokens: f64, budget: &Budget) -> MemoryInput {
    MemoryInput { chats: memos.iter().map(|memo| message(Role::User, memo, budget)).collect(), current_tokens, max_context_tokens }
}
const MEMOS: [&str; 8] = ["p0", "NewChat", "a", "b", "c", "d", "e", "f"];
/// A prefix before `NewChat`, a stored ID after it, and one chunk whose summary merges the four stored entries: every
/// phase except running out of messages runs.
fn history(budget: &Budget) -> MemoryInput { input(&MEMOS, 29.0, 30.0, budget) }
fn stored(budget: &Budget) -> Option<Value> { Some(Value::Text(text("a\nE1\n\nE2\n\nE3\n\nE4", budget))) }
const SEQUENCE: [&str; 12] = ["token", "token", "token", "token", "token", "token", "token", "summary", "token", "summary", "token", "token"];
/// The same messages under legacy Hypa: the first record entry matches no message, the second is selected after its
/// probe, its chunks are retrieved, and one chunk is summarized.
fn legacy(budget: &Budget) -> MemoryInput { input(&MEMOS, 21.0, 30.0, budget) }
fn record(budget: &Budget) -> Option<Value> {
    Some(Value::Text(text(r#"hypa:
[{"id": "zz", "supa": "s", "hypa": []}, {"id": "b", "supa": "Old.", "hypa": ["Alpha", "x", "Beta"]}]"#, budget)))
}
const LEGACY: [&str; 19] = ["token", "token", "token", "token", "token", "token", "token", "token", "token", "token", "prepare", "query",
    "token", "token", "token", "token", "token", "summary", "token"];
const LEGACY_RECORD: &str = r#"hypa:
[
  {
    "id": "e",
    "hypa": [
      "Alpha",
      "Beta",
      "S"
    ],
    "supa": "Old.\n\nS"
  },
  {
    "id": "zz",
    "supa": "s",
    "hypa": []
  },
  {
    "id": "b",
    "supa": "Old.",
    "hypa": [
      "Alpha",
      "x",
      "Beta"
    ]
  }
]"#;
fn reply(request: &HostRequest) -> HostReply {
    match request {
        HostRequest::PluginTokenizer { .. } => HostReply::Tokens(vec![0.0, 0.0, 0.0]),
        HostRequest::Llm(_) => HostReply::Value(Value::Undefined),
        HostRequest::Embed(_) => HostReply::Embeddings(Vec::new()),
        _ => panic!("unexpected host request"),
    }
}
fn kind(request: &HostRequest) -> &'static str {
    match request { HostRequest::PluginTokenizer { .. } => "token", HostRequest::Llm(_) => "summary", _ => "retrieval" }
}
fn resumed(sequence: &[&'static str]) -> Vec<&'static str> {
    sequence.iter().flat_map(|call| [*call, match *call { "token" => "token-resume", "summary" => "summary-resume", _ => "retrieval-resume" }]).collect()
}
fn complete(stage: &mut Stage, mut step: Result<Step<MemoryOutput>, StageError>, budget: &Budget) -> MemoryOutput {
    for _ in 0..64 {
        match step.unwrap() { Step::Done(output) => return output, Step::NeedsHost(request) => step = stage.resume(reply(&request), budget) }
    }
    panic!("the stage kept requesting host work");
}
fn advance(stage: &mut Stage, input: MemoryInput, position: usize, budget: &Budget) -> HostRequest {
    let mut step = stage.run(input, budget).unwrap();
    for _ in 0..position { let Step::NeedsHost(request) = step else { panic!() }; step = stage.resume(reply(&request), budget).unwrap(); }
    let Step::NeedsHost(request) = step else { panic!("no pending call at {position}") };
    request
}
fn fields(output: &MemoryOutput) -> Vec<(String, String)> {
    output.effects.iter().map(|effect| match effect {
        Effect::ChatField { field, value: Value::Text(value) } => (field.as_unicode().unwrap().to_owned(), value.as_unicode().unwrap().to_owned()),
        other => panic!("effect {other:?}"),
    }).collect()
}
fn memos(output: &MemoryOutput) -> Vec<&str> { output.chats.iter().map(|chat| chat.memo.as_ref().and_then(Text::as_unicode).unwrap()).collect() }

#[test]
fn stage_is_single_use_and_routes_each_reply_to_the_pending_dependency() {
    let b = budget();
    let log: Log = Arc::default();
    let mut idle = build(config(&b), stored(&b), true, &log);
    assert!(matches!(idle.resume(HostReply::Tokens(Vec::new()), &b), Err(StageError::Invalid(name)) if name == "supa-memory-not-suspended"));
    for suspend in [false, true] {
        let log: Log = Arc::default();
        let mut once = build(config(&b), stored(&b), suspend, &log);
        let step = once.run(history(&b), &b);
        let output = complete(&mut once, step, &b);
        assert!(output.error.is_none());
        assert_eq!(fields(&output), [("supaMemoryData".into(), "d\nS".into()), ("lastMemory".into(), "d".into())]);
        assert_eq!((memos(&output), output.current_tokens), (vec!["supaMemory", "d", "e", "f"], 30.0));
        assert_eq!(*log.lock().unwrap(), if suspend { resumed(&SEQUENCE) } else { SEQUENCE.to_vec() });
        assert!(matches!(once.run(history(&b), &b), Err(StageError::Invalid(name)) if name == "supa-memory-already-started"));
        assert!(matches!(once.resume(HostReply::Tokens(Vec::new()), &b), Err(StageError::Invalid(name)) if name == "supa-memory-not-suspended"));
    }
    // The reply goes to the dependency that asked; the stage neither inspects nor reroutes it.
    for (position, wrong) in [(0, HostReply::Value(Value::Undefined)), (7, HostReply::Tokens(Vec::new())), (9, HostReply::Embeddings(Vec::new()))] {
        let log: Log = Arc::default();
        let mut routed = build(config(&b), stored(&b), true, &log);
        let request = advance(&mut routed, history(&b), position, &b);
        let expected = match kind(&request) { "token" => "probe-token-reply", _ => "probe-summary-reply" };
        assert!(matches!(routed.resume(wrong, &b), Err(StageError::Invalid(name)) if name == expected));
        assert!(matches!(routed.resume(reply(&request), &b), Err(StageError::Invalid(name)) if name == "supa-memory-not-suspended"));
    }
}

#[test]
fn legacy_hypa_probes_every_entry_retrieves_and_writes_its_record_once() {
    let b = budget();
    for suspend in [false, true] {
        let log: Log = Arc::default();
        let mut once = build(hyper(&b), record(&b), suspend, &log);
        let step = once.run(legacy(&b), &b);
        let output = complete(&mut once, step, &b);
        assert!(output.error.is_none());
        assert_eq!(fields(&output), [("supaMemoryData".into(), LEGACY_RECORD.into()), ("lastMemory".into(), "e".into())]);
        assert_eq!((memos(&output), output.current_tokens), (vec!["hypaMemory", "supaMemory", "e", "f"], 29.0));
        assert_eq!(output.chats[0].content.as_unicode(), Some("past events: Alpha,Beta"));
        assert_eq!(*log.lock().unwrap(), if suspend { resumed(&LEGACY) } else { LEGACY.to_vec() });
    }
    let log: Log = Arc::default();
    let mut routed = build(hyper(&b), record(&b), true, &log);
    let request = advance(&mut routed, legacy(&b), 10, &b);
    assert!(matches!(routed.resume(HostReply::Tokens(Vec::new()), &b), Err(StageError::Invalid(name)) if name == "probe-retrieval-reply"));
    assert!(matches!(routed.resume(reply(&request), &b), Err(StageError::Invalid(name)) if name == "supa-memory-not-suspended"));
}

#[test]
fn cancellation_while_pending_reaches_no_dependency_and_publishes_nothing() {
    let plain = [(0, "token"), (1, "token"), (2, "token"), (3, "token"), (7, "summary"), (8, "token"), (9, "summary"), (11, "token")];
    let legacy_positions = [(1, "token"), (9, "token"), (10, "retrieval"), (11, "retrieval"), (12, "token")];
    for (as_hyper, position, pending) in plain.iter().map(|(at, kind)| (false, *at, *kind)).chain(legacy_positions.iter().map(|(at, kind)| (true, *at, *kind))) {
        let cancel = Arc::new(AtomicBool::new(false));
        let b = limited(cancel.clone());
        let log: Log = Arc::default();
        let (config, stored, input) = if as_hyper { (hyper(&b), record(&b), legacy(&b)) } else { (config(&b), stored(&b), history(&b)) };
        let mut cancelled = build(config, stored, true, &log);
        let request = advance(&mut cancelled, input, position, &b);
        assert_eq!(kind(&request), pending);
        let calls = log.lock().unwrap().len();
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(cancelled.resume(reply(&request), &b), Err(StageError::Cancelled)));
        assert_eq!(log.lock().unwrap().len(), calls, "the pending {pending} dependency never receives the reply");
        cancel.store(false, Ordering::Relaxed);
        assert!(matches!(cancelled.resume(reply(&request), &b), Err(StageError::Invalid(name)) if name == "supa-memory-not-suspended"));
    }
}

#[test]
fn returned_errors_publish_nothing_and_dependency_failures_end_the_run() {
    let b = budget();
    let boom = || StageError::Host(text("boom", &budget()).units().collect());
    // A `fail` reply is returned as the error with the messages and tokens at that point, and no stored change.
    let log: Log = Arc::default();
    let mut caught = failing(config(&b), stored(&b), false, &log, None, Some(Ok(SummaryOutput::Failed(text("SupaMemory: HTTP: quota", &b)))));
    let Ok(Step::Done(output)) = caught.run(history(&b), &b) else { panic!("returned error") };
    assert_eq!(output.error.as_ref().and_then(Text::as_unicode), Some("SupaMemory: HTTP: quota"));
    assert!(output.effects.is_empty());
    assert_eq!((memos(&output), output.current_tokens), (vec!["d", "e", "f"], 27.0));
    assert_eq!(log.lock().unwrap().len(), 8, "the run ends at the failed summary");
    // A thrown request or tokenizer failure propagates unchanged.
    for (tokens, summary, calls) in [(None, Some(Err(boom())), 8), (Some((3, boom())), None, 3)] {
        let log: Log = Arc::default();
        let mut thrown = failing(config(&b), stored(&b), false, &log, tokens, summary);
        assert!(matches!(thrown.run(history(&b), &b), Err(StageError::Host(message)) if String::from_utf16(&message).unwrap() == "boom"));
        assert_eq!(log.lock().unwrap().len(), calls);
    }
    let log: Log = Arc::default();
    let mut limited_summary = failing(config(&b), stored(&b), false, &log, None, Some(Err(StageError::Limit(Cow::Borrowed("probe")))));
    assert!(matches!(limited_summary.run(history(&b), &b), Err(StageError::Limit(name)) if name == "probe"));
    // A missing stored ID is returned after every message was counted away.
    let log: Log = Arc::default();
    let mut missing = build(config(&b), Some(Value::Text(text("zz\nData", &b))), false, &log);
    let Ok(Step::Done(output)) = missing.run(input(&["a", "b"], 30.0, 30.0, &b), &b) else { panic!("returned error") };
    assert_eq!(output.error.as_ref().and_then(Text::as_unicode), Some("SupaMemory: chat ID not found"));
    assert_eq!((output.chats.len(), output.current_tokens, output.effects.len()), (0, 34.0, 0));
    // An oversize first system message adds nothing to a batch, where TS retries it forever.
    let log: Log = Arc::default();
    let mut stuck = build(config(&b), None, false, &log);
    let mut system = input(&["s"], 0.0, 6.0, &b);
    system.chats[0].role = Role::System;
    assert!(matches!(stuck.run(system, &b), Err(StageError::Invalid(name)) if name == "supa-memory-no-progress"));
    assert_eq!(*log.lock().unwrap(), ["token"]);
}

#[test]
fn declines_and_early_returns_come_before_any_dependency_call() {
    let b = budget();
    let run = |config: SupaMemoryConfig, stored: Option<Value>, input: MemoryInput| {
        let log: Log = Arc::default();
        let mut stage = build(config, stored, true, &log);
        let result = stage.run(input, &b);
        assert!(log.lock().unwrap().is_empty(), "no dependency call before the result");
        result
    };
    // At or under the limit after the reserved 10 tokens, nothing is read or declined.
    let Ok(Step::Done(output)) = run(hyper(&b), Some(Value::Number(1.0)), input(&["a"], 20.0, 30.0, &b)) else { panic!() };
    assert_eq!((output.chats.len(), output.current_tokens, output.effects.len(), output.error.is_none()), (1, 30.0, 0, true));
    let declined = |result: Result<Step<MemoryOutput>, StageError>, field: &str| {
        assert!(matches!(&result, Err(StageError::Unsupported(name)) if name == field), "{field}: {:?}", result.as_ref().err());
    };
    let mut list = OwnedVec::new(&b).unwrap();
    for _ in 0..5 { list.push(Value::Text(text("x", &b))).unwrap(); }
    for value in [Value::Number(1.0), Value::Bool(true), Value::Array(list), Value::Object(OwnedVec::new(&b).unwrap())] {
        declined(run(config(&b), Some(value), history(&b)), "supaMemoryData");
    }
    // A legacy Hypa record TS cannot read is declined before the NewChat messages are counted. Entries after the selected
    // one are never read.
    let legacy_record = |json: &str| Some(Value::Text(text(&format!("hypa:\n{json}"), &b)));
    for (json, field) in [("[{", "supaMemoryData.json"), ("[null]", "supaMemoryData.entry"), (r#"[{"id": "zz"}, 1]"#, "supaMemoryData.entry"),
        (r#"[[]]"#, "supaMemoryData.entry"), (r#"[{"id": "b", "hypa": []}]"#, "supaMemoryData.supa"),
        (r#"[{"id": "b", "supa": "s"}]"#, "supaMemoryData.hypa"), (r#"[{"id": "b", "supa": "s", "hypa": ["ok", 1]}]"#, "supaMemoryData.hypa")] {
        declined(run(hyper(&b), legacy_record(json), legacy(&b)), field);
    }
    for json in [r#"[{"id": "b", "supa": "s", "hypa": []}, null]"#, r#" [{"id": 1}, {"id": "b", "supa": "s", "hypa": []}, "x"] "#] {
        let log: Log = Arc::default();
        let mut stage = build(hyper(&b), legacy_record(json), false, &log);
        assert!(matches!(stage.run(legacy(&b), &b), Ok(Step::Done(output)) if output.error.is_none()), "{json}");
    }
    // A record saved by legacy Hypa is reported before any message is counted.
    let Ok(Step::Done(output)) = run(config(&b), Some(Value::Text(text("hypa:\n[]", &b))), input(&["a", "b"], 30.0, 30.0, &b)) else { panic!() };
    assert_eq!(output.error.as_ref().and_then(Text::as_unicode), Some("SupaMemory: Data saved in hypaMemory, loaded as SupaMemory."));
    assert_eq!((output.chats.len(), output.current_tokens, output.effects.len()), (2, 40.0, 0));
    // A falsy or short record is no record.
    for value in [Value::Null, Value::Bool(false), Value::Number(0.0), Value::Text(text("", &b)), Value::Text(text("a\nxy", &b))] {
        let log: Log = Arc::default();
        let mut stage = build(config(&b), Some(value), false, &log);
        let Ok(Step::Done(output)) = stage.run(input(&["a", "b", "c", "d", "e"], 25.0, 30.0, &b), &b) else { panic!() };
        assert_eq!((output.error.is_none(), fields(&output)[0].1.as_str()), (true, "d\nS"));
    }
}
