//! Replays only the Hanurai policy boundary observed from real TS. Neither dependency below implements retrieval
//! or tokenizer accounting; the actual TS oracle records those observations separately from this replay.
use risunest_ir::chat::PromptMessage;
use risunest_ir::stage::{EmbedRequest, HostReply, HostRequest, MemoryInput, MemoryStage, StageError, Step, TokenCounter, TokenInput};
use risunest_ir::{json, Budget, Limits, Text};
use risunest_memory::hanurai::{Hanurai, HanuraiConfig, Retrieval, RetrievalOutput, ScoredDocument};
use serde_json::{json as j, Value};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::sync::{atomic::AtomicBool, Arc, Mutex};

fn budget() -> Budget {
    Budget::new(Limits { max_owned_bytes: 32 << 20, max_json_bytes: 16 << 20, max_text_units: 4 << 20,
        max_values: 1_000_000, max_depth: 128, max_work: 100_000_000 }, Arc::new(AtomicBool::new(false))).unwrap()
}
fn text(value: &Value, budget: &Budget) -> Text { Text::from_str(value.as_str().unwrap(), budget).unwrap() }
fn message(value: &Value, budget: &Budget) -> PromptMessage {
    PromptMessage::from_value(json::parse_json(&value.to_string(), budget).unwrap(), budget).unwrap()
}
fn value(message: &PromptMessage, budget: &Budget) -> Value {
    let encoded = json::stringify(&message.to_value(budget).unwrap(), budget).unwrap().unwrap();
    serde_json::from_str(encoded.as_unicode().unwrap()).unwrap()
}
fn number(value: &Value) -> f64 {
    match value.as_str() { Some("NaN") => f64::NAN, Some("Infinity") => f64::INFINITY,
        Some("-Infinity") => f64::NEG_INFINITY, _ => value.as_f64().unwrap() }
}
fn failure(event: &Value, budget: &Budget) -> Option<StageError> {
    event.get("failure").map(|failure| StageError::Host(text(&failure["message"], budget).units().collect()))
}

type Tape = Arc<Mutex<VecDeque<Value>>>;
fn next(tape: &Tape, kind: &str) -> Value {
    let event = tape.lock().unwrap().pop_front().expect("unrecorded policy dependency call");
    assert_eq!(event["kind"], kind, "dependency order changed");
    event
}

struct ReplayRetrieval {
    tape: Tape,
    suspend: bool,
    pending: Option<Result<RetrievalOutput, StageError>>,
    second_yield: bool,
}
impl ReplayRetrieval {
    fn deliver(&mut self, output: Result<RetrievalOutput, StageError>, texts: Vec<Text>, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        if self.suspend {
            self.pending = Some(output);
            self.second_yield = true;
            Ok(Step::NeedsHost(HostRequest::Embed(EmbedRequest { model: Text::from_str("policy-replay", budget)?, texts })))
        } else { output.map(Step::Done) }
    }
}
impl Retrieval for ReplayRetrieval {
    fn prepare(&mut self, documents: Vec<Text>, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        let event = next(&self.tape, "prepare");
        assert_eq!(j!(documents.iter().map(|text| text.as_unicode().unwrap()).collect::<Vec<_>>()), event["documents"]);
        let output = failure(&event, budget).map_or(Ok(RetrievalOutput::Prepared), Err);
        self.deliver(output, documents, budget)
    }
    fn query(&mut self, query: Text, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        let event = next(&self.tape, "query");
        assert_eq!(query.as_unicode().unwrap(), event["query"].as_str().unwrap());
        let output = if let Some(error) = failure(&event, budget) { Err(error) } else {
            Ok(RetrievalOutput::Scored(event["results"].as_array().unwrap().iter().map(|result|
                ScoredDocument { document: Some(text(&result[0], budget)), score: number(&result[1]) }).collect()))
        };
        self.deliver(output, vec![query], budget)
    }
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        assert!(matches!(reply, HostReply::Embeddings(_)), "retrieval received another dependency's reply");
        if self.second_yield {
            self.second_yield = false;
            return Ok(Step::NeedsHost(HostRequest::Embed(EmbedRequest { model: Text::from_str("policy-replay-followup", budget)?, texts: Vec::new() })));
        }
        self.pending.take().expect("pending retrieval").map(Step::Done)
    }
}

struct ReplayTokens { tape: Tape, suspend: bool, pending: Option<Result<u32, StageError>>, second_yield: bool }
impl TokenCounter for ReplayTokens {
    fn count(&mut self, input: TokenInput<'_>, budget: &Budget) -> Result<Step<u32>, StageError> {
        let TokenInput::Message(message) = input else { panic!("Hanurai counts complete messages") };
        let event = next(&self.tape, "token");
        assert_eq!(value(message, budget), event["message"], "message metadata or candidate changed");
        let result = if let Some(error) = failure(&event, budget) { Err(error) }
            else { Ok(u32::try_from(event["count"].as_u64().unwrap()).unwrap()) };
        if self.suspend {
            self.pending = Some(result);
            self.second_yield = true;
            Ok(Step::NeedsHost(HostRequest::PluginTokenizer { text: message.content.copy(budget)? }))
        } else { result.map(Step::Done) }
    }
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<u32>, StageError> {
        assert!(matches!(reply, HostReply::Tokens(_)), "tokenizer received another dependency's reply");
        if self.second_yield {
            self.second_yield = false;
            return Ok(Step::NeedsHost(HostRequest::PluginTokenizer { text: Text::from_str("policy-replay-followup", budget)? }));
        }
        self.pending.take().expect("pending token count").map(Step::Done)
    }
}

#[test]
fn real_typescript_policy_observations_match_immediate_and_resumed_native_policy() {
    let cases_bytes = include_bytes!("../../../../tests/fixtures/rust-core/memory/hanurai/policy.cases.json");
    let cases: Value = serde_json::from_slice(cases_bytes).unwrap();
    let expected: Value = serde_json::from_str(include_str!("../../../../tests/fixtures/rust-core/memory/hanurai/policy.expected.json")).unwrap();
    assert_eq!(cases["schema"], expected["schema"]);
    assert_eq!(expected["fixtureSha256"], Sha256::digest(cases_bytes).iter().map(|byte| format!("{byte:02x}")).collect::<String>());
    let cases = cases["cases"].as_array().unwrap();
    let expected = expected["cases"].as_array().unwrap();
    assert_eq!(cases.len(), expected.len());
    for (case, expected) in cases.iter().zip(expected) {
        assert_eq!(case["id"], expected["id"]);
        for suspend in [false, true] {
            replay(case, &expected["result"], suspend);
        }
    }
}

fn replay(case: &Value, expected: &Value, suspend: bool) {
    let budget = budget();
    assert_eq!(expected["boundary"], "hanurai-policy-replay");
    let tape = Arc::new(Mutex::new(expected["events"].as_array().unwrap().iter().cloned().collect()));
    let input = &case["input"];
    let config = HanuraiConfig { split: input["split"].as_bool().unwrap(), reserve_tokens: number(&input["reserve"]),
        context_limit_error: text(&expected["contextLimitError"], &budget) };
    let tokens = ReplayTokens { tape: Arc::clone(&tape), suspend, pending: None, second_yield: false };
    let retrieval = ReplayRetrieval { tape: Arc::clone(&tape), suspend, pending: None, second_yield: false };
    let mut stage = Hanurai::new(config, tokens, retrieval);
    let mut step = stage.run(MemoryInput {
        chats: input["chats"].as_array().unwrap().iter().map(|chat| message(chat, &budget)).collect(),
        current_tokens: number(&input["currentTokens"]), max_context_tokens: number(&input["maxContextTokens"]),
    }, &budget);
    loop {
        step = match step {
            Ok(Step::NeedsHost(request)) => stage.resume(match request {
                HostRequest::Embed(_) => HostReply::Embeddings(Vec::new()),
                HostRequest::PluginTokenizer { .. } => HostReply::Tokens(Vec::new()),
                _ => panic!("unexpected replay host kind"),
            }, &budget),
            Ok(Step::Done(output)) => {
                assert!(output.effects.is_empty(), "Hanurai has no stored-memory effects");
                let chats = Value::Array(output.chats.iter().map(|chat| value(chat, &budget)).collect());
                assert_eq!(chats, expected["outcome"]["chats"], "case {} suspend={suspend}", case["id"]);
                match output.error {
                    Some(error) => assert_eq!(error.as_unicode().unwrap(), expected["outcome"]["error"], "case {}", case["id"]),
                    None => {
                        assert!(expected["outcome"].get("error").is_none(), "case {}", case["id"]);
                        assert_eq!(output.current_tokens, number(&expected["outcome"]["tokens"]), "case {} suspend={suspend}", case["id"]);
                    }
                }
                break;
            }
            Err(StageError::Host(message)) => {
                assert_eq!(String::from_utf16(&message).unwrap(), expected["outcome"]["failure"]["message"], "case {}", case["id"]);
                break;
            }
            Err(error) => panic!("case {} suspend={suspend}: {error:?}", case["id"]),
        };
    }
    assert!(tape.lock().unwrap().is_empty(), "unconsumed observations for {}", case["id"]);
    assert!(matches!(stage.resume(HostReply::Cancelled, &budget), Err(StageError::Invalid(_))), "completed/failed policy cannot resume");
}
