//! Replays the committed TypeScript observations (`tests/fixtures/rust-core/providers/openai-compatible`) through the
//! native builders. The fixtures are produced by `benchmarks/rust-core-oracles/providers-openai-compatible.oracle.test.ts`
//! from the real `requestChatDataMain`; nothing in this file knows what a request should look like.

use super::js::tests::budget;
use super::params;
use super::reformat::reformat;
use super::owner::{self, read_buffered, resolve_target, BufferedOutcome, BufferedReply, CustomModelRef, Progress, Settled, TargetSource, ToolRun};
use super::request::{build_request, resolve_logit_bias, BuiltRequest, DecodedToolCall, RequestInputs};
use super::response::{Buffered, ParseContext, ToolCallEntry};
use super::settings::{OpenRouterProvider, Settings};
use super::stream::{SessionOptions, StreamContext, StreamEnd, StreamSession};
use super::BuildError;
use risunest_ir::chat::{MultimodalData, PromptMessage, Role};
use risunest_ir::provider::{FailureCategory, FailureStage, LlmFlag, LlmFormat, ProviderCall, ProviderFailure, ToolSpec};
use risunest_ir::{json, Budget, Error, Text, Value};
use crate::common::replay_kit::{lossy, message, model_descriptor, pair_list, parse, same_hash, sampling_settings, t, to_value, Db, Jv};

const CASES: [&[u8]; 3] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/openai-compatible/requests.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/openai-compatible/responses.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/openai-compatible/streams.cases.json"),
];
const EXPECTED: [&[u8]; 3] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/openai-compatible/requests.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/openai-compatible/responses.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/openai-compatible/streams.expected.json"),
];

const NANOGPT_CASES: [&[u8]; 3] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/nanogpt/requests.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/nanogpt/responses.cases.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/nanogpt/streams.cases.json"),
];
const NANOGPT_EXPECTED: [&[u8]; 3] = [
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/nanogpt/requests.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/nanogpt/responses.expected.json"),
    include_bytes!("../../../../../tests/fixtures/rust-core/providers/nanogpt/streams.expected.json"),
];

// ---------------------------------------------------------------------------------------------------------------
// A cloneable JSON tree that keeps key order, for navigating the fixtures.

// ---------------------------------------------------------------------------------------------------------------
// Case inputs.

fn settings(db: &Db, ai_model: &Text, custom_models: &[Jv], b: &Budget) -> Settings {
    let custom_params = custom_models.iter().find(|model| model.need("id").text(b).equals(ai_model))
        .and_then(|model| model.get("params")).map(|value| value.text(b));
    let additional_params = params::additional_parameters(
        ai_model, &pair_list(db.need("additionalParams"), b), db.flag("applyAdditionalParamsToAll"), custom_params.as_ref(), b,
    ).unwrap();
    let provider = db.get("openrouterProvider").filter(|value| matches!(value, Jv::Obj(_))).map(|value| {
        let list = |name: &str| value.get(name).map(|items| items.list().iter().map(|item| item.text(b)).collect()).unwrap_or_default();
        OpenRouterProvider { order: list("order"), only: list("only"), ignore: list("ignore") }
    });
    let replacement = db.text("systemContentReplacement", b);
    let role = match db.need("systemRoleReplacement").string().as_str() {
        "" => None, "system" => Some(Role::System), "user" => Some(Role::User), "assistant" => Some(Role::Assistant), other => panic!("role {other}"),
    };
    Settings {
        new_oai_handle: db.flag("newOAIHandle"),
        gpt_vision_quality: db.text("gptVisionQuality", b),
        reverse_proxy_ooba_mode: db.flag("reverseProxyOobaMode"),
        reverse_proxy_ooba_args: to_value(db.need("reverseProxyOobaArgs"), b),
        proxy_request_model: db.text("proxyRequestModel", b),
        custom_proxy_request_model: db.text("customProxyRequestModel", b),
        openrouter_request_model: db.text("openrouterRequestModel", b),
        generation_seed: db.num("generationSeed"),
        json_schema_enabled: db.flag("jsonSchemaEnabled"),
        strict_json_schema: match db.need("strictJsonSchema") { Jv::Bool(flag) => Some(*flag), _ => None },
        oai_prediction: db.text("OAIPrediction", b),
        openrouter_fallback: db.flag("openrouterFallback"),
        openrouter_middle_out: db.flag("openrouterMiddleOut"),
        openrouter_provider: provider,
        use_instruct_prompt: db.flag("useInstructPrompt"),
        deepseek_thinking_enabled: db.need("deepseekThinkingType").string() == "enabled",
        deepseek_reasoning_effort: db.get("deepseekReasoningEffort").map(|value| value.text(b)),
        autofill_request_url: db.flag("autofillRequestUrl"),
        open_ai_flex_processing: db.flag("openAIFlexProcessing"),
        openai_key: db.text("openAIKey", b),
        proxy_key: db.text("proxyKey", b),
        openrouter_key: db.text("openrouterKey", b),
        nanogpt_key: db.get("nanogptKey").map(|value| value.text(b)).unwrap_or_else(|| t("", b)),
        nanogpt_request_model: db.get("nanogptRequestModel").map(|value| value.text(b)).unwrap_or_else(|| t("", b)),
        nanogpt_use_subscription_endpoint: db.get("nanogptUseSubscriptionEndpoint").is_some_and(Jv::flag),
        nanogpt_provider: db.get("nanogptProvider").map(|value| value.text(b)).unwrap_or_else(|| t("", b)),
        oai_comp_api_keys: db.need("OaiCompAPIKeys").entries().iter().map(|(name, key)| (t(name, b), key.text(b))).collect(),
        gen_time: db.num("genTime"),
        additional_params,
        system_content_replacement: Some(replacement).filter(|value| value.len_units() != 0),
        system_role_replacement: role,
    }
}

/// Everything the owner resolves before `build_request`, kept alive for the request's lifetime.
struct Prepared {
    ai_model: Text,
    call: ProviderCall,
    key: Option<Text>,
    custom_url: Option<Text>,
    key_identifier: Option<Text>,
    provider_is_openai: bool,
    max_tokens: f64,
    logit_bias: Vec<(Text, f64)>,
    multi_gen: bool,
    main_model_has_image_input: bool,
    settings: Settings,
    decodings: Vec<(String, Jv)>,
    simplified_tool_use: bool,
    remember_tool_usage: bool,
    flags: Vec<LlmFlag>,
    extract_json: Option<Text>,
}

fn prepare(fixture: &Jv, entry: &Jv, model: &Jv, b: &Budget) -> Prepared {
    let db = Db::new(fixture, entry);
    let ai_model = entry.need("model").text(b);
    let custom_models: Vec<Jv> = entry.get("customModels").map(|items| items.list().to_vec()).unwrap_or_default();
    let settings = settings(&db, &ai_model, &custom_models, b);
    let mode = entry.get("mode").map(Jv::string).unwrap_or_else(|| "model".to_owned());
    let mut descriptor = model_descriptor(model, b);
    let key_identifier = model.get("keyIdentifier").filter(|_| model.has("keyIdentifier")).map(|value| value.text(b));
    let provider_is_openai = model.need("provider").num() == 0.0;

    let owned: Vec<(Text, Option<Text>, Option<Text>)> = custom_models.iter()
        .map(|model| (model.need("id").text(b), model.get("url").map(|value| value.text(b)), model.get("key").map(|value| value.text(b)))).collect();
    let references: Vec<CustomModelRef> = owned.iter().map(|(id, url, key)| CustomModelRef { id, url: url.as_ref(), key: key.as_ref() }).collect();
    let (proxy_key, force_replace_url, custom_proxy_request_model) = (db.text("proxyKey", b), db.text("forceReplaceUrl", b), db.text("customProxyRequestModel", b));
    let target = resolve_target(&TargetSource {
        ai_model: &ai_model, proxy_key: &proxy_key, force_replace_url: &force_replace_url, custom_proxy_request_model: &custom_proxy_request_model, custom_models: &references,
    }, b).unwrap();
    target.apply(&mut descriptor, b).unwrap();

    let stream = entry.get("stream").is_some_and(Jv::flag);
    let multi_gen = owner::multi_gen(db.num("genTime"), &ai_model, entry.get("continue").is_some_and(Jv::flag), entry.get("noMultiGen").is_some_and(Jv::flag));
    let max_tokens = entry.get("maxTokens").map(Jv::num).unwrap_or_else(|| db.num("maxResponse"));

    let messages: Vec<PromptMessage> = entry.need("messages").list().iter().map(|item| message(item, b)).collect();
    let messages = reformat(messages, &descriptor.flags, &settings, b).expect("reformat");
    let sampling = params::apply_parameters(&descriptor.parameters, &sampling_settings(&db, b), &t(&mode, b), &descriptor.id, b).expect("sampling");

    // Token bias.
    let tokens = entry.get("tokens");
    let strong = entry.get("strong");
    let lookup = |table: Option<&Jv>, text: &Text| -> Vec<f64> {
        table.and_then(|table| table.get(&lossy(&text.units().collect::<Vec<_>>()))).map(|ids| ids.list().iter().map(Jv::num).collect()).unwrap_or_default()
    };
    let existing: Vec<(Text, f64)> = entry.get("bias").map(|bias| bias.entries().iter().map(|(id, value)| (t(id, b), value.num())).collect()).unwrap_or_default();
    let bias_string: Vec<(Text, f64)> = entry.get("biasString").map(|items| items.list().iter().map(|pair| (pair.list()[0].text(b), pair.list()[1].num())).collect()).unwrap_or_default();
    let logit_bias = resolve_logit_bias(
        &existing, &bias_string, &mut |text| Ok(lookup(tokens, text)), &mut |text| Ok(lookup(strong, text)), b,
    ).unwrap();

    // Structured output: `convertInterfaceToSchema` of a JSON text is `JSON.parse`.
    let schema_text = entry.get("schema").map(Jv::string).or_else(|| (db.flag("jsonSchemaEnabled") && !descriptor.flags.contains(&LlmFlag::NoStructuredOutput)).then(|| db.need("jsonSchema").string()));
    let schema = schema_text.map(|text| json::parse_json(&text, b).expect("schema"));

    let tools: Vec<ToolSpec> = entry.get("tools").map(|items| items.list().iter().map(|tool| ToolSpec {
        name: tool.need("name").text(b), description: tool.need("description").text(b), input_schema: to_value(tool.need("inputSchema"), b),
    }).collect()).unwrap_or_default();

    let main_model_has_image_input = model.need("mainImageInput").flag();
    let flags = descriptor.flags.clone();
    let extract_json = entry.get("extractJson").map(|value| value.text(b)).filter(|value| value.len_units() != 0)
        .filter(|_| db.flag("jsonSchemaEnabled") || entry.has("schema"));
    let decodings = entry.get("decodings").map(|table| table.entries().to_vec()).unwrap_or_default();

    let call = ProviderCall { model: descriptor, messages, sampling, bias: Vec::new(), tools, schema, stream, mode: t(&mode, b) };
    Prepared {
        ai_model, call,
        key: target.key.or_else(|| entry.get("key").map(|value| value.text(b))),
        custom_url: target.custom_url.or_else(|| entry.get("customURL").map(|value| value.text(b))),
        key_identifier, provider_is_openai, max_tokens, logit_bias, multi_gen,
        main_model_has_image_input, settings, decodings, simplified_tool_use: db.flag("simplifiedToolUse"),
        remember_tool_usage: entry.get("rememberToolUsage").is_some_and(Jv::flag), flags, extract_json,
    }
}

fn build(prepared: &Prepared, b: &Budget) -> Result<BuiltRequest, BuildError> {
    let inputs = RequestInputs {
        ai_model: &prepared.ai_model,
        provider_is_openai: prepared.provider_is_openai,
        key_identifier: prepared.key_identifier.as_ref(),
        key: prepared.key.as_ref(),
        custom_url: prepared.custom_url.as_ref(),
        max_tokens: prepared.max_tokens,
        logit_bias: &prepared.logit_bias,
        multi_gen: prepared.multi_gen,
        main_model_has_image_input: prepared.main_model_has_image_input,
        settings: &prepared.settings,
    };
    let mut decode = |marker: &Text| -> Result<Option<DecodedToolCall>, Error> {
        let key = lossy(&marker.trim(b)?.units().collect::<Vec<_>>());
        let Some((_, entry)) = prepared.decodings.iter().find(|(name, _)| *name == key) else { return Ok(None) };
        let call = entry.need("call");
        Ok(Some(DecodedToolCall {
            id: call.need("id").text(b),
            name: call.need("name").text(b),
            arguments: call.need("arg").text(b),
            response_texts: entry.need("response").list().iter().filter(|item| item.need("type").string() == "text").map(|item| item.need("text").text(b)).collect(),
        }))
    };
    build_request(&prepared.call, &inputs, &mut decode, b)
}

fn text_of(bytes: &[u8]) -> String { String::from_utf8(bytes.to_vec()).expect("utf-8") }

fn sent_headers(request: &Jv) -> Vec<(String, String)> {
    request.need("headers").list().iter().map(|pair| (pair.list()[0].string(), pair.list()[1].string())).collect()
}

fn s(text: &Text) -> String { lossy(&text.units().collect::<Vec<_>>()) }

fn ids_of(cases: &[Jv], field: &str) -> Vec<String> {
    cases.iter().filter(|entry| entry.has(field)).map(|entry| entry.need("id").string()).collect()
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

// ---------------------------------------------------------------------------------------------------------------
// Requests.

const REQUEST_CASES: usize = 108;
const REQUEST_DECLINED: [&str; 4] = ["proxy-url-userinfo-declines", "proxy-url-dot-segment-declines", "proxy-url-numeric-host-declines", "proxy-url-not-a-url-declines"];

#[test]
fn request_bytes_match_the_typescript_requests() {
    replay_requests(CASES[0], EXPECTED[0], REQUEST_CASES, &REQUEST_DECLINED, &["gpt4o-tools-with-multigen-fails"], true);
}

#[test]
fn nanogpt_requests_match_the_typescript_requests() {
    replay_requests(NANOGPT_CASES[0], NANOGPT_EXPECTED[0], 33, &[], &[], false);
}

fn replay_requests(cases_bytes: &[u8], expected_bytes: &[u8], count: usize, declines: &[&str], failures: &[&str], check_flags: bool) {
    let b = budget();
    let fixture = parse(cases_bytes, &b);
    let expected = parse(expected_bytes, &b);
    same_hash(cases_bytes, &expected);
    let cases = fixture.need("cases").list();
    let observed = expected.need("cases").list();
    assert_eq!(cases.len(), count);
    assert_eq!(observed.len(), count);
    assert_eq!(ids_of(cases, "declines"), declines, "the declined request cases changed");
    let (mut sent, mut failed) = (0, Vec::new());
    for (entry, record) in cases.iter().zip(observed) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let result = record.need("result");
        let prepared = prepare(&fixture, entry, result.need("model"), &b);
        let outcome = build(&prepared, &b);
        if entry.has("declines") {
            let name = entry.need("declines").string();
            assert!(matches!(&outcome, Err(BuildError::Unsupported(found)) if *found == name), "{id}: expected the decline `{name}`, got {outcome:?}");
            continue;
        }
        let requests = result.need("requests").list();
        if requests.is_empty() {
            let Err(BuildError::Fail(text)) = outcome else { panic!("{id}: expected the same failure as TypeScript, got {outcome:?}") };
            assert_eq!(s(&text), result.need("preview").need("result").string(), "{id}");
            failed.push(id);
            continue;
        }
        let built = outcome.unwrap_or_else(|error| panic!("{id}: {error:?}"));
        assert_eq!(requests.len(), 1, "{id}");
        let request = &requests[0];
        assert_eq!(built.spec.method, risunest_ir::provider::HttpMethod::Post, "{id}");
        assert_eq!(request.need("method").string(), "POST", "{id}");
        let body = text_of(&built.spec.body);
        assert_eq!(body, request.need("body").string(), "{id}: body");
        assert_eq!(built.spec.headers, sent_headers(request), "{id}: headers");
        assert_eq!(built.spec.url, request.need("url").string(), "{id}: the URL handed to fetch");
        assert_eq!(s(&built.preview(&b).unwrap()), result.need("preview").need("result").string(), "{id}: preview");
        let streaming = result.need("outcome").need("type").string() == "streaming";
        assert_eq!(built.spec.response == risunest_ir::provider::ResponseKind::Sse, streaming, "{id}: response kind");
        sent += 1;
    }
    assert_eq!(failed, failures);
    assert_eq!(sent + failed.len(), count - declines.len());
    // The corpus must exercise the flags the builder branches on, not only name them.
    if !check_flags { return; }
    let seen: std::collections::HashSet<String> = observed.iter().flat_map(|record| record.need("result").need("model").need("flags").list().iter().map(Jv::string).collect::<Vec<_>>()).collect();
    for flag in ["DeveloperRole", "OAICompletionTokens", "requiresAlternateRole", "mustStartWithUserInput", "hasFirstSystemPrompt", "noStructuredOutput", "deepSeekPrefix", "deepSeekThinkingInput", "deepSeekThinkingToggle"] {
        assert!(seen.contains(flag), "no request case runs with the {flag} flag");
    }
}

#[test]
fn unsupported_inputs_decline_instead_of_guessing() {
    let b = budget();
    let fixture = parse(CASES[0], &b);
    let expected = parse(EXPECTED[0], &b);
    let find = |id: &str| {
        let at = fixture.need("cases").list().iter().position(|entry| entry.need("id").string() == id).expect(id);
        (&fixture.need("cases").list()[at], &expected.need("cases").list()[at])
    };
    let declined = |prepared: &Prepared| match build(prepared, &b) { Err(BuildError::Unsupported(name)) => name, other => panic!("expected a decline, got {other:?}") };

    let (entry, record) = find("gpt4o-basic");
    let mut prepared = prepare(&fixture, entry, record.need("result").need("model"), &b);
    prepared.call.model.format = LlmFormat::Mistral;
    assert_eq!(declined(&prepared), "format");

    let (entry, record) = find("openrouter-basic");
    let mut prepared = prepare(&fixture, entry, record.need("result").need("model"), &b);
    prepared.settings.use_instruct_prompt = true;
    assert_eq!(declined(&prepared), "openrouter-instruct-prompt");

    // `risu/free` is resolved by a network call in the owner; the literal id is not a model.
    let (entry, record) = find("openrouter-basic");
    let mut prepared = prepare(&fixture, entry, record.need("result").need("model"), &b);
    prepared.settings.openrouter_request_model = t("risu/free", &b);
    assert_eq!(declined(&prepared), "openrouter-free-model-unresolved");
    prepared.settings.openrouter_request_model = t("openai/gpt-4o-mini", &b);
    assert!(build(&prepared, &b).is_ok());

    let (entry, record) = find("gpt4o-image-low");
    let mut prepared = prepare(&fixture, entry, record.need("result").need("model"), &b);
    prepared.call.messages[1].multimodals.as_mut().unwrap()[0].data = MultimodalData::Asset(risunest_ir::chat::AssetRef(t("asset-1", &b)));
    assert_eq!(declined(&prepared), "multimodal-asset-reference");

    let (entry, record) = find("gpt4o-basic");
    let mut prepared = prepare(&fixture, entry, record.need("result").need("model"), &b);
    prepared.settings.additional_params = vec![(t("__proto__.polluted", &b), t("1", &b))];
    assert!(matches!(build(&prepared, &b), Err(BuildError::Unsupported(_))));
}

#[test]
fn debug_output_keeps_secrets_and_prompts_out() {
    let b = budget();
    let fixture = parse(CASES[0], &b);
    let expected = parse(EXPECTED[0], &b);
    let at = fixture.need("cases").list().iter().position(|entry| entry.need("id").string() == "proxy-additional-params").expect("case");
    let (entry, record) = (&fixture.need("cases").list()[at], &expected.need("cases").list()[at]);
    let prepared = prepare(&fixture, entry, record.need("result").need("model"), &b);
    let built = build(&prepared, &b).unwrap();
    let shown = format!("{built:?} {:?}", prepared.settings);
    for secret in ["sk-synthetic", "Bearer", "Hello", "Be brief", "acme"] {
        assert!(!shown.contains(secret), "Debug prints `{secret}`: {shown}");
    }
    assert!(shown.contains("Content-Type") && shown.contains("application/json"), "{shown}");
}

// ---------------------------------------------------------------------------------------------------------------
// Buffered responses, including the tool loop `requestHTTPOpenAI` runs.

#[derive(Debug, PartialEq)]
enum Out { Success(String), Multiline(Vec<String>), Fail(String) }

fn out_of(result: &Buffered) -> Out {
    match result {
        Buffered::Success(text) => Out::Success(s(text)),
        Buffered::Multiline(items) => Out::Multiline(items.iter().map(s).collect()),
        Buffered::Fail(text) => Out::Fail(s(text)),
        Buffered::ToolCalls(_) => panic!("a tool round is not an outcome"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind { Json, NotJson, Transport }

struct Loop<'a> {
    prepared: &'a Prepared,
    prefix: &'a Text,
    replies: std::collections::VecDeque<Jv>,
    tool_replies: std::collections::VecDeque<Jv>,
    bodies: Vec<String>,
    tool_names: Vec<String>,
    progress: Progress,
    /// The kind and status of the first reply, which decides the failure a top-level `Fail` maps to.
    first: Option<(Kind, u16)>,
    /// The top-level failure came from composing a tool round, not from a reply.
    composed_failure: bool,
}

impl Loop<'_> {
    fn drive(&mut self, built: &mut BuiltRequest, depth: usize, b: &Budget) -> Result<(Buffered, Option<ProviderFailure>), BuildError> {
        self.bodies.push(text_of(&built.spec.body));
        let reply = self.replies.pop_front().unwrap_or_else(|| {
            let units = |text: &str| Jv::Str(text.encode_utf16().collect());
            let message = Jv::Obj(vec![("role".into(), units("assistant")), ("content".into(), units("ok"))]);
            Jv::Obj(vec![("json".into(), Jv::Obj(vec![("choices".into(), Jv::Arr(vec![Jv::Obj(vec![("message".into(), message)])]))]))])
        });
        let status = reply.get("status").map(Jv::num).unwrap_or(200.0) as u16;
        let context = ParseContext {
            flags: &self.prepared.flags,
            multi_gen: self.prepared.multi_gen,
            extract_json: self.prepared.extract_json.as_ref(),
            http_error_prefix: self.prefix,
        };
        let (kind, outcome) = if let Some(message) = reply.get("throws") {
            let error = t(&format!("TypeError: {}", message.string()), b);
            (Kind::Transport, read_buffered(&BufferedReply::Transport { error: &error }, &context, b)?)
        } else if let Some(text) = reply.get("text") {
            let body = owner::response_text(text.string().as_bytes(), b)?;
            (Kind::NotJson, read_buffered(&BufferedReply::NotJson { status, body: &body }, &context, b)?)
        } else {
            let data = to_value(reply.need("json"), b);
            (Kind::Json, read_buffered(&BufferedReply::Json { status, data: &data }, &context, b)?)
        };
        if depth == 0 { self.first = Some((kind, status)); }
        let BufferedOutcome { result, failure } = outcome;
        let Buffered::ToolCalls(round) = result else { return Ok((result, failure)) };

        let tools = &self.prepared.call.tools;
        let mut ran = false;
        let answered = {
            let tool_replies = &mut self.tool_replies;
            let tool_names = &mut self.tool_names;
            let mut run = |name: &Text, _arguments: &Value| -> Result<ToolRun, Error> {
                ran = true;
                tool_names.push(s(name));
                let scripted = tool_replies.pop_front().expect("scripted tool reply");
                if let Some(message) = scripted.get("throws") { return Ok(ToolRun::Threw(t(&format!("Error: {}", message.string()), b))); }
                Ok(ToolRun::Texts(scripted.list().iter().filter(|item| item.need("type").string() == "text").map(|item| item.need("text").text(b)).collect()))
            };
            let mut encode = |call: &ToolCallEntry, _texts: &[Text]| -> Result<Result<Text, Text>, Error> {
                Ok(Ok(t(&format!("<tool_call>{}\u{f100}{}</tool_call>\n\n", s(call.id_text().expect("string id")), s(call.tool_name().expect("string name"))), b)))
            };
            owner::answer_tool_calls(&round.calls, tools, self.prepared.remember_tool_usage, &mut run, &mut encode, b)?
        };
        self.progress.tools_ran |= ran;
        built.push_tool_round(&round.assistant, self.prepared.simplified_tool_use, &answered.replies, b)?;
        let (follow_up, _) = self.drive(built, depth + 1, b)?;
        let composed = owner::compose_tool_round(&round, &answered.call_codes, self.prepared.simplified_tool_use, follow_up, b)?;
        if depth == 0 && matches!(composed, Buffered::Fail(_)) { self.composed_failure = true; }
        Ok((composed, None))
    }
}

fn outcome_of(record: &Jv) -> Out {
    let outcome = record.need("outcome");
    match outcome.need("type").string().as_str() {
        "success" => Out::Success(outcome.need("result").string()),
        "fail" => Out::Fail(outcome.need("result").string()),
        "multiline" => Out::Multiline(outcome.need("result").list().iter().map(|pair| pair.list()[1].string()).collect()),
        other => panic!("unexpected outcome {other}"),
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

/// The text a `ProviderFailure` keeps, worked out from the text TS shows.
fn expected_message(kind: Kind, shown: &str, prefix: &str) -> String {
    let rest = shown.strip_prefix(prefix).expect("the failure starts with the httpError text");
    match kind {
        // `httpError + JSON.stringify("SyntaxError: <message>")`: the message without the class name.
        Kind::NotJson | Kind::Transport => {
            let decoded: String = serde_json::from_str(rest).expect("a JSON string literal");
            bounded(decoded.split_once(": ").map_or(decoded.as_str(), |(_, message)| message))
        }
        Kind::Json => bounded(rest),
    }
}

const RESPONSE_CASES: usize = 108;
const RESPONSE_DECLINED: [&str; 6] = [
    "extract-dotted-reaches-object", "text-body-truncated-json", "text-body-unmodelled-message", "tool-arguments-not-text-declines",
    "tool-second-response-tool-calls-string-declines-after-tools-ran", "tool-calls-string-declines-before-tools",
];

#[test]
fn buffered_responses_match_the_typescript_outcomes() {
    replay_responses(CASES[1], EXPECTED[1], RESPONSE_CASES, &RESPONSE_DECLINED);
}

#[test]
fn nanogpt_responses_match_the_typescript_outcomes() {
    replay_responses(NANOGPT_CASES[1], NANOGPT_EXPECTED[1], 13, &[]);
}

fn replay_responses(cases_bytes: &[u8], expected_bytes: &[u8], count: usize, declines: &[&str]) {
    let b = budget();
    let fixture = parse(cases_bytes, &b);
    let expected = parse(expected_bytes, &b);
    same_hash(cases_bytes, &expected);
    let cases = fixture.need("cases").list();
    let observed = expected.need("cases").list();
    assert_eq!(cases.len(), count);
    assert_eq!(observed.len(), count);
    assert_eq!(ids_of(cases, "declines"), declines, "the declined response cases changed");
    let mut matched = 0;
    for (entry, record) in cases.iter().zip(observed) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let result = record.need("result");
        let prepared = prepare(&fixture, entry, result.need("model"), &b);
        let prefix = result.need("httpError").text(&b);
        let replies = match entry.get("replies") {
            Some(list) => list.list().to_vec(),
            None => entry.get("reply").into_iter().cloned().collect(),
        };
        let mut machine = Loop {
            prepared: &prepared,
            prefix: &prefix,
            replies: replies.into(),
            tool_replies: entry.get("toolReplies").map(|list| list.list().to_vec()).unwrap_or_default().into(),
            bodies: Vec::new(),
            tool_names: Vec::new(),
            progress: Progress::default(),
            first: None,
            composed_failure: false,
        };
        let mut built = build(&prepared, &b).unwrap_or_else(|error| panic!("{id}: {error:?}"));
        let driven = machine.drive(&mut built, 0, &b);
        if entry.has("declines") {
            assert_declined(&id, entry, driven.expect_err(&id), machine.progress, FailureStage::BufferedJson);
            continue;
        }
        let (buffered, failure) = driven.unwrap_or_else(|error| panic!("{id}: {error:?}"));
        let out = out_of(&buffered);
        assert_eq!(out, outcome_of(result), "{id}: outcome");
        matched += 1;
        let sent: Vec<String> = result.need("requests").list().iter().map(|request| request.need("body").string()).collect();
        assert_eq!(machine.bodies, sent, "{id}: request bodies");
        let called: Vec<String> = result.need("toolCalls").list().iter().map(|call| call.need("name").string()).collect();
        assert_eq!(machine.tool_names, called, "{id}: tools run");
        // The failure a top-level `Fail` maps to.
        if let Out::Fail(shown) = &out {
            if machine.composed_failure { continue; }
            let failure = failure.unwrap_or_else(|| panic!("{id}: a failure without a ProviderFailure"));
            let (kind, status) = machine.first.expect("a reply");
            match kind {
                Kind::Json => {
                    assert_eq!(failure.http_status, Some(status), "{id}");
                    if (200..300).contains(&status) { assert!(matches!(failure.category, FailureCategory::Decode), "{id}"); } else { assert!(matches!(failure.category, FailureCategory::Http), "{id}"); }
                }
                Kind::NotJson => {
                    assert_eq!(failure.http_status, Some(status), "{id}");
                    assert!(matches!(failure.category, FailureCategory::Decode), "{id}");
                }
                Kind::Transport => {
                    assert_eq!(failure.http_status, None, "{id}");
                    assert!(matches!(failure.category, FailureCategory::Network), "{id}");
                }
            }
            assert_eq!(failure.stage, Some(FailureStage::BufferedJson), "{id}");
            assert_eq!(s(failure.sdk_message.as_ref().expect("a message")), expected_message(kind, shown, &s(&prefix)), "{id}: sdk message");
        } else {
            assert!(failure.is_none(), "{id}: a failure for a successful outcome");
        }
    }
    assert_eq!(matched, count - declines.len());
}

// ---------------------------------------------------------------------------------------------------------------
// Streams, including the tool rounds `wrapToolStream` runs.

fn chunk_bytes(chunk: &Jv) -> Vec<u8> {
    if let Some(text) = chunk.get("utf8") { return text.string().into_bytes(); }
    let hex = chunk.need("hex").string();
    (0..hex.len() / 2).map(|index| u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).unwrap()).collect()
}

#[derive(Debug, PartialEq)]
enum StreamOut { Snapshots(Vec<String>), Errored(Vec<String>, String), Fail(String) }

fn stream_outcome(record: &Jv) -> StreamOut {
    let outcome = record.need("outcome");
    match outcome.need("type").string().as_str() {
        "streaming" => {
            let snapshots: Vec<String> = outcome.need("snapshots").list().iter().map(|snapshot| {
                assert_eq!(snapshot.entries().len(), 1, "a stream snapshot carries only the displayed text");
                snapshot.need("0").string()
            }).collect();
            match outcome.get("error") { Some(error) => StreamOut::Errored(snapshots, error.string()), None => StreamOut::Snapshots(snapshots) }
        }
        "fail" => StreamOut::Fail(outcome.need("result").string()),
        other => panic!("unexpected outcome {other}"),
    }
}

fn reply_status(reply: &Jv) -> u16 { reply.get("status").map(Jv::num).unwrap_or(200.0) as u16 }

fn accepted(reply: &Jv, b: &Budget) -> bool {
    let content_type = reply.get("contentType").map(|value| value.text(b)).unwrap_or_else(|| t("text/event-stream", b));
    super::stream::stream_start(reply_status(reply), Some(&content_type), b).expect("content type") == super::stream::StreamStart::Accept
}

struct StreamRun<'a> {
    prepared: &'a Prepared,
    bodies: Vec<String>,
    ran: Vec<String>,
    progress: Progress,
}

fn drive_stream(run: &mut StreamRun, built: &mut BuiltRequest, replies: Vec<Jv>, tool_replies: Vec<Jv>, b: &Budget) -> Result<(StreamOut, Option<ProviderFailure>), BuildError> {
    let prepared = run.prepared;
    let mut replies: std::collections::VecDeque<Jv> = replies.into();
    let mut tool_replies: std::collections::VecDeque<Jv> = tool_replies.into();
    run.bodies.push(text_of(&built.spec.body));
    let first = replies.pop_front().unwrap_or_else(|| Jv::Obj(vec![]));
    if !accepted(&first, b) {
        let body: Vec<u8> = first.get("chunks").map(|chunks| chunks.list().iter().flat_map(chunk_bytes).collect()).unwrap_or_default();
        let (shown, failure) = owner::stream_start_failure(reply_status(&first), &body, b)?;
        return Ok((StreamOut::Fail(s(&shown)), Some(failure)));
    }
    let context = StreamContext { flags: prepared.flags.clone(), multi_gen: prepared.multi_gen, extract_json: prepared.extract_json.as_ref().map(|text| text.copy(b)).transpose()? };
    let options = SessionOptions { simplified_tool_use: prepared.simplified_tool_use, deepseek_thinking_enabled: prepared.settings.deepseek_thinking_enabled };
    let mut session = StreamSession::new(context, options, b)?;
    let mut snapshots = Vec::new();
    let mut current = first;
    loop {
        for chunk in current.get("chunks").map(|chunks| chunks.list().to_vec()).unwrap_or_default() {
            if let Some(shown) = session.push(&chunk_bytes(&chunk), b)? {
                snapshots.push(s(&shown));
                run.progress.emitted = true;
            }
        }
        let round = match session.finish(b)? {
            StreamEnd::Complete => break,
            StreamEnd::Errored(error) => return Ok((StreamOut::Errored(snapshots, s(&error)), None)),
            StreamEnd::ToolRound(round) => round,
        };
        let mut ran = false;
        let answered = {
            let names = &mut run.ran;
            let mut call = |name: &Text, _arguments: &Value| -> Result<ToolRun, Error> {
                ran = true;
                names.push(s(name));
                let scripted = tool_replies.pop_front().expect("scripted tool reply");
                if let Some(message) = scripted.get("throws") { return Ok(ToolRun::Threw(t(&format!("Error: {}", message.string()), b))); }
                Ok(ToolRun::Texts(scripted.list().iter().filter(|item| item.need("type").string() == "text").map(|item| item.need("text").text(b)).collect()))
            };
            let mut encode = |entry: &ToolCallEntry, _texts: &[Text]| -> Result<Result<Text, Text>, Error> {
                Ok(Ok(t(&format!("<tool_call>{}\u{f100}{}</tool_call>\n\n", s(entry.id_text().expect("string id")), s(entry.tool_name().expect("string name"))), b)))
            };
            owner::answer_tool_calls(&round.calls, &prepared.call.tools, prepared.remember_tool_usage, &mut call, &mut encode, b)?
        };
        run.progress.tools_ran |= ran;
        built.push_tool_round(&round.assistant, prepared.simplified_tool_use, &answered.replies, b)?;
        run.bodies.push(text_of(&built.spec.body));
        let Some(next) = replies.pop_front() else { break };
        if !accepted(&next, b) { break; }
        snapshots.push(s(&session.continue_after_tools(&round, &answered.call_codes, b)?));
        run.progress.emitted = true;
        current = next;
    }
    Ok((StreamOut::Snapshots(snapshots), None))
}

const STREAM_CASES: usize = 103;
const STREAM_DECLINED: [&str; 5] = [
    "multigen-inherited-index-first-chunk", "multigen-inherited-index-after-snapshot", "multigen-tool-calls-key-index",
    "tool-index-inherited-name-after-snapshot", "tool-index-inherited-name-in-follow-up",
];

#[test]
fn streams_match_the_typescript_snapshots() {
    replay_streams(CASES[2], EXPECTED[2], STREAM_CASES, &STREAM_DECLINED);
}

#[test]
fn nanogpt_streams_match_the_typescript_snapshots() {
    replay_streams(NANOGPT_CASES[2], NANOGPT_EXPECTED[2], 12, &[]);
}

fn replay_streams(cases_bytes: &[u8], expected_bytes: &[u8], count: usize, declines: &[&str]) {
    let b = budget();
    let fixture = parse(cases_bytes, &b);
    let expected = parse(expected_bytes, &b);
    same_hash(cases_bytes, &expected);
    let cases = fixture.need("cases").list();
    let observed = expected.need("cases").list();
    assert_eq!(cases.len(), count);
    assert_eq!(observed.len(), count);
    assert_eq!(ids_of(cases, "declines"), declines, "the declined stream cases changed");
    let mut matched = 0;
    for (entry, record) in cases.iter().zip(observed) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let result = record.need("result");
        let prepared = prepare(&fixture, entry, result.need("model"), &b);
        let replies = match entry.get("replies") {
            Some(list) => list.list().to_vec(),
            None => entry.get("reply").into_iter().cloned().collect(),
        };
        let tool_replies = entry.get("toolReplies").map(|list| list.list().to_vec()).unwrap_or_default();
        let mut built = build(&prepared, &b).unwrap_or_else(|error| panic!("{id}: {error:?}"));
        let mut run = StreamRun { prepared: &prepared, bodies: Vec::new(), ran: Vec::new(), progress: Progress::default() };
        let driven = drive_stream(&mut run, &mut built, replies, tool_replies, &b);
        if entry.has("declines") {
            assert_declined(&id, entry, driven.expect_err(&id), run.progress, FailureStage::Transform);
            continue;
        }
        let (out, failure) = driven.unwrap_or_else(|error| panic!("{id}: {error:?}"));
        let want = stream_outcome(result);
        assert_eq!(out, want, "{id}: outcome");
        matched += 1;
        let sent: Vec<String> = result.need("requests").list().iter().map(|request| request.need("body").string()).collect();
        assert_eq!(run.bodies, sent, "{id}: request bodies");
        let called: Vec<String> = result.need("toolCalls").list().iter().map(|call| call.need("name").string()).collect();
        assert_eq!(run.ran, called, "{id}: tools run");
        if let StreamOut::Fail(shown) = &out {
            let failure = failure.unwrap_or_else(|| panic!("{id}: a failure without a ProviderFailure"));
            let first = if entry.has("reply") { entry.need("reply") } else { &entry.need("replies").list()[0] };
            let status = reply_status(first);
            assert_eq!(failure.http_status, Some(status), "{id}");
            if status != 200 { assert!(matches!(failure.category, FailureCategory::Http), "{id}"); } else { assert!(matches!(failure.category, FailureCategory::Decode), "{id}"); }
            assert_eq!(s(failure.sdk_message.as_ref().expect("a message")), bounded(shown), "{id}: sdk message");
        } else {
            assert!(failure.is_none(), "{id}");
        }
    }
    assert_eq!(matched, count - declines.len());
}

#[test]
fn an_error_response_without_a_body_names_what_it_was() {
    let b = budget();
    let prefix = t("HTTP: ", &b);
    let context = ParseContext { flags: &[], multi_gen: false, extract_json: None, http_error_prefix: &prefix };
    for (value, word) in [(Value::Undefined, "undefined"), (Value::Null, "null")] {
        match super::response::parse_buffered(false, &value, &context, &b) {
            Err(BuildError::Thrown(text)) => assert_eq!(s(&text), format!("Cannot read properties of {word} (reading 'error')")),
            other => panic!("{word}: {other:?}"),
        }
    }
    // A response that is not an object has no `error`, so the whole body is shown.
    match super::response::parse_buffered(false, &Value::Number(3.0), &context, &b) {
        Ok(Buffered::Fail(text)) => assert_eq!(s(&text), "HTTP: 3"),
        other => panic!("{other:?}"),
    }
}
