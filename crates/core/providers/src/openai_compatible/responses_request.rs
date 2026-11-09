//! `requestOpenAIResponseAPI` up to the request (`src/ts/process/request/openAI/responses.ts:46-376,740-854`): URL, headers
//! and body bytes of `LLMFormat.OpenAIResponseAPI`. The builder is pure: tool decoding, the plugin hooks and the local
//! network route (`getLocalNetworkRequestOptions`, a transport option) run in the owner.

use super::js::{self, J};
use super::schema::simplify_schema;
use crate::common::built::{utf8, wire_headers, BuiltRequest};
use crate::common::params;
use crate::common::settings::SamplingSettings;
use crate::common::tools::{DecodedToolCall, ToolResponsePart};
use crate::common::url::href;
use crate::common::{wire, BuildError};
use risunest_ir::chat::{Multimodal, MultimodalData, MultimodalKind, PromptMessage, Role};
use risunest_ir::provider::{HttpMethod, HttpRequestSpec, LlmFlag, LlmFormat, ProviderCall, ResponseKind};
use risunest_ir::{Budget, Error, Text};

/// The database fields `requestOpenAIResponseAPI` and the helpers it calls read.
pub struct ResponsesSettings {
    pub openai_key: Text,
    pub proxy_key: Text,
    /// `db.gptVisionQuality`, the `detail` of every image part.
    pub gpt_vision_quality: Text,
    /// `db.newOAIHandle`: an empty user text is still sent when it is `false`.
    pub new_oai_handle: bool,
    pub json_schema_enabled: bool,
    /// `db.strictJsonSchema`; `None` is `undefined` and leaves `strict` out.
    pub strict_json_schema: Option<bool>,
    pub autofill_request_url: bool,
    /// `db.OaiCompAPIKeys`.
    pub oai_comp_api_keys: Vec<(Text, Text)>,
    /// `db.modelTools.includes('search')`.
    pub model_tools_search: bool,
    /// `getAdditionalParameters(aiModel)`.
    pub additional_params: Vec<(Text, Text)>,
}

impl std::fmt::Debug for ResponsesSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const HIDDEN: &str = "<redacted>";
        formatter.debug_struct("ResponsesSettings")
            .field("openai_key", &HIDDEN)
            .field("proxy_key", &HIDDEN)
            .field("gpt_vision_quality", &self.gpt_vision_quality)
            .field("new_oai_handle", &self.new_oai_handle)
            .field("json_schema_enabled", &self.json_schema_enabled)
            .field("strict_json_schema", &self.strict_json_schema)
            .field("autofill_request_url", &self.autofill_request_url)
            .field("oai_comp_api_keys", &format_args!("{} entries, {HIDDEN}", self.oai_comp_api_keys.len()))
            .field("model_tools_search", &self.model_tools_search)
            .field("additional_params", &format_args!("{} entries, {HIDDEN}", self.additional_params.len()))
            .finish()
    }
}

/// What `requestChatDataMain` and `requestOpenAIResponseAPI` take from `arg` besides the [`ProviderCall`].
pub struct ResponsesInputs<'a> {
    /// `arg.aiModel`.
    pub ai_model: &'a Text,
    /// `arg.key`: the proxy key for `reverse_proxy`, the custom model key for `xcustom:::`.
    pub key: Option<&'a Text>,
    /// `arg.customURL`.
    pub custom_url: Option<&'a Text>,
    /// `modelInfo.keyIdentifier`.
    pub key_identifier: Option<&'a Text>,
    /// `arg.maxTokens`.
    pub max_tokens: f64,
    pub sampling: &'a SamplingSettings,
    pub settings: &'a ResponsesSettings,
}

fn s(value: &str, budget: &Budget) -> Result<J, Error> { js::str_value(value, budget) }

fn object(entries: Vec<(&str, J)>, budget: &Budget) -> Result<J, Error> {
    let mut result = js::new_object();
    for (key, value) in entries { js::set_str(&mut result, key, value, budget)?; }
    Ok(result)
}

fn output_message(text: &Text, budget: &Budget) -> Result<J, Error> {
    let part = object(vec![("type", s("output_text", budget)?), ("text", J::Str(text.copy(budget)?)), ("annotations", J::Arr(Vec::new()))], budget)?;
    object(vec![("content", J::Arr(vec![part])), ("role", s("assistant", budget)?), ("status", s("completed", budget)?), ("type", s("message", budget)?)], budget)
}

fn is_blank(units: &[u16], budget: &Budget) -> Result<bool, Error> { Ok(js::from_units(units, budget)?.trim(budget)?.len_units() == 0) }

/// `decodeRememberedToolCallsForResponses(text)`.
fn decode_remembered(
    content: &Text, decode: &mut dyn FnMut(&Text) -> Result<Option<DecodedToolCall>, Error>, budget: &Budget,
) -> Result<Vec<J>, BuildError> {
    let all = js::units(content);
    let (open, close) = (js::utf16("<tool_call>"), js::utf16("</tool_call>"));
    let mut items = Vec::new();
    let mut current: Vec<u16> = Vec::new();
    let mut at = 0;
    while at < all.len() {
        let Some(start) = js::find_units(&all, &open, at) else { current.extend_from_slice(&all[at..]); break };
        let Some(end) = js::find_units(&all, &close, start + open.len()) else { current.extend_from_slice(&all[at..]); break };
        current.extend_from_slice(&all[at..start]);
        if !is_blank(&current, budget)? {
            items.push(output_message(&js::from_units(&current, budget)?, budget)?);
            current.clear();
        }
        let inner = js::from_units(&all[start + open.len()..end], budget)?;
        if let Some(call) = decode(&inner)? {
            items.push(object(vec![
                ("type", s("function_call", budget)?), ("call_id", J::Str(call.id.copy(budget)?)), ("name", J::Str(call.name.copy(budget)?)),
                ("arguments", js::from_value(&call.arg, budget)?), ("status", s("completed", budget)?),
            ], budget)?);
            let texts: Vec<&Text> = call.response.iter().filter_map(|part| match part { ToolResponsePart::Text(text) => Some(text), _ => None }).collect();
            items.push(object(vec![
                ("type", s("function_call_output", budget)?), ("call_id", J::Str(call.id.copy(budget)?)), ("output", J::Str(js::join(&texts, "\n", budget)?)),
            ], budget)?);
        }
        at = end + close.len();
    }
    if !is_blank(&current, budget)? { items.push(output_message(&js::from_units(&current, budget)?, budget)?); }
    Ok(items)
}

fn input_part(item: &Multimodal, quality: &Text, budget: &Budget) -> Result<J, BuildError> {
    let MultimodalData::Inline(data) = &item.data else { return Err(BuildError::Unsupported("multimodal-asset-reference")) };
    Ok(if item.kind == MultimodalKind::Image {
        object(vec![("type", s("input_image", budget)?), ("detail", J::Str(quality.copy(budget)?)), ("image_url", J::Str(data.copy(budget)?))], budget)?
    } else {
        object(vec![("type", s("input_file", budget)?), ("file_data", J::Str(data.copy(budget)?))], budget)?
    })
}

/// `buildResponseInputItems(arg)`.
fn input_items(
    call: &ProviderCall, settings: &ResponsesSettings, decode: &mut dyn FnMut(&Text) -> Result<Option<DecodedToolCall>, Error>, budget: &Budget,
) -> Result<Vec<J>, BuildError> {
    let developer = call.model.flags.contains(&LlmFlag::DeveloperRole);
    let mut items = Vec::new();
    for message in &call.messages {
        message_items(message, developer, settings, decode, &mut items, budget)?;
    }
    Ok(items)
}

fn message_items(
    message: &PromptMessage, developer: bool, settings: &ResponsesSettings,
    decode: &mut dyn FnMut(&Text) -> Result<Option<DecodedToolCall>, Error>, items: &mut Vec<J>, budget: &Budget,
) -> Result<(), BuildError> {
    if message.extra.iter().any(|(key, _)| key.equals_ascii(b"tool_calls") || key.equals_ascii(b"tool_call_id")) { return Err(BuildError::Unsupported("message-tool-fields")); }
    match message.role {
        Role::Function => {}
        Role::Assistant => {
            if js::find_units(&js::units(&message.content), &js::utf16("<tool_call>"), 0).is_some() {
                items.extend(decode_remembered(&message.content, decode, budget)?);
            } else if message.content.len_units() != 0 {
                items.push(output_message(&message.content, budget)?);
            } else {
                items.push(object(vec![("content", J::Arr(Vec::new())), ("role", s("assistant", budget)?), ("status", s("completed", budget)?), ("type", s("message", budget)?)], budget)?);
            }
        }
        Role::User | Role::System => {
            let role = if message.role == Role::System && developer { "developer" } else if message.role == Role::System { "system" } else { "user" };
            let mut content = Vec::new();
            if message.content.len_units() != 0 || !settings.new_oai_handle {
                content.push(object(vec![("type", s("input_text", budget)?), ("text", J::Str(message.content.copy(budget)?))], budget)?);
            }
            for item in message.multimodals.iter().flatten() { content.push(input_part(item, &settings.gpt_vision_quality, budget)?); }
            if !content.is_empty() { items.push(object(vec![("content", J::Arr(content)), ("role", s(role, budget)?)], budget)?); }
        }
    }
    Ok(())
}

fn nullish(value: Option<&J>) -> bool { matches!(value, None | Some(J::Null | J::Undefined)) }

fn or_else<'a>(value: Option<&'a J>, fallback: &'a J) -> &'a J { if nullish(value) { fallback } else { value.expect("not nullish") } }

fn is_type(item: &J, name: &str) -> bool { matches!(js::get(item, "type"), Some(J::Str(text)) if text.equals_ascii(name.as_bytes())) }

/// `sanitizeResponsesContinuationItem(item)`.
pub(crate) fn sanitize_continuation(item: &J, budget: &Budget) -> Result<Option<J>, BuildError> {
    if matches!(item, J::Null | J::Undefined) { return Ok(None); }
    if is_type(item, "function_call") && js::get(item, "call_id").is_some_and(js::truthy) && js::get(item, "name").is_some_and(js::truthy) {
        let empty = s("", budget)?;
        let completed = s("completed", budget)?;
        return Ok(Some(object(vec![
            ("type", s("function_call", budget)?),
            ("call_id", js::clone_j(js::get(item, "call_id").expect("checked"), budget)?),
            ("name", js::clone_j(js::get(item, "name").expect("checked"), budget)?),
            ("arguments", js::clone_j(or_else(js::get(item, "arguments"), &empty), budget)?),
            ("status", js::clone_j(or_else(js::get(item, "status"), &completed), budget)?),
        ], budget)?));
    }
    if is_type(item, "message") {
        let no_content = J::Arr(Vec::new());
        let list = match or_else(js::get(item, "content"), &no_content) { J::Arr(list) => list, _ => return Err(BuildError::Unsupported("continuation-content-not-array")) };
        let mut content = Vec::new();
        for part in list {
            let empty = s("", budget)?;
            let no_annotations = J::Arr(Vec::new());
            if matches!(part, J::Null | J::Undefined) { continue; }
            if is_type(part, "output_text") {
                content.push(object(vec![
                    ("type", s("output_text", budget)?),
                    ("text", js::clone_j(or_else(js::get(part, "text"), &empty), budget)?),
                    ("annotations", js::clone_j(or_else(js::get(part, "annotations"), &no_annotations), budget)?),
                ], budget)?);
            } else if is_type(part, "refusal") {
                content.push(object(vec![("type", s("refusal", budget)?), ("refusal", js::clone_j(or_else(js::get(part, "refusal"), &empty), budget)?)], budget)?);
            }
        }
        let assistant = s("assistant", budget)?;
        let completed = s("completed", budget)?;
        return Ok(Some(object(vec![
            ("type", s("message", budget)?),
            ("role", js::clone_j(or_else(js::get(item, "role"), &assistant), budget)?),
            ("status", js::clone_j(or_else(js::get(item, "status"), &completed), budget)?),
            ("content", J::Arr(content)),
        ], budget)?));
    }
    Ok(None)
}

/// `toExternalResponsesBody(body)`.
fn external_body(body: &J, budget: &Budget) -> Result<J, BuildError> {
    let mut copy = js::clone_j(body, budget)?;
    js::delete(&mut copy, "__lastOutput");
    let store_false = matches!(js::get(&copy, "store"), Some(J::Bool(false)));
    if store_false {
        if let Some(J::Arr(input)) = js::get_mut(&mut copy, "input") {
            let mut next = Vec::new();
            for item in std::mem::take(input) {
                if let Some(sanitized) = sanitize_continuation(&item, budget)? { next.push(sanitized); continue; }
                if is_type(&item, "reasoning") { continue; }
                let mut item = item;
                if matches!(item, J::Obj(_)) { js::delete(&mut item, "id"); }
                next.push(item);
            }
            *input = next;
        }
    }
    Ok(copy)
}

fn effective_names(call: &ProviderCall) -> Vec<&Text> {
    call.model.parameters.iter().filter(|name| {
        ["temperature", "top_p", "reasoning_effort", "verbosity"].iter().any(|wanted| name.equals_ascii(wanted.as_bytes()))
            || ["reasoning_effort_min_medium", "reasoning_effort_none", "reasoning_effort_xhigh"].iter().any(|wanted| name.equals_ascii(wanted.as_bytes()))
    }).collect()
}

/// The `autofillRequestUrl` branch of `getResponsesRequestURL` for a URL `new URL` accepts.
fn autofill_url(url: &Text, budget: &Budget) -> Result<Text, BuildError> {
    let normalized = href(&utf8(url)?)?;
    let (before_fragment, fragment) = match normalized.split_once('#') { Some((head, tail)) => (head, Some(tail)), None => (normalized.as_str(), None) };
    let (before_query, query) = match before_fragment.split_once('?') { Some((head, tail)) => (head, Some(tail)), None => (before_fragment, None) };
    let authority_end = before_query.find("://").map(|at| at + 3).ok_or(BuildError::Unsupported("url-form"))?;
    let path_start = before_query[authority_end..].find('/').map(|at| authority_end + at).unwrap_or(before_query.len());
    let (origin, path) = before_query.split_at(path_start);
    let last = path.split('/').rfind(|segment| !segment.is_empty()).unwrap_or("");
    let mut has_version = false;
    for pair in query.unwrap_or("").split('&') {
        let name = pair.split('=').next().unwrap_or("");
        if name.contains(['%', '+']) { return Err(BuildError::Unsupported("url-form")); }
        if name == "api-version" { has_version = true; }
    }
    let append = |suffix: &str| -> String {
        let trimmed = path.strip_suffix('/').unwrap_or(path);
        format!("{trimmed}{suffix}")
    };
    let next_path = if has_version && path.contains("/responses") || last == "responses" { path.to_owned() }
        else if last == "v1" { append("/responses") } else { append("/v1/responses") };
    let mut output = format!("{origin}{next_path}");
    if let Some(query) = query { output.push('?'); output.push_str(query); }
    if let Some(fragment) = fragment { output.push('#'); output.push_str(fragment); }
    Ok(js::text(&output, budget)?)
}

const DEFAULT_URL: &str = "https://api.openai.com/v1/responses";

/// Builds the request `requestOpenAIResponseAPI` sends for `LLMFormat.OpenAIResponseAPI`. `call.schema` is the JSON schema
/// object `getOpenAIJSONSchema` wraps (the owner converts an interface text); `call.messages` are the messages after
/// `reformat`; `call.stream` is `arg.useStreaming`.
pub fn build_request(
    call: &ProviderCall, inputs: &ResponsesInputs<'_>,
    decode_tool_call: &mut dyn FnMut(&Text) -> Result<Option<DecodedToolCall>, Error>, budget: &Budget,
) -> Result<BuiltRequest, BuildError> {
    let settings = inputs.settings;
    let ai_model = inputs.ai_model;
    if call.model.format != LlmFormat::OpenAIResponseAPI { return Err(BuildError::Unsupported("format")); }
    if ai_model.equals_ascii(b"nanogpt") { return Err(BuildError::Unsupported("nanogpt")); }
    let reverse_proxy = ai_model.equals_ascii(b"reverse_proxy");
    for (key, _) in &settings.additional_params {
        let first = js::units(key).split(|&unit| unit == 46).next().map(<[u16]>::to_vec).unwrap_or_default();
        if first == js::utf16("input") || first == js::utf16("__lastOutput") { return Err(BuildError::Unsupported("additional-parameter-input")); }
    }
    if (settings.json_schema_enabled || call.schema.is_some()) && call.schema.is_none() { return Err(BuildError::Unsupported("schema-from-database")); }

    // Body.
    let mut tools: Vec<J> = Vec::new();
    for tool in &call.tools {
        tools.push(object(vec![
            ("type", s("function", budget)?), ("name", J::Str(tool.name.copy(budget)?)), ("description", J::Str(tool.description.copy(budget)?)),
            ("parameters", simplify_schema(&js::from_value(&tool.input_schema, budget)?, budget)?),
        ], budget)?);
    }
    if settings.model_tools_search { tools.push(object(vec![("type", s("web_search_preview", budget)?)], budget)?); }
    let model = match call.model.internal_id.as_ref().filter(|id| id.len_units() != 0) {
        Some(id) => id.copy(budget)?,
        None if ai_model.len_units() != 0 => ai_model.copy(budget)?,
        None => js::text("gpt-4.1", budget)?,
    };
    let items = input_items(call, settings, decode_tool_call, budget)?;
    let mut body = object(vec![
        ("model", J::Str(model)), ("input", J::Arr(items)), ("max_output_tokens", J::Num(inputs.max_tokens)),
        ("tools", J::Arr(tools)), ("store", J::Bool(false)),
    ], budget)?;
    let names: Vec<Text> = effective_names(call).into_iter().map(|name| name.copy(budget)).collect::<Result<_, _>>()?;
    for (name, value) in params::apply_parameters(&names, inputs.sampling, &call.mode, &call.model.id, budget)? {
        let key = if name.equals_ascii(b"reasoning_effort") { js::text("reasoning.effort", budget)? }
            else if name.equals_ascii(b"verbosity") { js::text("text.verbosity", budget)? } else { name };
        params::set_object_value(&mut body, &key, js::from_value(&value, budget)?, budget)?;
    }
    if matches!(js::get(&body, "tools"), Some(J::Arr(list)) if list.is_empty()) { js::delete(&mut body, "tools"); }
    if call.model.parameters.iter().any(|name| name.equals_ascii(b"reasoning_effort")) {
        if !js::get(&body, "reasoning").is_some_and(|value| !matches!(value, J::Null | J::Undefined)) {
            js::set_str(&mut body, "reasoning", js::new_object(), budget)?;
        }
        let reasoning = js::get_mut(&mut body, "reasoning").expect("set above");
        if !matches!(reasoning, J::Obj(_)) { return Err(BuildError::Unsupported("reasoning-not-object")); }
        if nullish(js::get(reasoning, "summary")) { js::set_str(reasoning, "summary", s("auto", budget)?, budget)?; }
    }
    if ai_model.equals_ascii(b"ollama-cloud") { js::delete(&mut body, "store"); }
    if let Some(schema) = &call.schema {
        if !matches!(js::get(&body, "text"), Some(J::Obj(_))) { js::set_str(&mut body, "text", js::new_object(), budget)?; }
        let wrapper = object(vec![
            ("type", s("json_schema", budget)?), ("name", s("format", budget)?),
            ("strict", settings.strict_json_schema.map_or(J::Undefined, J::Bool)), ("schema", js::from_value(schema, budget)?),
        ], budget)?;
        js::set_str(js::get_mut(&mut body, "text").expect("set above"), "format", wrapper, budget)?;
    }

    // URL and headers.
    let mut url = inputs.custom_url.map(|value| value.copy(budget)).transpose()?.unwrap_or(js::text(DEFAULT_URL, budget)?);
    if let Some(endpoint) = call.model.endpoint.as_ref().filter(|value| value.len_units() != 0) { url = endpoint.copy(budget)?; }
    let mut risu_identify = false;
    if js::starts_with(&url, "risu::") { risu_identify = true; url = url.slice(6, url.len_units(), budget)?; }
    if reverse_proxy && settings.autofill_request_url { url = autofill_url(&url, budget)?; }
    let bearer = |key: &Text| js::concat(&js::text("Bearer ", budget)?, key, budget);
    let mut authorization = match inputs.key { Some(key) => bearer(key)?, None => bearer(if reverse_proxy { &settings.proxy_key } else { &settings.openai_key })? };
    if let Some(identifier) = inputs.key_identifier.filter(|value| value.len_units() != 0) {
        authorization = match settings.oai_comp_api_keys.iter().find(|(name, _)| name.equals(identifier)) {
            Some((_, key)) => bearer(key)?,
            None => js::text("Bearer undefined", budget)?,
        };
    }
    let mut headers = js::new_object();
    js::set_str(&mut headers, "Authorization", J::Str(authorization), budget)?;
    js::set_str(&mut headers, "Content-Type", s("application/json", budget)?, budget)?;
    if risu_identify { js::set_str(&mut headers, "X-Proxy-Risu", s("RisuAI", budget)?, budget)?; }
    if reverse_proxy || js::starts_with(ai_model, "xcustom:::") {
        params::apply_additional_parameters(&mut body, &mut headers, &settings.additional_params, budget)?;
    }
    if !call.stream { js::set_str(&mut body, "stream", J::Bool(false), budget)?; }

    // The preview is taken before `body.stream = true`.
    let preview_body = external_body(&body, budget)?;
    let kind = if call.stream { ResponseKind::Sse } else { ResponseKind::Buffered };
    let wire_body = if call.stream {
        js::set_str(&mut body, "stream", J::Bool(true), budget)?;
        external_body(&body, budget)?
    } else { js::clone_j(&preview_body, budget)? };
    let bytes = wire::stringify(&wire_body, budget)?.ok_or(BuildError::Unsupported("undefined-body"))?;
    let raw_url = utf8(&url)?;
    let wire_url = if call.stream { raw_url.clone() } else { href(&raw_url)? };
    let spec = HttpRequestSpec { method: HttpMethod::Post, url: wire_url, headers: wire_headers(&headers, kind)?, body: bytes, response: kind };
    Ok(BuiltRequest { spec, raw_url, body: preview_body, headers })
}
