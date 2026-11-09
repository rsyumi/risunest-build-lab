use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::rng::Tape;
use risunest_ir::stage::{EmbedRequest, HostReply, HostRequest, MemoryInput, MemoryOutput, MemoryStage, StageError, Step, SubRequest, TokenCounter, TokenInput};
use risunest_ir::{Budget, Limits, OwnedVec, Text, Value};
use risunest_memory::hanurai::{Retrieval, RetrievalOutput, ScoredDocument};
use risunest_memory::hypa_v2::{SummaryOutput, SummaryService};
use risunest_memory::hypa_v3::{HypaV3, HypaV3Config, HypaV3PreparedHistory};
use std::borrow::Cow;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};

fn limited(cancel: Arc<AtomicBool>) -> Budget {
    Budget::new(Limits { max_owned_bytes: 16 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
        max_values: 100_000, max_depth: 128, max_work: 10_000_000 }, cancel).unwrap()
}
fn budget() -> Budget { limited(Arc::new(AtomicBool::new(false))) }
fn text(value: &str, budget: &Budget) -> Text { Text::from_str(value, budget).unwrap() }
fn object(properties: Vec<(&str, Value)>, budget: &Budget) -> Value {
    let mut value = Value::Object(OwnedVec::new(budget).unwrap());
    for (key, property) in properties { value.define(text(key, budget), property, budget).unwrap(); }
    value
}
fn array(values: Vec<Value>, budget: &Budget) -> Value {
    let mut array = OwnedVec::new(budget).unwrap();
    for value in values { array.push(value).unwrap(); }
    Value::Array(array)
}
fn summary(memos: Vec<Value>, budget: &Budget) -> Value {
    object(vec![("text", Value::Text(text("Stored.", budget))), ("chatMemos", array(memos, budget))], budget)
}
fn stored(summaries: Vec<Value>, budget: &Budget) -> Value { object(vec![("summaries", array(summaries, budget))], budget) }

/// Every dependency call and every resume it receives, in order.
type Log = Arc<Mutex<Vec<&'static str>>>;
fn leaf(kind: &str, budget: &Budget) -> Result<HostRequest, StageError> {
    Ok(match kind {
        "token" => HostRequest::PluginTokenizer { text: Text::from_str("probe", budget)? },
        "summary" => HostRequest::Llm(SubRequest { mode: Text::from_str("memory", budget)?, messages: Vec::new(), options: Value::Undefined }),
        _ => HostRequest::Embed(EmbedRequest { model: Text::from_str("probe", budget)?, texts: Vec::new() }),
    })
}
fn failed(reply: HostReply, name: &'static str) -> StageError {
    match reply {
        HostReply::Failed { message } => StageError::Host(message.units().collect()),
        HostReply::Cancelled => StageError::Cancelled,
        _ => StageError::Invalid(Cow::Borrowed(name)),
    }
}

/// Counts every message as three tokens. `fail_on` makes the n-th call (1-based) fail.
struct Tokens { log: Log, suspend: bool, calls: usize, fail_on: Option<(usize, StageError)> }
impl TokenCounter for Tokens {
    fn count(&mut self, input: TokenInput<'_>, budget: &Budget) -> Result<Step<u32>, StageError> {
        let TokenInput::Message(_) = input else { panic!("HypaV3 counts complete messages") };
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
struct Summaries { log: Log, suspend: bool, fail: Option<StageError> }
impl SummaryService for Summaries {
    fn summarize(&mut self, _: Text, budget: &Budget) -> Result<Step<SummaryOutput>, StageError> {
        self.log.lock().unwrap().push("summary");
        if let Some(error) = self.fail.take() { return Err(error); }
        if self.suspend { Ok(Step::NeedsHost(leaf("summary", budget)?)) } else { Ok(Step::Done(SummaryOutput::Success(text("S", budget)))) }
    }
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<SummaryOutput>, StageError> {
        self.log.lock().unwrap().push("summary-resume");
        match reply { HostReply::Value(_) => Ok(Step::Done(SummaryOutput::Success(text("S", budget)))), other => Err(failed(other, "probe-summary-reply")) }
    }
}
/// Scores every prepared document 1.
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

fn config(budget: &Budget) -> HypaV3Config {
    HypaV3Config { max_response: 0.0, memory_tokens_ratio: 0.5, extra_summarization_ratio: 0.0, max_chats_per_summary: 2.0,
        recent_memory_ratio: 0.3, similar_memory_ratio: 0.3, enable_similarity_correction: true, preserve_orphaned_memory: false,
        do_not_summarize_user_message: false, summary_chunk_separator: text("\\n\\n", budget), query_chat_count: 2.0,
        use_experimental_impl: false, contextual_embedding: false, prepared: None }
}
type Stage = HypaV3<Tokens, Search, Summaries, Tape>;
fn failing(config: HypaV3Config, stored: Option<Value>, suspend: bool, log: &Log, tokens: Option<(usize, StageError)>, summary: Option<StageError>) -> Stage {
    HypaV3::new(config, stored, Tokens { log: log.clone(), suspend, calls: 0, fail_on: tokens },
        Search { log: log.clone(), suspend, documents: Vec::new(), preparing: false }, Summaries { log: log.clone(), suspend, fail: summary },
        Tape::new(vec![0.25]))
}
fn build(config: HypaV3Config, stored: Option<Value>, suspend: bool, log: &Log) -> Stage { failing(config, stored, suspend, log, None, None) }
/// Two summarized batches, then a recent, similar (with a correction summary) and random selection: every
/// dependency kind is called and every phase runs.
fn history(budget: &Budget) -> MemoryInput {
    let chats = ["a", "b", "c", "d", "e", "f"].iter().map(|content| {
        let mut message = PromptMessage::new(Role::User, text(content, budget));
        message.memo = Some(text(content, budget));
        message
    }).collect();
    MemoryInput { chats, current_tokens: 20.0, max_context_tokens: 20.0 }
}
const SEQUENCE: [&str; 17] = ["token", "token", "token", "summary", "token", "token", "summary", "token", "prepare", "summary",
    "query", "query", "query", "token", "token", "token", "token"];
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
fn complete(stage: &mut Stage, mut step: Result<Step<MemoryOutput>, StageError>, budget: &Budget) -> MemoryOutput {
    for _ in 0..64 {
        match step.unwrap() { Step::Done(output) => return output, Step::NeedsHost(request) => step = stage.resume(reply(&request), budget) }
    }
    panic!("the stage kept requesting host work");
}
fn advance(stage: &mut Stage, position: usize, budget: &Budget) -> HostRequest {
    let mut step = stage.run(history(budget), budget).unwrap();
    for _ in 0..position { let Step::NeedsHost(request) = step else { panic!() }; step = stage.resume(reply(&request), budget).unwrap(); }
    let Step::NeedsHost(request) = step else { panic!("no pending call at {position}") };
    request
}

#[test]
fn stage_is_single_use_and_routes_each_reply_to_the_pending_dependency() {
    let b = budget();
    let log: Log = Arc::default();
    let mut idle = build(config(&b), None, true, &log);
    assert!(matches!(idle.resume(HostReply::Tokens(Vec::new()), &b), Err(StageError::Invalid(name)) if name == "hypa-v3-not-suspended"));
    for suspend in [false, true] {
        let log: Log = Arc::default();
        let mut once = build(config(&b), None, suspend, &log);
        let step = once.run(history(&b), &b);
        let output = complete(&mut once, step, &b);
        assert!(output.error.is_none());
        assert_eq!(output.effects.len(), 1);
        assert_eq!(*log.lock().unwrap(), if suspend { SEQUENCE.iter().flat_map(|call| [*call, match *call {
            "token" => "token-resume", "summary" => "summary-resume", _ => "retrieval-resume" }]).collect() } else { SEQUENCE.to_vec() });
        assert!(matches!(once.run(history(&b), &b), Err(StageError::Invalid(name)) if name == "hypa-v3-already-started"));
        assert!(matches!(once.resume(HostReply::Tokens(Vec::new()), &b), Err(StageError::Invalid(name)) if name == "hypa-v3-not-suspended"));
    }
    // The reply goes to the dependency that asked; the stage neither inspects nor reroutes it.
    for (position, wrong) in [(0, HostReply::Embeddings(Vec::new())), (3, HostReply::Tokens(Vec::new())), (8, HostReply::Value(Value::Undefined))] {
        let log: Log = Arc::default();
        let mut routed = build(config(&b), None, true, &log);
        let request = advance(&mut routed, position, &b);
        let expected = match kind(&request) { "token" => "probe-token-reply", "summary" => "probe-summary-reply", _ => "probe-retrieval-reply" };
        assert!(matches!(routed.resume(wrong, &b), Err(StageError::Invalid(name)) if name == expected));
        assert!(matches!(routed.resume(reply(&request), &b), Err(StageError::Invalid(name)) if name == "hypa-v3-not-suspended"));
    }
}

#[test]
fn cancellation_while_pending_reaches_no_dependency_and_publishes_nothing() {
    for (position, pending) in [(0, "token"), (3, "summary"), (8, "retrieval"), (9, "summary"), (10, "retrieval"), (16, "token")] {
        let cancel = Arc::new(AtomicBool::new(false));
        let b = limited(cancel.clone());
        let log: Log = Arc::default();
        let mut cancelled = build(config(&b), None, true, &log);
        let request = advance(&mut cancelled, position, &b);
        assert_eq!(kind(&request), pending);
        let calls = log.lock().unwrap().len();
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(cancelled.resume(reply(&request), &b), Err(StageError::Cancelled)));
        assert_eq!(log.lock().unwrap().len(), calls, "the pending {pending} dependency never receives the reply");
        cancel.store(false, Ordering::Relaxed);
        assert!(matches!(cancelled.resume(reply(&request), &b), Err(StageError::Invalid(name)) if name == "hypa-v3-not-suspended"));
    }
}

#[test]
fn caught_failures_return_memory_once_and_other_failures_end_the_run() {
    let b = budget();
    let boom = || StageError::Host(text("boom", &budget()).units().collect());
    // A host failure in the summary `try` returns the working memory with the error.
    let log: Log = Arc::default();
    let mut caught = failing(config(&b), Some(stored(vec![summary(vec![Value::Text(text("a", &b))], &b)], &b)), false, &log, None, Some(boom()));
    let Ok(Step::Done(output)) = caught.run(history(&b), &b) else { panic!("returned error") };
    assert_eq!(output.error.as_ref().and_then(Text::as_unicode), Some("[HypaV3] Summarization failed: Error: boom"));
    assert_eq!(output.effects.len(), 1, "the memory is published once");
    assert!(matches!(caught.resume(HostReply::Value(Value::Undefined), &b), Err(StageError::Invalid(name)) if name == "hypa-v3-not-suspended"));
    // Other summary failures are terminal: no output, no stored change.
    let log: Log = Arc::default();
    let mut limited_summary = failing(config(&b), None, false, &log, None, Some(StageError::Limit(Cow::Borrowed("probe"))));
    assert!(matches!(limited_summary.run(history(&b), &b), Err(StageError::Limit(name)) if name == "probe"));
    // A tokenizer host failure outside the similarity `try` propagates as TS throws it; inside, it returns memory.
    let log: Log = Arc::default();
    let mut thrown = failing(config(&b), None, false, &log, Some((1, boom())), None);
    assert!(matches!(thrown.run(history(&b), &b), Err(StageError::Host(message)) if String::from_utf16(&message).unwrap() == "[HypaV3] boom"));
    let log: Log = Arc::default();
    // The seventh count is the first similar candidate.
    let mut similar = failing(config(&b), None, false, &log, Some((7, boom())), None);
    let Ok(Step::Done(output)) = similar.run(history(&b), &b) else { panic!("returned error") };
    assert_eq!(output.error.as_ref().and_then(Text::as_unicode), Some("[HypaV3] Similarity search failed: Error: boom"));
    assert_eq!(output.effects.len(), 1);
    assert_eq!(log.lock().unwrap().len(), 14, "the run ends at the failed similar candidate count");
}

#[test]
fn declines_and_the_ratio_error_come_before_any_dependency_call() {
    let b = budget();
    let run = |config: HypaV3Config, stored: Option<Value>| {
        let log: Log = Arc::default();
        let mut stage = build(config, stored, true, &log);
        let result = stage.run(history(&b), &b);
        assert!(log.lock().unwrap().is_empty(), "no dependency call before the result");
        result
    };
    let declined = |result: Result<Step<MemoryOutput>, StageError>, field: &str| {
        assert!(matches!(&result, Err(StageError::Unsupported(name)) if name == field), "{field}: {:?}", result.as_ref().err());
    };
    let with = |change: &dyn Fn(&mut HypaV3Config)| -> HypaV3Config { let mut config = config(&b); change(&mut config); config };
    // The experimental implementation is selected before the ratio check, which precedes every other check.
    declined(run(with(&|c: &mut HypaV3Config| { c.use_experimental_impl = true; c.recent_memory_ratio = 0.9 }), None), "hypa-v3-experimental");
    let Ok(Step::Done(output)) = run(with(&|c: &mut HypaV3Config| { c.recent_memory_ratio = 0.7; c.similar_memory_ratio = 0.4; c.max_chats_per_summary = 0.0 }),
        Some(Value::Number(1.0))) else { panic!("ratio error") };
    assert_eq!(output.error.as_ref().and_then(Text::as_unicode),
        Some("[HypaV3] The sum of Recent Memory Ratio and Similar Memory Ratio is greater than 1."));
    assert!(output.effects.is_empty());
    assert_eq!((output.chats.len(), output.current_tokens), (6, 20.0));
    for value in [0.0, -1.0, 1.5, f64::NAN, f64::INFINITY] {
        declined(run(with(&|c: &mut HypaV3Config| c.max_chats_per_summary = value), None), "hypa-v3-max-chats-per-summary");
    }
    for value in [-1.0, 0.5, f64::NAN] { declined(run(with(&|c: &mut HypaV3Config| c.query_chat_count = value), None), "hypa-v3-query-chat-count"); }
    declined(run(with(&|c: &mut HypaV3Config| c.contextual_embedding = true), None), "hypa-v3-contextual-embedding");
    for separator in ["\\d", "/x/i", "/x/u", "[a]", "a|b", "x+", "\\01", "\\x41"] {
        declined(run(with(&|c: &mut HypaV3Config| c.summary_chunk_separator = text(separator, &b)), None), "hypa-v3-summary-chunk-separator");
    }
    // Both similarity checks apply only when similar memory is selected.
    let contextual = with(&|c: &mut HypaV3Config| { c.contextual_embedding = true; c.similar_memory_ratio = 0.0 });
    let pattern = with(&|c: &mut HypaV3Config| { c.summary_chunk_separator = text("\\d", &b); c.similar_memory_ratio = 0.0 });
    for config in [contextual, pattern] {
        let log: Log = Arc::default();
        let mut stage = build(config, None, false, &log);
        assert!(matches!(stage.run(history(&b), &b), Ok(Step::Done(output)) if output.error.is_none()));
    }
    let entry = |properties: Vec<(&str, Value)>| object(properties, &b);
    let stored_cases = [
        (Value::Text(text("x", &b)), "hypaV3Data"),
        (array(Vec::new(), &b), "hypaV3Data"),
        (entry(vec![("summary", array(Vec::new(), &b))]), "hypaV3Data.summaries"),
        (entry(vec![("summaries", entry(vec![]))]), "hypaV3Data.summaries"),
        (stored(vec![Value::Null], &b), "hypaV3Data.summaries"),
        (stored(vec![array(Vec::new(), &b)], &b), "hypaV3Data.summaries"),
        (stored(vec![entry(vec![("text", Value::Text(text("t", &b)))])], &b), "hypaV3Data.summaries.chatMemos"),
        (stored(vec![entry(vec![("text", Value::Text(text("t", &b))), ("chatMemos", Value::Text(text("a", &b)))])], &b), "hypaV3Data.summaries.chatMemos"),
        (stored(vec![entry(vec![("text", Value::Number(1.0)), ("chatMemos", array(Vec::new(), &b))])], &b), "hypaV3Data.summaries.text"),
        (stored(vec![entry(vec![("chatMemos", array(Vec::new(), &b))])], &b), "hypaV3Data.summaries.text"),
    ];
    for (value, field) in stored_cases { declined(run(config(&b), Some(value)), field); }
    // A falsy record is no record.
    for value in [Value::Null, Value::Bool(false), Value::Number(0.0), Value::Text(text("", &b))] {
        let log: Log = Arc::default();
        let mut stage = build(config(&b), Some(value), false, &log);
        assert!(matches!(stage.run(history(&b), &b), Ok(Step::Done(output)) if output.error.is_none()));
    }
}

#[test]
fn a_stale_prepared_boundary_returns_its_error_before_any_dependency_call() {
    let b = budget();
    let prepared = |boundary: &str| HypaV3PreparedHistory::Boundary { boundary_memo: text(boundary, &b),
        effective_memos: Vec::new(), history_start_index: 2 };
    for (memos, boundary) in [(vec![Value::Text(text("b", &b))], "a"), (Vec::new(), "a"), (vec![Value::Number(1.0)], "1")] {
        let log: Log = Arc::default();
        let mut config = config(&b);
        config.prepared = Some(prepared(boundary));
        config.preserve_orphaned_memory = true;
        let mut stage = build(config, Some(stored(vec![summary(memos, &b)], &b)), true, &log);
        let Ok(Step::Done(output)) = stage.run(history(&b), &b) else { panic!("stale boundary") };
        assert_eq!(output.error.as_ref().and_then(Text::as_unicode), Some("[HypaV3] Prepared history summary boundary became stale"));
        assert!(output.effects.is_empty() && log.lock().unwrap().is_empty());
    }
}
