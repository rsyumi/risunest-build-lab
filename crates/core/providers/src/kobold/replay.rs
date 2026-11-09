//! Replays the committed TypeScript observations (`tests/fixtures/rust-core/providers/kobold`) through the native
//! builder and reader. The fixtures are produced by `benchmarks/rust-core-oracles/providers-kobold.oracle.test.ts` from
//! the real `requestChatDataMain`; the rendered chat template is the recorded template-stage output.

use super::{build_request, read_buffered, KoboldInputs, KoboldOutcome, KoboldSettings};
use crate::common::js::tests::budget;
use crate::common::owner::{settle, Progress, Settled};
use crate::common::replay_kit::{parse, same_hash, Jv};
use crate::common::BuildError;
use crate::text_completion::testkit::{assert_request, assert_thrown, common, ids_of, messages, reply, s};
use risunest_ir::provider::{FailureStage, ProviderCall, ResponseKind};
use risunest_ir::Budget;

const CASES: [&[u8]; 2] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/kobold/requests.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/kobold/responses.cases.json"),
];
const EXPECTED: [&[u8]; 2] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/kobold/requests.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/kobold/responses.expected.json"),
];

fn build(fixture: &Jv, entry: &Jv, result: &Jv, b: &Budget) -> Result<crate::common::built::BuiltRequest, BuildError> {
    let common = common(fixture, entry, result.need("model"), b);
    let messages = messages(entry, &common, b)?;
    let settings = KoboldSettings {
        kobold_url: common.db.text("koboldURL", b), max_context: common.db.num("maxContext"), additional_params: common.additional_params,
    };
    let prompt = result.need("prompts").list().first().map(|value| value.text(b)).unwrap_or_else(|| panic!("no rendered template"));
    let max_tokens = entry.get("maxTokens").map(Jv::num).unwrap_or_else(|| common.db.num("maxResponse"));
    let call = ProviderCall {
        model: common.descriptor, messages, sampling: Vec::new(), bias: Vec::new(), tools: Vec::new(), schema: None, stream: false, mode: common.mode,
    };
    build_request(&call, &KoboldInputs { ai_model: &common.ai_model, prompt: &prompt, max_tokens, sampling: &common.sampling, settings: &settings }, b)
}

/// Builds every case and compares it with the TS request; returns `(sent, thrown, declined)`.
fn replay(set: usize, check_reply: bool) -> (usize, usize, usize, usize) {
    let b = budget();
    let fixture = parse(CASES[set], &b);
    let expected = parse(EXPECTED[set], &b);
    same_hash(CASES[set], &expected);
    let cases = fixture.need("cases").list();
    let records = expected.need("cases").list();
    assert_eq!(cases.len(), records.len());
    let (mut sent, mut thrown, mut declined, mut replies) = (0, 0, 0, 0);
    for (entry, record) in cases.iter().zip(records) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let result = record.need("result");
        let built = build(&fixture, entry, result, &b);
        let outcome = result.need("outcome");
        let requests = result.need("requests").list();
        let decline = entry.get("declines").map(Jv::string);
        let built = match built {
            Err(BuildError::Unsupported(found)) => {
                assert_eq!(Some(found.to_owned()), decline, "{id}: declined `{found}`");
                declined += 1;
                continue;
            }
            Err(BuildError::Thrown(text)) => { assert!(requests.is_empty(), "{id}"); assert_thrown(&id, outcome, &text); thrown += 1; continue; }
            Err(other) => panic!("{id}: {other:?}"),
            Ok(built) => built,
        };
        assert_eq!(requests.len(), 1, "{id}");
        assert_request(&id, &built.spec, &requests[0]);
        assert_eq!(built.spec.response, ResponseKind::Buffered, "{id}");
        assert_eq!(s(&built.preview(&b).unwrap()), result.need("preview").need("result").string(), "{id}: preview");
        sent += 1;
        if !check_reply { continue; }
        let scripted = reply(&entry.need("replies").list()[0], &b);
        let read = read_buffered(&scripted.borrow(), &b);
        if let Some(name) = decline {
            let error = read.expect_err(&format!("{id}: expected the decline `{name}`"));
            assert!(matches!(&error, BuildError::Unsupported(found) if *found == name), "{id}: {error:?}");
            assert!(matches!(settle(error, Progress::default(), FailureStage::BufferedJson), Settled::FallBack(found) if found == name), "{id}");
            declined += 1;
            continue;
        }
        let read = read.unwrap_or_else(|error| panic!("{id}: {error:?}"));
        match (&read.result, outcome.need("type").string().as_str()) {
            (KoboldOutcome::Success(text), "success") => { assert_eq!(s(text), outcome.need("result").string(), "{id}"); assert!(read.failure.is_none(), "{id}"); }
            (KoboldOutcome::Fail(text), "fail") => {
                assert_eq!(s(text), outcome.need("result").string(), "{id}");
                assert!(outcome.need("noRetry").flag(), "{id}: noRetry");
                assert!(read.failure.is_some(), "{id}");
            }
            (found, kind) => panic!("{id}: native {found:?}, TypeScript {kind}"),
        }
        replies += 1;
    }
    (sent, thrown, declined, replies)
}

#[test]
fn request_bytes_match_the_typescript_requests() {
    let b = budget();
    let fixture = parse(CASES[0], &b);
    assert_eq!(ids_of(fixture.need("cases").list(), "declines"), ["url-userinfo-declines", "url-invalid-declines"]);
    assert_eq!(replay(0, false), (32, 2, 2, 0));
}

#[test]
fn buffered_replies_match_the_typescript_outcomes() {
    let b = budget();
    let fixture = parse(CASES[1], &b);
    assert_eq!(ids_of(fixture.need("cases").list(), "declines"),
        ["text-number-declines", "text-missing-declines", "results-missing-throws", "results-empty-throws", "json-null-throws"]);
    assert_eq!(replay(1, true), (17, 0, 5, 12));
}
