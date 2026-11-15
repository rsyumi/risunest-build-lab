//! Replays the committed TypeScript observations (`tests/fixtures/rust-core/providers/cohere`) through the native
//! builders. The fixtures are produced by `benchmarks/rust-core-oracles/providers-cohere.oracle.test.ts` from the real
//! `requestChatDataMain`; nothing in this file knows what a request should look like.

use super::request::{build_request, CohereInputs, CohereSettings};
use super::response::{read_buffered, CohereOutcome};
use super::BuildError;
use crate::common::built::BuiltRequest;
use crate::common::js::tests::budget;
use crate::common::owner::{self, resolve_target, settle, BufferedReply, CustomModelRef, Progress, Settled, TargetSource};
use crate::common::params;
use crate::common::reformat::{reformat, SystemReplacement};
use crate::common::replay_kit::{lossy, message, model_descriptor, pair_list, parse, same_hash, sampling_settings, t, to_value, Db, Jv};
use crate::common::settings::SamplingSettings;
use risunest_ir::chat::Role;
use risunest_ir::provider::{FailureStage, HttpRequestSpec, ProviderCall, ResponseKind};
use risunest_ir::{json, Budget, Text};

const CASES: [&[u8]; 2] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/cohere/requests.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/cohere/responses.cases.json"),
];
const EXPECTED: [&[u8]; 2] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/cohere/requests.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/cohere/responses.expected.json"),
];

fn s(text: &Text) -> String { lossy(&text.units().collect::<Vec<_>>()) }

fn ids_of(cases: &[Jv], field: &str) -> Vec<String> {
    cases.iter().filter(|entry| entry.has(field)).map(|entry| entry.need("id").string()).collect()
}

struct Prepared {
    ai_model: Text,
    call: ProviderCall,
    key: Option<Text>,
    custom_url: Option<Text>,
    settings: CohereSettings,
    sampling: SamplingSettings,
}

fn prepare(fixture: &Jv, entry: &Jv, model: &Jv, b: &Budget) -> Result<Prepared, BuildError> {
    let db = Db::new(fixture, entry);
    let ai_model = entry.need("model").text(b);
    let custom_models: Vec<Jv> = entry.get("customModels").map(|items| items.list().to_vec()).unwrap_or_default();
    let custom_params = custom_models.iter().find(|found| found.need("id").text(b).equals(&ai_model)).and_then(|found| found.get("params")).map(|value| value.text(b));
    let additional_params = params::additional_parameters(
        &ai_model, &pair_list(db.need("additionalParams"), b), db.flag("applyAdditionalParamsToAll"), custom_params.as_ref(), b,
    ).unwrap();
    let settings = CohereSettings { cohere_key: db.text("cohereAPIKey", b), additional_params };
    let mode = entry.get("mode").map(Jv::string).unwrap_or_else(|| "model".to_owned());
    let mut descriptor = model_descriptor(model, b);

    let owned: Vec<(Text, Option<Text>, Option<Text>)> = custom_models.iter()
        .map(|found| (found.need("id").text(b), found.get("url").map(|value| value.text(b)), found.get("key").map(|value| value.text(b)))).collect();
    let references: Vec<CustomModelRef> = owned.iter().map(|(id, url, key)| CustomModelRef { id, url: url.as_ref(), key: key.as_ref() }).collect();
    let (proxy_key, force_replace_url, custom_proxy_request_model) = (db.text("proxyKey", b), db.text("forceReplaceUrl", b), db.text("customProxyRequestModel", b));
    let target = resolve_target(&TargetSource {
        ai_model: &ai_model, proxy_key: &proxy_key, force_replace_url: &force_replace_url, custom_proxy_request_model: &custom_proxy_request_model, custom_models: &references,
    }, b).unwrap();
    target.apply(&mut descriptor, b).unwrap();

    let messages = entry.need("messages").list().iter().map(|item| message(item, b)).collect();
    let replacement = Some(db.text("systemContentReplacement", b)).filter(|value| value.len_units() != 0);
    let role = match db.need("systemRoleReplacement").string().as_str() {
        "" => None, "system" => Some(Role::System), "user" => Some(Role::User), "assistant" => Some(Role::Assistant), other => panic!("role {other}"),
    };
    let messages = reformat(messages, &descriptor.flags, &SystemReplacement { content: replacement.as_ref(), role }, b)?;
    let call = ProviderCall { model: descriptor, messages, sampling: Vec::new(), bias: Vec::new(), tools: Vec::new(), schema: None, stream: false, mode: t(&mode, b) };
    Ok(Prepared { ai_model, call, key: target.key, custom_url: target.custom_url, settings, sampling: sampling_settings(&db, b) })
}

fn build(prepared: &Prepared, b: &Budget) -> Result<BuiltRequest, BuildError> {
    let inputs = CohereInputs {
        ai_model: &prepared.ai_model, key: prepared.key.as_ref(), custom_url: prepared.custom_url.as_ref(), sampling: &prepared.sampling, settings: &prepared.settings,
    };
    build_request(&prepared.call, &inputs, b)
}

fn assert_request(id: &str, spec: &HttpRequestSpec, want: &Jv) {
    let headers: Vec<(String, String)> = want.need("headers").list().iter().map(|pair| (pair.list()[0].string(), pair.list()[1].string())).collect();
    assert_eq!(want.need("method").string(), "POST", "{id}");
    assert_eq!(spec.url, want.need("url").string(), "{id}: url");
    assert_eq!(spec.headers, headers, "{id}: headers");
    assert_eq!(String::from_utf8(spec.body.clone()).expect("utf-8"), want.need("body").string(), "{id}: body");
}

const REQUEST_CASES: usize = 70;

#[test]
fn request_bytes_match_the_typescript_requests() {
    let b = budget();
    let fixture = parse(CASES[0], &b);
    let expected = parse(EXPECTED[0], &b);
    same_hash(CASES[0], &expected);
    let cases = fixture.need("cases").list();
    let observed = expected.need("cases").list();
    assert_eq!(cases.len(), REQUEST_CASES);
    assert_eq!(observed.len(), REQUEST_CASES);
    assert_eq!(ids_of(cases, "declines"), ["reverse-proxy-no-url-declines", "reverse-proxy-userinfo-declines", "other-format-declines"]);
    let (mut sent, mut thrown, mut declined, mut failed) = (0, 0, 0, 0);
    for (entry, record) in cases.iter().zip(observed) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let result = record.need("result");
        let outcome = prepare(&fixture, entry, result.need("model"), &b).and_then(|prepared| build(&prepared, &b));
        if let Some(name) = entry.get("declines").map(Jv::string) {
            assert!(matches!(&outcome, Err(BuildError::Unsupported(found)) if *found == name), "{id}: expected the decline `{name}`, got {outcome:?}");
            declined += 1;
            continue;
        }
        let preview = result.need("preview");
        let requests = result.need("requests").list();
        match (preview.need("type").string().as_str(), outcome) {
            ("thrown", Err(BuildError::Thrown(text))) => {
                assert!(requests.is_empty(), "{id}");
                assert!(preview.need("error").string().ends_with(&format!(": {}", s(&text))), "{id}: threw `{}`, TypeScript `{}`", s(&text), preview.need("error").string());
                thrown += 1;
            }
            ("fail", Err(BuildError::Fail(text))) => { assert_eq!(s(&text), preview.need("result").string(), "{id}"); failed += 1; }
            ("success", Ok(built)) => {
                assert_eq!(requests.len(), 1, "{id}");
                assert_request(&id, &built.spec, &requests[0]);
                assert_eq!(built.spec.response, ResponseKind::Buffered, "{id}");
                assert_eq!(s(&built.preview(&b).unwrap()), preview.need("result").string(), "{id}: preview");
                sent += 1;
            }
            (kind, other) => panic!("{id}: TypeScript ended with {kind}, native with {other:?}"),
        }
    }
    assert_eq!((sent, thrown, failed, declined), (60, 4, 3, 3));
}

fn json_text(value: &Jv) -> String {
    match value {
        Jv::Null => "null".into(),
        Jv::Bool(flag) => flag.to_string(),
        Jv::Num(number) => number.to_string(),
        other => {
            let b = budget();
            s(&json::stringify(&to_value(other, &b), &b).unwrap().expect("a value"))
        }
    }
}

const RESPONSE_CASES: usize = 31;
const RESPONSE_DECLINED: [&str; 4] = ["text-number-declines", "text-true-declines", "text-object-declines", "text-array-declines"];

#[test]
fn buffered_replies_match_the_typescript_outcomes() {
    let b = budget();
    let fixture = parse(CASES[1], &b);
    let expected = parse(EXPECTED[1], &b);
    same_hash(CASES[1], &expected);
    let cases = fixture.need("cases").list();
    let observed = expected.need("cases").list();
    assert_eq!(cases.len(), RESPONSE_CASES);
    assert_eq!(observed.len(), RESPONSE_CASES);
    assert_eq!(ids_of(cases, "declines"), RESPONSE_DECLINED);
    let mut compared = 0;
    for (entry, record) in cases.iter().zip(observed) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let result = record.need("result");
        let prepared = prepare(&fixture, entry, result.need("model"), &b).unwrap_or_else(|error| panic!("{id}: {error:?}"));
        let built = build(&prepared, &b).unwrap_or_else(|error| panic!("{id}: {error:?}"));
        let wire_requests = result.need("requests").list();
        assert_eq!(wire_requests.len(), 1, "{id}");
        assert_request(&id, &built.spec, &wire_requests[0]);
        let reply = &entry.need("replies").list()[0];
        let status = reply.get("status").map(Jv::num).unwrap_or(200.0) as u16;
        let error;
        let body;
        let data;
        let borrowed = if let Some(message) = reply.get("throws") {
            error = t(&format!("TypeError: {}", message.string()), &b);
            BufferedReply::Transport { error: &error }
        } else {
            let bytes = match reply.get("text") { Some(text) => text.string().into_bytes(), None => json_text(reply.need("json")).into_bytes() };
            body = owner::response_text(&bytes, &b).unwrap();
            match json::parse_json(&s(&body), &b) {
                Ok(value) => { data = value; BufferedReply::Json { status, data: &data } }
                Err(_) => BufferedReply::NotJson { status, body: &body },
            }
        };
        let ended = read_buffered(&borrowed, &b);
        let outcome = result.need("outcome");
        if let Some(name) = entry.get("declines").map(Jv::string) {
            let error = ended.expect_err(&format!("{id}: expected the decline `{name}`"));
            assert!(matches!(&error, BuildError::Unsupported(found) if *found == name), "{id}: expected the decline `{name}`, got {error:?}");
            assert!(matches!(settle(error, Progress::default(), FailureStage::BufferedJson), Settled::FallBack(found) if found == name), "{id}");
            continue;
        }
        let ended = ended.unwrap_or_else(|error| panic!("{id}: {error:?}"));
        match (&ended.result, outcome.need("type").string().as_str()) {
            (CohereOutcome::Success(text), "success") => {
                assert_eq!(s(text), outcome.need("result").string(), "{id}: result");
                assert!(ended.failure.is_none(), "{id}");
            }
            (CohereOutcome::Fail(text), "fail") => {
                assert_eq!(s(text), outcome.need("result").string(), "{id}: result");
                let failure = ended.failure.as_ref().unwrap_or_else(|| panic!("{id}: a failure"));
                if reply.has("json") && !(200..300).contains(&status) { assert_eq!(failure.http_status, Some(status), "{id}: failure status"); }
            }
            (found, kind) => panic!("{id}: native {found:?}, TypeScript {kind}"),
        }
        compared += 1;
    }
    assert_eq!(compared, RESPONSE_CASES - RESPONSE_DECLINED.len());
}
