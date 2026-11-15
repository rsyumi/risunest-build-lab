//! Replays the committed TypeScript observations (`tests/fixtures/rust-core/providers/horde`) through the native
//! builder and poll machine. The fixtures are produced by `benchmarks/rust-core-oracles/providers-horde.oracle.test.ts`
//! from the real `requestChatDataMain`; the rendered chat template and the character and user names are recorded TS
//! inputs, and every request of a run (the create request, each status request and the detached cancel) is compared.

use super::{build_request, HordeInputs, HordeOutcome, HordeReply, HordeSession, HordeSettings, ReplyContext, Step, PREVIEW_RESULT};
use crate::common::js::tests::budget;
use crate::common::owner::{settle, Progress, Settled};
use crate::common::replay_kit::{parse, same_hash, t, Jv};
use crate::common::BuildError;
use crate::text_completion::testkit::{assert_request, assert_thrown, common, ids_of, messages, reply_bytes, s};
use risunest_ir::provider::{FailureStage, HttpRequestSpec, ProviderCall, ResponseKind};

const CASES: [&[u8]; 2] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/horde/requests.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/horde/responses.cases.json"),
];
const EXPECTED: [&[u8]; 2] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/horde/requests.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/horde/responses.expected.json"),
];

/// A request the machine asked for, with whether it carries the abort signal.
struct Sent { spec: HttpRequestSpec, abortable: bool }

/// Builds every case, drives the poll machine with the scripted replies and compares the run with the TS run; returns
/// `(compared, thrown before any request, declined)`.
fn replay(set: usize) -> (usize, usize, usize) {
    let b = budget();
    let fixture = parse(CASES[set], &b);
    let expected = parse(EXPECTED[set], &b);
    same_hash(CASES[set], &expected);
    let cases = fixture.need("cases").list();
    let records = expected.need("cases").list();
    assert_eq!(cases.len(), records.len());
    let (mut compared, mut thrown, mut declined) = (0, 0, 0);
    for (entry, record) in cases.iter().zip(records) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let result = record.need("result");
        let outcome = result.need("outcome");
        let wanted = result.need("requests").list();
        let common = common(&fixture, entry, result.need("model"), &b);
        let formated = messages(entry, &common, &b).expect("reformat");
        let db = &common.db;
        let settings = HordeSettings {
            api_key: db.need("hordeConfig").need("apiKey").text(&b), max_context: db.num("maxContext"), max_response: db.num("maxResponse"),
            temperature: db.num("temperature"), top_k: db.num("top_k"), top_p: db.num("top_p"), additional_params: common.additional_params,
        };
        let prompt = result.need("prompts").list().first().map(|value| value.text(&b)).unwrap_or_else(|| panic!("{id}: no rendered template"));
        let names = result.need("names");
        let (char_name, user_name) = (names.need("char").text(&b), names.need("user").text(&b));
        let call = ProviderCall {
            model: common.descriptor, messages: formated, sampling: Vec::new(), bias: Vec::new(), tools: Vec::new(), schema: None, stream: false, mode: common.mode,
        };
        let built = match build_request(&call, &HordeInputs { ai_model: &common.ai_model, prompt: &prompt, settings: &settings }, &b) {
            Err(BuildError::Thrown(text)) => { assert!(wanted.is_empty(), "{id}"); assert_thrown(&id, outcome, &text); thrown += 1; continue; }
            Err(other) => panic!("{id}: {other:?}"),
            Ok(built) => built,
        };
        assert_eq!(PREVIEW_RESULT, result.need("preview").need("result").string(), "{id}: preview");

        let context = ReplyContext { formated: &call.messages, char_name: &char_name, user_name: &user_name };
        let mut replies = entry.need("replies").list().iter();
        let (mut sent, mut sleeps): (Vec<Sent>, Vec<u32>) = (Vec::new(), Vec::new());
        let ended = (|| {
            let (mut session, mut step) = HordeSession::start(&built, &b)?;
            loop {
                step = match step {
                    Step::Send(request) => {
                        assert_eq!(request.spec.response, ResponseKind::Buffered, "{id}");
                        sent.push(Sent { spec: request.spec, abortable: request.abortable });
                        let reply = replies.next().unwrap_or_else(|| panic!("{id}: the tape ran dry"));
                        let error;
                        let body;
                        let answer = if let Some(message) = reply.get("throws") {
                            error = t(&format!("TypeError: {}", message.string()), &b);
                            HordeReply::Transport { error: &error }
                        } else {
                            body = reply_bytes(reply, &b);
                            HordeReply::Http { status: reply.get("status").map_or(200, |value| value.num() as u16), body: &body }
                        };
                        session.replied(answer, &context, &b)?
                    }
                    Step::Wait { ms } => { sleeps.push(ms); session.waited()? }
                    Step::Done { outcome, detached } => {
                        sent.extend(detached.map(|spec| Sent { spec, abortable: false }));
                        return Ok(outcome);
                    }
                };
            }
        })();

        let decline = entry.get("declines").map(Jv::string);
        if let Some(name) = decline {
            let error = ended.expect_err(&format!("{id}: expected the decline `{name}`"));
            assert!(matches!(&error, BuildError::Unsupported(found) if *found == name), "{id}: {error:?}");
            // The job exists on the server by now, so the decline cannot fall back to the TS path.
            assert!(matches!(settle(error, Progress { emitted: false, tools_ran: true }, FailureStage::BufferedJson), Settled::Terminal(_)), "{id}");
            assert!(sent.len() <= wanted.len(), "{id}");
            for (at, (sent, want)) in sent.iter().zip(wanted).enumerate() { assert_request(&format!("{id} #{at}"), &sent.spec, want); }
            declined += 1;
            continue;
        }
        assert_eq!(sent.len(), wanted.len(), "{id}: request count");
        for (at, (sent, want)) in sent.iter().zip(wanted).enumerate() {
            assert_request(&format!("{id} #{at}"), &sent.spec, want);
            assert_eq!(sent.abortable, want.need("signal").flag(), "{id} #{at}: signal");
        }
        let waits: Vec<u32> = result.need("sleeps").list().iter().map(|ms| ms.num() as u32).collect();
        assert_eq!(sleeps, waits, "{id}: waits");
        match (ended, outcome.need("type").string().as_str()) {
            (Ok(HordeOutcome::Success(text)), "success") => {
                assert_eq!(s(&text), outcome.need("result").string(), "{id}: result");
                assert!(!outcome.has("noRetry"), "{id}: noRetry");
            }
            (Ok(HordeOutcome::Fail { result, no_retry }), "fail") => {
                assert_eq!(s(&result), outcome.need("result").string(), "{id}: result");
                assert_eq!(no_retry, outcome.get("noRetry").is_some_and(Jv::flag), "{id}: noRetry");
            }
            (Err(BuildError::Thrown(text)), "thrown") => assert_thrown(&id, outcome, &text),
            (found, kind) => panic!("{id}: native {found:?}, TypeScript {kind}"),
        }
        compared += 1;
    }
    (compared, thrown, declined)
}

#[test]
fn create_requests_match_the_typescript_requests() {
    let b = budget();
    assert!(ids_of(parse(CASES[0], &b).need("cases").list(), "declines").is_empty());
    assert_eq!(replay(0), (23, 1, 0));
}

#[test]
fn poll_runs_match_the_typescript_runs() {
    let b = budget();
    assert_eq!(ids_of(parse(CASES[1], &b).need("cases").list(), "declines"), ["text-number-declines", "id-form-declines", "id-object-declines"]);
    assert_eq!(replay(1), (32, 0, 3));
}
