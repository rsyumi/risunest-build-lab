//! Replays the committed TypeScript observations (`tests/fixtures/rust-core/providers/google`) through the native
//! builders. The fixtures are produced by `benchmarks/rust-core-oracles/providers-google.oracle.test.ts` from the real
//! `requestChatDataMain`; nothing in this file knows what a request should look like.

use super::request::{build_request, RequestInputs};
use super::response::{read_buffered, read_stream_start, Buffered, ReplyContext};
use super::settings::Settings;
use super::stream::StreamSession;
use super::token::{self, TokenPlan};
use super::BuildError;
use crate::common::built::BuiltRequest;
use crate::common::owner::{resolve_target, settle, CustomModelRef, Progress, Settled, TargetSource};
use crate::common::params;
use crate::common::reformat::{reformat, SystemReplacement};
use crate::common::replay_kit::{clock_ms, lossy, message, model_descriptor, pair_list, parse, same_hash, sampling_settings, t, to_value, Db, Jv};
use crate::common::settings::SamplingSettings;
use crate::common::tools::{DecodedToolCall, ToolResponsePart};
use crate::common::js::tests::budget;
use risunest_ir::chat::Role;
use risunest_ir::provider::{FailureStage, HttpRequestSpec, LlmFormat, ProviderCall, ResponseKind, ToolSpec};
use risunest_ir::{json, Budget, Error, Text, Value};

const CASES: [&[u8]; 3] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/google/requests.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/google/responses.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/google/streams.cases.json"),
];
const EXPECTED: [&[u8]; 3] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/google/requests.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/google/responses.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/google/streams.expected.json"),
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
    call: ProviderCall,
    key: Option<Text>,
    custom_url: Option<Text>,
    max_tokens: f64,
    image_response: bool,
    now_ms: i64,
    settings: Settings,
    sampling: SamplingSettings,
    decodings: Vec<(String, Jv)>,
    client_email: Text,
    private_key: Text,
    cached_token: Text,
    expires: f64,
}

fn prepare(fixture: &Jv, entry: &Jv, model: &Jv, b: &Budget) -> Result<Prepared, BuildError> {
    let db = Db::new(fixture, entry);
    let ai_model = entry.need("model").text(b);
    let custom_models: Vec<Jv> = entry.get("customModels").map(|items| items.list().to_vec()).unwrap_or_default();
    let custom_params = custom_models.iter().find(|found| found.need("id").text(b).equals(&ai_model)).and_then(|found| found.get("params")).map(|value| value.text(b));
    let additional_params = params::additional_parameters(
        &ai_model, &pair_list(db.need("additionalParams"), b), db.flag("applyAdditionalParamsToAll"), custom_params.as_ref(), b,
    ).unwrap();
    let google = db.need("google");
    let settings = Settings {
        access_token: google.need("accessToken").text(b),
        project_id: google.need("projectId").text(b),
        vertex_region: db.text("vertexRegion", b),
        vision_high: db.need("gptVisionQuality").string() == "high",
        save_signatures: db.flag("saveSignatures"),
        json_schema_enabled: db.flag("jsonSchemaEnabled"),
        anti_server_overloads: db.flag("antiServerOverloads"),
        simplified_tool_use: db.flag("simplifiedToolUse"),
        stream_gemini_thoughts: db.flag("streamGeminiThoughts"),
        additional_params,
    };
    let mode = entry.get("mode").map(Jv::string).unwrap_or_else(|| "model".to_owned());
    let mut descriptor = model_descriptor(model, b);

    let owned: Vec<(Text, Option<Text>, Option<Text>)> = custom_models.iter()
        .map(|found| (found.need("id").text(b), found.get("url").map(|value| value.text(b)), found.get("key").map(|value| value.text(b)))).collect();
    let references: Vec<CustomModelRef> = owned.iter().map(|(id, url, key)| CustomModelRef { id, url: url.as_ref(), key: key.as_ref() }).collect();
    let (proxy_key, force_replace_url, custom_proxy_request_model) = (db.text("proxyKey", b), db.text("forceReplaceUrl", b), db.text("customProxyRequestModel", b));
    let target = resolve_target(&TargetSource {
        ai_model: &ai_model, proxy_key: &proxy_key, force_replace_url: &force_replace_url, custom_proxy_request_model: &custom_proxy_request_model, custom_models: &references,
    }, b)?;
    target.apply(&mut descriptor, b)?;

    let stream = entry.get("stream").is_some_and(Jv::flag);
    let max_tokens = entry.get("maxTokens").map(Jv::num).unwrap_or_else(|| db.num("maxResponse"));
    let messages = entry.need("messages").list().iter().map(|item| message(item, b)).collect();
    let replacement = Some(db.text("systemContentReplacement", b)).filter(|value| value.len_units() != 0);
    let role = match db.need("systemRoleReplacement").string().as_str() {
        "" => None, "system" => Some(Role::System), "user" => Some(Role::User), "assistant" => Some(Role::Assistant), other => panic!("role {other}"),
    };
    let messages = reformat(messages, &descriptor.flags, &SystemReplacement { content: replacement.as_ref(), role }, b)?;
    let tools: Vec<ToolSpec> = entry.get("tools").map(|items| items.list().iter().map(|tool| ToolSpec {
        name: tool.need("name").text(b), description: tool.need("description").text(b), input_schema: to_value(tool.need("inputSchema"), b),
    }).collect()).unwrap_or_default();
    let schema = entry.get("schema").map(|value| to_value(value, b));
    let decodings = entry.get("decodings").map(|table| table.entries().to_vec()).unwrap_or_default();
    let call = ProviderCall { model: descriptor, messages, sampling: Vec::new(), bias: Vec::new(), tools, schema, stream, mode: t(&mode, b) };
    let now_ms = entry.get("clock").map(|clock| clock_ms(&clock.string())).unwrap_or(0);
    Ok(Prepared {
        call, key: target.key, custom_url: target.custom_url, max_tokens, image_response: entry.get("imageResponse").is_some_and(Jv::flag), now_ms,
        settings, sampling: sampling_settings(&db, b), decodings,
        client_email: db.text("vertexClientEmail", b), private_key: db.text("vertexPrivateKey", b), cached_token: db.text("vertexAccessToken", b),
        expires: db.num("vertexAccessTokenExpires"),
    })
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

fn build(prepared: &Prepared, bearer: Option<&Text>, b: &Budget) -> Result<BuiltRequest, BuildError> {
    let inputs = RequestInputs {
        key: prepared.key.as_ref(), custom_url: prepared.custom_url.as_ref(), max_tokens: prepared.max_tokens, image_response: prepared.image_response,
        bearer, sampling: &prepared.sampling, settings: &prepared.settings,
    };
    let mut decode = |marker: &Text| -> Result<Option<DecodedToolCall>, Error> {
        let key = lossy(&marker.trim(b)?.units().collect::<Vec<_>>());
        Ok(prepared.decodings.iter().find(|(name, _)| *name == key).map(|(_, entry)| decoded(entry, b)))
    };
    build_request(&prepared.call, &inputs, &mut decode, b)
}

fn mint_reply(reply: &Jv) -> Result<(u16, Vec<u8>), Text> {
    let b = budget();
    if let Some(message) = reply.get("throws") { return Err(t(&format!("TypeError: {}", message.string()), &b)); }
    let status = reply.get("status").map(Jv::num).unwrap_or(200.0) as u16;
    let body = if let Some(text) = reply.get("text") { text.string().into_bytes() } else { json_text(reply.need("json")).into_bytes() };
    Ok((status, body))
}

/// The body a `json` reply is sent as.
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

/// What the TypeScript run did, reduced to what the native path answers for.
#[derive(Debug, PartialEq)]
enum Ended { Built, Fail(String), Thrown(String) }

fn ended_of(preview: &Jv, outcome: &Jv) -> Ended {
    let _ = outcome;
    match preview.need("type").string().as_str() {
        "success" => Ended::Built,
        "fail" => Ended::Fail(preview.need("result").string()),
        "thrown" => Ended::Thrown(preview.need("error").string()),
        other => panic!("unexpected preview {other}"),
    }
}

fn assert_request(id: &str, which: &str, spec: &HttpRequestSpec, want: &Jv) {
    assert_eq!(want.need("method").string(), "POST", "{id}: {which}");
    assert_eq!(spec.url, want.need("url").string(), "{id}: {which} url");
    assert_eq!(spec.headers, sent_headers(want), "{id}: {which} headers");
    assert_eq!(text_of(&spec.body), want.need("body").string(), "{id}: {which} body");
}

const REQUEST_CASES: usize = 154;
const REQUEST_DECLINED: [&str; 14] = [
    "signature-with-setting-declines", "json-schema-enabled-declines", "schema-argument-declines", "project-with-unsafe-character-declines",
    "region-with-unsafe-character-declines", "key-with-ampersand-declines", "key-with-space-declines", "custom-model-url-with-query-declines",
    "custom-model-url-with-fragment-declines", "custom-model-unsafe-id-declines", "token-key-not-base64-declines", "token-key-bad-length-declines",
    "token-key-not-der-declines", "token-reply-number-token-declines",
];

#[test]
fn requests_and_tokens_match_the_typescript_run() {
    let b = budget();
    let fixture = parse(CASES[0], &b);
    let expected = parse(EXPECTED[0], &b);
    same_hash(CASES[0], &expected);
    let cases = fixture.need("cases").list();
    let observed = expected.need("cases").list();
    assert_eq!(cases.len(), REQUEST_CASES);
    assert_eq!(observed.len(), REQUEST_CASES);
    assert_eq!(ids_of(cases, "declines"), REQUEST_DECLINED, "the declined request cases changed");
    let (mut declined, mut compared) = (0, 0);
    for (entry, record) in cases.iter().zip(observed) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let result = record.need("result");
        let want = ended_of(result.need("preview"), result.need("outcome"));
        let decline = entry.get("declines").map(Jv::string);
        let check_decline = |found: &BuildError| {
            let name = decline.as_deref().unwrap_or_else(|| panic!("{id}: unexpected decline {found:?}"));
            assert!(matches!(found, BuildError::Unsupported(got) if *got == name), "{id}: expected the decline `{name}`, got {found:?}");
        };
        let prepared = match prepare(&fixture, entry, result.need("model"), &b) {
            Ok(prepared) => prepared,
            Err(BuildError::Thrown(text)) => {
                assert_eq!(want, Ended::Thrown(want_text(&want)), "{id}");
                assert!(want_text(&want).ends_with(&format!(": {}", s(&text))), "{id}: threw `{}`, TypeScript `{}`", s(&text), want_text(&want));
                compared += 1;
                continue;
            }
            Err(other) => panic!("{id}: {other:?}"),
        };
        let wire_requests = result.need("requests").list();
        let preview_requests = result.need("previewRequests").list();

        // The access token of a Vertex request.
        let vertex = prepared.call.model.format == LlmFormat::VertexAIGemini;
        let mut bearer: Option<Text> = None;
        let mut ended: Option<Ended> = None;
        let mut minted = false;
        if vertex {
            match token::plan(prepared.expires, prepared.now_ms, &prepared.client_email, &prepared.private_key) {
                TokenPlan::Cached => bearer = Some(prepared.cached_token.copy(&b).unwrap()),
                TokenPlan::Missing => ended = Some(Ended::Fail(token::MISSING_CREDENTIALS.to_owned())),
                TokenPlan::Mint => match token::token_request(&prepared.client_email, &prepared.private_key, prepared.now_ms, &b) {
                    Err(BuildError::Thrown(text)) => ended = Some(Ended::Thrown(s(&text))),
                    Err(found) => { check_decline(&found); declined += 1; continue; }
                    Ok(spec) => {
                        assert_request(&id, "token request (preview run)", &spec, &preview_requests[0]);
                        assert_request(&id, "token request", &spec, &wire_requests[0]);
                        let reply = mint_reply(&entry.need("mint").list()[0]);
                        let answer = match &reply { Ok((status, body)) => Ok((*status, body.as_slice())), Err(error) => Err(error) };
                        match token::read_token(answer, prepared.now_ms, &b) {
                            Ok(found) => {
                                let cache = result.need("cache");
                                assert_eq!(s(&found.token), cache.need("token").string(), "{id}: cached token");
                                assert_eq!(found.expires_ms as f64, cache.need("expires").num(), "{id}: token expiry");
                                bearer = Some(found.token);
                                minted = true;
                            }
                            Err(BuildError::Thrown(text)) => ended = Some(Ended::Thrown(s(&text))),
                            Err(found) => { check_decline(&found); declined += 1; continue; }
                        }
                    }
                },
            }
        }
        if let Some(ended) = ended {
            match (&ended, &want) {
                (Ended::Thrown(found), Ended::Thrown(shown)) => assert!(shown.ends_with(&format!(": {found}")), "{id}: threw `{found}`, TypeScript `{shown}`"),
                (found, shown) => assert_eq!(found, shown, "{id}"),
            }
            compared += 1;
            continue;
        }

        let built = match build(&prepared, bearer.as_ref(), &b) {
            Ok(built) => built,
            Err(BuildError::Thrown(text)) => {
                assert!(want_text(&want).ends_with(&format!(": {}", s(&text))), "{id}: threw `{}`, TypeScript `{}`", s(&text), want_text(&want));
                compared += 1;
                continue;
            }
            Err(found) => { check_decline(&found); declined += 1; continue; }
        };
        assert!(decline.is_none(), "{id}: expected a decline");
        assert_eq!(want, Ended::Built, "{id}");
        let sent = wire_requests.last().expect("the request");
        assert_request(&id, "request", &built.spec, sent);
        assert_eq!(built.spec.response, if result.need("outcome").need("type").string() == "streaming" { ResponseKind::Sse } else { ResponseKind::Buffered }, "{id}: response kind");
        assert_eq!(wire_requests.len(), if minted { 2 } else { 1 }, "{id}: requests");
        assert_eq!(s(&built.preview(&b).unwrap()), result.need("preview").need("result").string(), "{id}: preview");
        if !minted && vertex {
            assert_eq!(s(bearer.as_ref().unwrap()), result.need("cache").need("token").string(), "{id}: cached token");
        }
        compared += 1;
    }
    assert_eq!((declined, compared), (REQUEST_DECLINED.len(), REQUEST_CASES - REQUEST_DECLINED.len()));
}

fn want_text(want: &Ended) -> String {
    match want { Ended::Thrown(text) | Ended::Fail(text) => text.clone(), Ended::Built => String::new() }
}

// ---------------------------------------------------------------------------------------------------------------
// Replies.

/// The request of a reply case, with the cached Vertex token (these cases never let it expire).
fn built_for(fixture: &Jv, entry: &Jv, model: &Jv, b: &Budget) -> (Prepared, BuiltRequest) {
    let prepared = prepare(fixture, entry, model, b).unwrap_or_else(|error| panic!("{error:?}"));
    let bearer = (prepared.call.model.format == LlmFormat::VertexAIGemini).then(|| prepared.cached_token.copy(b).unwrap());
    let built = build(&prepared, bearer.as_ref(), b).unwrap_or_else(|error| panic!("{error:?}"));
    (prepared, built)
}

fn context(prepared: &Prepared) -> ReplyContext {
    ReplyContext {
        vertex: prepared.call.model.format == LlmFormat::VertexAIGemini,
        aborted: false,
        anti_server_overloads: prepared.settings.anti_server_overloads,
        save_signatures: prepared.settings.save_signatures,
    }
}

fn chunk_bytes(chunk: &Jv) -> Vec<u8> {
    if let Some(text) = chunk.get("utf8") { return text.string().into_bytes(); }
    let hex = chunk.need("hex").string();
    (0..hex.len() / 2).map(|at| u8::from_str_radix(&hex[at * 2..at * 2 + 2], 16).unwrap()).collect()
}

fn stringified(value: &Value, b: &Budget) -> String { s(&json::stringify(value, b).unwrap().expect("a value")) }

const RESPONSE_CASES: usize = 57;
const RESPONSE_DECLINED: [&str; 6] = [
    "text-number-declines", "text-object-declines", "parts-object-declines", "function-call-declines", "signature-with-setting-declines",
    "inline-audio-declines",
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
        let ctx = context(&prepared);
        let replies = entry.need("replies").list();
        let wire_requests = result.need("requests").list();
        let outcome = result.need("outcome");
        let mut attempt = 0;
        let (status, ended) = loop {
            let (status, body) = mint_reply(&replies[attempt]).unwrap_or_else(|_| panic!("{id}: a transport error"));
            match read_buffered(status, &body, &ctx, &b) {
                Ok(Buffered::Retry) => attempt += 1,
                other => break (status, other),
            }
        };
        if let Some(name) = entry.get("declines").map(Jv::string) {
            let error = ended.expect_err(&format!("{id}: expected the decline `{name}`"));
            assert!(matches!(&error, BuildError::Unsupported(found) if *found == name), "{id}: expected the decline `{name}`, got {error:?}");
            let settled = settle(error, Progress::default(), FailureStage::BufferedJson);
            assert!(matches!(settled, Settled::FallBack(found) if found == name), "{id}: a decline before any effect falls back");
            continue;
        }
        assert_eq!(wire_requests.len(), attempt + 1, "{id}: the number of requests");
        for (at, sent) in wire_requests.iter().enumerate() { assert_request(&id, &format!("request {at}"), &built.spec, sent); }
        let kind = outcome.need("type").string();
        match ended {
            Ok(Buffered::Success { text, meta }) => {
                assert_eq!(kind, "success", "{id}");
                assert_eq!(s(&text), outcome.need("result").string(), "{id}: result");
                let shown: Vec<[String; 3]> = meta.iter().map(|event| [&event.usage_metadata, &event.model_status, &event.prompt_feedback]
                    .map(|value| if matches!(value, Value::Undefined) { "undefined".to_owned() } else { stringified(value, &b) })).collect();
                match id.as_str() {
                    "extra-fields-ignored" => assert_eq!(shown, [["{\"promptTokenCount\":3}".to_owned(), "undefined".into(), "undefined".into()]], "{id}: interceptor payload"),
                    "usage-metadata-with-status-and-feedback" => assert_eq!(shown, [["{\"totalTokenCount\":5}".to_owned(), "{\"a\":1}".into(), "{\"blockReason\":\"NONE\"}".into()]], "{id}: interceptor payload"),
                    _ => assert!(shown.is_empty(), "{id}: no interceptor payload"),
                }
            }
            Ok(Buffered::Fail { text, server_error, failure }) => {
                assert_eq!(kind, "fail", "{id}");
                assert_eq!(s(&text), outcome.need("result").string(), "{id}: result");
                assert_eq!(server_error, outcome.get("failByServerError").is_some_and(Jv::flag), "{id}: failByServerError");
                if !(200..300).contains(&status) { assert_eq!(failure.http_status, Some(status), "{id}: failure status"); }
            }
            Ok(Buffered::Retry) => unreachable!(),
            Err(BuildError::Thrown(text)) => {
                assert_eq!(kind, "thrown", "{id}");
                assert!(outcome.need("error").string().ends_with(&format!(": {}", s(&text))), "{id}: threw `{}`, TypeScript `{}`", s(&text), outcome.need("error").string());
            }
            Err(other) => panic!("{id}: {other:?}"),
        }
        compared += 1;
    }
    assert_eq!(compared, RESPONSE_CASES - RESPONSE_DECLINED.len());
}

const STREAM_CASES: usize = 66;
const STREAM_DECLINED: [&str; 5] = ["text-number-declines", "text-after-text-object-declines", "function-call-after-text-declines", "function-call-first-declines", "signature-with-setting-declines"];

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
        let ctx = context(&prepared);
        let replies = entry.need("replies").list();
        let wire_requests = result.need("requests").list();
        let outcome = result.need("outcome");
        let mut attempt = 0;
        let mut failed = false;
        loop {
            let reply = &replies[attempt];
            let status = reply.get("status").map(Jv::num).unwrap_or(200.0) as u16;
            if reply.get("chunks").is_some() && status == 200 { break; }
            let body = reply.need("text").string().into_bytes();
            match read_stream_start(status, &body, &ctx, &b).unwrap() {
                Buffered::Retry => attempt += 1,
                Buffered::Fail { text, server_error, failure } => {
                    assert_eq!(wire_requests.len(), attempt + 1, "{id}: the number of requests");
                    assert_eq!(outcome.need("type").string(), "fail", "{id}");
                    assert_eq!(s(&text), outcome.need("result").string(), "{id}: result");
                    assert_eq!(server_error, outcome.get("failByServerError").is_some_and(Jv::flag), "{id}: failByServerError");
                    assert_eq!(failure.http_status, Some(status), "{id}: failure status");
                    failed = true;
                    break;
                }
                other => panic!("{id}: {other:?}"),
            }
        }
        if failed { compared += 1; continue; }
        // A declined tool round sends a follow-up request that the native path leaves to the TypeScript one.
        let declined = entry.has("declines");
        assert!(declined || wire_requests.len() == attempt + 1, "{id}: the number of requests");
        for (at, sent) in wire_requests.iter().enumerate().take(attempt + 1) { assert_request(&id, &format!("request {at}"), &built.spec, sent); }
        let mut session = StreamSession::new(prepared.settings.stream_gemini_thoughts, prepared.settings.save_signatures);
        let mut snapshots = Vec::new();
        let mut stopped = None;
        for chunk in replies[attempt].need("chunks").list() {
            match session.feed(&chunk_bytes(chunk), &b) {
                Ok(Some(text)) => snapshots.push(s(&text)),
                Ok(None) => {}
                Err(error) => { stopped = Some(error); break; }
            }
        }
        if let Some(name) = entry.get("declines").map(Jv::string) {
            let error = stopped.unwrap_or_else(|| panic!("{id}: expected the decline `{name}`"));
            assert!(matches!(&error, BuildError::Unsupported(found) if *found == name), "{id}: expected the decline `{name}`, got {error:?}");
            let settled = settle(error, Progress { emitted: !snapshots.is_empty(), tools_ran: false }, FailureStage::Body);
            match (entry.has("terminal"), settled) {
                (true, Settled::Terminal(terminal)) => assert_eq!(terminal.reason, name, "{id}"),
                (false, Settled::FallBack(found)) => assert_eq!(found, name, "{id}"),
                (_, other) => panic!("{id}: unexpected settlement {other:?}"),
            }
            continue;
        }
        assert!(stopped.is_none(), "{id}: {stopped:?}");
        assert_eq!(outcome.need("type").string(), "streaming", "{id}");
        assert!(!outcome.has("error"), "{id}: the stream ended with an error");
        let want: Vec<String> = outcome.need("snapshots").list().iter().map(|snapshot| {
            assert_eq!(snapshot.entries().len(), 1, "{id}: a stream snapshot carries only the displayed text");
            snapshot.need("0").string()
        }).collect();
        assert_eq!(snapshots, want, "{id}: snapshots");
        let shown = session.finish().map(|found| (found.usage_metadata, found.model_status));
        match id.as_str() {
            "usage-metadata" => assert_eq!(shown, Some((Some("{\"totalTokenCount\":5}".to_owned()), None)), "{id}: interceptor payload"),
            "usage-metadata-and-model-status" => assert_eq!(shown, Some((Some("{\"a\":1}".to_owned()), Some("{\"b\":2}".to_owned()))), "{id}: interceptor payload"),
            _ => assert_eq!(shown, None, "{id}: no interceptor payload"),
        }
        compared += 1;
    }
    assert_eq!(compared, STREAM_CASES - STREAM_DECLINED.len());
}
