use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::stage::{Effect, EmbedRequest, HostReply, HostRequest, MemoryInput, MemoryOutput, MemoryStage, StageError, Step, SubRequest, TokenCounter, TokenInput};
use risunest_ir::{Budget, Limits, OwnedVec, Text, Value};
use risunest_memory::hanurai::{Retrieval, RetrievalOutput};
use risunest_memory::hypa_v2::{HypaV2, HypaV2Config, SummaryOutput, SummaryService};
use std::borrow::Cow;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};

fn limited(owned: usize, work: usize, cancel: Arc<AtomicBool>) -> Budget {
    Budget::new(Limits { max_owned_bytes: owned, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
        max_values: 100_000, max_depth: 128, max_work: work }, cancel).unwrap()
}
fn budget() -> Budget { limited(16 << 20, 10_000_000, Arc::new(AtomicBool::new(false))) }
fn text(value: &str, budget: &Budget) -> Text { Text::from_str(value, budget).unwrap() }
fn chat(content: &str, memo: &str, budget: &Budget) -> PromptMessage {
    let mut message = PromptMessage::new(Role::User, text(content, budget));
    message.memo = Some(text(memo, budget));
    message
}
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
fn config(allocated: f64, max_response: f64) -> HypaV2Config { HypaV2Config { max_response, allocated_tokens: allocated, chunk_size: 100.0 } }

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

struct Tokens { log: Log, suspend: bool, cancel_on: Option<(Arc<AtomicBool>, usize)>, calls: usize }
impl TokenCounter for Tokens {
    fn count(&mut self, input: TokenInput<'_>, budget: &Budget) -> Result<Step<u32>, StageError> {
        let TokenInput::Message(_) = input else { panic!("HypaV2 counts complete messages") };
        self.log.lock().unwrap().push("token");
        self.calls += 1;
        // A dependency can observe cancellation while it completes; the stage must not publish afterwards.
        if let Some((cancel, at)) = &self.cancel_on { if self.calls == *at { cancel.store(true, Ordering::Relaxed); } }
        if self.suspend { Ok(Step::NeedsHost(leaf("token", budget)?)) } else { Ok(Step::Done(3)) }
    }
    fn resume(&mut self, reply: HostReply, _: &Budget) -> Result<Step<u32>, StageError> {
        self.log.lock().unwrap().push("token-resume");
        match reply { HostReply::Tokens(ids) => Ok(Step::Done(ids.len() as u32)), other => Err(failed(other, "probe-token-reply")) }
    }
}
struct Summaries { log: Log, suspend: bool }
impl SummaryService for Summaries {
    fn summarize(&mut self, _: Text, budget: &Budget) -> Result<Step<SummaryOutput>, StageError> {
        self.log.lock().unwrap().push("summary");
        if self.suspend { Ok(Step::NeedsHost(leaf("summary", budget)?)) } else { Ok(Step::Done(SummaryOutput::Success(text("S", budget)))) }
    }
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<SummaryOutput>, StageError> {
        self.log.lock().unwrap().push("summary-resume");
        match reply { HostReply::Value(_) => Ok(Step::Done(SummaryOutput::Success(text("S", budget)))), other => Err(failed(other, "probe-summary-reply")) }
    }
}
struct Search { log: Log, suspend: bool, preparing: bool }
impl Retrieval for Search {
    fn prepare(&mut self, _: Vec<Text>, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        self.log.lock().unwrap().push("prepare");
        self.preparing = true;
        if self.suspend { Ok(Step::NeedsHost(leaf("retrieval", budget)?)) } else { Ok(Step::Done(RetrievalOutput::Prepared)) }
    }
    fn query(&mut self, _: Text, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        self.log.lock().unwrap().push("query");
        self.preparing = false;
        if self.suspend { Ok(Step::NeedsHost(leaf("retrieval", budget)?)) } else { Ok(Step::Done(RetrievalOutput::Scored(Vec::new()))) }
    }
    fn resume(&mut self, reply: HostReply, _: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        self.log.lock().unwrap().push("retrieval-resume");
        match reply {
            HostReply::Embeddings(_) if self.preparing => Ok(Step::Done(RetrievalOutput::Prepared)),
            HostReply::Embeddings(_) => Ok(Step::Done(RetrievalOutput::Scored(Vec::new()))),
            other => Err(failed(other, "probe-retrieval-reply")),
        }
    }
}
type Stage = HypaV2<Tokens, Search, Summaries>;
fn stage(stored: Option<Value>, suspend: bool, log: &Log) -> Stage {
    HypaV2::new(config(0.0, 0.0), stored, Tokens { log: log.clone(), suspend, cancel_on: None, calls: 0 },
        Search { log: log.clone(), suspend, preparing: false }, Summaries { log: log.clone(), suspend })
}
/// One summarized batch, then main, preparation, three queries and the wrapper: every dependency is called.
fn history(budget: &Budget) -> MemoryInput {
    MemoryInput { chats: ["first", "a", "b", "c", "d", "e"].iter().map(|content| chat(content, content, budget)).collect(),
        current_tokens: 7.0, max_context_tokens: 5.0 }
}
const SEQUENCE: [&str; 10] = ["token", "token", "summary", "token", "token", "prepare", "query", "query", "query", "token"];
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

#[test]
fn stage_is_single_use_and_routes_each_reply_to_the_pending_dependency() {
    let b = budget();
    let log: Log = Arc::default();
    let mut idle = stage(None, true, &log);
    assert!(matches!(idle.resume(HostReply::Tokens(Vec::new()), &b), Err(StageError::Invalid(name)) if name == "hypa-v2-not-suspended"));
    for suspend in [false, true] {
        let log: Log = Arc::default();
        let mut once = stage(None, suspend, &log);
        let step = once.run(history(&b), &b);
        let output = complete(&mut once, step, &b);
        assert_eq!(output.effects.len(), 1);
        assert_eq!(*log.lock().unwrap(), if suspend { SEQUENCE.iter().flat_map(|call| [*call, match *call {
            "token" => "token-resume", "summary" => "summary-resume", _ => "retrieval-resume" }]).collect() } else { SEQUENCE.to_vec() });
        assert!(matches!(once.run(history(&b), &b), Err(StageError::Invalid(name)) if name == "hypa-v2-already-started"));
        assert!(matches!(once.resume(HostReply::Tokens(Vec::new()), &b), Err(StageError::Invalid(name)) if name == "hypa-v2-not-suspended"));
    }
    // The reply goes to the dependency that asked; the stage neither inspects nor reroutes it.
    for (position, wrong) in [(0, HostReply::Embeddings(Vec::new())), (2, HostReply::Tokens(Vec::new())), (5, HostReply::Value(Value::Undefined))] {
        let log: Log = Arc::default();
        let mut routed = stage(None, true, &log);
        let mut step = routed.run(history(&b), &b).unwrap();
        for _ in 0..position { let Step::NeedsHost(request) = step else { panic!() }; step = routed.resume(reply(&request), &b).unwrap(); }
        let Step::NeedsHost(request) = step else { panic!() };
        let expected = match kind(&request) { "token" => "probe-token-reply", "summary" => "probe-summary-reply", _ => "probe-retrieval-reply" };
        assert!(matches!(routed.resume(wrong, &b), Err(StageError::Invalid(name)) if name == expected));
        assert!(matches!(routed.resume(reply(&request), &b), Err(StageError::Invalid(name)) if name == "hypa-v2-not-suspended"));
    }
    // A host failure is terminal at the dependency that observed it.
    let log: Log = Arc::default();
    let mut failing = stage(None, true, &log);
    assert!(matches!(failing.run(history(&b), &b), Ok(Step::NeedsHost(HostRequest::PluginTokenizer { .. }))));
    let failure = HostReply::Failed { message: Text::from_units(&[0xd800], &b).unwrap() };
    assert!(matches!(failing.resume(failure, &b), Err(StageError::Host(message)) if message == [0xd800]));
    assert_eq!(*log.lock().unwrap(), ["token", "token-resume"]);
}

#[test]
fn cancellation_while_pending_or_at_return_reaches_no_dependency_and_publishes_nothing() {
    for (position, pending) in [(0, "token"), (2, "summary"), (5, "retrieval"), (6, "retrieval"), (9, "token")] {
        let cancel = Arc::new(AtomicBool::new(false));
        let b = limited(16 << 20, 10_000_000, cancel.clone());
        let log: Log = Arc::default();
        let mut cancelled = stage(None, true, &log);
        let mut step = cancelled.run(history(&b), &b).unwrap();
        for _ in 0..position { let Step::NeedsHost(request) = step else { panic!() }; step = cancelled.resume(reply(&request), &b).unwrap(); }
        let Step::NeedsHost(request) = step else { panic!("pending {pending}") };
        assert_eq!(kind(&request), pending);
        let calls = log.lock().unwrap().len();
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(cancelled.resume(reply(&request), &b), Err(StageError::Cancelled)));
        assert_eq!(log.lock().unwrap().len(), calls, "the pending {pending} dependency never receives the reply");
        cancel.store(false, Ordering::Relaxed);
        assert!(matches!(cancelled.resume(reply(&request), &b), Err(StageError::Invalid(name)) if name == "hypa-v2-not-suspended"));
    }
    // Cancellation observed by the wrapper count, the last dependency call, still publishes no stored memory.
    let cancel = Arc::new(AtomicBool::new(false));
    let b = limited(16 << 20, 10_000_000, cancel.clone());
    let log: Log = Arc::default();
    let tokens = Tokens { log: log.clone(), suspend: false, cancel_on: Some((cancel.clone(), 5)), calls: 0 };
    let mut stage = HypaV2::new(config(0.0, 0.0), None, tokens, Search { log: log.clone(), suspend: false, preparing: false },
        Summaries { log: log.clone(), suspend: false });
    assert!(matches!(stage.run(history(&b), &b), Err(StageError::Cancelled)));
    assert_eq!(*log.lock().unwrap(), SEQUENCE);
}

#[test]
fn unsupported_stored_records_decline_before_any_dependency_call() {
    let b = budget();
    let main = |id: Value, b: &Budget| object(vec![("id", id), ("text", Value::Text(text("m", b))), ("chatMemos", array(Vec::new(), b))], b);
    let cases = [
        (Value::Number(1.0), "hypaV2Data"),
        (object(vec![("mainChunks", array(Vec::new(), &b))], &b), "hypaV2Data"),
        (object(vec![("mainChunks", Value::Null), ("chunks", array(Vec::new(), &b))], &b), "hypaV2Data"),
        (object(vec![("mainChunks", array(vec![main(Value::Text(text("1", &b)), &b)], &b)), ("chunks", array(Vec::new(), &b))], &b), "hypaV2Data.mainChunks"),
        (object(vec![("mainChunks", array(vec![Value::Null], &b)), ("chunks", array(Vec::new(), &b))], &b), "hypaV2Data.mainChunks"),
        (object(vec![("mainChunks", array(vec![object(vec![("id", Value::Number(1.0))], &b)], &b)), ("chunks", array(Vec::new(), &b))], &b), "hypaV2Data.mainChunks"),
        (object(vec![("mainChunks", array(Vec::new(), &b)), ("chunks", array(vec![Value::Null], &b))], &b), "hypaV2Data.chunks"),
        (object(vec![("mainChunks", array(Vec::new(), &b)), ("chunks", array(vec![object(vec![("text", Value::Number(1.0))], &b)], &b))], &b), "hypaV2Data.chunks"),
    ];
    for (stored, field) in cases {
        let log: Log = Arc::default();
        let mut declined = stage(Some(stored), true, &log);
        assert!(matches!(declined.run(history(&b), &b), Err(StageError::Unsupported(name)) if name == field));
        assert!(log.lock().unwrap().is_empty(), "{field} declines before any dependency call");
    }
}

#[test]
fn javascript_number_arithmetic_of_settings_reaches_the_result_and_error_text() {
    let b = budget();
    // `currentTokens -= maxResponse; currentTokens += allocated`, then the wrapper count, then `-= allocated`. The
    // `(0.2, 0.3, 13.3)` row rounds differently under every other order of these operations, including dropping the
    // virtual reserve, so only the TS order produces its bits.
    for (current, max_response, allocated) in [(0.1, 0.2, 0.3), (1.0, f64::NAN, 0.0), (-0.25, -1.5, 0.75), (0.2, 0.3, 13.3)] {
        let log: Log = Arc::default();
        let mut stage = HypaV2::new(config(allocated, max_response), None, Tokens { log: log.clone(), suspend: false, cancel_on: None, calls: 0 },
            Search { log: log.clone(), suspend: false, preparing: false }, Summaries { log: log.clone(), suspend: false });
        let step = stage.run(MemoryInput { chats: Vec::new(), current_tokens: current, max_context_tokens: 100.0 }, &b);
        let output = complete(&mut stage, step, &b);
        let expected = current - max_response + allocated + 3.0 - allocated;
        assert!(output.error.is_none());
        assert_eq!(output.current_tokens.to_bits(), expected.to_bits(), "{current} {max_response} {allocated}");
        if allocated == 13.3 { assert_eq!(output.current_tokens, 2.900000000000002); }
    }
    let log: Log = Arc::default();
    let mut stage = HypaV2::new(config(f64::INFINITY, 0.0), None, Tokens { log: log.clone(), suspend: false, cancel_on: None, calls: 0 },
        Search { log: log.clone(), suspend: false, preparing: false }, Summaries { log: log.clone(), suspend: false });
    let step = stage.run(MemoryInput { chats: Vec::new(), current_tokens: -0.0, max_context_tokens: 1e21 }, &b);
    let output = complete(&mut stage, step, &b);
    assert_eq!(output.error.unwrap().as_unicode(), Some("[HypaV2] Input tokens (Infinity) exceeds max context size (1e+21), but can't summarize last 4 messages. Please increase max context size to at least Infinity."));
    assert_eq!(output.current_tokens, f64::INFINITY);
    assert!(output.effects.is_empty() && log.lock().unwrap().is_empty());
}

#[test]
fn memo_index_growth_is_owned_before_the_first_dependency_call() {
    let initial = budget();
    let chats = |count: usize| (0..count).map(|index| chat("x", &format!("{index:04096}"), &initial)).collect::<Vec<_>>();
    // 64 distinct 4096-unit memo keys hold 512 KiB of UTF-16 in a table whose own charge stays far below the 256 KiB
    // limit, so only the charged key units cross it; 16 such keys fit.
    for (count, limit) in [(16, false), (64, true)] {
        let log: Log = Arc::default();
        let constrained = limited(256 << 10, 10_000_000, Arc::new(AtomicBool::new(false)));
        let mut probe = stage(None, true, &log);
        let result = probe.run(MemoryInput { chats: chats(count), current_tokens: 0.0, max_context_tokens: 100.0 }, &constrained);
        if limit { assert!(matches!(result, Err(StageError::Limit(name)) if name == "owned-bytes")); assert!(log.lock().unwrap().is_empty()); }
        else { assert!(matches!(result, Ok(Step::NeedsHost(_)))); }
    }
}

#[test]
fn legacy_memo_range_expansion_is_charged_as_work() {
    let initial = budget();
    let mut chats: Vec<_> = (0..5000).map(|_| chat("x", "m", &initial)).collect();
    chats[4999].memo = Some(text("z", &initial));
    let legacy = |b: &Budget| object(vec![("mainChunks", array(vec![object(vec![("text", Value::Text(text("old", b))),
        ("targetId", Value::Text(text("z", b)))], b)], b)), ("chunks", array(Vec::new(), b))], b);
    let current = |b: &Budget| object(vec![("mainChunks", array(Vec::new(), b)), ("chunks", array(Vec::new(), b))], b);
    let mut results = Vec::new();
    for stored in [current(&initial), legacy(&initial)] {
        let log: Log = Arc::default();
        let constrained = limited(16 << 20, 45_000, Arc::new(AtomicBool::new(false)));
        let mut probe = stage(Some(stored), true, &log);
        let chats = chats.iter().map(|message| chat("x", message.memo.as_ref().unwrap().as_unicode().unwrap(), &initial)).collect();
        let result = probe.run(MemoryInput { chats, current_tokens: 0.0, max_context_tokens: 100.0 }, &constrained);
        results.push((matches!(result, Ok(Step::NeedsHost(_))), matches!(result, Err(StageError::Limit(name)) if name == "work"), log.lock().unwrap().len()));
    }
    // Admitting the same 5000 memos fits; expanding the legacy main over all of them does not.
    assert_eq!(results, [(true, false, 1), (false, true, 0)]);
}

/// What a payload the stage built holds under its budget: every growth goes to the next power of two, so a text holds
/// that capacity over its UTF-8 bytes (UTF-16 units when it has an isolated surrogate) and a container over its length.
fn charged_text(text: &Text) -> usize {
    let bytes = match text.as_unicode() { Some(value) => value.len(), None => text.len_units() * 2 };
    if bytes == 0 { 0 } else { bytes.next_power_of_two() }
}
fn charged_value(value: &Value) -> usize {
    let capacity = |length: usize| if length == 0 { 0 } else { length.next_power_of_two() };
    match value {
        Value::Text(text) => charged_text(text),
        Value::Array(items) => capacity(items.as_slice().len()) * std::mem::size_of::<Value>()
            + items.as_slice().iter().map(charged_value).sum::<usize>(),
        Value::Object(properties) => capacity(properties.as_slice().len()) * std::mem::size_of::<(Text, Value)>()
            + properties.as_slice().iter().map(|(key, value)| charged_text(key) + charged_value(value)).sum::<usize>(),
        _ => 0,
    }
}

#[test]
fn returned_text_and_value_leases_stay_owned_until_the_output_drops() {
    let b = budget();
    // The input chats belong to their own budget, so the run budget holds only what the stage itself built.
    let inputs = budget();
    let log: Log = Arc::default();
    let mut probe = stage(None, true, &log);
    let step = probe.run(history(&inputs), &b);
    assert!(matches!(step, Ok(Step::NeedsHost(_))));
    let output = complete(&mut probe, step, &b);
    let completed = b.owned_bytes();
    drop(probe);
    // A completed stage retains nothing, and what remains is exactly the memory message's content and memo, the
    // effect's field name and the stored value: the containers leave the budget and no scratch travels with them.
    assert_eq!(b.owned_bytes(), completed);
    let memory = &output.chats[0];
    let [Effect::ChatField { field, value }] = output.effects.as_slice() else { panic!("one stored memory") };
    let expected = charged_text(&memory.content) + memory.memo.as_ref().map_or(0, charged_text) + charged_text(field) + charged_value(value);
    assert_eq!(b.owned_bytes() - Budget::context_bytes(), expected);
    drop(output);
    assert_eq!(b.owned_bytes(), Budget::context_bytes());
    assert_eq!(inputs.owned_bytes(), Budget::context_bytes());
}

#[allow(dead_code)]
fn stage_is_send<T: TokenCounter + Send, R: Retrieval + Send, S: SummaryService + Send>(stage: HypaV2<T, R, S>) -> impl Send { stage }
