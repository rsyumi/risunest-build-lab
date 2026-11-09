//! Replays the committed TypeScript observations (`tests/fixtures/rust-core/providers/novelai`) through the native
//! builder and reader. The fixtures are produced by `benchmarks/rust-core-oracles/providers-novelai.oracle.test.ts`
//! from the real `requestChatDataMain`; the bias token sequences come from the case token table and the character and
//! user names are recorded TS inputs.

use super::{build_request, read_buffered, NovelAiInputs, NovelAiOutcome, NovelAiSettings, ReplyContext, PREVIEW_RESULT};
use crate::common::js::tests::budget;
use crate::common::owner::{settle, Progress, Settled};
use crate::common::replay_kit::{parse, same_hash, to_value, Jv};
use crate::common::BuildError;
use crate::text_completion::testkit::{assert_request, common, ids_of, messages, reply, s};
use risunest_ir::provider::{FailureStage, ProviderCall, ResponseKind};

const CASES: [&[u8]; 2] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/novelai/requests.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/novelai/responses.cases.json"),
];
const EXPECTED: [&[u8]; 2] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/novelai/requests.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/novelai/responses.expected.json"),
];

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
        let common = common(&fixture, entry, result.need("model"), &b);
        let formated = messages(entry, &common, &b).expect("reformat");
        let db = &common.db;
        let settings = NovelAiSettings {
            token: db.need("novelai").need("token").text(&b), nai_settings: to_value(db.need("NAIsettings"), &b),
            adventure: db.flag("NAIadventure"), append_name: db.flag("NAIappendName"), additional_params: common.additional_params,
        };
        let names = result.need("names");
        let (char_name, user_name) = (names.need("char").text(&b), names.need("user").text(&b));
        let bias: Vec<_> = entry.get("biasString").map(|items| items.list().iter().map(|pair| (pair.list()[0].text(&b), pair.list()[1].num())).collect()).unwrap_or_default();
        let tokens = entry.get("tokens");
        let bias_tokens: Vec<Vec<f64>> = bias.iter().map(|(text, _)| {
            tokens.and_then(|table| table.get(&s(text))).map(|list| list.list().iter().map(Jv::num).collect()).unwrap_or_default()
        }).collect();
        let key = if common.ai_model.equals_ascii(b"reverse_proxy") { Some(db.text("proxyKey", &b)) } else {
            entry.get("customModels").and_then(|items| items.list().iter().find(|found| found.need("id").text(&b).equals(&common.ai_model)).map(|found| found.need("key").text(&b)))
        };
        let call = ProviderCall {
            model: common.descriptor, messages: formated, sampling: Vec::new(), bias, tools: Vec::new(), schema: None, stream: false, mode: common.mode,
        };
        let inputs = NovelAiInputs {
            ai_model: &common.ai_model, key: key.as_ref(), max_tokens: entry.get("maxTokens").map(Jv::num).unwrap_or_else(|| db.num("maxResponse")),
            temperature: db.num("temperature") / 100.0, bias_tokens: &bias_tokens, char_name: &char_name, user_name: &user_name,
            continued: entry.get("continue").is_some_and(Jv::flag), settings: &settings,
        };
        let decline = entry.get("declines").map(Jv::string);
        let request = match build_request(&call, &inputs, &b) {
            Err(BuildError::Unsupported(found)) => {
                assert_eq!(Some(found.to_owned()), decline, "{id}: declined `{found}`");
                // TS throws before any request.
                assert!(result.need("requests").list().is_empty(), "{id}");
                assert_eq!(result.need("outcome").need("type").string(), "thrown", "{id}");
                declined += 1;
                continue;
            }
            Err(other) => panic!("{id}: {other:?}"),
            Ok(request) => request,
        };
        let requests = result.need("requests").list();
        assert_eq!(requests.len(), 1, "{id}");
        assert_request(&id, &request.spec, &requests[0]);
        assert_eq!(request.spec.response, ResponseKind::Buffered, "{id}");
        assert_eq!(PREVIEW_RESULT, result.need("preview").need("result").string(), "{id}: preview");
        sent += 1;
        if !check_reply { assert!(decline.is_none(), "{id}: expected the decline"); continue; }
        let scripted = reply(&entry.need("replies").list()[0], &b);
        let http_error = result.need("httpError").text(&b);
        let context = ReplyContext { http_error: &http_error, formated: &call.messages, char_name: &char_name, user_name: &user_name };
        let read = read_buffered(&scripted.borrow(), &context, &b);
        let outcome = result.need("outcome");
        if let Some(name) = decline {
            let error = read.expect_err(&format!("{id}: expected the decline `{name}`"));
            assert!(matches!(&error, BuildError::Unsupported(found) if *found == name), "{id}: {error:?}");
            assert!(matches!(settle(error, Progress::default(), FailureStage::BufferedJson), Settled::FallBack(found) if found == name), "{id}");
            assert_eq!(outcome.need("type").string(), "thrown", "{id}");
            declined += 1;
            continue;
        }
        let read = read.unwrap_or_else(|error| panic!("{id}: {error:?}"));
        match (&read.result, outcome.need("type").string().as_str()) {
            (NovelAiOutcome::Success(text), "success") => assert!(read.failure.is_none(), "{id}: {}", s(text)),
            (NovelAiOutcome::Fail(_), "fail") => assert!(read.failure.is_some(), "{id}"),
            (found, kind) => panic!("{id}: native {found:?}, TypeScript {kind}"),
        }
        let (NovelAiOutcome::Success(text) | NovelAiOutcome::Fail(text)) = &read.result;
        assert_eq!(s(text), outcome.need("result").string(), "{id}: result");
        assert!(!outcome.has("noRetry"), "{id}: noRetry");
        replies += 1;
    }
    (sent, declined, replies)
}

#[test]
fn request_bytes_match_the_typescript_requests() {
    let b = budget();
    assert_eq!(ids_of(parse(CASES[0], &b).need("cases").list(), "declines"), ["separator-number-declines"]);
    assert_eq!(replay(0, false), (28, 1, 0));
}

#[test]
fn buffered_replies_match_the_typescript_outcomes() {
    let b = budget();
    assert_eq!(ids_of(parse(CASES[1], &b).need("cases").list(), "declines"), ["output-number-declines", "json-null-throws"]);
    assert_eq!(replay(1, true), (15, 2, 13));
}
