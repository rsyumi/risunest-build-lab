//! Replays the actual TS SupaMemory and legacy Hypa observations through the public stage, immediately and with every
//! dependency suspended. The replay feeds back the recorded tokenizer counts and retrieval results, and answers the
//! product summary adapter's request with the recorded auxiliary-model or completions reply after checking it is the
//! request TS sent; it does not replay trimming, probing, chunking, merge or record decisions.
use risunest_ir::chat::{MessageRole, PromptMessage};
use risunest_ir::provider::{FailureCategory, HttpMethod, OutputShape, ProviderFailure, ProviderOutput};
use risunest_ir::stage::{Effect, EmbedRequest, HostReply, HostRequest, MemoryInput, MemoryOutput, MemoryStage, StageError, Step, TextContext, TextOutput, TextProcessor, TokenCounter, TokenInput};
use risunest_ir::{parse_json, Budget, Limits, OwnedVec, Text, Value as Ir};
use risunest_memory::hanurai::{Retrieval, RetrievalOutput, ScoredDocument};
use risunest_memory::summary::Summary;
use risunest_memory::supa::{SupaMemory, SupaMemoryConfig};
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
        (J::Object(a), J::Object(b)) => a.len() == b.len() && a.iter().zip(b).all(|((ka, va), (kb, vb))| ka == kb && same_js_value(va, vb)),
        _ => left == right,
    }
}
fn host(failure: &J, b: &Budget) -> StageError {
    assert_eq!(failure["name"], "Error", "a host failure surfaces as an Error");
    StageError::Host(Text::from_str(failure["message"].as_str().unwrap(), b).unwrap().units().collect())
}
fn field<'a>(value: &'a Ir, key: &str, b: &Budget) -> Option<&'a Ir> { value.get(key.as_bytes(), b).unwrap() }
// A fixture string, with lone surrogates carried by the `$utf16` tag.
fn tagged_text(value: &J, b: &Budget) -> Text {
    match value.get("$utf16") {
        Some(units) => Text::from_units(&units.as_array().unwrap().iter().map(|unit| unit.as_u64().unwrap() as u16).collect::<Vec<_>>(), b).unwrap(),
        None => Text::from_str(value.as_str().unwrap(), b).unwrap(),
    }
}
fn number(value: &Ir) -> f64 { match value { Ir::Number(n) => *n, _ => panic!("number") } }
fn text(value: &Ir, b: &Budget) -> Text { match value { Ir::Text(text) => text.copy(b).unwrap(), _ => panic!("text") } }

type Tape = Arc<Mutex<VecDeque<J>>>;
fn next(tape: &Tape, kind: &str) -> J {
    let event = tape.lock().unwrap().pop_front().unwrap_or_else(|| panic!("unrecorded {kind} call"));
    assert_eq!(event["kind"], kind, "dependency order changed");
    event
}
fn leaf_request(kind: &str, b: &Budget) -> HostRequest {
    match kind {
        "tokenizer" => HostRequest::PluginTokenizer { text: Text::from_str("replay-tokenizer-leaf", b).unwrap() },
        _ => HostRequest::Embed(EmbedRequest { model: Text::from_str("replay-retrieval-leaf", b).unwrap(), texts: Vec::new() }),
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

/// `tokenizer.tokenizeChat` for messages and the module `tokenize` for text, in one recorded order.
struct Tokens { tape: Tape, held: Yielding<u32> }
impl TokenCounter for Tokens {
    fn count(&mut self, input: TokenInput<'_>, b: &Budget) -> Result<Step<u32>, StageError> {
        let event = match input {
            TokenInput::Message(message) => {
                let event = next(&self.tape, "tokenizeChat");
                assert!(same_js_value(&pack_message(message, b), &event["message"]), "counted {} where TS counted {}", pack_message(message, b), event["message"]);
                event
            }
            TokenInput::Text(value) => {
                let event = next(&self.tape, "tokenize");
                assert_eq!(pack_text(value), event["text"], "tokenized text");
                event
            }
            TokenInput::Messages(_) => panic!("SupaMemory counts one message or one text at a time"),
        };
        let result = match event.get("failure") { Some(failure) => Err(host(failure, b)), None => Ok(event["count"].as_u64().unwrap() as u32) };
        self.held.deliver(result, b)
    }
    fn resume(&mut self, reply: HostReply, b: &Budget) -> Result<Step<u32>, StageError> {
        assert!(matches!(reply, HostReply::Tokens(_)), "tokenizer received another dependency's reply");
        self.held.resume(b)
    }
}

/// `parseChatML`'s CBS pass over fixture text without CBS syntax (`hasCbsSyntax`), which `risuChatParser` returns as is.
struct Plain;
impl TextProcessor for Plain {
    fn process(&mut self, input: &Text, _: &TextContext, b: &Budget) -> Result<TextOutput, StageError> {
        let value = String::from_utf16_lossy(&input.units().collect::<Vec<_>>()).to_lowercase();
        assert!(!["{{", "{#", "<user>", "<char>", "<bot>"].iter().any(|syntax| value.contains(syntax)), "fixture text with CBS syntax: {value}");
        Ok(TextOutput { text: input.copy(b)?, effects: Vec::new() })
    }
    fn apply(&mut self, _: &Effect, _: &Budget) -> Result<(), StageError> { Ok(()) }
}

// Objects compare by key, since the request carries the message fields in IR order.
fn same_members(left: &J, right: &J) -> bool {
    match (left, right) {
        (J::Array(a), J::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_members(a, b)),
        (J::Object(a), J::Object(b)) => a.len() == b.len() && a.iter().all(|(key, value)| b.get(key).is_some_and(|other| same_members(value, other))),
        _ => same_js_value(left, right),
    }
}
/// What `requestChatData` returned, as the owner delivers it.
fn llm_reply(reply: &J, b: &Budget) -> HostReply {
    let output = |texts: Vec<Text>, shape| HostReply::Llm(Ok(ProviderOutput { texts, shape, thoughts: Vec::new(), tool_calls: Vec::new(), usage: None, finish: None }));
    match reply["type"].as_str().unwrap() {
        "success" => output(vec![tagged_text(&reply["result"], b)], OutputShape::Text),
        "fail" => HostReply::Llm(Err(ProviderFailure { category: FailureCategory::Http, stage: None, http_status: None,
            sdk_message: Some(tagged_text(&reply["result"], b)), body_failure: None })),
        "streaming" => output(vec![Text::from_str("", b).unwrap()], OutputShape::Streamed),
        "multiline" => {
            let pairs = reply["result"].as_array().unwrap();
            output(pairs.iter().map(|pair| tagged_text(&pair[1], b)).collect(),
                OutputShape::Multiline(pairs.iter().map(|pair| if pair[0] == "user" { MessageRole::User } else { MessageRole::Char }).collect()))
        }
        other => panic!("reply type {other}"),
    }
}
/// Checks the adapter's request against the next recorded `requestChatData` or `globalFetch` call and answers it with
/// what TS received.
fn summary_reply(request: &HostRequest, tape: &Tape, b: &Budget) -> HostReply {
    match request {
        HostRequest::Llm(sub) => {
            let event = next(tape, "summary");
            assert_eq!(sub.mode.as_unicode(), event["model"].as_str(), "mode");
            let messages = J::Array(sub.messages.iter().map(|message| pack_message(message, b)).collect());
            assert!(same_members(&messages, &event["formated"]), "messages {messages} != {}", event["formated"]);
            assert_eq!(pack(&sub.options), event["options"], "options");
            assert_eq!(event["signal"], "absent", "SupaMemory passes no signal");
            match event.get("failure") {
                Some(failure) => {
                    assert_eq!(failure["name"], "Error", "a host failure surfaces as an Error");
                    HostReply::Failed { message: Text::from_str(failure["message"].as_str().unwrap(), b).unwrap() }
                }
                None => llm_reply(&event["reply"], b),
            }
        }
        HostRequest::Http(spec) => {
            let event = next(tape, "fetch");
            assert_eq!((spec.method, event["method"].as_str()), (HttpMethod::Post, Some("POST")));
            assert_eq!(Some(spec.url.as_str()), event["url"].as_str(), "url");
            // The row holds the values `globalFetch` received; the plugin's `new Headers` strips HTTP whitespace from each.
            let recorded: Vec<J> = event["headers"].as_array().unwrap().iter()
                .map(|pair| j!([pair[0], pair[1].as_str().unwrap().trim_matches(&['\t', '\n', '\r', ' '][..])])).collect();
            assert_eq!(J::Array(spec.headers.iter().map(|(name, value)| j!([name, value])).collect()), J::Array(recorded), "headers");
            assert_eq!(std::str::from_utf8(&spec.body).ok(), event["body"].as_str(), "body");
            assert_eq!(event["other"], j!([]), "no other globalFetch argument");
            let reply = &event["reply"];
            match reply.get("transport") {
                Some(message) => HostReply::Failed { message: Text::from_str(message.as_str().unwrap(), b).unwrap() },
                None => HostReply::Http { status: reply["status"].as_u64().unwrap() as u16, body: reply["body"].as_str().unwrap().as_bytes().to_vec() },
            }
        }
        _ => panic!("not a summary request"),
    }
}

/// `HypaProcesser.addText` and `similaritySearch` over the recorded documents and ranked results.
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
            // `similaritySearch` returns ranked contents without their scores.
            None => Ok(RetrievalOutput::Scored(event["result"].as_array().unwrap().iter()
                .map(|document| ScoredDocument { document: Some(tagged_text(document, b)), score: 0.0 }).collect())),
        };
        self.held.deliver(result, b)
    }
    fn resume(&mut self, reply: HostReply, b: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        assert!(matches!(reply, HostReply::Embeddings(_)), "retrieval received another dependency's reply");
        self.held.resume(b)
    }
}

fn complete<M: MemoryStage>(stage: &mut M, mut step: Result<Step<MemoryOutput>, StageError>, tape: &Tape, b: &Budget) -> Result<MemoryOutput, StageError> {
    loop {
        step = match step? {
            Step::Done(output) => return Ok(output),
            Step::NeedsHost(request) => stage.resume(match request {
                HostRequest::PluginTokenizer { .. } => HostReply::Tokens(Vec::new()),
                HostRequest::Embed(_) => HostReply::Embeddings(Vec::new()),
                HostRequest::Llm(_) | HostRequest::Http(_) => summary_reply(&request, tape, b),
                _ => panic!("unexpected replay host kind"),
            }, b),
        };
    }
}

/// The summary function under the oracle's database defaults and the case's settings.
fn summary(input: &Ir, b: &Budget) -> Summary<Plain> {
    let settings = field(input, "settings", b).unwrap();
    let setting = |key: &str, default: &str| field(settings, key, b).map_or_else(|| Text::from_str(default, b).unwrap(), |value| text(value, b));
    Summary::supa(&setting("supaModelType", "subModel"), &setting("supaMemoryPrompt", ""), Some(&setting("supaMemoryKey", "")), Plain, b).unwrap()
}

/// The oracle's database defaults under the case's settings.
fn config(input: &Ir, as_hyper: bool, b: &Budget) -> SupaMemoryConfig {
    let settings = field(input, "settings", b).unwrap();
    SupaMemoryConfig {
        as_hyper,
        remove_punctuation: matches!(field(settings, "removePunctuationHypa", b), Some(Ir::Bool(true))),
        max_chunk_size: field(settings, "maxSupaChunkSize", b).map_or(1200.0, number),
        char_name: text(field(input, "charName", b).unwrap(), b),
        user_name: field(settings, "username", b).map_or_else(|| Text::from_str("User", b).unwrap(), |name| text(name, b)),
    }
}

const KINDS: [&str; 6] = ["tokenizeChat", "tokenize", "summary", "fetch", "prepare", "query"];
/// Embedding and cache work inside `HypaProcesser`, which the retrieval dependency owns.
const RETRIEVAL_INTERNAL: [&str; 3] = ["local-embedding", "cache-read", "cache-write"];

struct Set { scope: &'static str, as_hyper: bool }
const SUPA: Set = Set { scope: "actual-supamemory-submodel-policy", as_hyper: false };
const LEGACY: Set = Set { scope: "actual-legacy-hypa-submodel-minilm-policy", as_hyper: true };

fn replay(case: &Ir, expected: &J, id: &str, yields: u8, set: &Set) {
    let b = budget();
    assert_eq!(expected["scope"], set.scope);
    let input = field(case, "input", &b).unwrap();
    let mut previous: Option<Text> = None;
    let invocations = field(input, "invocations", &b).unwrap().array().unwrap();
    for (index, invocation) in invocations.iter().enumerate() {
        let observed = &expected["invocations"][index];
        let context = format!("{id} invocation {index} yields={yields}");
        assert!(same_js_value(&observed["roomBefore"], &observed["roomAfter"]), "{context}: TS changed the room record");
        let stored = if field(invocation, "persistPrevious", &b).is_some() {
            Some(Ir::Text(previous.take().expect("persisted memory")))
        } else {
            let mut room = unpack(field(input, "room", &b).unwrap(), &b);
            room.take(b"supaMemoryData", &b).unwrap()
        };
        let chats = unpack(field(input, "chats", &b).unwrap(), &b);
        let chats: Vec<PromptMessage> = match chats { Ir::Array(mut values) => values.as_mut_slice().iter_mut()
            .map(|value| PromptMessage::from_value(std::mem::replace(value, Ir::Undefined), &b).unwrap()).collect(), _ => panic!() };
        let events = observed["events"].as_array().unwrap();
        for event in events {
            let kind = event["kind"].as_str().unwrap();
            assert!(KINDS.contains(&kind) || set.as_hyper && RETRIEVAL_INTERNAL.contains(&kind), "{context}: unreplayed event {kind}");
        }
        let tape: Tape = Arc::new(Mutex::new(events.iter().filter(|event| KINDS.contains(&event["kind"].as_str().unwrap())).cloned().collect()));
        let memory_input = MemoryInput { chats, current_tokens: number(&unpack(field(invocation, "currentTokens", &b).unwrap(), &b)),
            max_context_tokens: number(&unpack(field(invocation, "maxContextTokens", &b).unwrap(), &b)) };
        let mut stage = SupaMemory::new(config(input, set.as_hyper, &b), stored, Tokens { tape: tape.clone(), held: Yielding::new(yields, "tokenizer") },
            Search { tape: tape.clone(), held: Yielding::new(yields, "retrieval") }, summary(input, &b));
        let step = stage.run(memory_input, &b);
        let result = complete(&mut stage, step, &tape, &b);
        assert!(matches!(stage.resume(HostReply::Cancelled, &b), Err(StageError::Invalid(_))), "{context}: finished stage resumed");
        assert!(tape.lock().unwrap().is_empty(), "{context}: unconsumed observations {:?}", tape.lock().unwrap().front());
        let outcome = &observed["outcome"];
        match result {
            Err(StageError::Host(message)) => assert_eq!(String::from_utf16(&message).unwrap(), outcome["thrown"]["message"], "{context}"),
            Err(error) => panic!("{context}: {error:?}"),
            Ok(output) => {
                let returned = &outcome["returned"];
                assert!(returned.is_object(), "{context}: TS threw {}", outcome["thrown"]);
                let chats = J::Array(output.chats.iter().map(|chat| pack_message(chat, &b)).collect());
                assert!(same_js_value(&chats, &returned["chats"]), "{context}: chats {chats} != {}", returned["chats"]);
                assert!(same_js_value(&pack_number(output.current_tokens), &returned["currentTokens"]), "{context}: tokens {}", output.current_tokens);
                assert_eq!(output.error.as_ref().map(pack_text).as_ref(), returned.get("error"), "{context}: error");
                // The caller stores `memory ?? supaMemoryData` and `lastId ?? lastMemory`, and nothing when an error is returned.
                let mut fields = Vec::new();
                if output.error.is_none() {
                    if let Some(memory) = returned.get("memory") { fields.push(("supaMemoryData", memory.clone())); }
                    if let Some(last) = returned.get("lastId").filter(|last| last.is_string()) { fields.push(("lastMemory", last.clone())); }
                }
                let effects: Vec<(String, J)> = output.effects.iter().map(|effect| match effect {
                    Effect::ChatField { field, value } => (field.as_unicode().unwrap().to_owned(), pack(value)),
                    other => panic!("{context}: effect {other:?}"),
                }).collect();
                assert_eq!(effects.len(), fields.len(), "{context}: effects {effects:?}");
                for ((field, value), (name, memory)) in effects.iter().zip(&fields) {
                    assert_eq!(field, name, "{context}: effect order");
                    assert!(same_js_value(value, memory), "{context}: {name} {value} != {memory}");
                }
                if let Some(Effect::ChatField { value: Ir::Text(memory), .. }) = output.effects.first() { previous = Some(memory.copy(&b).unwrap()); }
            }
        }
    }
}

fn replay_all(bytes: &[u8], expected: &str, set: &Set) {
    let expected: J = serde_json::from_str(expected).unwrap();
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
        for yields in [0, 2] { replay(case, &expected["result"], &id, yields, set); }
    }
}

#[test]
fn real_typescript_supa_memory_policy_matches_immediate_and_suspended_native_policy() {
    replay_all(include_bytes!("../../../../tests/fixtures/rust-core/memory/supa/policy.cases.json"),
        include_str!("../../../../tests/fixtures/rust-core/memory/supa/policy.expected.json"), &SUPA);
}

#[test]
fn real_typescript_legacy_hypa_policy_matches_immediate_and_suspended_native_policy() {
    replay_all(include_bytes!("../../../../tests/fixtures/rust-core/memory/hypa-legacy/policy.cases.json"),
        include_str!("../../../../tests/fixtures/rust-core/memory/hypa-legacy/policy.expected.json"), &LEGACY);
}
