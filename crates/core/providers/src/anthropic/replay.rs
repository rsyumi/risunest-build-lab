//! Replays the committed TypeScript observations (`tests/fixtures/rust-core/providers/anthropic`) through the native
//! builders. The fixtures are produced by `benchmarks/rust-core-oracles/providers-anthropic.oracle.test.ts` from the real
//! `requestChatDataMain`; nothing in this file knows what a request should look like.

use super::owner::{self, answer_tool_use, read_buffered, resolve_target, BufferedOutcome, BufferedReply, CustomModelRef, Progress, Settled, TargetSource};
use super::batch::{BatchReply, BatchSession, Step};
use super::request::{build_request, push_tool_round, BuiltRequest, DecodedToolCall, RequestInputs, ToolResponsePart};
use super::response::{Buffered, ParseContext};
use super::stream::StreamSession;
use super::settings::{Settings, ThinkingType};
use super::BuildError;
use crate::common::js::tests::budget;
use crate::common::params;
use crate::common::reformat::{reformat, SystemReplacement};
use crate::common::replay_kit::{clock_ms, lossy, message, model_descriptor, pair_list, parse, same_hash, sampling_settings, t, to_value, Db, Jv};
use risunest_ir::chat::Role;
use risunest_ir::provider::{FailureCategory, FailureStage, ProviderCall, ProviderFailure, ToolSpec};
use risunest_ir::{json, Budget, Error, Text, Value};

const CASES: [&[u8]; 5] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/anthropic/requests.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/anthropic/responses.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/anthropic/streams.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/anthropic/bedrock.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/anthropic/batching.cases.json"),
];
const EXPECTED: [&[u8]; 5] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/anthropic/requests.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/anthropic/responses.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/anthropic/streams.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/anthropic/bedrock.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/anthropic/batching.expected.json"),
];

fn s(text: &Text) -> String { lossy(&text.units().collect::<Vec<_>>()) }

fn ids_of(cases: &[Jv], field: &str) -> Vec<String> {
    cases.iter().filter(|entry| entry.has(field)).map(|entry| entry.need("id").string()).collect()
}

fn settings(db: &Db, ai_model: &Text, custom_models: &[Jv], b: &Budget) -> Settings {
    let custom_params = custom_models.iter().find(|model| model.need("id").text(b).equals(ai_model))
        .and_then(|model| model.get("params")).map(|value| value.text(b));
    let additional_params = params::additional_parameters(
        ai_model, &pair_list(db.need("additionalParams"), b), db.flag("applyAdditionalParamsToAll"), custom_params.as_ref(), b,
    ).unwrap();
    Settings {
        claude_api_key: db.text("claudeAPIKey", b),
        proxy_key: db.text("proxyKey", b),
        autofill_request_url: db.flag("autofillRequestUrl"),
        claude_one_hour_caching: db.flag("claude1HourCaching"),
        thinking_type: match db.need("thinkingType").string().as_str() {
            "off" => ThinkingType::Off, "budget" => ThinkingType::Budget, "adaptive" => ThinkingType::Adaptive, _ => ThinkingType::Other,
        },
        adaptive_thinking_effort: match db.need("adaptiveThinkingEffort") { Jv::Null => None, value => Some(value.text(b)) },
        use_plain_fetch: db.flag("usePlainFetch"),
        claude_batching: db.flag("claudeBatching"),
        additional_params,
    }
}

/// Everything the owner resolves before `build_request`, kept alive for the request's lifetime.
struct Prepared {
    ai_model: Text,
    call: ProviderCall,
    key: Option<Text>,
    custom_url: Option<Text>,
    max_tokens: f64,
    now_ms: i64,
    bedrock: bool,
    settings: Settings,
    decodings: Vec<(String, Jv)>,
}

fn prepare(fixture: &Jv, entry: &Jv, model: &Jv, b: &Budget) -> Result<Prepared, BuildError> {
    let db = Db::new(fixture, entry);
    let ai_model = entry.need("model").text(b);
    let custom_models: Vec<Jv> = entry.get("customModels").map(|items| items.list().to_vec()).unwrap_or_default();
    let settings = settings(&db, &ai_model, &custom_models, b);
    let mode = entry.get("mode").map(Jv::string).unwrap_or_else(|| "model".to_owned());
    let mut descriptor = model_descriptor(model, b);

    let owned: Vec<(Text, Option<Text>, Option<Text>)> = custom_models.iter()
        .map(|model| (model.need("id").text(b), model.get("url").map(|value| value.text(b)), model.get("key").map(|value| value.text(b)))).collect();
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
    let sampling = params::apply_parameters(&descriptor.parameters, &sampling_settings(&db, b), &t(&mode, b), &descriptor.id, b)?;
    let tools: Vec<ToolSpec> = entry.get("tools").map(|items| items.list().iter().map(|tool| ToolSpec {
        name: tool.need("name").text(b), description: tool.need("description").text(b), input_schema: to_value(tool.need("inputSchema"), b),
    }).collect()).unwrap_or_default();
    let decodings = entry.get("decodings").map(|table| table.entries().to_vec()).unwrap_or_default();
    let call = ProviderCall { model: descriptor, messages, sampling, bias: Vec::new(), tools, schema: None, stream, mode: t(&mode, b) };
    let now_ms = entry.get("clock").map(|clock| clock_ms(&clock.string())).unwrap_or(0);
    let bedrock = call.model.format == risunest_ir::provider::LlmFormat::AWSBedrockClaude && !ai_model.equals_ascii(b"reverse_proxy");
    Ok(Prepared { ai_model, call, key: target.key, custom_url: target.custom_url, max_tokens, now_ms, bedrock, settings, decodings })
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
    let inputs = RequestInputs {
        ai_model: &prepared.ai_model, key: prepared.key.as_ref(), custom_url: prepared.custom_url.as_ref(), max_tokens: prepared.max_tokens, now_ms: prepared.now_ms, settings: &prepared.settings,
    };
    let mut decode = |marker: &Text| -> Result<Option<DecodedToolCall>, Error> {
        let key = lossy(&marker.trim(b)?.units().collect::<Vec<_>>());
        Ok(prepared.decodings.iter().find(|(name, _)| *name == key).map(|(_, entry)| decoded(entry, b)))
    };
    build_request(&prepared.call, &inputs, &mut decode, b)
}

fn text_of(bytes: &[u8]) -> String { String::from_utf8(bytes.to_vec()).expect("utf-8") }

fn sent_headers(request: &Jv) -> Vec<(String, String)> {
    request.need("headers").list().iter().map(|pair| (pair.list()[0].string(), pair.list()[1].string())).collect()
}

// ---------------------------------------------------------------------------------------------------------------
// Requests.

const REQUEST_CASES: usize = 113;
const REQUEST_DECLINED: [&str; 3] = ["proxy-empty-url-is-kept", "custom-model-empty-url-is-kept", "custom-model-userinfo-url"];

#[test]
fn request_bytes_match_the_typescript_requests() {
    let b = budget();
    let fixture = parse(CASES[0], &b);
    let expected = parse(EXPECTED[0], &b);
    same_hash(CASES[0], &expected);
    let cases = fixture.need("cases").list();
    let observed = expected.need("cases").list();
    assert_eq!(cases.len(), observed.len());
    assert_eq!(cases.len(), REQUEST_CASES, "the number of request cases changed");
    assert_eq!(ids_of(cases, "declines"), REQUEST_DECLINED, "the declined request cases changed");
    let (mut sent, mut failed, mut thrown) = (0, Vec::new(), Vec::new());
    for (entry, record) in cases.iter().zip(observed) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let result = record.need("result");
        let outcome = prepare(&fixture, entry, result.need("model"), &b).and_then(|prepared| build(&prepared, &b));
        if entry.has("declines") {
            let name = entry.need("declines").string();
            assert!(matches!(&outcome, Err(BuildError::Unsupported(found)) if *found == name), "{id}: expected the decline `{name}`, got {outcome:?}");
            continue;
        }
        let requests = result.need("requests").list();
        if requests.is_empty() {
            match outcome {
                Err(BuildError::Fail(text)) => { assert_eq!(s(&text), result.need("preview").need("result").string(), "{id}"); failed.push(id); }
                Err(BuildError::Thrown(text)) => {
                    assert_eq!(format!("TypeError: {}", s(&text)), result.need("preview").need("error").string(), "{id}");
                    thrown.push(id);
                }
                other => panic!("{id}: expected the same failure as TypeScript, got {other:?}"),
            }
            continue;
        }
        let built = outcome.unwrap_or_else(|error| panic!("{id}: {error:?}"));
        assert_eq!(requests.len(), 1, "{id}");
        let request = &requests[0];
        assert_eq!(built.spec.method, risunest_ir::provider::HttpMethod::Post, "{id}");
        assert_eq!(request.need("method").string(), "POST", "{id}");
        assert_eq!(text_of(&built.spec.body), request.need("body").string(), "{id}: body");
        assert_eq!(s(&built.preview(&b).unwrap()), result.need("preview").need("result").string(), "{id}: preview");
        if !entry.has("transport") {
            assert_eq!(built.spec.headers, sent_headers(request), "{id}: headers");
            assert_eq!(built.spec.url, request.need("url").string(), "{id}: the URL handed to fetch");
        }
        let streaming = result.need("outcome").need("type").string() == "streaming";
        assert_eq!(built.spec.response == risunest_ir::provider::ResponseKind::Sse, streaming, "{id}: response kind");
        sent += 1;
    }
    assert_eq!(failed, ["legacy-no-input-fails", "legacy-function-only-fails"]);
    assert_eq!(thrown, ["first-system-only-throws"]);
    assert_eq!(sent + failed.len() + thrown.len(), REQUEST_CASES - REQUEST_DECLINED.len());
}

/// A case marked `declines` must end as the decline it names: before a snapshot or a tool run the owner falls back, and
/// afterwards (`terminal`) the request ends with a failure.
fn assert_declined(id: &str, entry: &Jv, error: BuildError, progress: Progress, stage: FailureStage) {
    let name = entry.need("declines").string();
    let terminal = entry.get("terminal").is_some_and(Jv::flag);
    match owner::settle(error, progress, stage) {
        Settled::FallBack(found) => assert!(!terminal && found == name, "{id}: fell back with `{found}`, expected `{name}` (terminal: {terminal})"),
        Settled::Terminal(found) => {
            assert!(terminal && found.reason == name, "{id}: terminal `{}`, expected `{name}` (terminal: {terminal})", found.reason);
            assert!(matches!(found.failure.category, FailureCategory::Compat) && found.failure.http_status.is_none() && found.failure.sdk_message.is_none(), "{id}");
        }
        other => panic!("{id}: expected the decline `{name}`, got {other:?}"),
    }
}

/// `text.trim()` cut to 2048 UTF-16 units, dropping a character that would not fit whole.
fn bounded(text: &str) -> String {
    let (mut units, mut kept) = (0, String::new());
    for character in text.trim().chars() {
        units += character.len_utf16();
        if units > 2048 { break; }
        kept.push(character);
    }
    kept
}

fn replies_of(entry: &Jv) -> Vec<Jv> {
    match entry.get("replies") {
        Some(list) => list.list().to_vec(),
        None => entry.get("reply").into_iter().cloned().collect(),
    }
}

fn reply_status(reply: &Jv) -> u16 { reply.get("status").map(Jv::num).unwrap_or(200.0) as u16 }

// ---------------------------------------------------------------------------------------------------------------
// Buffered responses, including the tool rounds `requestClaudeHTTP` runs.

#[derive(Debug, PartialEq)]
enum Out { Success(String), Fail(String), Thrown(String) }

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind { Json, NotJson, Transport }

fn tool_parts(reply: &Jv, b: &Budget) -> Vec<ToolResponsePart> {
    reply.list().iter().map(|item| match item.need("type").string().as_str() {
        "text" => ToolResponsePart::Text(item.need("text").text(b)),
        "image" => ToolResponsePart::Image { mime_type: item.need("mimeType").text(b), data: item.need("data").text(b) },
        other => ToolResponsePart::Other(t(other, b)),
    }).collect()
}

struct Loop<'a> {
    prepared: &'a Prepared,
    extract_json: Option<Text>,
    remember: bool,
    replies: std::collections::VecDeque<Jv>,
    tool_replies: std::collections::VecDeque<Jv>,
    bodies: Vec<String>,
    tool_names: Vec<String>,
    progress: Progress,
    /// The kind and status of the reply the outcome came from, which decides the failure a `Fail` maps to.
    last: Option<(Kind, u16)>,
    remembered: Text,
}

impl Loop<'_> {
    fn drive(&mut self, built: &mut BuiltRequest, b: &Budget) -> Result<(Buffered, Option<ProviderFailure>), BuildError> {
        loop {
            self.bodies.push(text_of(&built.spec.body));
            // The harness answers an unscripted request with an event stream when the body asks for one.
            let streaming = parse(built.spec.body.as_slice(), b).get("stream").is_some_and(|value| matches!(value, Jv::Bool(true)));
            let unscripted: &[u8] = if streaming { br#"{"text":"data: [DONE]\n\n"}"# } else { br#"{"json":{"content":[{"type":"text","text":"ok"}]}}"# };
            let reply = self.replies.pop_front().unwrap_or_else(|| parse(unscripted, b));
            let status = reply_status(&reply);
            let context = ParseContext { extract_json: self.extract_json.as_ref(), additional_output: &self.remembered, bedrock: self.prepared.bedrock };
            let (kind, outcome) = if let Some(message) = reply.get("throws") {
                let error = t(&format!("TypeError: {}", message.string()), b);
                (Kind::Transport, read_buffered(&BufferedReply::Transport { error: &error }, &context, b)?)
            } else if let Some(source) = reply.get("text") {
                let body = owner::response_text(source.string().as_bytes(), b)?;
                match json::parse(&source.string(), b) {
                    Ok(data) => (Kind::Json, read_buffered(&BufferedReply::Json { status, data: &data }, &context, b)?),
                    Err(_) => (Kind::NotJson, read_buffered(&BufferedReply::NotJson { status, body: &body }, &context, b)?),
                }
            } else {
                let data = to_value(reply.need("json"), b);
                (Kind::Json, read_buffered(&BufferedReply::Json { status, data: &data }, &context, b)?)
            };
            self.last = Some((kind, status));
            let BufferedOutcome { result, failure } = outcome;
            let Buffered::ToolUse(round) = result else { return Ok((result, failure)) };

            let mut ran = false;
            let answered = {
                let tool_replies = &mut self.tool_replies;
                let tool_names = &mut self.tool_names;
                let mut run = |name: &Text, _input: &Value| -> Result<Result<Vec<ToolResponsePart>, Text>, Error> {
                    ran = true;
                    tool_names.push(s(name));
                    let scripted = tool_replies.pop_front().expect("scripted tool reply");
                    if let Some(message) = scripted.get("throws") { return Ok(Err(t(&message.string(), b))); }
                    Ok(Ok(tool_parts(&scripted, b)))
                };
                let mut encode = |call: &super::response::ToolUseCall, _parts: &[ToolResponsePart]| -> Result<Result<Text, Text>, Error> {
                    Ok(Ok(t(&format!("<tool_call>{}\u{f100}{}</tool_call>\n\n", s(call.id.as_ref().expect("a text id")), s(&call.name)), b)))
                };
                answer_tool_use(&round, self.remember, &mut run, &mut encode, b)
            };
            self.progress.tools_ran |= ran;
            let answered = answered?;
            if let Some(code) = &answered.remembered { self.remembered.append(code)?; }
            push_tool_round(built, &round, &answered.replies, b)?;
        }
    }
}

fn outcome_of(record: &Jv) -> Out {
    let outcome = record.need("outcome");
    match outcome.need("type").string().as_str() {
        "success" => Out::Success(outcome.need("result").string()),
        "fail" => Out::Fail(outcome.need("result").string()),
        "thrown" => Out::Thrown(outcome.need("error").string()),
        other => panic!("unexpected outcome {other}"),
    }
}

/// The text a `ProviderFailure` keeps, worked out from the text TS shows.
fn expected_message(kind: Kind, shown: &str) -> String {
    match kind {
        // `JSON.stringify("SyntaxError: <message>")`: the message without the class name.
        Kind::NotJson | Kind::Transport => {
            let decoded: String = serde_json::from_str(shown).expect("a JSON string literal");
            bounded(decoded.split_once(": ").map_or(decoded.as_str(), |(_, message)| message))
        }
        Kind::Json => bounded(shown),
    }
}

const RESPONSE_CASES: usize = 76;
const RESPONSE_DECLINED: [&str; 6] = [
    "content-string-declines", "content-object-declines", "content-null-entry-declines", "extract-json-dotted-object-declines",
    "tool-name-not-text-declines", "tool-reverse-proxy-null-message-declines",
];

/// Replays the reply side of one case: the replies (and tool replies) it scripts through the buffered reader, against what
/// TypeScript returned. `false` when the case is a decline.
fn check_reply_case(fixture: &Jv, entry: &Jv, result: &Jv, id: &str, b: &Budget) -> bool {
    let prepared = prepare(fixture, entry, result.need("model"), b).unwrap_or_else(|error| panic!("{id}: {error:?}"));
    let db = Db::new(fixture, entry);
    let extract_json = (entry.has("extractJson") && db.flag("jsonSchemaEnabled")).then(|| db.text("jsonSchema", b));
    let mut machine = Loop {
        prepared: &prepared,
        extract_json,
        remember: entry.get("rememberToolUsage").is_some_and(Jv::flag),
        replies: replies_of(entry).into(),
        tool_replies: entry.get("toolReplies").map(|list| list.list().to_vec()).unwrap_or_default().into(),
        bodies: Vec::new(),
        tool_names: Vec::new(),
        progress: Progress::default(),
        last: None,
        remembered: t("", b),
    };
    let mut built = build(machine.prepared, b).unwrap_or_else(|error| panic!("{id}: {error:?}"));
    let driven = machine.drive(&mut built, b);
    if entry.has("declines") {
        assert_declined(id, entry, driven.expect_err(id), machine.progress, FailureStage::BufferedJson);
        return false;
    }
    let sent: Vec<String> = result.need("requests").list().iter().map(|request| request.need("body").string()).collect();
    assert_eq!(machine.bodies, sent, "{id}: request bodies");
    let called: Vec<String> = result.need("toolCalls").list().iter().map(|call| call.need("name").string()).collect();
    assert_eq!(machine.tool_names, called, "{id}: tools run");
    let want = outcome_of(result);
    let (buffered, failure) = match driven {
        Ok(pair) => pair,
        Err(BuildError::Thrown(text)) => {
            let Out::Thrown(shown) = want else { panic!("{id}: threw, expected {want:?}") };
            assert!(shown.ends_with(&format!(": {}", s(&text))), "{id}: threw `{}`, expected `{shown}`", s(&text));
            return true;
        }
        Err(other) => panic!("{id}: {other:?}"),
    };
    let out = match &buffered {
        Buffered::Success(text) => Out::Success(s(text)),
        Buffered::Fail { text, server_error } => {
            match result.need("outcome").get("failByServerError") {
                Some(flag) => assert_eq!(*server_error, flag.flag(), "{id}: failByServerError"),
                None => assert!(!server_error, "{id}: failByServerError"),
            }
            Out::Fail(s(text))
        }
        Buffered::ToolUse(_) => panic!("{id}: a tool round is not an outcome"),
    };
    assert_eq!(out, want, "{id}: outcome");
    if let Out::Fail(shown) = &out {
        let failure = failure.unwrap_or_else(|| panic!("{id}: a failure without a ProviderFailure"));
        let (kind, status) = machine.last.expect("a reply");
        match kind {
            Kind::Json => {
                assert_eq!(failure.http_status, Some(status), "{id}");
                let category = if (200..300).contains(&status) { FailureCategory::Decode } else { FailureCategory::Http };
                assert_eq!(failure.category, category, "{id}");
            }
            Kind::NotJson => {
                assert_eq!(failure.http_status, Some(status), "{id}");
                assert_eq!(failure.category, FailureCategory::Decode, "{id}");
            }
            Kind::Transport => {
                assert_eq!(failure.http_status, None, "{id}");
                assert_eq!(failure.category, FailureCategory::Network, "{id}");
            }
        }
        assert_eq!(failure.stage, Some(FailureStage::BufferedJson), "{id}");
        assert_eq!(s(failure.sdk_message.as_ref().expect("a message")), expected_message(kind, shown), "{id}: sdk message");
    } else {
        assert!(failure.is_none(), "{id}: a failure for a successful outcome");
    }
    true
}

#[test]
fn buffered_responses_match_the_typescript_outcomes() {
    let b = budget();
    let fixture = parse(CASES[1], &b);
    let expected = parse(EXPECTED[1], &b);
    same_hash(CASES[1], &expected);
    let cases = fixture.need("cases").list();
    let observed = expected.need("cases").list();
    assert_eq!(cases.len(), RESPONSE_CASES);
    assert_eq!(observed.len(), RESPONSE_CASES);
    assert_eq!(ids_of(cases, "declines"), RESPONSE_DECLINED, "the declined response cases changed");
    let mut matched = 0;
    for (entry, record) in cases.iter().zip(observed) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let result = record.need("result");
        if check_reply_case(&fixture, entry, result, &id, &b) { matched += 1; }
    }
    assert_eq!(matched, RESPONSE_CASES - RESPONSE_DECLINED.len());
}

// ---------------------------------------------------------------------------------------------------------------
// Streams.

fn chunk_bytes(chunk: &Jv) -> Vec<u8> {
    if let Some(text) = chunk.get("utf8") { return text.string().into_bytes(); }
    let hex = chunk.need("hex").string();
    (0..hex.len() / 2).map(|index| u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).unwrap()).collect()
}

fn body_of(reply: &Jv) -> Vec<Vec<u8>> {
    if let Some(chunks) = reply.get("chunks") { return chunks.list().iter().map(chunk_bytes).collect(); }
    if let Some(text) = reply.get("text") { return vec![text.string().into_bytes()]; }
    if let Some(json) = reply.get("json") { return vec![json_text(json).into_bytes()]; }
    Vec::new()
}

/// The body a `json` reply is sent as.
fn json_text(value: &Jv) -> String {
    match value {
        Jv::Null => "null".into(),
        Jv::Bool(flag) => flag.to_string(),
        Jv::Num(number) => number.to_string(),
        Jv::Str(units) => format!("{:?}", lossy(units)),
        Jv::Arr(items) => format!("[{}]", items.iter().map(json_text).collect::<Vec<_>>().join(",")),
        Jv::Obj(entries) => format!("{{{}}}", entries.iter().map(|(key, item)| format!("{key:?}:{}", json_text(item))).collect::<Vec<_>>().join(",")),
    }
}

#[derive(Debug, PartialEq)]
enum StreamOut { Snapshots(Vec<String>), Fail(String) }

fn stream_outcome(record: &Jv) -> StreamOut {
    let outcome = record.need("outcome");
    match outcome.need("type").string().as_str() {
        "streaming" => {
            assert!(!outcome.has("error"), "the stream ended with an error");
            StreamOut::Snapshots(outcome.need("snapshots").list().iter().map(|snapshot| {
                assert_eq!(snapshot.entries().len(), 1, "a stream snapshot carries only the displayed text");
                snapshot.need("0").string()
            }).collect())
        }
        "fail" => StreamOut::Fail(outcome.need("result").string()),
        other => panic!("unexpected outcome {other}"),
    }
}

const STREAM_CASES: usize = 65;
const STREAM_DECLINED: [&str; 2] = ["text-delta-object-after-text-declines", "text-delta-object-first-declines"];

#[test]
fn streams_match_the_typescript_snapshots() {
    let b = budget();
    let fixture = parse(CASES[2], &b);
    let expected = parse(EXPECTED[2], &b);
    same_hash(CASES[2], &expected);
    let cases = fixture.need("cases").list();
    let observed = expected.need("cases").list();
    assert_eq!(cases.len(), STREAM_CASES);
    assert_eq!(observed.len(), STREAM_CASES);
    assert_eq!(ids_of(cases, "declines"), STREAM_DECLINED, "the declined stream cases changed");
    let mut matched = 0;
    for (entry, record) in cases.iter().zip(observed) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let result = record.need("result");
        let prepared = prepare(&fixture, entry, result.need("model"), &b).unwrap_or_else(|error| panic!("{id}: {error:?}"));
        let built = build(&prepared, &b).unwrap_or_else(|error| panic!("{id}: {error:?}"));
        let sent: Vec<String> = result.need("requests").list().iter().map(|request| request.need("body").string()).collect();
        assert_eq!(sent.first().cloned(), Some(text_of(&built.spec.body)), "{id}: request body");
        let first = replies_of(entry).remove(0);
        let status = reply_status(&first);
        let body = body_of(&first);
        if status != 200 {
            let (shown, failure) = owner::stream_start_failure(status, &body.concat(), &b).unwrap();
            assert_eq!(StreamOut::Fail(s(&shown)), stream_outcome(result), "{id}");
            assert_eq!((failure.http_status, &failure.category), (Some(status), &FailureCategory::Http), "{id}");
            assert_eq!(failure.stage, Some(FailureStage::Body), "{id}");
            assert_eq!(s(failure.sdk_message.as_ref().expect("a message")), bounded(&s(&shown)), "{id}: sdk message");
            matched += 1;
            continue;
        }
        let mut session = StreamSession::new();
        let mut snapshots = Vec::new();
        let mut failed = None;
        for chunk in &body {
            match session.feed(chunk, &b) {
                Ok(text) => snapshots.push(s(&text)),
                Err(error) => { failed = Some(error); break; }
            }
        }
        if entry.has("declines") {
            let error = failed.unwrap_or_else(|| panic!("{id}: expected a decline"));
            assert_declined(&id, entry, error, Progress { emitted: !snapshots.is_empty(), tools_ran: false }, FailureStage::Transform);
            continue;
        }
        assert!(failed.is_none(), "{id}: {failed:?}");
        if let Some(text) = session.finish(&b).unwrap() { snapshots.push(s(&text)); }
        assert_eq!(StreamOut::Snapshots(snapshots), stream_outcome(result), "{id}: snapshots");
        matched += 1;
    }
    assert_eq!(matched, STREAM_CASES - STREAM_DECLINED.len());
}

// ---------------------------------------------------------------------------------------------------------------
// Declines and redaction.

fn prepared_case(id: &str, b: &Budget) -> Prepared {
    let fixture = parse(CASES[0], b);
    let expected = parse(EXPECTED[0], b);
    let at = fixture.need("cases").list().iter().position(|entry| entry.need("id").string() == id).expect("case");
    prepare(&fixture, &fixture.need("cases").list()[at], expected.need("cases").list()[at].need("result").need("model"), b).unwrap()
}

#[test]
fn debug_output_keeps_secrets_and_prompts_out() {
    let b = budget();
    let prepared = prepared_case("custom-model-params", &b);
    let built = build(&prepared, &b).unwrap();
    let shown = format!("{built:?} {:?}", prepared.settings);
    for secret in ["sk-ant-synthetic", "sk-custom-synthetic", "yes", "Hello", "Be brief"] {
        assert!(!shown.contains(secret), "Debug prints `{secret}`: {shown}");
    }
    assert!(shown.contains("Content-Type") && shown.contains("application/json"), "{shown}");
}

// ---------------------------------------------------------------------------------------------------------------
// Bedrock: the signed request at a fixed clock, and the reply read by the Bedrock branch.

const BEDROCK_CASES: usize = 128;
const BEDROCK_DECLINED: [&str; 8] = [
    "model-slash-declines", "model-space-declines", "model-percent-declines", "model-non-ascii-declines", "region-with-dot-declines",
    "region-with-slash-declines", "param-header-lone-surrogate-declines", "param-header-non-ascii-name-declines",
];

#[test]
fn bedrock_requests_and_replies_match_the_typescript_run() {
    let b = budget();
    let fixture = parse(CASES[3], &b);
    let expected = parse(EXPECTED[3], &b);
    same_hash(CASES[3], &expected);
    let cases = fixture.need("cases").list();
    let observed = expected.need("cases").list();
    assert_eq!(cases.len(), BEDROCK_CASES);
    assert_eq!(observed.len(), BEDROCK_CASES);
    assert_eq!(ids_of(cases, "declines"), BEDROCK_DECLINED, "the declined Bedrock cases changed");
    let (mut declined, mut threw, mut signed) = (0, 0, 0);
    for (entry, record) in cases.iter().zip(observed) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let result = record.need("result");
        let prepared = prepare(&fixture, entry, result.need("model"), &b).unwrap_or_else(|error| panic!("{id}: {error:?}"));
        let outcome = build(&prepared, &b);
        if entry.has("declines") {
            let name = entry.need("declines").string();
            assert!(matches!(&outcome, Err(BuildError::Unsupported(found)) if *found == name), "{id}: expected the decline `{name}`, got {outcome:?}");
            declined += 1;
            continue;
        }
        let requests = result.need("requests").list();
        if requests.is_empty() {
            let Err(BuildError::Thrown(text)) = outcome else { panic!("{id}: expected the TypeScript throw, got {outcome:?}") };
            for shown in [result.need("preview").need("error").string(), result.need("outcome").need("error").string()] {
                assert!(shown.ends_with(&format!(": {}", s(&text))), "{id}: threw `{}`, TypeScript `{shown}`", s(&text));
            }
            threw += 1;
            continue;
        }
        let built = outcome.unwrap_or_else(|error| panic!("{id}: {error:?}"));
        assert_eq!(requests.len(), 1, "{id}");
        let request = &requests[0];
        assert_eq!(request.need("method").string(), "POST", "{id}");
        assert_eq!(built.spec.method, risunest_ir::provider::HttpMethod::Post, "{id}");
        assert_eq!(built.spec.url, request.need("url").string(), "{id}: url");
        assert_eq!(built.spec.headers, sent_headers(request), "{id}: signed headers");
        assert_eq!(text_of(&built.spec.body), request.need("body").string(), "{id}: body");
        assert_eq!(built.spec.response, risunest_ir::provider::ResponseKind::Buffered, "{id}: Bedrock never streams");
        let preview = result.need("preview");
        assert_eq!(preview.need("type").string(), "success", "{id}");
        assert_eq!(s(&built.preview(&b).unwrap()), preview.need("result").string(), "{id}: preview");
        check_reply_case(&fixture, entry, result, &id, &b);
        signed += 1;
    }
    assert_eq!((declined, threw), (BEDROCK_DECLINED.len(), 7));
    assert_eq!(signed, BEDROCK_CASES - declined - threw);
}

// ---------------------------------------------------------------------------------------------------------------
// claudeBatching: the machine against the tape of the TypeScript run, with a fake clock and the abort at the n-th wait.

const BATCH_CASES: usize = 125;
const BATCH_DECLINED: [&str; 6] = [
    "create-id-number-declines", "create-id-object-declines", "create-id-true-declines", "create-id-with-slash-declines",
    "create-id-with-space-declines", "create-id-with-query-declines",
];

#[derive(Debug, PartialEq)]
enum Ended { Rejected(String), Thrown(String), Streamed { snapshots: Vec<String>, error: Option<String> } }

struct Run { ended: Result<Ended, BuildError>, sent: Vec<super::batch::BatchRequest>, sleeps: Vec<u32> }

fn run_batch(prepared: &Prepared, entry: &Jv, built: &BuiltRequest, b: &Budget) -> Run {
    let mut replies: std::collections::VecDeque<Jv> = replies_of(entry).into();
    let abort_at = entry.get("abortAtSleep").map(|value| value.num() as usize).unwrap_or(0);
    let (mut sent, mut sleeps, mut clock) = (Vec::new(), Vec::new(), prepared.now_ms);
    let mut snapshots = Vec::new();
    let ended = (|| {
        let (mut session, mut step) = BatchSession::start(built, &t("00000000-0000-4000-8000-000000000001", b), b)?;
        loop {
            step = match step {
                Step::Send(request) => {
                    sent.push(request);
                    let reply = replies.pop_front().unwrap_or_else(|| parse(br#"{"json":{"content":[{"type":"text","text":"ok"}]}}"#, b));
                    clock += reply.get("advanceMs").map_or(0, |value| value.num() as i64);
                    // The harness ends a tape that runs dry by moving the clock a simulated day.
                    if sent.len() > 400 { clock += 90_000_000; }
                    let error;
                    let body;
                    let answer = if let Some(message) = reply.get("throws") {
                        error = t(&format!("TypeError: {}", message.string()), b);
                        BatchReply::Transport { error: &error }
                    } else {
                        body = body_of(&reply).concat();
                        BatchReply::Http { status: reply_status(&reply), body: &body }
                    };
                    session.replied(answer, clock, b)?
                }
                Step::Wait { ms } => {
                    sleeps.push(ms);
                    clock += i64::from(ms);
                    session.waited(abort_at != 0 && sleeps.len() >= abort_at, clock, b)?
                }
                Step::Rejected(text) => return Ok(Ended::Rejected(s(&text))),
                Step::Done(text) => {
                    snapshots.extend(text.map(|text| s(&text)));
                    return Ok(Ended::Streamed { snapshots, error: None });
                }
                Step::Error(text) => {
                    let message = s(&text);
                    return Ok(Ended::Streamed { snapshots, error: Some(if message.is_empty() { "Error".into() } else { format!("Error: {message}") }) });
                }
            };
            if let Step::Wait { .. } = &step { assert!(session.is_streaming()); }
        }
    })();
    let ended = match ended {
        Err(BuildError::Thrown(text)) => Ok(Ended::Thrown(s(&text))),
        other => other,
    };
    Run { ended, sent, sleeps }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' { decoded.push(u8::from_str_radix(&text[at + 1..at + 3], 16).unwrap()); at += 3; } else { decoded.push(bytes[at]); at += 1; }
    }
    String::from_utf8(decoded).expect("utf-8")
}

fn batch_outcome(record: &Jv) -> Ended {
    let outcome = record.need("outcome");
    match outcome.need("type").string().as_str() {
        "fail" => Ended::Rejected(outcome.need("result").string()),
        "thrown" => Ended::Thrown(outcome.need("error").string()),
        "streaming" => Ended::Streamed {
            snapshots: outcome.need("snapshots").list().iter().map(|snapshot| snapshot.need("0").string()).collect(),
            error: outcome.get("error").map(Jv::string),
        },
        other => panic!("unexpected outcome {other}"),
    }
}

#[test]
fn claude_batching_matches_the_typescript_run() {
    let b = budget();
    let fixture = parse(CASES[4], &b);
    let expected = parse(EXPECTED[4], &b);
    same_hash(CASES[4], &expected);
    let cases = fixture.need("cases").list();
    let observed = expected.need("cases").list();
    assert_eq!(cases.len(), BATCH_CASES);
    assert_eq!(observed.len(), BATCH_CASES);
    assert_eq!(ids_of(cases, "declines"), BATCH_DECLINED, "the declined batching cases changed");
    let (mut declined, mut compared) = (0, 0);
    for (entry, record) in cases.iter().zip(observed) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let result = record.need("result");
        let prepared = prepare(&fixture, entry, result.need("model"), &b).unwrap_or_else(|error| panic!("{id}: {error:?}"));
        let built = build(&prepared, &b).unwrap_or_else(|error| panic!("{id}: {error:?}"));
        assert_eq!(s(&built.preview(&b).unwrap()), result.need("preview").need("result").string(), "{id}: preview");
        let run = run_batch(&prepared, entry, &built, &b);
        let wanted = result.need("requests").list();
        if entry.has("declines") {
            let Err(error) = run.ended else { panic!("{id}: expected a decline") };
            // The batch exists on the server by now, so the decline cannot fall back to the TS path.
            assert_declined(&id, entry, error, Progress { emitted: false, tools_ran: true }, FailureStage::Transform);
            assert_eq!(run.sent.len(), 1, "{id}");
            declined += 1;
            continue;
        }
        assert_eq!(run.sent.len(), wanted.len(), "{id}: request count");
        for (at, (sent, want)) in run.sent.iter().zip(wanted).enumerate() {
            let method = if sent.spec.method == risunest_ir::provider::HttpMethod::Post { "POST" } else { "GET" };
            assert_eq!(method, want.need("method").string(), "{id}: request {at} method");
            if entry.has("transport") {
                // The web build sends every request through the proxy, which carries the target in two headers.
                let carried = sent_headers(want);
                let header = |name: &str| percent_decode(&carried.iter().find(|(key, _)| key == name).unwrap_or_else(|| panic!("{id}: no {name}")).1);
                assert_eq!(sent.spec.url, header("risu-url"), "{id}: request {at} url");
                let inner = parse(header("risu-header").as_bytes(), &b);
                let pairs: Vec<(String, String)> = inner.entries().iter().map(|(name, value)| (name.clone(), value.string())).collect();
                assert_eq!(sent.spec.headers, pairs, "{id}: request {at} headers");
            } else {
                assert_eq!(sent.spec.url, want.need("url").string(), "{id}: request {at} url");
                assert_eq!(sent.spec.headers, sent_headers(want), "{id}: request {at} headers");
            }
            let body = want.get("body").filter(|body| !matches!(body, Jv::Null)).map(Jv::string).unwrap_or_default();
            assert_eq!(text_of(&sent.spec.body), body, "{id}: request {at} body");
            // The create request and every read before the cancel carry the abort signal; the cancel and what follows do not.
            let cancelled = run.sent[..=at].iter().any(|earlier| earlier.spec.url.ends_with("/cancel"));
            assert_eq!(sent.abortable, !cancelled, "{id}: request {at} signal");
        }
        let sleeps: Vec<u32> = result.need("sleeps").list().iter().map(|ms| ms.num() as u32).collect();
        assert_eq!(run.sleeps, sleeps, "{id}: waits");
        let ended = run.ended.unwrap_or_else(|error| panic!("{id}: {error:?}"));
        match (&ended, batch_outcome(result)) {
            (Ended::Thrown(found), Ended::Thrown(shown)) => assert!(shown.ends_with(&format!(": {found}")), "{id}: threw `{found}`, TypeScript `{shown}`"),
            (found, shown) => assert_eq!(*found, shown, "{id}"),
        }
        compared += 1;
    }
    assert_eq!((declined, compared), (BATCH_DECLINED.len(), BATCH_CASES - BATCH_DECLINED.len()));
}
