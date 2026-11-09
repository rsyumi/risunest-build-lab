//! Replays the committed TypeScript observations (`tests/fixtures/rust-core/providers/novellist`) through the native
//! builder and reader. The fixtures are produced by `benchmarks/rust-core-oracles/providers-novellist.oracle.test.ts`
//! from the real `requestChatDataMain`; the character and user names are recorded TS inputs.

use super::{build_request, read_buffered, NovelListInputs, NovelListOutcome, NovelListSettings, ReplyContext, Speaker};
use crate::common::js::tests::budget;
use crate::common::owner::{settle, Progress, Settled};
use crate::common::replay_kit::{parse, same_hash, to_value, Jv};
use crate::common::BuildError;
use crate::text_completion::testkit::{assert_request, common, ids_of, json_text, messages, reply, s};
use risunest_ir::provider::{FailureStage, ProviderCall, ResponseKind};
use risunest_ir::json;

const CASES: [&[u8]; 2] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/novellist/requests.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/novellist/responses.cases.json"),
];
const EXPECTED: [&[u8]; 2] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/novellist/requests.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/novellist/responses.expected.json"),
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
        let settings = NovelListSettings { api_key: db.text("novellistAPI", &b), ainconfig: to_value(db.need("ainconfig"), &b), additional_params: common.additional_params };
        let names = result.need("names");
        let (char_name, user_name) = (names.need("char").text(&b), names.need("user").text(&b));
        let bias = entry.get("biasString").map(|items| items.list().iter().map(|pair| (pair.list()[0].text(&b), pair.list()[1].num())).collect()).unwrap_or_default();
        let call = ProviderCall {
            model: common.descriptor, messages: formated, sampling: Vec::new(), bias, tools: Vec::new(), schema: None, stream: false, mode: common.mode,
        };
        let inputs = NovelListInputs {
            ai_model: &common.ai_model, custom_url: common.custom_url.as_ref(), max_tokens: entry.get("maxTokens").map(Jv::num).unwrap_or_else(|| db.num("maxResponse")),
            temperature: db.num("temperature") / 100.0, char_name: &char_name, user_name: &user_name, continued: entry.get("continue").is_some_and(Jv::flag), settings: &settings,
        };
        let decline = entry.get("declines").map(Jv::string);
        let request = match build_request(&call, &inputs, &b) {
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
        let context = ReplyContext { formated: &call.messages, char_name: &char_name, user_name: &user_name };
        let read = read_buffered(&scripted.borrow(), &context, &b);
        let outcome = result.need("outcome");
        if let Some(name) = decline {
            let error = read.expect_err(&format!("{id}: expected the decline `{name}`"));
            assert!(matches!(&error, BuildError::Unsupported(found) if *found == name), "{id}: {error:?}");
            assert!(matches!(settle(error, Progress::default(), FailureStage::BufferedJson), Settled::FallBack(found) if found == name), "{id}");
            declined += 1;
            continue;
        }
        let read = read.unwrap_or_else(|error| panic!("{id}: {error:?}"));
        match (&read.result, outcome.need("type").string().as_str()) {
            (NovelListOutcome::Multiline(lines), "multiline") => {
                let want = outcome.need("result").list();
                assert_eq!(lines.len(), want.len(), "{id}: lines");
                for ((speaker, text), pair) in lines.iter().zip(want) {
                    let role = match speaker { Speaker::Char => "char", Speaker::User => "user" };
                    assert_eq!(role, pair.list()[0].string(), "{id}: role");
                    assert_eq!(s(text), pair.list()[1].string(), "{id}: text");
                }
                assert!(read.failure.is_none(), "{id}");
            }
            (NovelListOutcome::Fail(value), "fail") => {
                let found = s(&json::stringify(value, &b).unwrap().expect("a value"));
                assert_eq!(found, json_text(outcome.need("result"), &b), "{id}: result");
                assert!(read.failure.is_some(), "{id}");
            }
            (found, kind) => panic!("{id}: native {found:?}, TypeScript {kind}"),
        }
        replies += 1;
    }
    (sent, declined, replies)
}

#[test]
fn request_bytes_match_the_typescript_requests() {
    let b = budget();
    assert_eq!(ids_of(parse(CASES[0], &b).need("cases").list(), "declines"), ["reverse-proxy-invalid-url-declines"]);
    assert_eq!(replay(0, false), (24, 1, 0));
}

#[test]
fn buffered_replies_match_the_typescript_outcomes() {
    let b = budget();
    assert_eq!(ids_of(parse(CASES[1], &b).need("cases").list(), "declines"), ["error-number-declines", "data-missing-throws", "json-null-throws"]);
    assert_eq!(replay(1, true), (24, 3, 21));
}
