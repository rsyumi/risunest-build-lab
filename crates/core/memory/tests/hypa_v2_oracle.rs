//! Replays the actual TS HypaV2 observations through the public stage. The policy replay feeds back the recorded
//! tokenizer, summary and retrieval results; the composition replay runs the real `HypaV1Retrieval` over the
//! recorded cache and local-embedding boundaries instead. Neither replays retrieval math as a policy decision.
use risunest_ir::chat::PromptMessage;
use risunest_ir::stage::{EmbedRequest, Effect, HostReply, HostRequest, MemoryInput, MemoryOutput, MemoryStage, StageError, Step, SubRequest, TokenCounter, TokenInput};
use risunest_ir::{json, parse_json, Budget, Limits, OwnedVec, Text, Value as Ir};
use risunest_memory::hanurai::{Retrieval, RetrievalOutput, ScoredDocument};
use risunest_memory::hypa_v1::*;
use risunest_memory::hypa_v2::{HypaV2, HypaV2Config, SummaryOutput, SummaryService};
use serde_json::{json as j, Value as J};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::sync::{atomic::AtomicBool, Arc, Mutex};

fn budget() -> Budget {
    Budget::new(Limits { max_owned_bytes: 64 << 20, max_json_bytes: 16 << 20, max_text_units: 4 << 20,
        max_values: 1_000_000, max_depth: 128, max_work: 100_000_000 }, Arc::new(AtomicBool::new(false))).unwrap()
}

// The fixture tags of the TS `pack`/`unpack` helpers, over IR values so stored key order survives.
fn unpack(value: &Ir, b: &Budget) -> Ir {
    match value {
        Ir::Array(items) => {
            let mut out = OwnedVec::new(b).unwrap();
            for item in items.as_slice() { out.push(unpack(item, b)).unwrap(); }
            Ir::Array(out)
        }
        Ir::Object(properties) => {
            if let [(key, tagged)] = properties.as_slice() {
                if key.equals_ascii(b"$undefined") { return Ir::Undefined; }
                if key.equals_ascii(b"$number") {
                    let Ir::Text(number) = tagged else { panic!("$number tag") };
                    return Ir::Number(match number.as_unicode().unwrap() { "-0" => -0.0, "NaN" => f64::NAN,
                        "Infinity" => f64::INFINITY, "-Infinity" => f64::NEG_INFINITY, other => other.parse().unwrap() });
                }
                if key.equals_ascii(b"$utf16") {
                    let units: Vec<u16> = tagged.array().unwrap().iter().map(|unit| match unit { Ir::Number(n) => *n as u16, _ => panic!() }).collect();
                    return Ir::Text(Text::from_units(&units, b).unwrap());
                }
            }
            let mut out = Ir::Object(OwnedVec::new(b).unwrap());
            for (key, value) in properties.as_slice() { out.define(key.copy(b).unwrap(), unpack(value, b), b).unwrap(); }
            out
        }
        Ir::Text(text) => Ir::Text(text.copy(b).unwrap()),
        Ir::Number(n) => Ir::Number(*n),
        Ir::Bool(v) => Ir::Bool(*v),
        Ir::Null => Ir::Null,
        Ir::Undefined => Ir::Undefined,
    }
}
fn pack_text(text: &Text) -> J {
    let units: Vec<u16> = text.units().collect();
    match String::from_utf16(&units) { Ok(value) => J::String(value), Err(_) => j!({"$utf16": units}) }
}
fn pack_number(n: f64) -> J {
    if n.is_nan() { j!({"$number": "NaN"}) } else if n == f64::INFINITY { j!({"$number": "Infinity"}) }
    else if n == f64::NEG_INFINITY { j!({"$number": "-Infinity"}) } else if n == 0.0 && n.is_sign_negative() { j!({"$number": "-0"}) }
    else { j!(n) }
}
fn pack(value: &Ir) -> J {
    match value {
        Ir::Undefined => j!({"$undefined": true}),
        Ir::Null => J::Null,
        Ir::Bool(v) => J::Bool(*v),
        Ir::Number(n) => pack_number(*n),
        Ir::Text(text) => pack_text(text),
        Ir::Array(items) => J::Array(items.as_slice().iter().map(pack).collect()),
        Ir::Object(properties) => J::Object(properties.as_slice().iter()
            .map(|(key, value)| (String::from_utf16_lossy(&key.units().collect::<Vec<_>>()), pack(value))).collect()),
    }
}
fn pack_message(message: &PromptMessage, b: &Budget) -> J { pack(&message.to_value(b).unwrap()) }
// JSON has one number type; serde_json keeps integral and floating representations apart.
fn same_js_value(left: &J, right: &J) -> bool {
    match (left, right) {
        (J::Number(a), J::Number(b)) => a.as_f64().unwrap().to_bits() == b.as_f64().unwrap().to_bits(),
        (J::Array(a), J::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_js_value(a, b)),
        (J::Object(a), J::Object(b)) => a.len() == b.len() && a.iter().all(|(key, value)| b.get(key).is_some_and(|other| same_js_value(value, other))),
        _ => left == right,
    }
}
fn host(failure: &J, b: &Budget) -> StageError {
    StageError::Host(Text::from_str(failure["message"].as_str().unwrap(), b).unwrap().units().collect())
}
fn field<'a>(value: &'a Ir, key: &str, b: &Budget) -> Option<&'a Ir> { value.get(key.as_bytes(), b).unwrap() }
fn number(value: &Ir) -> f64 { match value { Ir::Number(n) => *n, _ => panic!("number") } }

type Tape = Arc<Mutex<VecDeque<J>>>;
fn next(tape: &Tape, kind: &str) -> J {
    let event = tape.lock().unwrap().pop_front().unwrap_or_else(|| panic!("unrecorded {kind} call"));
    assert_eq!(event["kind"], kind, "dependency order changed");
    event
}
fn leaf_request(kind: &str, b: &Budget) -> HostRequest {
    match kind {
        "tokenizer" => HostRequest::PluginTokenizer { text: Text::from_str("replay-tokenizer-leaf", b).unwrap() },
        "summary" => HostRequest::Llm(SubRequest { mode: Text::from_str("memory", b).unwrap(), messages: Vec::new(), options: Ir::Undefined }),
        _ => HostRequest::Embed(EmbedRequest { model: Text::from_str(kind, b).unwrap(), texts: Vec::new() }),
    }
}

/// Holds a recorded result across `yields` host round trips, so every dependency phase also runs suspended.
struct Yielding<T> { yields: u8, remaining: u8, pending: Option<Result<T, StageError>>, kind: &'static str }
impl<T> Yielding<T> {
    fn new(yields: u8, kind: &'static str) -> Self { Self { yields, remaining: 0, pending: None, kind } }
    fn deliver(&mut self, result: Result<T, StageError>, b: &Budget) -> Result<Step<T>, StageError> {
        if self.yields == 0 { return result.map(Step::Done); }
        assert!(self.pending.is_none(), "{} called while suspended", self.kind);
        self.pending = Some(result);
        self.remaining = self.yields;
        Ok(Step::NeedsHost(leaf_request(self.kind, b)))
    }
    fn resume(&mut self, b: &Budget) -> Result<Step<T>, StageError> {
        self.remaining -= 1;
        if self.remaining > 0 { Ok(Step::NeedsHost(leaf_request(self.kind, b))) }
        else { self.pending.take().expect("pending replay").map(Step::Done) }
    }
}

struct Tokens { tape: Tape, held: Yielding<u32> }
impl TokenCounter for Tokens {
    fn count(&mut self, input: TokenInput<'_>, b: &Budget) -> Result<Step<u32>, StageError> {
        let TokenInput::Message(message) = input else { panic!("HypaV2 counts complete messages") };
        let event = next(&self.tape, "tokenizeChat");
        assert!(same_js_value(&pack_message(message, b), &event["message"]), "counted {} where TS counted {}", pack_message(message, b), event["message"]);
        let result = match event.get("failure") { Some(failure) => Err(host(failure, b)), None => Ok(event["count"].as_u64().unwrap() as u32) };
        self.held.deliver(result, b)
    }
    fn resume(&mut self, reply: HostReply, b: &Budget) -> Result<Step<u32>, StageError> {
        assert!(matches!(reply, HostReply::Tokens(_)), "tokenizer received another dependency's reply");
        self.held.resume(b)
    }
}

struct Summaries { tape: Tape, held: Yielding<SummaryOutput> }
impl SummaryService for Summaries {
    fn summarize(&mut self, input: Text, b: &Budget) -> Result<Step<SummaryOutput>, StageError> {
        let event = next(&self.tape, "summarizer");
        assert_eq!(pack_text(&input), event["input"], "summary batch text");
        // The TS helper catches a summarizer rejection and returns it as a failed summary.
        let output = match event.get("failure") {
            Some(failure) => SummaryOutput::Failed(Text::from_str(&format!("SupaMemory: Summarizer: {}: {}",
                failure["name"].as_str().unwrap(), failure["message"].as_str().unwrap()), b)?),
            None => SummaryOutput::Success(Text::from_str(event["reply"].as_str().unwrap(), b)?),
        };
        self.held.deliver(Ok(output), b)
    }
    fn resume(&mut self, reply: HostReply, b: &Budget) -> Result<Step<SummaryOutput>, StageError> {
        assert!(matches!(reply, HostReply::Value(_)), "summary received another dependency's reply");
        self.held.resume(b)
    }
}

struct Search { tape: Tape, held: Yielding<RetrievalOutput> }
impl Retrieval for Search {
    fn prepare(&mut self, documents: Vec<Text>, b: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        let event = next(&self.tape, "prepare");
        assert_eq!(J::Array(documents.iter().map(pack_text).collect()), event["input"], "prepared documents");
        let result = match event.get("failure") { Some(failure) => Err(host(failure, b)), None => Ok(RetrievalOutput::Prepared) };
        self.held.deliver(result, b)
    }
    fn query(&mut self, query: Text, b: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        let event = next(&self.tape, "query");
        assert_eq!(pack_text(&query), event["input"], "query text");
        let result = match event.get("failure") {
            Some(failure) => Err(host(failure, b)),
            None => Ok(RetrievalOutput::Scored(event["result"].as_array().unwrap().iter().map(|entry| ScoredDocument {
                document: if entry[0].get("$undefined").is_some() { None } else { Some(Text::from_str(entry[0].as_str().unwrap(), b).unwrap()) },
                score: if let Some(tag) = entry[1].get("$number") { match tag.as_str().unwrap() { "NaN" => f64::NAN, "-0" => -0.0,
                    "Infinity" => f64::INFINITY, _ => f64::NEG_INFINITY } } else { entry[1].as_f64().unwrap() },
            }).collect())),
        };
        self.held.deliver(result, b)
    }
    fn resume(&mut self, reply: HostReply, b: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        assert!(matches!(reply, HostReply::Embeddings(_)), "retrieval received another dependency's reply");
        self.held.resume(b)
    }
}

fn hex(key: &CacheKey) -> J { J::String(String::from_utf8(key.hex().to_vec()).unwrap()) }
fn le_bytes(bits: &[u32]) -> J { J::Array(bits.iter().flat_map(|bits| bits.to_le_bytes()).map(|byte| j!(byte)).collect()) }
type Durable = Arc<Mutex<Vec<(String, Vec<u32>)>>>;
struct Cache { tape: Tape, durable: Durable }
impl ImmediateCache for Cache {
    fn read_batch(&mut self, keys: &[CacheKey], b: &Budget) -> Result<OwnedVec<Option<CacheRow>>, CacheError> {
        let event = next(&self.tape, "cache-read");
        assert_eq!(J::Array(keys.iter().map(hex).collect()), event["keys"], "cache read keys");
        if event.get("failure").is_some() { return Err(CacheError::Unavailable); }
        let durable = self.durable.lock().unwrap();
        let mut rows = OwnedVec::new(b).map_err(|e| CacheError::Terminal(e.into()))?;
        for key in keys {
            if let Some((_, bits)) = durable.iter().find(|(stored, _)| J::String(stored.clone()) == hex(key)) {
                let mut values = OwnedVec::new(b).map_err(|e| CacheError::Terminal(e.into()))?;
                values.extend(bits).map_err(|e| CacheError::Terminal(e.into()))?;
                rows.push(Some(CacheRow { key: *key, dimensions: bits.len(), bits: values })).map_err(|e| CacheError::Terminal(e.into()))?;
            }
        }
        assert_eq!(rows.as_slice().len(), event["rows"].as_array().unwrap().len(), "cache rows read");
        Ok(rows)
    }
    fn write_batch(&mut self, _: &EmbeddingTarget, entries: &[CacheWrite], _: &Budget) -> Result<(), CacheError> {
        // The real bridge rejects empty vectors during frame construction, before native IO.
        if entries.iter().any(|entry| entry.bits.as_slice().is_empty()) { return Err(CacheError::Unavailable); }
        let event = next(&self.tape, "cache-write");
        assert_eq!(J::Array(entries.iter().map(|entry| entry.key.as_ref().map_or(j!({"$undefined": true}), hex)).collect()), event["keys"]);
        let rows = event["rows"].as_array().unwrap();
        assert_eq!(rows.len(), entries.len());
        for (row, entry) in rows.iter().zip(entries) { assert_eq!(row["bytes"], le_bytes(entry.bits.as_slice()), "cache row bytes"); }
        if event.get("failure").is_some() { return Err(CacheError::Unavailable); }
        let mut durable = self.durable.lock().unwrap();
        for entry in entries {
            let key = hex(entry.key.as_ref().unwrap()).as_str().unwrap().to_owned();
            let bits = entry.bits.as_slice().to_vec();
            match durable.iter_mut().find(|(stored, _)| *stored == key) { Some(old) => old.1 = bits, None => durable.push((key, bits)) }
        }
        Ok(())
    }
}
struct Embeddings { tape: Tape, held: Yielding<EmbeddingBatch> }
impl EmbeddingService for Embeddings {
    fn embed(&mut self, request: EmbeddingRequest, b: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        let event = next(&self.tape, "local-embedding");
        let purpose = match request.purpose { EmbeddingPurpose::Document => "document", EmbeddingPurpose::Query => "query" };
        assert_eq!(event["purpose"], purpose);
        let input = match &request.input { EmbeddingInput::Scalar(text) => pack_text(text),
            EmbeddingInput::Batch(texts) => J::Array(texts.as_slice().iter().map(pack_text).collect()) };
        assert_eq!(input, event["input"], "embedding input");
        let result = match event.get("failure") {
            Some(failure) => Err(host(failure, b)),
            None => {
                let mut batch = OwnedVec::new(b)?;
                for vector in event["reply"].as_array().unwrap() {
                    let mut bits = OwnedVec::new(b)?;
                    for value in vector["f32bits"].as_array().unwrap() { bits.push(value.as_u64().unwrap() as u32)?; }
                    batch.push(Some(EmbeddingVector::F32(bits)))?;
                }
                Ok(batch)
            }
        };
        self.held.deliver(result, b)
    }
    fn resume(&mut self, reply: HostReply, b: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        assert!(matches!(reply, HostReply::Embeddings(_)), "embedding service received another dependency's reply");
        self.held.resume(b)
    }
}

fn complete<M: MemoryStage>(stage: &mut M, mut step: Result<Step<MemoryOutput>, StageError>, b: &Budget) -> Result<MemoryOutput, StageError> {
    loop {
        step = match step? {
            Step::Done(output) => return Ok(output),
            Step::NeedsHost(request) => stage.resume(match request {
                HostRequest::PluginTokenizer { .. } => HostReply::Tokens(Vec::new()),
                HostRequest::Embed(_) => HostReply::Embeddings(Vec::new()),
                HostRequest::Llm(_) => HostReply::Value(Ir::Undefined),
                _ => panic!("unexpected replay host kind"),
            }, b),
        };
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode { Policy, Composition }
impl Mode {
    fn kinds(self) -> &'static [&'static str] {
        match self {
            Mode::Policy => &["tokenizeChat", "summarizer", "prepare", "query"],
            Mode::Composition => &["tokenizeChat", "summarizer", "cache-read", "local-embedding", "cache-write"],
        }
    }
}

struct Corpus { cases: Ir, expected: J, budget: Budget }
fn corpus() -> Corpus {
    let bytes = include_bytes!("../../../../tests/fixtures/rust-core/memory/hypa-v2/policy.cases.json");
    let expected: J = serde_json::from_str(include_str!("../../../../tests/fixtures/rust-core/memory/hypa-v2/policy.expected.json")).unwrap();
    assert_eq!(expected["fixtureSha256"], Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect::<String>());
    let budget = budget();
    let cases = parse_json(std::str::from_utf8(bytes).unwrap(), &budget).unwrap();
    assert_eq!(number(field(&cases, "schema", &budget).unwrap()), expected["schema"].as_f64().unwrap());
    Corpus { cases, expected, budget }
}

/// Cases the frozen IR cannot carry: `TokenCounter` returns `u32`, so a NaN or infinite TS message count has no
/// native form, and `PromptMessage.memo` cannot hold `null`.
const NOT_EXPRESSIBLE: [&str; 4] = ["undefined-null-empty-memos-json-readback", "nan-token-count-propagates-through-real-tokenizer",
    "positive-infinite-wrapper-token-count", "negative-infinite-wrapper-token-count"];
fn expressible(case: &Ir, expected: &J, b: &Budget) -> bool {
    let chats = field(field(case, "input", b).unwrap(), "chats", b).unwrap().array().unwrap();
    let null_memo = chats.iter().any(|chat| matches!(field(chat, "memo", b), Some(Ir::Null)));
    let counts = expected["result"]["invocations"].as_array().unwrap().iter().flat_map(|invocation| invocation["events"].as_array().unwrap())
        .filter(|event| event["kind"] == "tokenizeChat" && event.get("failure").is_none())
        .all(|event| event["count"].as_u64().is_some_and(|count| count <= u64::from(u32::MAX)));
    !null_memo && counts
}

fn run_corpus(mode: Mode) {
    let Corpus { cases, expected, budget: b } = corpus();
    let cases = field(&cases, "cases", &b).unwrap().array().unwrap();
    let expected = expected["cases"].as_array().unwrap();
    assert_eq!(cases.len(), expected.len());
    let mut excluded = Vec::new();
    let mut mutated = Vec::new();
    for (case, expected) in cases.iter().zip(expected) {
        let Some(Ir::Text(id)) = field(case, "id", &b) else { panic!("case id") };
        let id = id.as_unicode().unwrap().to_owned();
        assert_eq!(expected["id"], id.as_str());
        if !expressible(case, expected, &b) { excluded.push(id); continue; }
        for yields in [0, 2] { replay(case, &expected["result"], &id, mode, yields, &mut mutated); }
    }
    assert_eq!(excluded, NOT_EXPRESSIBLE, "cases outside the frozen IR");
    // TS normalizes a malformed stored chatMemos in the caller's room record in place. On success the caller replaces
    // that record with the returned memory, which `replay` checks carries the same normalization; on error the native
    // stage publishes nothing, so the room keeps its original record. That error case is the accepted difference.
    mutated.dedup();
    assert_eq!(mutated, [("malformed-memos-mutated-before-returned-error".to_owned(), true),
        ("successful-current-memo-normalization-preserves-fields".to_owned(), false)]);
}

fn replay(case: &Ir, expected: &J, id: &str, mode: Mode, yields: u8, mutated: &mut Vec<(String, bool)>) {
    let b = budget();
    assert_eq!(expected["scope"], "actual-hypav2-distilbart-minilm-policy");
    let input = field(case, "input", &b).unwrap();
    let config = || HypaV2Config { max_response: field(input, "maxResponse", &b).map_or(0.0, number),
        allocated_tokens: number(field(input, "allocatedTokens", &b).unwrap()), chunk_size: number(field(input, "chunkSize", &b).unwrap()) };
    let durable: Durable = Arc::new(Mutex::new(Vec::new()));
    let mut previous: Option<Text> = None;
    let invocations = field(input, "invocations", &b).unwrap().array().unwrap();
    for (index, invocation) in invocations.iter().enumerate() {
        let observed = &expected["invocations"][index];
        let context = format!("{id} invocation {index} mode={} yields={yields}", if mode == Mode::Policy { "policy" } else { "composition" });
        let stored = if field(invocation, "persistPrevious", &b).is_some() {
            // The fixture feeds the previous successful memory back through JSON, as stored data would be.
            Some(json::parse_json_text(previous.as_ref().expect("persisted memory"), &b).unwrap())
        } else {
            let mut room = unpack(field(input, "room", &b).unwrap(), &b);
            room.take(b"hypaV2Data", &b).unwrap()
        };
        let chats = unpack(field(input, "chats", &b).unwrap(), &b);
        let chats: Vec<PromptMessage> = match chats { Ir::Array(mut values) => values.as_mut_slice().iter_mut()
            .map(|value| PromptMessage::from_value(std::mem::replace(value, Ir::Undefined), &b).unwrap()).collect(), _ => panic!() };
        let tape: Tape = Arc::new(Mutex::new(observed["events"].as_array().unwrap().iter()
            .filter(|event| mode.kinds().contains(&event["kind"].as_str().unwrap())).cloned().collect()));
        let memory_input = MemoryInput { chats, current_tokens: number(&unpack(field(invocation, "currentTokens", &b).unwrap(), &b)),
            max_context_tokens: number(&unpack(field(invocation, "maxContextTokens", &b).unwrap(), &b)) };
        let tokens = Tokens { tape: tape.clone(), held: Yielding::new(yields, "tokenizer") };
        let summaries = Summaries { tape: tape.clone(), held: Yielding::new(yields, "summary") };
        let result = match mode {
            Mode::Policy => {
                let mut stage = HypaV2::new(config(), stored, tokens, Search { tape: tape.clone(), held: Yielding::new(yields, "retrieval") }, summaries);
                let step = stage.run(memory_input, &b);
                let result = complete(&mut stage, step, &b);
                assert!(matches!(stage.resume(HostReply::Cancelled, &b), Err(StageError::Invalid(_))), "{context}: finished stage resumed");
                result
            }
            Mode::Composition => {
                let target = EmbeddingTarget::capture(&Text::from_str("MiniLM", &b).unwrap(), None, &Text::from_str("", &b).unwrap(),
                    &Text::from_str("", &b).unwrap(), &b).unwrap();
                let retrieval = HypaV1Retrieval::new(target, Cache { tape: tape.clone(), durable: durable.clone() },
                    Embeddings { tape: tape.clone(), held: Yielding::new(yields, "embedding-service") }, 1024).unwrap();
                let mut stage = HypaV2::new(config(), stored, tokens, retrieval, summaries);
                let step = stage.run(memory_input, &b);
                let result = complete(&mut stage, step, &b);
                assert!(matches!(stage.resume(HostReply::Cancelled, &b), Err(StageError::Invalid(_))), "{context}: finished stage resumed");
                result
            }
        };
        assert!(tape.lock().unwrap().is_empty(), "{context}: unconsumed observations {:?}", tape.lock().unwrap().front());
        let outcome = &observed["outcome"];
        let mut failed = false;
        let mut published = None;
        match result {
            Err(StageError::Host(message)) => {
                assert_eq!(String::from_utf16(&message).unwrap(), outcome["thrown"]["message"], "{context}");
                failed = true;
            }
            Err(error) => panic!("{context}: {error:?}"),
            Ok(output) => {
                let returned = &outcome["returned"];
                assert!(returned.is_object(), "{context}: TS threw {}", outcome["thrown"]);
                let chats = J::Array(output.chats.iter().map(|chat| pack_message(chat, &b)).collect());
                assert!(same_js_value(&chats, &returned["chats"]), "{context}: chats {chats} != {}", returned["chats"]);
                assert!(same_js_value(&pack_number(output.current_tokens), &returned["currentTokens"]), "{context}: tokens {}", output.current_tokens);
                match returned.get("error") {
                    Some(error) => {
                        assert_eq!(output.error.as_ref().map(pack_text).as_ref(), Some(error), "{context}");
                        assert!(output.effects.is_empty(), "{context}: an error publishes no stored memory");
                        failed = true;
                    }
                    None => {
                        assert!(output.error.is_none(), "{context}");
                        let [Effect::ChatField { field, value }] = output.effects.as_slice() else { panic!("{context}: one stored-memory effect") };
                        assert_eq!(field.as_unicode(), Some("hypaV2Data"));
                        assert!(same_js_value(&pack(value), &returned["memory"]), "{context}: memory {} != {}", pack(value), returned["memory"]);
                        let persisted = json::stringify(value, &b).unwrap().unwrap();
                        assert_eq!(persisted.as_unicode(), observed["memoryJSON"].as_str(), "{context}: persisted JSON and key order");
                        previous = Some(persisted);
                        published = Some(pack(value));
                    }
                }
            }
        }
        if !same_js_value(&observed["roomBefore"], &observed["roomAfter"]) {
            if let Some(memory) = &published {
                assert!(same_js_value(&memory["mainChunks"], &observed["roomAfter"]["hypaV2Data"]["mainChunks"]), "{context}: normalized mains");
            }
            mutated.push((id.to_owned(), failed));
        }
        if mode == Mode::Composition {
            let rows: Vec<J> = durable.lock().unwrap().iter().map(|(key, bits)| j!({"key": key, "bytes": le_bytes(bits)})).collect();
            let expected_rows: Vec<J> = expected["durableCache"].as_array().unwrap().iter().map(|row| j!({"key": row["key"], "bytes": row["bytes"]})).collect();
            if index + 1 == invocations.len() { assert_eq!(rows, expected_rows, "{context}: durable cache"); }
        }
    }
}

#[test]
fn real_typescript_hypa_v2_policy_matches_immediate_and_suspended_native_policy() { run_corpus(Mode::Policy); }

#[test]
fn real_hypa_v1_retrieval_composes_with_hypa_v2_over_recorded_cache_and_inference() { run_corpus(Mode::Composition); }
