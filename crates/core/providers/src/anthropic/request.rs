//! `requestClaude` (`src/ts/process/request/anthropic.ts:77-520`): URL, headers and body bytes. The builder is pure: plugin
//! replacers, the request trigger and MCP run in the owner and arrive here as resolved inputs.

use super::response::ToolUse;
pub use crate::common::tools::{DecodedToolCall, ToolResponsePart};
use crate::common::tools::tool_call_matches;
use super::settings::{Settings, ThinkingType};
use super::BuildError;
use crate::common::built::{finish, utf8};
pub use crate::common::built::BuiltRequest;
use crate::common::js::{self, J};
use crate::common::params;
use crate::common::schema::simplify_schema;
use risunest_ir::chat::{Multimodal, MultimodalData, MultimodalKind, Role};
use risunest_ir::provider::{LlmFlag, LlmFormat, ProviderCall};
use risunest_ir::{Budget, Error, Text};

/// What `requestChatDataMain` and `requestClaude` take from `arg` and the database besides the [`ProviderCall`].
pub struct RequestInputs<'a> {
    /// `arg.aiModel`: the selected model id (`reverse_proxy`, `xcustom:::...` or a model id).
    pub ai_model: &'a Text,
    /// `arg.key`: the proxy key for `reverse_proxy`, the custom model key for `xcustom:::`.
    pub key: Option<&'a Text>,
    /// `arg.customURL`: `db.forceReplaceUrl` for `reverse_proxy`, the custom model URL for `xcustom:::`.
    pub custom_url: Option<&'a Text>,
    /// `arg.maxTokens`.
    pub max_tokens: f64,
    /// `Date.now()` in milliseconds, which a Bedrock request is signed at; other requests do not read it.
    pub now_ms: i64,
    pub settings: &'a Settings,
}

struct Chat { role: &'static str, content: Vec<J> }

const ANTHROPIC_VERSION: &str = "2023-06-01";

fn s(value: &str, budget: &Budget) -> Result<J, Error> { js::str_value(value, budget) }

fn text_block(text: Text, budget: &Budget) -> Result<J, Error> {
    let mut block = js::new_object();
    js::set_str(&mut block, "type", s("text", budget)?, budget)?;
    js::set_str(&mut block, "text", J::Str(text), budget)?;
    Ok(block)
}

fn is_type(block: &J, name: &str) -> bool { matches!(js::get(block, "type"), Some(J::Str(text)) if text.equals_ascii(name.as_bytes())) }

/// `{ type: 'ephemeral' }` with `ttl: "1h"` for the one-hour setting.
fn mark_cache(block: &mut J, one_hour: bool, budget: &Budget) -> Result<(), Error> {
    let mut control = js::new_object();
    js::set_str(&mut control, "type", s("ephemeral", budget)?, budget)?;
    if one_hour { js::set_str(&mut control, "ttl", s("1h", budget)?, budget)?; }
    js::set_str(block, "cache_control", control, budget)
}

/// `parts[index]` of `value.split(separator)`; `None` is `undefined`.
fn split_part(value: &[u16], separator: u16, index: usize, budget: &Budget) -> Result<Option<Text>, Error> {
    match value.split(|&unit| unit == separator).nth(index) {
        Some(part) => Ok(Some(js::from_units(part, budget)?)),
        None => Ok(None),
    }
}

/// An image part: `dataurl.split(',')[1]` is the data and `dataurl.split(';')[0].split(':')[1]` the media type.
fn image_block(item: &Multimodal, budget: &Budget) -> Result<J, BuildError> {
    let MultimodalData::Inline(data) = &item.data else { return Err(BuildError::Unsupported("multimodal-asset-reference")) };
    let all = js::units(data);
    let base64 = split_part(&all, 44, 1, budget)?;
    let head = split_part(&all, 59, 0, budget)?.expect("a split has a first part");
    let media_type = split_part(&js::units(&head), 58, 1, budget)?;
    let optional = |value: Option<Text>| value.map_or(J::Undefined, J::Str);
    let mut source = js::new_object();
    js::set_str(&mut source, "type", s("base64", budget)?, budget)?;
    js::set_str(&mut source, "media_type", optional(media_type), budget)?;
    js::set_str(&mut source, "data", optional(base64), budget)?;
    let mut block = js::new_object();
    js::set_str(&mut block, "type", s("image", budget)?, budget)?;
    js::set_str(&mut block, "source", source, budget)?;
    Ok(block)
}

/// `addClaudeChat`.
fn add_chat(
    chats: &mut Vec<Chat>, role: &'static str, content: &Text, cache: bool, multimodals: Option<&[Multimodal]>, one_hour: bool, budget: &Budget,
) -> Result<(), BuildError> {
    let images = |chat: &mut Vec<J>| -> Result<(), BuildError> {
        for item in multimodals.unwrap_or(&[]) {
            if item.kind == MultimodalKind::Image { chat.insert(0, image_block(item, budget)?); }
        }
        Ok(())
    };
    if let Some(last) = chats.last_mut().filter(|last| last.role == role) {
        let blocks = &mut last.content;
        match blocks.last_mut() {
            Some(block) if is_type(block, "text") => {
                let Some(J::Str(text)) = js::get_mut(block, "text") else { unreachable!("text blocks hold text") };
                text.append(&js::text("\n\n", budget)?)?;
                text.append(content)?;
            }
            _ => blocks.push(text_block(content.copy(budget)?, budget)?),
        }
        images(blocks)?;
        if cache {
            if let Some(block) = blocks.last_mut() { mark_cache(block, one_hour, budget)?; }
        }
        return Ok(());
    }
    let mut blocks = vec![text_block(content.copy(budget)?, budget)?];
    images(&mut blocks)?;
    if cache { mark_cache(&mut blocks[0], one_hour, budget)?; }
    chats.push(Chat { role, content: blocks });
    Ok(())
}

fn tool_response_block(part: &ToolResponsePart, budget: &Budget) -> Result<J, Error> {
    Ok(match part {
        ToolResponsePart::Text(text) => text_block(text.copy(budget)?, budget)?,
        ToolResponsePart::Image { mime_type, data } => {
            let mut source = js::new_object();
            js::set_str(&mut source, "type", s("base64", budget)?, budget)?;
            js::set_str(&mut source, "media_type", J::Str(mime_type.copy(budget)?), budget)?;
            js::set_str(&mut source, "data", J::Str(data.copy(budget)?), budget)?;
            let mut block = js::new_object();
            js::set_str(&mut block, "type", s("image", budget)?, budget)?;
            js::set_str(&mut block, "source", source, budget)?;
            block
        }
        ToolResponsePart::Other(kind) => text_block(js::concat(&js::text("Unsupported tool response type: ", budget)?, kind, budget)?, budget)?,
    })
}

/// The `tool_result` block both `requestClaude` and the tool round build.
pub(crate) fn tool_result_block(tool_use_id: J, parts: &[ToolResponsePart], budget: &Budget) -> Result<J, Error> {
    let content = parts.iter().map(|part| tool_response_block(part, budget)).collect::<Result<Vec<_>, _>>()?;
    let mut block = js::new_object();
    js::set_str(&mut block, "type", s("tool_result", budget)?, budget)?;
    js::set_str(&mut block, "tool_use_id", tool_use_id, budget)?;
    js::set_str(&mut block, "content", J::Arr(content), budget)?;
    Ok(block)
}

/// The two chats a decoded `<tool_call>` marker becomes.
fn tool_chats(call: &DecodedToolCall, budget: &Budget) -> Result<(Chat, Chat), Error> {
    let mut input = js::new_object();
    js::set_str(&mut input, "type", s("tool_use", budget)?, budget)?;
    js::set_str(&mut input, "id", J::Str(call.id.copy(budget)?), budget)?;
    js::set_str(&mut input, "name", J::Str(call.name.copy(budget)?), budget)?;
    js::set_str(&mut input, "input", js::from_value(&call.arg, budget)?, budget)?;
    let result = tool_result_block(J::Str(call.id.copy(budget)?), &call.response, budget)?;
    Ok((Chat { role: "assistant", content: vec![input] }, Chat { role: "user", content: vec![result] }))
}

fn effective_thinking(settings: &Settings, flags: &[LlmFlag]) -> ThinkingType {
    let has = |flag: LlmFlag| flags.contains(&flag);
    if settings.thinking_type == ThinkingType::Budget && !has(LlmFlag::ClaudeThinking) && has(LlmFlag::ClaudeAdaptiveThinking) { ThinkingType::Adaptive } else { settings.thinking_type }
}

fn autofill_url(url: &Text, budget: &Budget) -> Result<Text, Error> {
    let suffix = if js::ends_with(url, "v1") { "/messages" }
        else if js::ends_with(url, "v1/") { "messages" }
        else if !(js::ends_with(url, "messages") || js::ends_with(url, "messages/")) {
            if js::ends_with(url, "/") { "v1/messages" } else { "/v1/messages" }
        } else { return url.copy(budget) };
    js::concat(url, &js::text(suffix, budget)?, budget)
}

/// `body.max_tokens > 8192` after the additional parameters may have replaced it.
fn exceeds_8192(value: Option<&J>, budget: &Budget) -> Result<bool, BuildError> {
    Ok(match value {
        None | Some(J::Undefined) => false,
        Some(J::Null) => false,
        Some(J::Bool(_)) => false,
        Some(J::Num(number)) => *number > 8192.0,
        Some(J::Str(text)) => risunest_ir::num::to_number(text, budget)? > 8192.0,
        Some(J::Arr(_) | J::Obj(_)) => return Err(BuildError::Unsupported("max-tokens-form")),
    })
}

/// Builds the request `requestClaude` sends.
///
/// `call.messages` are the messages after `reformat`; `call.sampling` is the [`params::apply_parameters`] result;
/// `call.stream` is `arg.useStreaming`. `decode_tool_call` answers `decodeToolCall` for a `<tool_call>` marker in
/// message content (`None` for a marker it cannot decode).
pub fn build_request(
    call: &ProviderCall, inputs: &RequestInputs<'_>,
    decode_tool_call: &mut dyn FnMut(&Text) -> Result<Option<DecodedToolCall>, Error>, budget: &Budget,
) -> Result<BuiltRequest, BuildError> {
    let settings = inputs.settings;
    let ai_model = inputs.ai_model;
    let flags = &call.model.flags;
    let reverse_proxy = ai_model.equals_ascii(b"reverse_proxy");
    if !matches!(call.model.format, LlmFormat::Anthropic | LlmFormat::AnthropicLegacy | LlmFormat::NanoGPTMessages | LlmFormat::AWSBedrockClaude) {
        return Err(BuildError::Unsupported("format"));
    }
    // `aiModel === 'ollama-cloud'` carries the Ollama format, so `requestClaude` never sees it from `requestChatDataMain`.
    if ai_model.equals_ascii(b"ollama-cloud") { return Err(BuildError::Unsupported("ollama-cloud-anthropic")); }

    let mut url = inputs.custom_url.map_or_else(|| js::text("https://api.anthropic.com/v1/messages", budget), |url| url.copy(budget))?;
    let api_key = match inputs.key.filter(|key| key.len_units() != 0) {
        Some(key) => key.copy(budget)?,
        None => if reverse_proxy { settings.proxy_key.copy(budget)? } else { settings.claude_api_key.copy(budget)? },
    };
    if reverse_proxy && settings.autofill_request_url { url = autofill_url(&url, budget)?; }

    // Messages.
    let mut chats: Vec<Chat> = Vec::new();
    let mut system_prompt = js::text("", budget)?;
    let one_hour = settings.claude_one_hour_caching;
    for message in &call.messages {
        let cache = message.cache_point == Some(true);
        let media = message.multimodals.as_deref();
        match message.role {
            Role::User => add_chat(&mut chats, "user", &message.content, cache, media, one_hour, budget)?,
            Role::Assistant => add_chat(&mut chats, "assistant", &message.content, cache, media, one_hour, budget)?,
            Role::System => {
                if chats.is_empty() {
                    system_prompt.append(&js::text("\n\n", budget)?)?;
                    system_prompt.append(&message.content)?;
                } else {
                    add_chat(&mut chats, "user", &js::concat(&js::text("System: ", budget)?, &message.content, budget)?, cache, None, one_hour, budget)?;
                }
            }
            Role::Function => {}
        }
    }
    if chats.is_empty() && system_prompt.len_units() == 0 { return Err(BuildError::Fail(js::text("No input", budget)?)); }
    let start = |budget: &Budget| -> Result<Chat, Error> { Ok(Chat { role: "user", content: vec![text_block(js::text("Start", budget)?, budget)?] }) };
    let mut system_unset = system_prompt.len_units() == 0;
    if chats.is_empty() {
        chats.push(start(budget)?);
        system_prompt = js::text("", budget)?;
        system_unset = true;
    }
    if chats[0].role != "user" { chats.insert(0, start(budget)?); }

    // `<tool_call>` markers become a tool_use and a tool_result chat before the chat that held them.
    let mut at = 0;
    while at < chats.len() {
        let mut index = 0;
        while index < chats[at].content.len() {
            let block = &chats[at].content[index];
            if is_type(block, "text") {
                let Some(J::Str(text)) = js::get(block, "text") else { return Err(BuildError::Unsupported("text-block-not-text")) };
                let all = js::units(text);
                let found = tool_call_matches(&all);
                if !found.is_empty() {
                    let mut kept: Vec<u16> = Vec::with_capacity(all.len());
                    let mut from = 0;
                    let mut pairs = Vec::new();
                    for (start, inner, end, stop) in found {
                        kept.extend_from_slice(&all[from..start]);
                        from = stop;
                        if let Some(decoded) = decode_tool_call(&js::from_units(&all[inner..end], budget)?)? { pairs.push(tool_chats(&decoded, budget)?); }
                    }
                    kept.extend_from_slice(&all[from..]);
                    for (assistant, user) in pairs {
                        chats.insert(at, assistant);
                        chats.insert(at + 1, user);
                        at += 2;
                    }
                    let rewritten = js::from_units(&kept, budget)?;
                    js::set_str(&mut chats[at].content[index], "text", J::Str(rewritten), budget)?;
                }
            }
            index += 1;
        }
        at += 1;
    }

    let messages: Vec<J> = if reverse_proxy {
        chats.iter().map(|chat| match chat.content.first() {
            Some(block) if is_type(block, "text") => {
                let mut object = js::new_object();
                js::set_str(&mut object, "role", s(chat.role, budget)?, budget)?;
                js::set_str(&mut object, "content", js::clone_j(js::get(block, "text").expect("text blocks hold text"), budget)?, budget)?;
                Ok(object)
            }
            _ => Ok(J::Undefined),
        }).collect::<Result<_, Error>>()?
    } else {
        chats.into_iter().map(|chat| {
            let mut object = js::new_object();
            js::set_str(&mut object, "role", s(chat.role, budget)?, budget)?;
            js::set_str(&mut object, "content", J::Arr(chat.content), budget)?;
            Ok(object)
        }).collect::<Result<_, Error>>()?
    };

    // Body.
    let mut body = js::new_object();
    js::set_str(&mut body, "model", call.model.internal_id.as_ref().map_or(Ok(J::Undefined), |id| id.copy(budget).map(J::Str))?, budget)?;
    js::set_str(&mut body, "messages", J::Arr(messages), budget)?;
    js::set_str(&mut body, "system", J::Str(system_prompt.trim(budget)?), budget)?;
    js::set_str(&mut body, "max_tokens", J::Num(inputs.max_tokens), budget)?;
    js::set_str(&mut body, "stream", J::Bool(call.stream), budget)?;
    for (name, value) in &call.sampling {
        let renamed = if name.equals_ascii(b"thinking_tokens") { js::text("thinking.budget_tokens", budget)? } else { name.copy(budget)? };
        params::set_object_value(&mut body, &renamed, js::from_value(value, budget)?, budget)?;
    }

    let has = |flag: LlmFlag| flags.contains(&flag);
    // `body?.thinking?.budget_tokens`, reduced to the comparisons the branches make.
    #[derive(PartialEq)]
    enum Tokens { Zero, Positive, Null, Other }
    let tokens = match js::get(&body, "thinking").and_then(|thinking| js::get(thinking, "budget_tokens")) {
        Some(J::Num(number)) if *number == 0.0 => Tokens::Zero,
        Some(J::Num(number)) if *number > 0.0 => Tokens::Positive,
        Some(J::Null) => Tokens::Null,
        _ => Tokens::Other,
    };
    match effective_thinking(settings, flags) {
        ThinkingType::Off => {
            if has(LlmFlag::ClaudeThinkingOnByDefault) {
                let mut thinking = js::new_object();
                js::set_str(&mut thinking, "type", s("disabled", budget)?, budget)?;
                js::set_str(&mut body, "thinking", thinking, budget)?;
            } else { js::delete(&mut body, "thinking"); }
        }
        ThinkingType::Adaptive if has(LlmFlag::ClaudeAdaptiveThinking) => {
            js::delete(&mut body, "thinking");
            let mut thinking = js::new_object();
            js::set_str(&mut thinking, "type", s("adaptive", budget)?, budget)?;
            js::set_str(&mut thinking, "display", s("summarized", budget)?, budget)?;
            js::set_str(&mut body, "thinking", thinking, budget)?;
            let mut effort = match &settings.adaptive_thinking_effort { Some(effort) => effort.copy(budget)?, None => js::text("high", budget)? };
            if effort.equals_ascii(b"xhigh") && !has(LlmFlag::ClaudeXHighEffort) { effort = js::text("high", budget)?; }
            let mut config = js::new_object();
            js::set_str(&mut config, "effort", J::Str(effort), budget)?;
            js::set_str(&mut body, "output_config", config, budget)?;
        }
        _ => match tokens {
            Tokens::Zero | Tokens::Null => js::delete(&mut body, "thinking"),
            Tokens::Positive => {
                let thinking = js::get_mut(&mut body, "thinking").expect("budget_tokens read from it");
                js::set_str(thinking, "type", s("enabled", budget)?, budget)?;
                js::set_str(thinking, "display", s("summarized", budget)?, budget)?;
            }
            Tokens::Other => {}
        },
    }
    if system_unset { js::delete(&mut body, "system"); }

    let additional = &settings.additional_params;
    let mut custom_beta = false;
    for (key, _) in additional {
        if !js::starts_with(key, "header::") { continue; }
        let name = key.slice(8, key.len_units(), budget)?;
        if name.units().any(|unit| unit >= 128) { return Err(BuildError::Unsupported("header-name-non-ascii")); }
        custom_beta |= name.units().map(|unit| (unit as u8).to_ascii_lowercase()).eq(b"anthropic-beta".iter().copied());
    }
    if call.model.format == LlmFormat::AWSBedrockClaude && !reverse_proxy {
        return super::bedrock::build(body, &api_key, call.model.internal_id.as_ref(), additional, inputs.now_ms, budget);
    }

    let mut headers = js::new_object();
    js::set_str(&mut headers, "Content-Type", s("application/json", budget)?, budget)?;
    js::set_str(&mut headers, "x-api-key", J::Str(api_key), budget)?;
    js::set_str(&mut headers, "anthropic-version", s(ANTHROPIC_VERSION, budget)?, budget)?;
    js::set_str(&mut headers, "accept", s("application/json", budget)?, budget)?;
    if settings.use_plain_fetch { js::set_str(&mut headers, "anthropic-dangerous-direct-browser-access", s("true", budget)?, budget)?; }

    if !call.tools.is_empty() {
        let mut tools = Vec::new();
        for tool in &call.tools {
            let mut entry = js::new_object();
            js::set_str(&mut entry, "name", J::Str(tool.name.copy(budget)?), budget)?;
            js::set_str(&mut entry, "description", J::Str(tool.description.copy(budget)?), budget)?;
            js::set_str(&mut entry, "input_schema", simplify_schema(&js::from_value(&tool.input_schema, budget)?, budget)?, budget)?;
            tools.push(entry);
        }
        js::set_str(&mut body, "tools", J::Arr(tools), budget)?;
    }
    params::apply_additional_parameters(&mut body, &mut headers, additional, budget)?;

    let mut betas: Vec<&str> = Vec::new();
    if exceeds_8192(js::get(&body, "max_tokens"), budget)? { betas.push("output-128k-2025-02-19"); }
    if settings.claude_one_hour_caching { betas.push("extended-cache-ttl-2025-04-11"); }
    if !betas.is_empty() && !custom_beta { js::set_str(&mut headers, "anthropic-beta", s(&betas.join(","), budget)?, budget)?; }

    finish(utf8(&url)?, body, &headers, call.stream, budget)
}

/// A response whose content holds `tool_use` blocks: the request TS sends next appends the model's blocks to the
/// assistant message and one `tool_result` per `tool_use` to a new user message, and turns streaming off.
pub fn push_tool_round(
    request: &mut BuiltRequest, round: &ToolUse, results: &[Vec<ToolResponsePart>], budget: &Budget,
) -> Result<(), BuildError> {
    let blocks = &round.blocks;
    let mut response: Vec<J> = Vec::new();
    let mut next = 0;
    {
        let J::Arr(messages) = js::get_mut(&mut request.body, "messages").ok_or(BuildError::Unsupported("body-messages"))? else { return Err(BuildError::Unsupported("body-messages")) };
        for block in blocks {
            let last_is_assistant = match messages.last() {
                Some(last @ J::Obj(_)) => matches!(js::get(last, "role"), Some(J::Str(role)) if role.equals_ascii(b"assistant")),
                _ => return Err(BuildError::Unsupported("tool-round-message")),
            };
            if !last_is_assistant {
                let mut assistant = js::new_object();
                js::set_str(&mut assistant, "role", s("assistant", budget)?, budget)?;
                js::set_str(&mut assistant, "content", J::Arr(Vec::new()), budget)?;
                messages.push(assistant);
            }
            let last = messages.last_mut().expect("pushed above");
            if let Some(J::Str(text)) = js::get(last, "content") {
                let converted = vec![text_block(text.copy(budget)?, budget)?];
                js::set_str(last, "content", J::Arr(converted), budget)?;
            }
            if is_type(block, "tool_use") {
                let id = js::get(block, "id").map(|id| js::clone_j(id, budget)).transpose()?.unwrap_or(J::Undefined);
                let parts = results.get(next).ok_or(BuildError::Unsupported("tool-round-results"))?;
                next += 1;
                response.push(tool_result_block(id, parts, budget)?);
            }
            let Some(J::Arr(content)) = js::get_mut(last, "content") else { return Err(BuildError::Unsupported("tool-round-message")) };
            content.push(js::clone_j(block, budget)?);
        }
        let mut user = js::new_object();
        js::set_str(&mut user, "role", s("user", budget)?, budget)?;
        js::set_str(&mut user, "content", J::Arr(response), budget)?;
        messages.push(user);
    }
    js::set_str(&mut request.body, "stream", J::Bool(false), budget)?;
    let bytes = crate::common::wire::stringify(&request.body, budget)?.ok_or(BuildError::Unsupported("undefined-body"))?;
    request.spec.body = bytes;
    Ok(())
}
