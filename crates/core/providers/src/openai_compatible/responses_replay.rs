//! Replays the committed TypeScript observations (`tests/fixtures/rust-core/providers/openai-responses`) through the
//! native builders. The fixtures are produced by `benchmarks/rust-core-oracles/providers-openai-responses.oracle.test.ts`
//! from the real `requestChatDataMain`; nothing in this file knows what a request should look like.

use super::js::tests::budget;
use super::responses_request::{build_request, ResponsesInputs, ResponsesSettings};
use super::responses_response::{read_buffered, ResponsesContext, ResponsesOutcome};
use super::responses_stream::StreamSession;
use super::stream::{stream_start, StreamStart};
use super::BuildError;
use crate::common::built::BuiltRequest;
use crate::common::owner::{self, resolve_target, settle, BufferedReply, CustomModelRef, Progress, Settled, TargetSource};
use crate::common::params;
use crate::common::reformat::{reformat, SystemReplacement};
use crate::common::replay_kit::{lossy, message, model_descriptor, pair_list, parse, same_hash, sampling_settings, t, to_value, Db, Jv};
use crate::common::settings::SamplingSettings;
use crate::common::tools::{DecodedToolCall, ToolResponsePart};
use risunest_ir::chat::Role;
use risunest_ir::provider::{FailureStage, HttpRequestSpec, ProviderCall, ResponseKind, ToolSpec};
use risunest_ir::{json, Budget, Error, Text};

const CASES: [&[u8]; 3] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/openai-responses/requests.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/openai-responses/responses.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/openai-responses/streams.cases.json"),
];
const EXPECTED: [&[u8]; 3] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/openai-responses/requests.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/openai-responses/responses.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/openai-responses/streams.expected.json"),
];

fn s(text: &Text) -> String { lossy(&text.units().collect::<Vec<_>>()) }

fn text_of(bytes: &[u8]) -> String { String::from_utf8(bytes.to_vec()).expect("utf-8") }

fn sent_headers(request: &Jv) -> Vec<(String, String)> {
    request.need("headers").list().iter().map(|pair| (pair.list()[0].string(), pair.list()[1].string())).collect()
}

fn ids_of(cases: &[Jv], field: &str) -> Vec<String> {
    cases.iter().filter(|entry| entry.has(field)).map(|entry| entry.need("id").string()).collect()
}

struct Prepared {
    ai_model: Text,
    call: ProviderCall,
    key: Option<Text>,
    custom_url: Option<Text>,
    key_identifier: Option<Text>,
    max_tokens: f64,
    settings: ResponsesSettings,
    sampling: SamplingSettings,
    decodings: Vec<(String, Jv)>,
    extract_json: Option<Text>,
}

fn prepare(fixture: &Jv, entry: &Jv, model: &Jv, b: &Budget) -> Prepared {
    let db = Db::new(fixture, entry);
    let ai_model = entry.need("model").text(b);
    let custom_models: Vec<Jv> = entry.get("customModels").map(|items| items.list().to_vec()).unwrap_or_default();
    let custom_params = custom_models.iter().find(|found| found.need("id").text(b).equals(&ai_model)).and_then(|found| found.get("params")).map(|value| value.text(b));
    let additional_params = params::additional_parameters(
        &ai_model, &pair_list(db.need("additionalParams"), b), db.flag("applyAdditionalParamsToAll"), custom_params.as_ref(), b,
    ).unwrap();
    let settings = ResponsesSettings {
        openai_key: db.text("openAIKey", b),
        proxy_key: db.text("proxyKey", b),
        gpt_vision_quality: db.text("gptVisionQuality", b),
        new_oai_handle: db.flag("newOAIHandle"),
        json_schema_enabled: db.flag("jsonSchemaEnabled"),
        strict_json_schema: Some(!matches!(db.need("strictJsonSchema"), Jv::Bool(false))),
        autofill_request_url: db.flag("autofillRequestUrl"),
        oai_comp_api_keys: db.need("OaiCompAPIKeys").entries().iter().map(|(name, key)| (t(name, b), key.text(b))).collect(),
        model_tools_search: db.need("modelTools").list().iter().any(|tool| tool.string() == "search"),
        additional_params,
    };
    let mode = entry.get("mode").map(Jv::string).unwrap_or_else(|| "model".to_owned());
    let mut descriptor = model_descriptor(model, b);
    let key_identifier = model.get("keyIdentifier").filter(|value| !matches!(value, Jv::Null)).map(|value| value.text(b));

    let owned: Vec<(Text, Option<Text>, Option<Text>)> = custom_models.iter()
        .map(|found| (found.need("id").text(b), found.get("url").map(|value| value.text(b)), found.get("key").map(|value| value.text(b)))).collect();
    let references: Vec<CustomModelRef> = owned.iter().map(|(id, url, key)| CustomModelRef { id, url: url.as_ref(), key: key.as_ref() }).collect();
    let (proxy_key, force_replace_url, custom_proxy_request_model) = (db.text("proxyKey", b), db.text("forceReplaceUrl", b), db.text("customProxyRequestModel", b));
    let target = resolve_target(&TargetSource {
        ai_model: &ai_model, proxy_key: &proxy_key, force_replace_url: &force_replace_url, custom_proxy_request_model: &custom_proxy_request_model, custom_models: &references,
    }, b).unwrap();
    target.apply(&mut descriptor, b).unwrap();

    let stream = entry.get("stream").is_some_and(Jv::flag);
    let max_tokens = entry.get("maxTokens").map(Jv::num).unwrap_or_else(|| db.num("maxResponse"));
    let messages = entry.need("messages").list().iter().map(|item| message(item, b)).collect();
    let replacement = Some(db.text("systemContentReplacement", b)).filter(|value| value.len_units() != 0);
    let role = match db.need("systemRoleReplacement").string().as_str() {
        "" => None, "system" => Some(Role::System), "user" => Some(Role::User), "assistant" => Some(Role::Assistant), other => panic!("role {other}"),
    };
    let messages = reformat(messages, &descriptor.flags, &SystemReplacement { content: replacement.as_ref(), role }, b).expect("reformat");
    let tools: Vec<ToolSpec> = entry.get("tools").map(|items| items.list().iter().map(|tool| ToolSpec {
        name: tool.need("name").text(b), description: tool.need("description").text(b), input_schema: to_value(tool.need("inputSchema"), b),
    }).collect()).unwrap_or_default();
    let schema = entry.get("schema").map(|value| json::parse_json(&value.string(), b).expect("schema"));
    let extract_json = entry.get("extractJson").map(|value| value.text(b)).filter(|value| value.len_units() != 0)
        .filter(|_| db.flag("jsonSchemaEnabled") || entry.has("schema"));
    let decodings = entry.get("decodings").map(|table| table.entries().to_vec()).unwrap_or_default();
    let call = ProviderCall { model: descriptor, messages, sampling: Vec::new(), bias: Vec::new(), tools, schema, stream, mode: t(&mode, b) };
    Prepared { ai_model, call, key: target.key, custom_url: target.custom_url, key_identifier, max_tokens, settings, sampling: sampling_settings(&db, b), decodings, extract_json }
}

fn decoded(entry: &Jv, b: &Budget) -> DecodedToolCall {
    let call = entry.need("call");
    DecodedToolCall {
        id: call.need("id").text(b),
        name: call.need("name").text(b),
        arg: to_value(call.need("arg"), b),
        response: entry.need("response").list().iter().map(|item| match item.need("type").string().as_str() {
            "text" => ToolResponsePart::Text(item.need("text").text(b)),
            "image" => ToolResponsePart::Image { mime_type: item.need("mimeType").text(b), data: item.need("data").text(b) },
            other => ToolResponsePart::Other(t(other, b)),
        }).collect(),
    }
}

fn build(prepared: &Prepared, b: &Budget) -> Result<BuiltRequest, BuildError> {
    let inputs = ResponsesInputs {
        ai_model: &prepared.ai_model, key: prepared.key.as_ref(), custom_url: prepared.custom_url.as_ref(), key_identifier: prepared.key_identifier.as_ref(),
        max_tokens: prepared.max_tokens, sampling: &prepared.sampling, settings: &prepared.settings,
    };
    let mut decode = |marker: &Text| -> Result<Option<DecodedToolCall>, Error> {
        let key = lossy(&marker.trim(b)?.units().collect::<Vec<_>>());
        Ok(prepared.decodings.iter().find(|(name, _)| *name == key).map(|(_, entry)| decoded(entry, b)))
    };
    build_request(&prepared.call, &inputs, &mut decode, b)
}

fn assert_request(id: &str, which: &str, spec: &HttpRequestSpec, want: &Jv) {
    assert_eq!(want.need("method").string(), "POST", "{id}: {which}");
    assert_eq!(spec.url, want.need("url").string(), "{id}: {which} url");
    assert_eq!(spec.headers, sent_headers(want), "{id}: {which} headers");
    assert_eq!(text_of(&spec.body), want.need("body").string(), "{id}: {which} body");
}

// ---------------------------------------------------------------------------------------------------------------
// Requests.

const REQUEST_CASES: usize = 139;
const REQUEST_DECLINED: [&str; 12] = [
    "reverse-proxy-no-url-declines", "custom-model-no-url-declines", "schema-database-declines", "additional-input-declines", "additional-input-nested-declines",
    "autofill-userinfo-declines", "autofill-ip-host-declines", "autofill-not-a-url-declines", "autofill-encoded-query-name-declines", "nanogpt-declines",
    "other-format-declines", "url-userinfo-buffered-declines",
];

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
    let mut declined_ids = ids_of(cases, "declines");
    declined_ids.sort();
    let mut known = REQUEST_DECLINED.to_vec();
    known.sort();
    assert_eq!(declined_ids, known, "the declined request cases changed");
    let (mut sent, mut failed, mut thrown, mut declined) = (0, Vec::new(), 0, 0);
    for (entry, record) in cases.iter().zip(observed) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let result = record.need("result");
        let prepared = prepare(&fixture, entry, result.need("model"), &b);
        let outcome = build(&prepared, &b);
        if let Some(name) = entry.get("declines").map(Jv::string) {
            assert!(matches!(&outcome, Err(BuildError::Unsupported(found)) if *found == name), "{id}: expected the decline `{name}`, got {outcome:?}");
            declined += 1;
            continue;
        }
        let requests = result.need("requests").list();
        if requests.is_empty() {
            let preview = result.need("preview");
            match (preview.need("type").string().as_str(), outcome) {
                ("fail", Err(BuildError::Fail(text))) => assert_eq!(s(&text), preview.need("result").string(), "{id}"),
                ("thrown", Err(BuildError::Thrown(text))) => {
                    assert!(preview.need("error").string().ends_with(&format!(": {}", s(&text))), "{id}: threw `{}`, TypeScript `{}`", s(&text), preview.need("error").string());
                    thrown += 1;
                    continue;
                }
                (kind, other) => panic!("{id}: TypeScript ended with {kind}, native with {other:?}"),
            }
            failed.push(id);
            continue;
        }
        let built = outcome.unwrap_or_else(|error| panic!("{id}: {error:?}"));
        assert_eq!(requests.len(), 1, "{id}");
        assert_request(&id, "request", &built.spec, &requests[0]);
        let streaming = result.need("outcome").need("type").string() == "streaming";
        assert_eq!(built.spec.response, if streaming { ResponseKind::Sse } else { ResponseKind::Buffered }, "{id}: response kind");
        assert_eq!(s(&built.preview(&b).unwrap()), result.need("preview").need("result").string(), "{id}: preview");
        sent += 1;
    }
    assert_eq!(declined, REQUEST_DECLINED.len());
    assert_eq!(thrown, 1);
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(sent + failed.len() + thrown + declined, REQUEST_CASES);
}

// ---------------------------------------------------------------------------------------------------------------
// Replies.

fn built_for(fixture: &Jv, entry: &Jv, model: &Jv, b: &Budget) -> (Prepared, BuiltRequest) {
    let prepared = prepare(fixture, entry, model, b);
    let built = build(&prepared, b).unwrap_or_else(|error| panic!("{error:?}"));
    (prepared, built)
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

fn chunk_bytes(chunk: &Jv) -> Vec<u8> {
    if let Some(text) = chunk.get("utf8") { return text.string().into_bytes(); }
    let hex = chunk.need("hex").string();
    (0..hex.len() / 2).map(|at| u8::from_str_radix(&hex[at * 2..at * 2 + 2], 16).unwrap()).collect()
}

fn reply_status(reply: &Jv) -> u16 { reply.get("status").map(Jv::num).unwrap_or(200.0) as u16 }

const RESPONSE_CASES: usize = 70;
const RESPONSE_DECLINED: [&str; 6] = [
    "function-call-declines", "output-string-declines", "output-object-declines", "content-string-declines", "text-number-declines", "refusal-object-declines",
];

#[test]
fn buffered_replies_match_the_typescript_outcomes() {
    let b = budget();
    let fixture = parse(CASES[1], &b);
    let expected = parse(EXPECTED[1], &b);
    same_hash(CASES[1], &expected);
    let cases = fixture.need("cases").list();
    let observed = expected.need("cases").list();
    assert_eq!(cases.len(), RESPONSE_CASES, "the number of reply cases changed");
    assert_eq!(observed.len(), RESPONSE_CASES);
    assert_eq!(ids_of(cases, "declines"), RESPONSE_DECLINED, "the declined reply cases changed");
    let mut compared = 0;
    for (entry, record) in cases.iter().zip(observed) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let result = record.need("result");
        let (prepared, built) = built_for(&fixture, entry, result.need("model"), &b);
        let prefix = result.need("httpError").text(&b);
        let context = ResponsesContext { extract_json: prepared.extract_json.as_ref(), http_error_prefix: &prefix };
        let reply = &entry.need("replies").list()[0];
        let status = reply_status(reply);
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
        let ended = read_buffered(&borrowed, &context, &b);
        let wire_requests = result.need("requests").list();
        assert_request(&id, "request", &built.spec, &wire_requests[0]);
        let outcome = result.need("outcome");
        if let Some(name) = entry.get("declines").map(Jv::string) {
            let error = ended.expect_err(&format!("{id}: expected the decline `{name}`"));
            assert!(matches!(&error, BuildError::Unsupported(found) if *found == name), "{id}: expected the decline `{name}`, got {error:?}");
            let settled = settle(error, Progress::default(), FailureStage::BufferedJson);
            assert!(matches!(settled, Settled::FallBack(found) if found == name), "{id}: a decline before any effect falls back");
            continue;
        }
        assert_eq!(wire_requests.len(), 1, "{id}: the number of requests");
        let ended = ended.unwrap_or_else(|error| panic!("{id}: {error:?}"));
        match (&ended.result, outcome.need("type").string().as_str()) {
            (ResponsesOutcome::Success(text), "success") => {
                assert_eq!(s(text), outcome.need("result").string(), "{id}: result");
                assert!(ended.failure.is_none(), "{id}");
            }
            (ResponsesOutcome::Fail(text), "fail") => {
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

const STREAM_CASES: usize = 74;
const STREAM_DECLINED: [&str; 7] = [
    "function-call-item-declines", "function-call-arguments-delta-declines", "function-call-in-completed-declines", "delta-number-declines",
    "done-text-number-declines", "reasoning-delta-number-declines", "completed-output-string-declines",
];

#[test]
fn streams_match_the_typescript_snapshots() {
    let b = budget();
    let fixture = parse(CASES[2], &b);
    let expected = parse(EXPECTED[2], &b);
    same_hash(CASES[2], &expected);
    let cases = fixture.need("cases").list();
    let observed = expected.need("cases").list();
    assert_eq!(cases.len(), STREAM_CASES, "the number of stream cases changed");
    assert_eq!(observed.len(), STREAM_CASES);
    assert_eq!(ids_of(cases, "declines"), STREAM_DECLINED, "the declined stream cases changed");
    let mut compared = 0;
    for (entry, record) in cases.iter().zip(observed) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let result = record.need("result");
        let (prepared, built) = built_for(&fixture, entry, result.need("model"), &b);
        assert_eq!(built.spec.response, ResponseKind::Sse, "{id}");
        let first = &entry.need("replies").list()[0];
        let wire_requests = result.need("requests").list();
        let outcome = result.need("outcome");
        assert_request(&id, "request", &built.spec, &wire_requests[0]);
        let content_type = first.get("contentType").map(|value| value.text(&b)).unwrap_or_else(|| t(if first.has("text") { "application/json" } else { "text/event-stream" }, &b));
        let status = reply_status(first);
        let body: Vec<u8> = match first.get("text") {
            Some(text) => text.string().into_bytes(),
            None => first.get("chunks").map(|chunks| chunks.list().iter().flat_map(chunk_bytes).collect()).unwrap_or_default(),
        };
        if stream_start(status, Some(&content_type), &b).unwrap() != StreamStart::Accept {
            let (shown, failure) = owner::stream_start_failure(status, &body, &b).unwrap();
            assert_eq!(outcome.need("type").string(), "fail", "{id}");
            assert_eq!(s(&shown), outcome.need("result").string(), "{id}: result");
            assert_eq!(failure.http_status, Some(status), "{id}: failure status");
            compared += 1;
            continue;
        }
        assert_eq!(wire_requests.len(), if entry.has("declines") && entry.has("toolReplies") { 2 } else { 1 }, "{id}: the number of requests");
        let mut session = StreamSession::new(prepared.extract_json.as_ref().map(|text| text.copy(&b).unwrap()));
        let mut snapshots: Vec<String> = Vec::new();
        let mut stopped = None;
        for chunk in first.need("chunks").list() {
            match session.feed(&chunk_bytes(chunk), &b) {
                Ok(found) => snapshots.extend(found.iter().map(s)),
                Err(error) => { stopped = Some(error); break; }
            }
        }
        let mut decline = None;
        if stopped.is_none() {
            match session.finish(&b) {
                Ok(end) => { snapshots.extend(end.snapshots.iter().map(s)); decline = end.decline.map(BuildError::Unsupported); }
                Err(error) => stopped = Some(error),
            }
        }
        let want: Vec<String> = outcome.need("snapshots").list().iter().map(|snapshot| {
            assert_eq!(snapshot.entries().len(), 1, "{id}: a stream snapshot carries only the displayed text");
            snapshot.need("0").string()
        }).collect();
        if let Some(name) = entry.get("declines").map(Jv::string) {
            let error = stopped.or(decline).unwrap_or_else(|| panic!("{id}: expected the decline `{name}`"));
            assert!(matches!(&error, BuildError::Unsupported(found) if *found == name), "{id}: expected the decline `{name}`, got {error:?}");
            assert_eq!(&want[..snapshots.len().min(want.len())], &snapshots[..snapshots.len().min(want.len())], "{id}: snapshots before the decline");
            let settled = settle(error, Progress { emitted: !snapshots.is_empty(), tools_ran: false }, FailureStage::Body);
            match (entry.has("terminal"), settled) {
                (true, Settled::Terminal(terminal)) => assert_eq!(terminal.reason, name, "{id}"),
                (false, Settled::FallBack(found)) => assert_eq!(found, name, "{id}"),
                (_, other) => panic!("{id}: unexpected settlement {other:?}"),
            }
            compared += 1;
            continue;
        }
        assert!(stopped.is_none() && decline.is_none(), "{id}: {stopped:?} {decline:?}");
        assert_eq!(outcome.need("type").string(), "streaming", "{id}");
        assert_eq!(snapshots, want, "{id}: snapshots");
        compared += 1;
    }
    assert_eq!(compared, STREAM_CASES);
}
