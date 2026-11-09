//! Replays the response oracle (`benchmarks/rust-core-oracles/response.oracle.test.ts`): what `applyGenerationResponse`
//! received and left in the conversation, with the plugin hook tapes it called, through the Rust stage over the real
//! regex runner. Each case runs twice: with the owner draining effects at every host request and without.

mod response_support;

use response_support::*;
use risunest_ir::stage::{HostReply, HostRequest, StageError};
use risunest_ir::{Budget, Value};
use risunest_prompt::response::{ResponseOutcome, ResponseStage};
use sha2::{Digest, Sha256};

const APPLICATION_CASES: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/response/application.cases.json");
const APPLICATION: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/response/application.expected.json");
const OLLAMA_CASES: &[u8] = include_bytes!("../../../../tests/fixtures/native-generation/ollama-owner-response.json");
const OLLAMA: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/response/ollama.expected.json");

/// Ollama replies the stage never receives as text: the provider decode hands TS a non-string result, which `trim`
/// rejects, or the adapter throws before the response is applied.
const NOT_STAGE_INPUT: &[&str] = &["array-coercion", "array-shadowed-tostring", "continue-array", "array-marker", "number-without-thinking"];
/// Failures TS throws that the stage returns as its own error, with the TS message.
const STAGE_FAILURES: &[(&str, &str)] = &[("continue-empty-conversation", "Cannot continue an empty conversation")];

fn digest(bytes: &[u8]) -> String { Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect() }
fn lf(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    for (at, byte) in bytes.iter().enumerate() { if !(*byte == b'\r' && bytes.get(at + 1) == Some(&b'\n')) { output.push(*byte); } }
    output
}

#[derive(Default)]
struct Tally { compared: usize, declined: usize, failed: usize, stale: usize, excluded: Vec<String>, hooks: usize, mismatches: Vec<String> }

fn tape_reply(hook: &Value, budget: &Budget) -> HostReply {
    let reply = field(hook, "reply", budget).unwrap();
    if let Some(Value::Text(value)) = field(reply, "value", budget) { return HostReply::Value(Value::Text(value.copy(budget).unwrap())); }
    if field(reply, "null", budget).is_some() { return HostReply::Value(Value::Null); }
    if field(reply, "undefined", budget).is_some() { return HostReply::Value(Value::Undefined); }
    if let Some(Value::Text(message)) = field(reply, "throw", budget) { return HostReply::Failed { message: message.copy(budget).unwrap() }; }
    panic!("unknown hook reply")
}

fn replay(id: &str, recorded: &Value, decline: Option<&str>, drain: bool, tally: &mut Tally) {
    let budget = budget();
    let mode = if drain { "drained" } else { "undrained" };
    let mut mismatches = Vec::new();
    let mut differ = |what: &str, detail: String| mismatches.push(format!("{id} ({mode}): {what}: {detail}"));
    let Some(entry) = field(recorded, "entry", &budget).filter(|entry| !matches!(entry, Value::Null)) else {
        differ("entry", "TS never applied a response".into());
        tally.mismatches.extend(mismatches);
        return;
    };
    let mut inputs = inputs(entry, &budget);
    let conversation = copy(&inputs.input.conversation, &budget);
    let stages = stages(&mut inputs, None, &budget);
    let mut stage = ResponseStage::new(inputs.input, stages);
    let hooks = values(field(recorded, "hooks", &budget).unwrap());
    let mut unexpected = Vec::new();
    let (owner, result) = drive(&mut stage, conversation, drain, &budget, |index, request, budget| match (request, hooks.get(index)) {
        (HostRequest::PluginHook { .. }, Some(hook)) => tape_reply(hook, budget),
        (request, _) => { unexpected.push(format!("{request:?}")); HostReply::Unsupported("unexpected".into()) }
    });
    for request in unexpected { differ("request", request); }

    if let Some(name) = decline {
        tally.declined += 1;
        let declined = matches!(&result, Err(StageError::Unsupported(declined)) if declined == name);
        if !declined { differ("decline", format!("{name} expected, got {result:?}")); }
        if !owner.served.is_empty() || !owner.effects.is_empty() {
            differ("decline", format!("after {} host requests and {} effects", owner.served.len(), owner.effects.len()));
        }
        tally.mismatches.extend(mismatches);
        return;
    }

    tally.hooks += hooks.len();
    let tape: Vec<String> = hooks.iter().map(|hook| match field(hook, "data", &budget) { Some(Value::Text(data)) => string(data), _ => panic!("hook data") }).collect();
    let served: Vec<String> = owner.served.iter().map(|served| served.data.clone()).collect();
    if served != tape { differ("hooks", format!("{served:?} != {tape:?}")); }

    let exit = field(recorded, "exit", &budget).unwrap();
    let stored = json(field(exit, "conversation", &budget).unwrap(), &budget);
    let replayed = json(&owner.conversation, &budget);
    if replayed != stored { differ("conversation", format!("\n  rust {replayed}\n  ts   {stored}")); }
    if json(stage.conversation(), &budget) != replayed { differ("stage conversation", "differs from the delivered effects".into()); }

    match (field(exit, "error", &budget), field(exit, "application", &budget), &result) {
        (Some(Value::Text(message)), _, Err(error)) => {
            tally.failed += 1;
            let message = string(message);
            let matches = match error {
                StageError::Host(units) => String::from_utf16_lossy(units) == message,
                StageError::Invalid(name) => STAGE_FAILURES.iter().any(|(failure, text)| failure == name && *text == message),
                _ => false,
            };
            if !matches { differ("error", format!("{error:?} for {message:?}")); }
        }
        (None, Some(Value::Null), Ok(ResponseOutcome::Stale { .. })) => tally.stale += 1,
        (None, Some(application), Ok(ResponseOutcome::Applied(applied))) if !matches!(application, Value::Null) => {
            let result = match field(application, "result", &budget) { Some(Value::Text(result)) => string(result), _ => panic!("result") };
            if string(&applied.result) != result { differ("result", format!("{:?} != {result:?}", string(&applied.result))); }
            let flag = |key: &str| matches!(field(application, key, &budget), Some(Value::Bool(true)));
            if applied.emotion_changed != flag("emoChanged") { differ("emoChanged", format!("{}", applied.emotion_changed)); }
            if applied.resend_chat != flag("resendChat") { differ("resendChat", format!("{}", applied.resend_chat)); }
            let output = &values(field(&owner.conversation, "message", &budget).unwrap())[applied.output];
            if json(field(output, "chatId", &budget).unwrap(), &budget) != json(field(entry, "generationId", &budget).unwrap(), &budget) {
                differ("output", format!("index {} is not the output message", applied.output));
            }
        }
        (error, application, result) => differ("outcome", format!("{result:?} for error {error:?} application {application:?}")),
    }

    let expected: Option<Vec<(String, String)>> = field(recorded, "emotions", &budget).filter(|value| !matches!(value, Value::Null)).map(|list| {
        values(list).iter().map(|entry| {
            let entry = values(entry);
            assert!(matches!(entry[2], Value::Number(time) if time == NOW));
            match (&entry[0], &entry[1]) { (Value::Text(name), Value::Text(image)) => (string(name), string(image)), _ => panic!("emotion") }
        }).collect()
    });
    if owner.emotions() != expected { differ("emotions", format!("{:?} != {expected:?}", owner.emotions())); }
    tally.compared += 1;
    tally.mismatches.extend(mismatches);
}

fn header(file: &Value, cases: &[u8], budget: &Budget) -> usize {
    let recorded = match field(file, "fixtureSha256", budget) { Some(Value::Text(sha)) => string(sha), _ => panic!("fixtureSha256") };
    assert_eq!(recorded, digest(cases), "the expected file was recorded from other cases");
    assert!(matches!(field(file, "schema", budget), Some(Value::Number(schema)) if *schema == 1.0));
    values(field(file, "cases", budget).unwrap()).len()
}

#[test]
fn replays_the_typescript_application_oracle() {
    let budget = budget();
    assert!(!APPLICATION_CASES.contains(&b'\r'));
    let file = parse(std::str::from_utf8(APPLICATION).unwrap(), &budget);
    let cases = parse(std::str::from_utf8(APPLICATION_CASES).unwrap(), &budget);
    let count = header(&file, APPLICATION_CASES, &budget);
    let definitions = values(field(&cases, "cases", &budget).unwrap());
    assert_eq!(count, definitions.len());
    let mut tally = Tally::default();
    for (recorded, definition) in values(field(&file, "cases", &budget).unwrap()).iter().zip(definitions) {
        let id = match field(recorded, "id", &budget) { Some(Value::Text(id)) => string(id), _ => panic!("id") };
        assert_eq!(Some(id.clone()), field(definition, "id", &budget).map(|id| match id { Value::Text(id) => string(id), _ => panic!() }));
        let decline = field(definition, "decline", &budget).map(|name| match name { Value::Text(name) => string(name), _ => panic!() });
        let result = field(recorded, "result", &budget).unwrap();
        for drain in [false, true] { replay(&id, result, decline.as_deref(), drain, &mut tally); }
    }
    println!("application: {} cases, {} replays compared ({} failed, {} stale, {} declined), {} hook calls",
        count, tally.compared, tally.failed, tally.stale, tally.declined, tally.hooks);
    assert!(tally.mismatches.is_empty(), "{} mismatches:\n{}", tally.mismatches.len(), tally.mismatches.join("\n"));
    assert_eq!(tally.compared + tally.declined, 2 * count);
    assert!(tally.declined >= 2 * 5 && tally.failed >= 2 * 3 && tally.compared >= 2 * 40);
}

#[test]
fn replays_the_typescript_ollama_oracle() {
    let budget = budget();
    let source = lf(OLLAMA_CASES);
    let file = parse(std::str::from_utf8(OLLAMA).unwrap(), &budget);
    let count = header(&file, &source, &budget);
    let mut tally = Tally::default();
    for recorded in values(field(&file, "cases", &budget).unwrap()) {
        let id = match field(recorded, "id", &budget) { Some(Value::Text(id)) => string(id), _ => panic!("id") };
        let result = field(recorded, "result", &budget).unwrap();
        if NOT_STAGE_INPUT.contains(&id.as_str()) {
            // What reaches the stage there is not text, or nothing at all.
            let entry = field(result, "entry", &budget).filter(|entry| !matches!(entry, Value::Null));
            let text = entry.and_then(|entry| field(entry, "response", &budget)).and_then(|response| field(response, "result", &budget));
            assert!(!matches!(text, Some(Value::Text(_))), "{id} reaches the stage as text");
            tally.excluded.push(id);
            continue;
        }
        for drain in [false, true] { replay(&id, result, None, drain, &mut tally); }
    }
    println!("ollama: {} cases, {} replays compared, {} excluded ({})", count, tally.compared, tally.excluded.len(), tally.excluded.join(", "));
    assert!(tally.mismatches.is_empty(), "{} mismatches:\n{}", tally.mismatches.len(), tally.mismatches.join("\n"));
    assert_eq!(tally.excluded.len(), NOT_STAGE_INPUT.len());
    assert_eq!(tally.compared, 2 * (count - NOT_STAGE_INPUT.len()));
}
