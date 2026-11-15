//! `requestOpenAI` (`src/ts/process/request/openAI/requests.ts:34-654`) for `LLMFormat.OpenAICompatible`: URL, headers and
//! body bytes. The builder is pure: plugin replacers, the request trigger, tokenization, token bias and MCP run in the
//! owner and arrive here as resolved inputs.

use super::js::{self, J};
pub use crate::common::built::BuiltRequest;
use crate::common::built::{finish, utf8};
use super::params;
use super::schema::simplify_schema;
use super::settings::Settings;
use super::response::ToolCallEntry;
use super::BuildError;
use risunest_ir::chat::{Multimodal, MultimodalData, PromptMessage, Role};
use risunest_ir::provider::{LlmFlag, LlmFormat, ProviderCall};
use risunest_ir::{Budget, Error, Text};

/// What `requestChatDataMain` and `requestOpenAI` take from `arg` and the database besides the `ProviderCall`.
pub struct RequestInputs<'a> {
    /// `arg.aiModel`: the selected model id (`reverse_proxy`, `openrouter`, `xcustom:::...` or a model id).
    pub ai_model: &'a Text,
    /// `modelInfo.provider === LLMProvider.OpenAI`.
    pub provider_is_openai: bool,
    /// `modelInfo.keyIdentifier`.
    pub key_identifier: Option<&'a Text>,
    /// `arg.key`: the proxy key for `reverse_proxy`, the custom model key for `xcustom:::`.
    pub key: Option<&'a Text>,
    /// `arg.customURL`: `db.forceReplaceUrl` for `reverse_proxy`, the custom model URL for `xcustom:::`.
    pub custom_url: Option<&'a Text>,
    /// `arg.maxTokens`.
    pub max_tokens: f64,
    /// `arg.bias` after tokenization, see [`resolve_logit_bias`].
    pub logit_bias: &'a [(Text, f64)],
    /// `arg.multiGen`.
    pub multi_gen: bool,
    /// `supportsInlayImage()`: whether the main model (`db.aiModel`), not the target model, accepts images.
    pub main_model_has_image_input: bool,
    pub settings: &'a Settings,
}

/// `decodeToolCall(...)` and the stored tool response, for a `<tool_call>` marker inside message content.
pub struct DecodedToolCall {
    pub id: Text,
    pub name: Text,
    pub arguments: Text,
    /// The texts of the stored response entries whose type is `text`.
    pub response_texts: Vec<Text>,
}

const LEGACY_MODELS: &[(&str, &str)] = &[
    ("gpt35", "gpt-3.5-turbo"), ("gpt35_0613", "gpt-3.5-turbo-0613"), ("gpt35_16k", "gpt-3.5-turbo-16k"),
    ("gpt35_16k_0613", "gpt-3.5-turbo-16k-0613"), ("gpt4", "gpt-4"), ("gpt45", "gpt-4.5-preview"), ("gpt4_32k", "gpt-4-32k"),
    ("gpt4_0613", "gpt-4-0613"), ("gpt4_32k_0613", "gpt-4-32k-0613"), ("gpt4_1106", "gpt-4-1106-preview"),
    ("gpt4_0125", "gpt-4-0125-preview"), ("gptvi4_1106", "gpt-4-vision-preview"), ("gpt35_0125", "gpt-3.5-turbo-0125"),
    ("gpt35_1106", "gpt-3.5-turbo-1106"), ("gpt35_0301", "gpt-3.5-turbo-0301"), ("gpt4_0314", "gpt-4-0314"),
    ("gpt4_turbo_20240409", "gpt-4-turbo-2024-04-09"), ("gpt4_turbo", "gpt-4-turbo"), ("gpt4o", "gpt-4o"),
    ("gpt4o-2024-05-13", "gpt-4o-2024-05-13"), ("gpt4om", "gpt-4o-mini"), ("gpt4om-2024-07-18", "gpt-4o-mini-2024-07-18"),
    ("gpt4o-2024-08-06", "gpt-4o-2024-08-06"), ("gpt4o-2024-11-20", "gpt-4o-2024-11-20"), ("gpt4o-chatgpt", "chatgpt-4o-latest"),
    ("gpt4o1-preview", "o1-preview"), ("gpt4o1-mini", "o1-mini"),
];

fn legacy_model(name: &Text) -> Option<&'static str> {
    LEGACY_MODELS.iter().find(|(from, _)| name.equals_ascii(from.as_bytes())).map(|(_, to)| *to)
}

fn role_name(role: Role) -> &'static str {
    match role { Role::System => "system", Role::User => "user", Role::Assistant => "assistant", Role::Function => "function" }
}

fn multimodal_to_j(item: &Multimodal, budget: &Budget) -> Result<J, BuildError> {
    let MultimodalData::Inline(data) = &item.data else { return Err(BuildError::Unsupported("multimodal-asset-reference")) };
    let mut object = js::new_object();
    let kind = match item.kind {
        risunest_ir::chat::MultimodalKind::Image => "image",
        risunest_ir::chat::MultimodalKind::Video => "video",
        risunest_ir::chat::MultimodalKind::Audio => "audio",
        risunest_ir::chat::MultimodalKind::Signature => "signature",
    };
    js::set_str(&mut object, "type", js::str_value(kind, budget)?, budget)?;
    js::set_str(&mut object, "base64", J::Str(data.copy(budget)?), budget)?;
    if let Some(width) = item.width { js::set_str(&mut object, "width", J::Num(width), budget)?; }
    if let Some(height) = item.height { js::set_str(&mut object, "height", J::Num(height), budget)?; }
    for (key, value) in &item.extra { js::set(&mut object, key.copy(budget)?, js::from_value(value, budget)?, budget)?; }
    Ok(object)
}

/// The `OpenAIChat` object in the key order of `PromptMessage::to_value`.
fn message_to_j(message: &PromptMessage, budget: &Budget) -> Result<J, BuildError> {
    let mut object = js::new_object();
    js::set_str(&mut object, "role", js::str_value(role_name(message.role), budget)?, budget)?;
    js::set_str(&mut object, "content", J::Str(message.content.copy(budget)?), budget)?;
    let strings = |items: &Vec<Text>| -> Result<J, Error> { Ok(J::Arr(items.iter().map(|item| Ok(J::Str(item.copy(budget)?))).collect::<Result<_, Error>>()?)) };
    if let Some(name) = &message.name { js::set_str(&mut object, "name", J::Str(name.copy(budget)?), budget)?; }
    if let Some(memo) = &message.memo { js::set_str(&mut object, "memo", J::Str(memo.copy(budget)?), budget)?; }
    if let Some(attr) = &message.attr { js::set_str(&mut object, "attr", strings(attr)?, budget)?; }
    if let Some(items) = message.multimodals.as_ref().filter(|items| !items.is_empty()) {
        let parts = items.iter().map(|item| multimodal_to_j(item, budget)).collect::<Result<Vec<_>, _>>()?;
        js::set_str(&mut object, "multimodals", J::Arr(parts), budget)?;
    }
    if let Some(thoughts) = &message.thoughts { js::set_str(&mut object, "thoughts", strings(thoughts)?, budget)?; }
    if let Some(flag) = message.removable { js::set_str(&mut object, "removable", J::Bool(flag), budget)?; }
    if let Some(flag) = message.cache_point { js::set_str(&mut object, "cachePoint", J::Bool(flag), budget)?; }
    for (key, value) in &message.extra { js::set(&mut object, key.copy(budget)?, js::from_value(value, budget)?, budget)?; }
    Ok(object)
}

fn tool_call_message(original: &J, call: &DecodedToolCall, content: Text, budget: &Budget) -> Result<J, BuildError> {
    let mut message = js::clone_j(original, budget)?;
    js::set_str(&mut message, "role", js::str_value("assistant", budget)?, budget)?;
    js::set_str(&mut message, "content", J::Str(content), budget)?;
    let mut function = js::new_object();
    js::set_str(&mut function, "name", J::Str(call.name.copy(budget)?), budget)?;
    js::set_str(&mut function, "arguments", J::Str(call.arguments.copy(budget)?), budget)?;
    let mut entry = js::new_object();
    js::set_str(&mut entry, "id", J::Str(call.id.copy(budget)?), budget)?;
    js::set_str(&mut entry, "type", js::str_value("function", budget)?, budget)?;
    js::set_str(&mut entry, "function", function, budget)?;
    js::set_str(&mut message, "tool_calls", J::Arr(vec![entry]), budget)?;
    Ok(message)
}

/// `processToolCalls(text, originalMessage)`.
fn process_tool_calls(
    content: &Text, original: &J, decode: &mut dyn FnMut(&Text) -> Result<Option<DecodedToolCall>, Error>, budget: &Budget,
) -> Result<Vec<J>, BuildError> {
    let all = js::units(content);
    let open = js::utf16("<tool_call>");
    let close = js::utf16("</tool_call>");
    let mut processed = Vec::new();
    let mut current: Vec<u16> = Vec::new();
    let mut at = 0;
    while at < all.len() {
        let Some(start) = js::find_units(&all, &open, at) else { current.extend_from_slice(&all[at..]); break };
        let Some(end) = js::find_units(&all, &close, start + open.len()) else { current.extend_from_slice(&all[at..]); break };
        current.extend_from_slice(&all[at..start]);
        let inner = js::from_units(&all[start + open.len()..end], budget)?;
        if let Some(call) = decode(&inner)? {
            processed.push(tool_call_message(original, &call, js::from_units(&current, budget)?, budget)?);
            let mut reply = js::new_object();
            js::set_str(&mut reply, "role", js::str_value("tool", budget)?, budget)?;
            let texts: Vec<&Text> = call.response_texts.iter().collect();
            js::set_str(&mut reply, "content", J::Str(js::join(&texts, "\n", budget)?), budget)?;
            js::set_str(&mut reply, "tool_call_id", J::Str(call.id.copy(budget)?), budget)?;
            js::set_str(&mut reply, "cachePoint", J::Bool(true), budget)?;
            processed.push(reply);
            current.clear();
        }
        at = end + close.len();
    }
    let rest = js::from_units(&current, budget)?;
    if rest.trim(budget)?.len_units() != 0 {
        let mut message = js::clone_j(original, budget)?;
        js::set_str(&mut message, "role", js::str_value("assistant", budget)?, budget)?;
        js::set_str(&mut message, "content", J::Str(rest), budget)?;
        processed.push(message);
    }
    Ok(processed)
}

fn string_of(value: Option<&J>) -> Option<&Text> { match value { Some(J::Str(text)) => Some(text), _ => None } }

/// `new URL(url).hostname === 'api.openai.com'`.
/// The host is compared after ASCII case folding and percent decoding only: the IDNA mapping `new URL` also applies (full-width
/// letters, for example) is not modelled, so such a URL is not recognised as official.
fn is_official_openai_url(url: &Text) -> bool {
    let Some(source) = url.as_unicode() else { return false };
    let cleaned: String = source.trim_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect();
    let Some((scheme, rest)) = cleaned.split_once(':') else { return false };
    if scheme.is_empty() || !scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) { return false; }
    let special = matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https" | "ws" | "wss" | "ftp");
    let authority = if special { rest.trim_start_matches(['/', '\\']) } else {
        match rest.strip_prefix("//") { Some(value) => value, None => return false }
    };
    let end = authority.find(|c: char| matches!(c, '/' | '?' | '#') || (special && c == '\\')).unwrap_or(authority.len());
    let authority = &authority[..end];
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = if host_port.starts_with('[') { return false } else { host_port.rsplit_once(':').map_or(host_port, |(host, port)| if port.chars().all(|c| c.is_ascii_digit()) { host } else { host_port }) };
    if special { percent_decode(host).eq_ignore_ascii_case("api.openai.com") } else { host == "api.openai.com" }
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Some(byte) = value.get(index + 1..index + 3).and_then(|digits| u8::from_str_radix(digits, 16).ok()) { output.push(byte); index += 3; continue; }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn autofill_url(url: &Text, budget: &Budget) -> Result<Text, Error> {
    let suffix = if js::ends_with(url, "v1") { "/chat/completions" }
        else if js::ends_with(url, "v1/") { "chat/completions" }
        else if !(js::ends_with(url, "completions") || js::ends_with(url, "completions/")) {
            if js::ends_with(url, "/") { "v1/chat/completions" } else { "/v1/chat/completions" }
        } else { return url.copy(budget) };
    js::concat(url, &js::text(suffix, budget)?, budget)
}

/// Builds the request `requestOpenAI` sends for `LLMFormat.OpenAICompatible`.
///
/// `convertInterfaceToSchema` (an interface text rendered through `risuChatParser`) runs in the owner: `call.schema`
/// arrives as the schema object, and a database schema the owner did not convert declines.
///
/// `call.messages` are the messages after `reformat`; `call.sampling` is the [`params::apply_parameters`] result;
/// `call.schema` is the JSON schema object `getOpenAIJSONSchema` wraps (`convertInterfaceToSchema` output) when
/// structured output is on; `call.stream` is `arg.useStreaming`. `decode_tool_call` answers `decodeToolCall` for a
/// `<tool_call>` marker in message content.
pub fn build_request(
    call: &ProviderCall, inputs: &RequestInputs<'_>,
    decode_tool_call: &mut dyn FnMut(&Text) -> Result<Option<DecodedToolCall>, Error>, budget: &Budget,
) -> Result<BuiltRequest, BuildError> {
    let settings = inputs.settings;
    let ai_model = inputs.ai_model;
    let flags = &call.model.flags;
    let flagged = |flag: LlmFlag| flags.contains(&flag);
    if !matches!(call.model.format, LlmFormat::OpenAICompatible | LlmFormat::NanoGPT) { return Err(BuildError::Unsupported("format")); }
    let nanogpt = ai_model.equals_ascii(b"nanogpt");
    if settings.use_instruct_prompt && ai_model.equals_ascii(b"openrouter") { return Err(BuildError::Unsupported("openrouter-instruct-prompt")); }
    // `getFreeOpenRouterModels()` is a network call the owner makes; a value that is still `risu/free` was not resolved.
    if ai_model.equals_ascii(b"openrouter") && settings.openrouter_request_model.equals_ascii(b"risu/free") { return Err(BuildError::Unsupported("openrouter-free-model-unresolved")); }
    let reverse_proxy = ai_model.equals_ascii(b"reverse_proxy");
    let openrouter = ai_model.equals_ascii(b"openrouter");

    // Messages.
    let mut chat: Vec<J> = Vec::new();
    for message in &call.messages {
        let wire = message_to_j(message, budget)?;
        if message.content.len_units() != 0 && js::find_units(&js::units(&message.content), &js::utf16("<tool_call>"), 0).is_some() {
            chat.extend(process_tool_calls(&message.content, &wire, decode_tool_call, budget)?);
        } else if message.role == Role::User && message.multimodals.as_ref().is_some_and(|items| !items.is_empty()) {
            let mut value = wire;
            let mut parts = Vec::new();
            for item in message.multimodals.as_ref().expect("checked") {
                let MultimodalData::Inline(data) = &item.data else { return Err(BuildError::Unsupported("multimodal-asset-reference")) };
                let mut url = js::new_object();
                js::set_str(&mut url, "url", J::Str(data.copy(budget)?), budget)?;
                js::set_str(&mut url, "detail", J::Str(settings.gpt_vision_quality.copy(budget)?), budget)?;
                let mut part = js::new_object();
                js::set_str(&mut part, "type", js::str_value("image_url", budget)?, budget)?;
                js::set_str(&mut part, "image_url", url, budget)?;
                parts.push(part);
            }
            let mut text_part = js::new_object();
            js::set_str(&mut text_part, "type", js::str_value("text", budget)?, budget)?;
            js::set_str(&mut text_part, "text", J::Str(message.content.copy(budget)?), budget)?;
            parts.push(text_part);
            js::set_str(&mut value, "content", J::Arr(parts), budget)?;
            chat.push(value);
        } else {
            chat.push(wire);
        }
    }

    let mut ooba_system: Vec<Text> = Vec::new();
    let count = chat.len();
    for (index, message) in chat.iter_mut().enumerate() {
        let role = string_of(js::get(message, "role")).map(|role| role.copy(budget)).transpose()?;
        let is_function = role.as_ref().is_some_and(|role| role.equals_ascii(b"function"));
        if !is_function {
            let keep_name = string_of(js::get(message, "name"))
                .is_some_and(|name| js::starts_with(name, "example_") && settings.new_oai_handle && name.len_units() != 0);
            if !keep_name { js::set_str(message, "name", J::Undefined, budget)?; }
            if settings.new_oai_handle && string_of(js::get(message, "memo")).is_some_and(|memo| js::starts_with(memo, "NewChat")) {
                js::set_str(message, "content", js::str_value("", budget)?, budget)?;
            }
            let is_assistant = role.as_ref().is_some_and(|role| role.equals_ascii(b"assistant"));
            if flagged(LlmFlag::DeepSeekPrefix) && index == count - 1 && is_assistant {
                js::set_str(message, "prefix", J::Bool(true), budget)?;
            }
            if flagged(LlmFlag::DeepSeekThinkingInput) && index == count - 1 && is_assistant {
                if let Some(J::Arr(thoughts)) = js::get(message, "thoughts") {
                    if !thoughts.is_empty() {
                        let texts = thoughts.iter().map(|item| match item { J::Str(text) => Ok(text), _ => Err(BuildError::Unsupported("thought-not-text")) }).collect::<Result<Vec<_>, _>>()?;
                        let joined = js::join(&texts, "\n", budget)?;
                        js::set_str(message, "reasoning_content", J::Str(joined), budget)?;
                    }
                }
            }
            for key in ["memo", "removable", "attr", "multimodals", "thoughts", "cachePoint"] { js::delete(message, key); }
        }
        if reverse_proxy && settings.reverse_proxy_ooba_mode && role.as_ref().is_some_and(|role| role.equals_ascii(b"system")) {
            if let Some(content) = string_of(js::get(message, "content")) {
                ooba_system.push(content.copy(budget)?);
                js::set_str(message, "content", js::str_value("", budget)?, budget)?;
            }
        }
    }
    if !ooba_system.is_empty() {
        let parts: Vec<&Text> = ooba_system.iter().collect();
        let mut message = js::new_object();
        js::set_str(&mut message, "role", js::str_value("system", budget)?, budget)?;
        js::set_str(&mut message, "content", J::Str(js::join(&parts, "\n", budget)?), budget)?;
        chat.push(message);
    }
    if settings.new_oai_handle {
        chat.retain(|message| {
            let content_blank = matches!(js::get(message, "content"), Some(J::Str(text)) if text.len_units() == 0);
            let has_media = matches!(js::get(message, "multimodals"), Some(J::Arr(items)) if !items.is_empty());
            let has_calls = js::get(message, "tool_calls").is_some_and(js::truthy);
            let is_tool = matches!(js::get(message, "role"), Some(J::Str(text)) if text.equals_ascii(b"tool"));
            !content_blank || has_media || has_calls || is_tool
        });
    }
    if flagged(LlmFlag::DeveloperRole) {
        for message in chat.iter_mut() {
            if matches!(js::get(message, "role"), Some(J::Str(text)) if text.equals_ascii(b"system")) {
                js::set_str(message, "role", js::str_value("developer", budget)?, budget)?;
            }
        }
    }

    // Body.
    let mut request_model = if reverse_proxy || openrouter { settings.proxy_request_model.copy(budget)? } else { ai_model.copy(budget)? };
    if reverse_proxy { request_model = settings.custom_proxy_request_model.copy(budget)?; }
    let model = if nanogpt { settings.nanogpt_request_model.copy(budget)? }
        else if openrouter { settings.openrouter_request_model.copy(budget)? }
        else if let Some(mapped) = legacy_model(&request_model) { js::text(mapped, budget)? }
        else if let Some(internal) = call.model.internal_id.as_ref().filter(|value| value.len_units() != 0) { internal.copy(budget)? }
        else if request_model.len_units() == 0 { js::text("gpt-3.5-turbo", budget)? }
        else { request_model };
    let mut body = js::new_object();
    js::set_str(&mut body, "model", J::Str(model), budget)?;
    js::set_str(&mut body, "messages", J::Arr(chat), budget)?;
    js::set_str(&mut body, "max_tokens", J::Num(inputs.max_tokens), budget)?;
    let mut bias = js::new_object();
    for (key, value) in inputs.logit_bias { js::set(&mut bias, key.copy(budget)?, J::Num(*value), budget)?; }
    js::set_str(&mut body, "logit_bias", bias, budget)?;
    js::set_str(&mut body, "stream", J::Bool(false), budget)?;
    if matches!(js::get(&body, "logit_bias"), Some(J::Obj(entries)) if entries.is_empty()) { js::delete(&mut body, "logit_bias"); }
    if flagged(LlmFlag::OAICompletionTokens) {
        let value = js::get(&body, "max_tokens").map(|value| js::clone_j(value, budget)).transpose()?.unwrap_or(J::Undefined);
        js::set_str(&mut body, "max_completion_tokens", value, budget)?;
        js::delete(&mut body, "max_tokens");
    }
    if settings.generation_seed > 0.0 { js::set_str(&mut body, "seed", J::Num(settings.generation_seed), budget)?; }
    if (settings.json_schema_enabled || call.schema.is_some()) && !flagged(LlmFlag::NoStructuredOutput) {
        let schema = call.schema.as_ref().ok_or(BuildError::Unsupported("schema-from-database"))?;
        let mut wrapper = js::new_object();
        js::set_str(&mut wrapper, "name", js::str_value("format", budget)?, budget)?;
        js::set_str(&mut wrapper, "strict", settings.strict_json_schema.map_or(J::Undefined, J::Bool), budget)?;
        js::set_str(&mut wrapper, "schema", js::from_value(schema, budget)?, budget)?;
        let mut format = js::new_object();
        js::set_str(&mut format, "type", js::str_value("json_schema", budget)?, budget)?;
        js::set_str(&mut format, "json_schema", wrapper, budget)?;
        js::set_str(&mut body, "response_format", format, budget)?;
    }
    if settings.oai_prediction.len_units() != 0 {
        let mut prediction = js::new_object();
        js::set_str(&mut prediction, "type", js::str_value("content", budget)?, budget)?;
        js::set_str(&mut prediction, "content", J::Str(settings.oai_prediction.copy(budget)?), budget)?;
        js::set_str(&mut body, "prediction", prediction, budget)?;
    }
    if openrouter {
        if settings.openrouter_fallback { js::set_str(&mut body, "route", js::str_value("fallback", budget)?, budget)?; }
        let transforms = if settings.openrouter_middle_out { vec![js::str_value("middle-out", budget)?] } else { Vec::new() };
        js::set_str(&mut body, "transforms", J::Arr(transforms), budget)?;
        if let Some(provider) = &settings.openrouter_provider {
            let mut object = js::new_object();
            for (name, list) in [("order", &provider.order), ("only", &provider.only), ("ignore", &provider.ignore)] {
                if list.is_empty() { continue; }
                let items = list.iter().map(|item| Ok(J::Str(item.copy(budget)?))).collect::<Result<Vec<_>, Error>>()?;
                js::set_str(&mut object, name, J::Arr(items), budget)?;
            }
            if matches!(&object, J::Obj(entries) if !entries.is_empty()) { js::set_str(&mut body, "provider", object, budget)?; }
        }
    }
    for (name, value) in &call.sampling { params::set_object_value(&mut body, name, js::from_value(value, budget)?, budget)?; }
    if flagged(LlmFlag::DeepSeekThinkingToggle) {
        if settings.deepseek_thinking_enabled {
            let mut thinking = js::new_object();
            js::set_str(&mut thinking, "type", js::str_value("enabled", budget)?, budget)?;
            let effort = match &settings.deepseek_reasoning_effort { Some(effort) => J::Str(effort.copy(budget)?), None => js::str_value("high", budget)? };
            js::set_str(&mut thinking, "reasoning_effort", effort, budget)?;
            js::set_str(&mut body, "thinking", thinking, budget)?;
            for key in ["temperature", "top_p", "frequency_penalty", "presence_penalty"] { js::delete(&mut body, key); }
        } else {
            let mut thinking = js::new_object();
            js::set_str(&mut thinking, "type", js::str_value("disabled", budget)?, budget)?;
            js::set_str(&mut body, "thinking", thinking, budget)?;
        }
    }
    if !call.tools.is_empty() {
        let mut tools = Vec::new();
        for tool in &call.tools {
            let mut function = js::new_object();
            js::set_str(&mut function, "name", J::Str(tool.name.copy(budget)?), budget)?;
            js::set_str(&mut function, "description", J::Str(tool.description.copy(budget)?), budget)?;
            js::set_str(&mut function, "parameters", simplify_schema(&js::from_value(&tool.input_schema, budget)?, budget)?, budget)?;
            let mut entry = js::new_object();
            js::set_str(&mut entry, "type", js::str_value("function", budget)?, budget)?;
            js::set_str(&mut entry, "function", function, budget)?;
            tools.push(entry);
        }
        js::set_str(&mut body, "tools", J::Arr(tools), budget)?;
    }
    if reverse_proxy && settings.reverse_proxy_ooba_mode {
        if let Some(entries) = settings.reverse_proxy_ooba_args.object() {
            for (key, value) in entries {
                if matches!(value, risunest_ir::Value::Undefined | risunest_ir::Value::Null) { continue; }
                if key.equals_ascii(b"__proto__") { return Err(BuildError::Unsupported("additional-parameter-prototype-key")); }
                js::set(&mut body, key.copy(budget)?, js::from_value(value, budget)?, budget)?;
            }
        }
    }
    if inputs.main_model_has_image_input {
        let proxy_gpt = js::starts_with(&settings.proxy_request_model, "gpt")
            || (settings.proxy_request_model.equals_ascii(b"custom") && js::starts_with(&settings.custom_proxy_request_model, "gpt"));
        if !(js::starts_with(ai_model, "gpt") || (reverse_proxy && proxy_gpt)) { js::delete(&mut body, "logit_bias"); }
    }

    // URL and headers.
    let mut url = if nanogpt {
        js::text(if settings.nanogpt_use_subscription_endpoint { "https://nano-gpt.com/api/subscription/v1/chat/completions" }
            else { "https://nano-gpt.com/api/v1/chat/completions" }, budget)?
    } else if openrouter { js::text("https://openrouter.ai/api/v1/chat/completions", budget)? }
        else if let Some(custom) = inputs.custom_url { custom.copy(budget)? }
        else { js::text("https://api.openai.com/v1/chat/completions", budget)? };
    if let Some(endpoint) = call.model.endpoint.as_ref().filter(|value| value.len_units() != 0) { url = endpoint.copy(budget)?; }
    let mut risu_identify = false;
    if js::starts_with(&url, "risu::") { risu_identify = true; url = url.slice(6, url.len_units(), budget)?; }
    if reverse_proxy && settings.autofill_request_url { url = autofill_url(&url, budget)?; }
    let custom_endpoint = reverse_proxy || js::starts_with(ai_model, "xcustom:::");
    if settings.open_ai_flex_processing && (inputs.provider_is_openai || (custom_endpoint && is_official_openai_url(&url))) {
        js::set_str(&mut body, "service_tier", js::str_value("flex", budget)?, budget)?;
    }
    let bearer = |key: &Text| js::concat(&js::text("Bearer ", budget)?, key, budget);
    let default_key = if nanogpt { &settings.nanogpt_key } else if reverse_proxy { &settings.proxy_key } else if openrouter { &settings.openrouter_key } else { &settings.openai_key };
    let mut authorization = match inputs.key { Some(key) => bearer(key)?, None => bearer(default_key)? };
    if let Some(identifier) = inputs.key_identifier.filter(|value| value.len_units() != 0) {
        authorization = match settings.oai_comp_api_keys.iter().find(|(name, _)| name.equals(identifier)) {
            Some((_, key)) => bearer(key)?,
            None => js::text("Bearer undefined", budget)?,
        };
    }
    let mut headers = js::new_object();
    js::set_str(&mut headers, "Authorization", J::Str(authorization), budget)?;
    js::set_str(&mut headers, "Content-Type", js::str_value("application/json", budget)?, budget)?;
    if openrouter {
        js::set_str(&mut headers, "X-Title", js::str_value("RisuAI", budget)?, budget)?;
        js::set_str(&mut headers, "HTTP-Referer", js::str_value("https://risuai.xyz", budget)?, budget)?;
    }
    if nanogpt && settings.nanogpt_provider.len_units() != 0 {
        js::set_str(&mut headers, "X-Provider", J::Str(settings.nanogpt_provider.copy(budget)?), budget)?;
    }
    if risu_identify { js::set_str(&mut headers, "X-Proxy-Risu", js::str_value("RisuAI", budget)?, budget)?; }
    if inputs.multi_gen {
        if !call.tools.is_empty() {
            return Err(BuildError::Fail(js::text("MultiGen mode cannot be used with tool calls. Please disable one of them.", budget)?));
        }
        js::set_str(&mut body, "n", J::Num(settings.gen_time), budget)?;
    }
    params::apply_additional_parameters(&mut body, &mut headers, &settings.additional_params, budget)?;
    js::set_str(&mut body, "stream", J::Bool(call.stream), budget)?;
    finish(utf8(&url)?, body, &headers, call.stream, budget)
}

/// One `{ role: 'tool' }` message a tool round appends. `tool_call_id` is the id the model sent, whatever its JSON type;
/// a model that sent none leaves the key out.
#[derive(Debug)]
pub struct ToolReply {
    pub(crate) tool_call_id: J,
    pub content: Text,
}

impl ToolReply {
    /// `'No tool found with name: ' + toolCall.function.name`.
    pub fn no_tool(call: &ToolCallEntry, budget: &Budget) -> Result<Self, BuildError> {
        Ok(Self::with(call, js::concat(&js::text("No tool found with name: ", budget)?, &call.name_label(budget)?, budget)?, budget)?)
    }
    /// `'Tool call failed with no text response'`.
    pub fn no_text(call: &ToolCallEntry, budget: &Budget) -> Result<Self, Error> {
        Self::with(call, js::text("Tool call failed with no text response", budget)?, budget)
    }
    /// `'Tool call failed with error: ' + error` for the text `String(error)` gives.
    pub fn failed(call: &ToolCallEntry, error: &Text, budget: &Budget) -> Result<Self, Error> {
        Self::with(call, js::concat(&js::text("Tool call failed with error: ", budget)?, error, budget)?, budget)
    }
    /// The text of the first `text` entry of the tool response.
    pub fn text(call: &ToolCallEntry, content: &Text, budget: &Budget) -> Result<Self, Error> { Self::with(call, content.copy(budget)?, budget) }
    fn with(call: &ToolCallEntry, content: Text, budget: &Budget) -> Result<Self, Error> {
        Ok(Self { tool_call_id: js::clone_j(&call.id, budget)?, content })
    }
}

impl BuiltRequest {
    /// The follow-up request after a tool round: pushes the assistant message (its content cleared when
    /// `simplified_tool_use` is on and it has content) and one tool message per reply onto `body.messages`, then
    /// serializes the same body again. URL and headers do not change.
    pub fn push_tool_round(
        &mut self, assistant: &super::response::AssistantMessage, simplified_tool_use: bool, replies: &[ToolReply], budget: &Budget,
    ) -> Result<(), BuildError> {
        let mut message = js::clone_j(&assistant.0, budget)?;
        if simplified_tool_use && js::get(&message, "content").is_some_and(js::truthy) {
            js::set_str(&mut message, "content", J::Str(js::text("", budget)?), budget)?;
        }
        let J::Arr(messages) = self.body_messages()? else { return Err(BuildError::Unsupported("body-messages")) };
        messages.push(message);
        for reply in replies {
            let mut tool = js::new_object();
            js::set_str(&mut tool, "role", js::str_value("tool", budget)?, budget)?;
            js::set_str(&mut tool, "content", J::Str(reply.content.copy(budget)?), budget)?;
            js::set_str(&mut tool, "tool_call_id", js::clone_j(&reply.tool_call_id, budget)?, budget)?;
            messages.push(tool);
        }
        let bytes = super::wire::stringify(&self.body, budget)?.ok_or(BuildError::Unsupported("undefined-body"))?;
        self.spec.body = bytes;
        Ok(())
    }

    fn body_messages(&mut self) -> Result<&mut J, BuildError> {
        js::get_mut(&mut self.body, "messages").ok_or(BuildError::Unsupported("body-messages"))
    }
}

/// The token-bias loop of `requestOpenAI` (`requests.ts:184-200`): turns `arg.biasString` into `(token id, bias)`
/// pairs appended to the pairs `arg.bias` already holds. `tokenize` is `tokenizeNum` and `strong_ban` the token ids
/// `strongBan` sets to `-100`; both run in the owner because tokenization belongs to the tokenizer lane.
pub fn resolve_logit_bias(
    existing: &[(Text, f64)], bias_string: &[(Text, f64)],
    tokenize: &mut dyn FnMut(&Text) -> Result<Vec<f64>, Error>, strong_ban: &mut dyn FnMut(&Text) -> Result<Vec<f64>, Error>,
    budget: &Budget,
) -> Result<Vec<(Text, f64)>, Error> {
    let key = |id: f64| risunest_ir::Value::Number(id).to_text(budget);
    let mut output = Vec::new();
    for (key_text, value) in existing { output.push((key_text.copy(budget)?, *value)); }
    for (text, value) in bias_string {
        if js::starts_with(text, "[[") && js::ends_with(text, "]]") {
            let empty = js::text("", budget)?;
            let inner = js::replace_first(&js::replace_first(text, "[[", &empty, budget)?, "]]", &empty, budget)?;
            output.push((key(js::parse_int(&inner).unwrap_or(f64::NAN))?, *value));
            continue;
        }
        if *value == -101.0 {
            for id in strong_ban(text)? { output.push((key(id)?, -100.0)); }
            continue;
        }
        for id in tokenize(text)? { output.push((key(id)?, *value)); }
    }
    Ok(output)
}
