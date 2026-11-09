//! Replays the actual TS HypaV3 observations through the public stage, immediately and with every dependency
//! suspended. The replay feeds back the recorded tokenizer counts, auxiliary-model replies (through the public
//! reply helper), retrieval results and `Math.random` values; it does not replay selection or ranking decisions.
use risunest_ir::chat::PromptMessage;
use risunest_ir::rng::{Rng, Tape as RandomTape};
use risunest_ir::stage::{EmbedRequest, Effect, HostReply, HostRequest, MemoryInput, MemoryOutput, MemoryStage, StageError, Step, SubRequest, TokenCounter, TokenInput};
use risunest_ir::{json, parse_json, Budget, Error, Limits, OwnedVec, Text, Value as Ir};
use risunest_memory::hanurai::{Retrieval, RetrievalOutput, ScoredDocument};
use risunest_memory::hypa_v2::{SummaryOutput, SummaryService};
use risunest_memory::hypa_v3::{sub_model_summary, HypaV3, HypaV3Config, HypaV3PreparedHistory};
use serde_json::{json as j, Value as J};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::sync::{atomic::AtomicBool, Arc, Mutex};

const DEFAULT_PROMPT: &str = "[Summarize the ongoing role story, It must also remove redundancy and unnecessary text and content from the output.]";

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
fn tagged_number(value: &J) -> f64 {
    match value.get("$number").and_then(J::as_str) {
        Some("NaN") => f64::NAN, Some("-0") => -0.0, Some("Infinity") => f64::INFINITY, Some("-Infinity") => f64::NEG_INFINITY,
        Some(other) => panic!("number tag {other}"),
        None => value.as_f64().unwrap(),
    }
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
        (J::Object(a), J::Object(b)) => a.len() == b.len() && a.iter().zip(b).all(|((ka, va), (kb, vb))| ka == kb && same_js_value(va, vb)),
        _ => left == right,
    }
}
fn host(failure: &J, b: &Budget) -> StageError {
    assert_eq!(failure["name"], "Error", "a host failure surfaces as an Error");
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
        let TokenInput::Message(message) = input else { panic!("HypaV3 counts complete messages") };
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

/// The auxiliary-model summary service over the recorded `requestChatData` exchange.
struct Summaries { tape: Tape, held: Yielding<SummaryOutput> }
impl SummaryService for Summaries {
    fn summarize(&mut self, input: Text, b: &Budget) -> Result<Step<SummaryOutput>, StageError> {
        let event = next(&self.tape, "summary");
        // The default prompt has no ChatML markers, so TS sends the batch text and the prompt as two messages.
        assert_eq!(event["formated"], j!([{"role": "user", "content": pack_text(&input)}, {"role": "system", "content": DEFAULT_PROMPT}]), "summary request");
        assert_eq!(event["model"], "memory");
        let result = match event.get("failure") {
            Some(failure) => Err(host(failure, b)),
            None => {
                let reply = &event["reply"];
                let result = Text::from_str(reply["result"].as_str().unwrap(), b)?;
                match reply["type"].as_str().unwrap() {
                    "success" => sub_model_summary(&result, b),
                    "fail" => Ok(SummaryOutput::Failed(Text::from_str(&format!("Error: {}", reply["result"].as_str().unwrap()), b)?)),
                    other => panic!("reply type {other}"),
                }
            }
        };
        self.held.deliver(result, b)
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
        assert_eq!(J::Array(documents.iter().map(pack_text).collect()), event["input"], "prepared chunks");
        let result = match event.get("failure") { Some(failure) => Err(host(failure, b)), None => Ok(RetrievalOutput::Prepared) };
        self.held.deliver(result, b)
    }
    fn query(&mut self, query: Text, b: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        let event = next(&self.tape, "query");
        assert_eq!(pack_text(&query), event["input"], "query text");
        let result = match event.get("failure") {
            Some(failure) => Err(host(failure, b)),
            None => Ok(RetrievalOutput::Scored(event["result"].as_array().unwrap().iter().map(|entry| ScoredDocument {
                document: Some(Text::from_str(entry[0].as_str().unwrap(), b).unwrap()), score: tagged_number(&entry[1]),
            }).collect())),
        };
        self.held.deliver(result, b)
    }
    fn resume(&mut self, reply: HostReply, b: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        assert!(matches!(reply, HostReply::Embeddings(_)), "retrieval received another dependency's reply");
        self.held.resume(b)
    }
}

struct Random(Arc<Mutex<RandomTape>>);
impl Rng for Random {
    fn random(&mut self) -> Result<f64, Error> { self.0.lock().unwrap().random() }
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

/// `createHypaV3Preset` defaults under the case's settings.
fn config(input: &Ir, b: &Budget) -> HypaV3Config {
    let settings = field(input, "settings", b).unwrap();
    let ratio = |key: &str, default: f64| field(settings, key, b).map_or(default, number);
    let flag = |key: &str| matches!(field(settings, key, b), Some(Ir::Bool(true)));
    let separator = match field(settings, "summaryChunkSeparator", b) { Some(Ir::Text(text)) => text.copy(b).unwrap(), _ => Text::from_str("\\n\\n", b).unwrap() };
    let texts = |value: &Ir| value.array().unwrap().iter().map(|memo| match memo { Ir::Text(text) => text.copy(b).unwrap(), _ => panic!("memo") }).collect();
    let prepared = field(input, "prepared", b).map(|prepared| {
        let effective_memos = texts(field(prepared, "effectiveMessageMemos", b).unwrap());
        match field(prepared, "historyStartIndex", b) {
            Some(start) => HypaV3PreparedHistory::Boundary { effective_memos, history_start_index: number(start) as usize,
                boundary_memo: match field(prepared, "boundaryMemo", b) { Some(Ir::Text(text)) => text.copy(b).unwrap(), _ => panic!("boundary") } },
            None => HypaV3PreparedHistory::Window { effective_memos },
        }
    });
    HypaV3Config {
        max_response: field(input, "maxResponse", b).map_or(0.0, number),
        memory_tokens_ratio: ratio("memoryTokensRatio", 0.2), extra_summarization_ratio: ratio("extraSummarizationRatio", 0.0),
        max_chats_per_summary: ratio("maxChatsPerSummary", 6.0), recent_memory_ratio: ratio("recentMemoryRatio", 0.4),
        similar_memory_ratio: ratio("similarMemoryRatio", 0.4), enable_similarity_correction: flag("enableSimilarityCorrection"),
        preserve_orphaned_memory: flag("preserveOrphanedMemory"), do_not_summarize_user_message: flag("doNotSummarizeUserMessage"),
        summary_chunk_separator: separator, query_chat_count: ratio("queryChatCount", 3.0), use_experimental_impl: flag("useExperimentalImpl"),
        contextual_embedding: false, prepared,
    }
}

const KINDS: [&str; 4] = ["tokenizeChat", "summary", "prepare", "query"];

fn replay(case: &Ir, expected: &J, id: &str, yields: u8) {
    let b = budget();
    assert_eq!(expected["scope"], "actual-hypav3-submodel-minilm-policy");
    let input = field(case, "input", &b).unwrap();
    let mut previous: Option<Text> = None;
    let invocations = field(input, "invocations", &b).unwrap().array().unwrap();
    for (index, invocation) in invocations.iter().enumerate() {
        let observed = &expected["invocations"][index];
        let context = format!("{id} invocation {index} yields={yields}");
        assert!(same_js_value(&observed["roomBefore"], &observed["roomAfter"]), "{context}: TS changed the room record");
        let stored = if field(invocation, "persistPrevious", &b).is_some() {
            // The fixture feeds the previous successful memory back through JSON, as stored data would be.
            Some(json::parse_json_text(previous.as_ref().expect("persisted memory"), &b).unwrap())
        } else {
            let mut room = unpack(field(input, "room", &b).unwrap(), &b);
            room.take(b"hypaV3Data", &b).unwrap()
        };
        let chats = unpack(field(input, "chats", &b).unwrap(), &b);
        let chats: Vec<PromptMessage> = match chats { Ir::Array(mut values) => values.as_mut_slice().iter_mut()
            .map(|value| PromptMessage::from_value(std::mem::replace(value, Ir::Undefined), &b).unwrap()).collect(), _ => panic!() };
        let tape: Tape = Arc::new(Mutex::new(observed["events"].as_array().unwrap().iter()
            .filter(|event| KINDS.contains(&event["kind"].as_str().unwrap())).cloned().collect()));
        let draws: Vec<f64> = observed["random"].as_array().unwrap().iter().map(tagged_number).collect();
        let random = Arc::new(Mutex::new(RandomTape::new(draws.clone())));
        let memory_input = MemoryInput { chats, current_tokens: number(&unpack(field(invocation, "currentTokens", &b).unwrap(), &b)),
            max_context_tokens: number(&unpack(field(invocation, "maxContextTokens", &b).unwrap(), &b)) };
        let mut stage = HypaV3::new(config(input, &b), stored, Tokens { tape: tape.clone(), held: Yielding::new(yields, "tokenizer") },
            Search { tape: tape.clone(), held: Yielding::new(yields, "retrieval") }, Summaries { tape: tape.clone(), held: Yielding::new(yields, "summary") },
            Random(random.clone()));
        let step = stage.run(memory_input, &b);
        let result = complete(&mut stage, step, &b);
        assert!(matches!(stage.resume(HostReply::Cancelled, &b), Err(StageError::Invalid(_))), "{context}: finished stage resumed");
        assert!(tape.lock().unwrap().is_empty(), "{context}: unconsumed observations {:?}", tape.lock().unwrap().front());
        assert_eq!(random.lock().unwrap().consumed(), draws.len(), "{context}: Math.random draws");
        let outcome = &observed["outcome"];
        match result {
            Err(StageError::Host(message)) => assert_eq!(String::from_utf16(&message).unwrap(), outcome["thrown"]["message"], "{context}"),
            Err(error) => panic!("{context}: {error:?}"),
            Ok(output) if outcome.get("thrown").is_some() => {
                // An error TS throws is returned with its message and no stored change.
                assert_eq!(output.error.as_ref().map(pack_text).as_ref(), Some(&outcome["thrown"]["message"]), "{context}");
                assert!(output.effects.is_empty(), "{context}: a thrown error publishes no stored memory");
            }
            Ok(output) => {
                let returned = &outcome["returned"];
                let chats = J::Array(output.chats.iter().map(|chat| pack_message(chat, &b)).collect());
                assert!(same_js_value(&chats, &returned["chats"]), "{context}: chats {chats} != {}", returned["chats"]);
                assert!(same_js_value(&pack_number(output.current_tokens), &returned["currentTokens"]), "{context}: tokens {}", output.current_tokens);
                assert_eq!(output.error.as_ref().map(pack_text).as_ref(), returned.get("error"), "{context}: error");
                match returned.get("memory") {
                    None => assert!(output.effects.is_empty(), "{context}: no memory returned"),
                    Some(memory) => {
                        let [Effect::ChatField { field, value }] = output.effects.as_slice() else { panic!("{context}: one stored-memory effect") };
                        assert_eq!(field.as_unicode(), Some("hypaV3Data"));
                        assert!(same_js_value(&pack(value), memory), "{context}: memory {} != {memory}", pack(value));
                        let persisted = json::stringify(value, &b).unwrap().unwrap();
                        assert_eq!(persisted.as_unicode(), observed["memoryJSON"].as_str(), "{context}: persisted JSON and key order");
                        if output.error.is_none() { previous = Some(persisted); }
                    }
                }
            }
        }
    }
}

#[test]
fn real_typescript_hypa_v3_policy_matches_immediate_and_suspended_native_policy() {
    let bytes = include_bytes!("../../../../tests/fixtures/rust-core/memory/hypa-v3/policy.cases.json");
    let expected: J = serde_json::from_str(include_str!("../../../../tests/fixtures/rust-core/memory/hypa-v3/policy.expected.json")).unwrap();
    assert_eq!(expected["fixtureSha256"], Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect::<String>());
    let b = budget();
    let cases = parse_json(std::str::from_utf8(bytes).unwrap(), &b).unwrap();
    assert_eq!(number(field(&cases, "schema", &b).unwrap()), expected["schema"].as_f64().unwrap());
    let cases = field(&cases, "cases", &b).unwrap().array().unwrap();
    let expected = expected["cases"].as_array().unwrap();
    assert_eq!(cases.len(), expected.len());
    for (case, expected) in cases.iter().zip(expected) {
        let Some(Ir::Text(id)) = field(case, "id", &b) else { panic!("case id") };
        let id = id.as_unicode().unwrap().to_owned();
        assert_eq!(expected["id"], id.as_str());
        for yields in [0, 2] { replay(case, &expected["result"], &id, yields); }
    }
}
