//! Replays the committed TypeScript observations (`tests/fixtures/rust-core/providers/ooba`) of `ooba` and `mancer`
//! through the native builders and readers. The fixtures are produced by
//! `benchmarks/rust-core-oracles/providers-ooba.oracle.test.ts` from the real `requestChatDataMain`; the rendered chat
//! template and the character and user names are recorded TS inputs.

use super::{completions, legacy, OobaBuffered, OobaInputs, OobaOutcome, OobaSettings};
use crate::common::built::BuiltRequest;
use crate::common::js::tests::budget;
use crate::common::owner::{settle, Progress, Settled};
use crate::common::replay_kit::{parse, same_hash, to_value, Jv};
use crate::common::BuildError;
use crate::text_completion::testkit::{assert_request, common, ids_of, messages, reply, s};
use risunest_ir::chat::PromptMessage;
use risunest_ir::provider::{FailureStage, LlmFormat, ProviderCall, ResponseKind};
use risunest_ir::Budget;

const CASES: [&[u8]; 2] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/ooba/requests.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/ooba/responses.cases.json"),
];
const EXPECTED: [&[u8]; 2] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/ooba/requests.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/ooba/responses.expected.json"),
];

struct Built { request: Result<BuiltRequest, BuildError>, legacy: bool, messages: Vec<PromptMessage> }

fn build(fixture: &Jv, entry: &Jv, result: &Jv, b: &Budget) -> Built {
    let common = common(fixture, entry, result.need("model"), b);
    let messages = messages(entry, &common, b).expect("reformat");
    let db = &common.db;
    let ooba = to_value(db.need("ooba"), b);
    let user_prefix = db.need("ooba").get("formating").and_then(|formating| formating.get("userPrefix")).map(|value| value.text(b));
    let settings = OobaSettings {
        blocking_url: db.text("textgenWebUIBlockingURL", b), ooba, user_prefix, username: db.text("username", b),
        local_stop_strings: db.get("localStopStrings").map(|items| items.list().iter().map(|item| item.text(b)).collect()),
        top_p: db.num("top_p"), presence_penalty: db.num("PresensePenalty"), frequency_penalty: db.num("frequencyPenalty"),
        max_response: db.num("maxResponse"), temperature: db.num("temperature"), ooba_args: to_value(db.need("reverseProxyOobaArgs"), b),
        mancer_header: db.text("mancerHeader", b), additional_params: common.additional_params,
    };
    let prompt = result.need("prompts").list().first().map(|value| value.text(b)).expect("a rendered template");
    let inputs = OobaInputs {
        ai_model: &common.ai_model, prompt: &prompt, max_tokens: entry.get("maxTokens").map(Jv::num).unwrap_or_else(|| db.num("maxResponse")),
        temperature: db.num("temperature") / 100.0, presence_penalty: None, frequency_penalty: None, settings: &settings,
    };
    let stream = entry.get("stream").is_some_and(Jv::flag);
    let legacy = common.descriptor.format == LlmFormat::OobaLegacy;
    let call = ProviderCall {
        model: common.descriptor, messages: Vec::new(), sampling: Vec::new(), bias: Vec::new(), tools: Vec::new(), schema: None, stream, mode: common.mode,
    };
    let request = if legacy { legacy::build_request(&call, &inputs, b) } else { completions::build_request(&call, &inputs, b) };
    Built { request, legacy, messages }
}

fn replay(set: usize, check_reply: bool) -> (usize, usize, usize) {
    let b = budget();
    let fixture = parse(CASES[set], &b);
    let expected = parse(EXPECTED[set], &b);
    same_hash(CASES[set], &expected);
    let cases = fixture.need("cases").list();
    let records = expected.need("cases").list();
    assert_eq!(cases.len(), records.len());
    let (mut sent, mut declined, mut replies) = (0, 0, 0);
    for (entry, record) in cases.iter().zip(records) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let result = record.need("result");
        let built = build(&fixture, entry, result, &b);
        let outcome = result.need("outcome");
        let decline = entry.get("declines").map(Jv::string);
        let request = match built.request {
            Err(BuildError::Unsupported(found)) => { assert_eq!(Some(found.to_owned()), decline, "{id}: declined `{found}`"); declined += 1; continue; }
            Err(other) => panic!("{id}: {other:?}"),
            Ok(request) => request,
        };
        let requests = result.need("requests").list();
        assert_eq!(requests.len(), 1, "{id}");
        assert_request(&id, &request.spec, &requests[0]);
        assert_eq!(request.spec.response, ResponseKind::Buffered, "{id}");
        assert_eq!(s(&request.preview(&b).unwrap()), result.need("preview").need("result").string(), "{id}: preview");
        sent += 1;
        if !check_reply { assert!(decline.is_none(), "{id}: expected the decline"); continue; }
        let scripted = reply(&entry.need("replies").list()[0], &b);
        let http_error = result.need("httpError").text(&b);
        let names = result.need("names");
        let (char_name, user_name) = (names.need("char").text(&b), names.need("user").text(&b));
        let read: Result<OobaBuffered, BuildError> = if built.legacy {
            let context = legacy::ReplyContext { http_error: &http_error, formated: &built.messages, char_name: &char_name, user_name: &user_name };
            legacy::read_buffered(&scripted.borrow(), &context, &b)
        } else {
            completions::read_buffered(&scripted.borrow(), &http_error, &b)
        };
        if let Some(name) = decline {
            let error = read.expect_err(&format!("{id}: expected the decline `{name}`"));
            assert!(matches!(&error, BuildError::Unsupported(found) if *found == name), "{id}: {error:?}");
            assert!(matches!(settle(error, Progress::default(), FailureStage::BufferedJson), Settled::FallBack(found) if found == name), "{id}");
            declined += 1;
            continue;
        }
        let read = read.unwrap_or_else(|error| panic!("{id}: {error:?}"));
        match (&read.result, outcome.need("type").string().as_str()) {
            (OobaOutcome::Success(text), "success") => { assert_eq!(s(text), outcome.need("result").string(), "{id}"); assert!(read.failure.is_none(), "{id}"); }
            (OobaOutcome::Fail(text), "fail") => { assert_eq!(s(text), outcome.need("result").string(), "{id}"); assert!(read.failure.is_some(), "{id}"); }
            (found, kind) => panic!("{id}: native {found:?}, TypeScript {kind}"),
        }
        replies += 1;
    }
    (sent, declined, replies)
}

#[test]
fn request_bytes_match_the_typescript_requests() {
    let b = budget();
    let fixture = parse(CASES[0], &b);
    assert_eq!(ids_of(fixture.need("cases").list(), "declines"), [
        "ooba-local-stop-strings-declines", "ooba-url-invalid-declines", "ooba-url-userinfo-declines", "legacy-local-stop-strings-declines",
        "legacy-url-invalid-declines", "legacy-stream-declines",
    ]);
    assert_eq!(replay(0, false), (53, 6, 0));
}

#[test]
fn buffered_replies_match_the_typescript_outcomes() {
    let b = budget();
    let fixture = parse(CASES[1], &b);
    assert_eq!(ids_of(fixture.need("cases").list(), "declines"), [
        "ooba-text-number-declines", "ooba-choices-missing-throws", "legacy-text-number-declines", "legacy-results-missing-declines", "legacy-results-empty-declines",
    ]);
    assert_eq!(replay(1, true), (26, 5, 21));
}

