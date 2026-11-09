//! `requestGoogleCloudVertex` up to the request (`src/ts/process/request/google.ts:44-535`): the Gemini contents, the
//! generation config, the endpoint and the headers. The builder is pure: the Vertex access token, the tool decoding and
//! the plugin hooks arrive as resolved inputs.

use super::settings::Settings;
use crate::common::built::{utf8, wire_headers, BuiltRequest};
use crate::common::js::{self, J};
use crate::common::params;
use crate::common::schema::simplify_schema;
use crate::common::settings::SamplingSettings;
use crate::common::tools::{tool_call_matches, DecodedToolCall, ToolResponsePart};
use crate::common::url::href;
use crate::common::{wire, BuildError};
use risunest_ir::chat::{Multimodal, MultimodalData, MultimodalKind, Role};
use risunest_ir::provider::{HttpMethod, HttpRequestSpec, LlmFlag, LlmFormat, ProviderCall, ResponseKind};
use risunest_ir::{Budget, Error, Text};

/// What `requestGoogleCloudVertex` takes from `arg` besides the [`ProviderCall`].
pub struct RequestInputs<'a> {
    /// `arg.key`: the proxy key or the custom model key.
    pub key: Option<&'a Text>,
    /// `arg.customURL`.
    pub custom_url: Option<&'a Text>,
    /// `arg.maxTokens`.
    pub max_tokens: f64,
    /// `arg.imageResponse`.
    pub image_response: bool,
    /// The access token of a `VertexAIGemini` request: the cached `db.vertexAccessToken`, or the token
    /// [`super::token::read_token`] returned. The owner resolves it with [`super::token::plan`] before building, so a
    /// failure to mint comes before any of the errors this builder can raise.
    pub bearer: Option<&'a Text>,
    pub sampling: &'a SamplingSettings,
    pub settings: &'a Settings,
}

struct Chat { role: Option<&'static str>, parts: Vec<J> }

fn s(value: &str, budget: &Budget) -> Result<J, Error> { js::str_value(value, budget) }

fn text_part(text: Text, budget: &Budget) -> Result<J, Error> {
    let mut part = js::new_object();
    js::set_str(&mut part, "text", J::Str(text), budget)?;
    Ok(part)
}

/// `part.text` where JavaScript reads it as truthy.
fn truthy_text(part: &J) -> Option<&Text> {
    match js::get(part, "text") { Some(J::Str(text)) if text.len_units() != 0 => Some(text), _ => None }
}

/// The element `index` of `text.split(separator)`.
fn split_at(text: &[u16], separator: u16, index: usize) -> Option<&[u16]> { text.split(|&unit| unit == separator).nth(index) }

fn units_or_undefined(units: Option<&[u16]>, budget: &Budget) -> Result<J, Error> {
    Ok(match units { Some(units) => J::Str(js::from_units(units, budget)?), None => J::Undefined })
}

/// The `inlineData` part for a data URL, read the way `google.ts:80-90` splits it.
fn inline_part(data: &Text, budget: &Budget) -> Result<J, Error> {
    let all = js::units(data);
    let base64 = split_at(&all, 44, 1);
    let media = split_at(&all, 59, 0).and_then(|head| split_at(head, 58, 1));
    let mut inline = js::new_object();
    js::set_str(&mut inline, "mimeType", units_or_undefined(media, budget)?, budget)?;
    js::set_str(&mut inline, "data", units_or_undefined(base64, budget)?, budget)?;
    let mut part = js::new_object();
    js::set_str(&mut part, "inlineData", inline, budget)?;
    Ok(part)
}

fn has(flags: &[LlmFlag], flag: LlmFlag) -> bool { flags.contains(&flag) }

/// `/^gemini-3-.*-preview$/.test(id) || /^gemini-3\.[5678]-flash/.test(id)`.
fn vertex_global_only(id: &Text) -> bool {
    let all = js::units(id);
    let text = |value: &str| js::utf16(value);
    let (head, tail) = (text("gemini-3-"), text("-preview"));
    let preview = all.len() >= head.len() + tail.len() && all.starts_with(&head) && all.ends_with(&tail)
        && all[head.len()..all.len() - tail.len()].iter().all(|&unit| !matches!(unit, 10 | 13 | 0x2028 | 0x2029));
    let flash = all.starts_with(&text("gemini-3.")) && matches!(all.get(9), Some(&unit) if (0x35..=0x38).contains(&unit))
        && all[10..].starts_with(&text("-flash"));
    preview || flash
}

fn safe_segment(text: &Text) -> bool {
    text.units().all(|unit| unit < 128 && ((unit as u8).is_ascii_alphanumeric() || matches!(unit as u8, b'.' | b'_' | b'-')))
}

/// `URLSearchParams` serialization of one value (`application/x-www-form-urlencoded`).
fn form_encode(value: &Text) -> Result<String, BuildError> {
    let plain = value.as_unicode().ok_or(BuildError::Unsupported("google-key-form"))?;
    let mut output = String::new();
    for byte in plain.bytes() {
        match byte {
            b'*' | b'-' | b'.' | b'_' | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' => output.push(byte as char),
            b' ' => output.push('+'),
            _ => output.push_str(&format!("%{byte:02X}")),
        }
    }
    Ok(output)
}

/// A key interpolated into the query as it is must read the same after `fetch` has parsed the URL.
fn raw_key(key: &Text) -> Result<String, BuildError> {
    let plain = key.as_unicode().filter(|plain| plain.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'~' | b'*')));
    plain.map(str::to_owned).ok_or(BuildError::Unsupported("google-key-form"))
}

fn tool_chat(call: &DecodedToolCall, budget: &Budget) -> Result<(Chat, Chat), BuildError> {
    let mut function_call = js::new_object();
    js::set_str(&mut function_call, "name", J::Str(call.name.copy(budget)?), budget)?;
    js::set_str(&mut function_call, "args", js::from_value(&call.arg, budget)?, budget)?;
    let mut call_part = js::new_object();
    js::set_str(&mut call_part, "functionCall", function_call, budget)?;
    let mut data = Vec::new();
    for response in &call.response {
        if let ToolResponsePart::Text(text) = response { data.push(J::Str(text.copy(budget)?)); }
    }
    let mut payload = js::new_object();
    js::set_str(&mut payload, "data", J::Arr(data), budget)?;
    let mut function_response = js::new_object();
    js::set_str(&mut function_response, "name", J::Str(call.name.copy(budget)?), budget)?;
    js::set_str(&mut function_response, "response", payload, budget)?;
    let mut response_part = js::new_object();
    js::set_str(&mut response_part, "functionResponse", function_response, budget)?;
    Ok((Chat { role: Some("model"), parts: vec![call_part] }, Chat { role: None, parts: vec![response_part] }))
}

fn chat_value(chat: Chat, budget: &Budget) -> Result<J, Error> {
    let mut object = js::new_object();
    if let Some(role) = chat.role { js::set_str(&mut object, "role", s(role, budget)?, budget)?; }
    js::set_str(&mut object, "parts", J::Arr(chat.parts), budget)?;
    Ok(object)
}

enum Segment { Text { content: Text, role: Option<&'static str> }, Call(Box<DecodedToolCall>) }

fn thrown(message: &str, budget: &Budget) -> BuildError {
    match js::text(message, budget) { Ok(text) => BuildError::Thrown(text), Err(error) => BuildError::Budget(error) }
}

/// `call.messages` are the messages after `reformat`. `decode_tool_call` answers `decodeToolCall` for a `<tool_call>`
/// marker in message content.
pub fn build_request(
    call: &ProviderCall, inputs: &RequestInputs<'_>,
    decode_tool_call: &mut dyn FnMut(&Text) -> Result<Option<DecodedToolCall>, Error>, budget: &Budget,
) -> Result<BuiltRequest, BuildError> {
    let settings = inputs.settings;
    let flags = &call.model.flags;
    let vertex = call.model.format == LlmFormat::VertexAIGemini;
    if !vertex && call.model.format != LlmFormat::GoogleCloud { return Err(BuildError::Unsupported("format")); }
    let internal_id = call.model.internal_id.as_ref().ok_or(BuildError::Unsupported("model-internal-id"))?;

    // `formated[0].role === 'system'`
    let mut messages = call.messages.iter();
    let mut system_prompt = js::text("", budget)?;
    let first = call.messages.first().ok_or_else(|| thrown("Cannot read properties of undefined (reading 'role')", budget))?;
    if first.role == Role::System {
        system_prompt = first.content.copy(budget)?;
        messages.next();
    }

    let mut chats: Vec<Chat> = Vec::new();
    for message in messages {
        let model_role = if message.role == Role::User { "user" } else { "model" };
        if let Some(media) = message.multimodals.as_deref().filter(|media| !media.is_empty()) {
            let mut parts = vec![text_part(message.content.copy(budget)?, budget)?];
            for modal in media {
                let allowed = match modal.kind {
                    MultimodalKind::Image => has(flags, LlmFlag::HasImageInput),
                    MultimodalKind::Audio => has(flags, LlmFlag::HasAudioInput),
                    MultimodalKind::Video => has(flags, LlmFlag::HasVideoInput),
                    MultimodalKind::Signature => false,
                };
                if allowed { parts.push(inline_part(inline_data(modal)?, budget)?); }
                if modal.kind == MultimodalKind::Signature && settings.save_signatures { return Err(BuildError::Unsupported("signature-multimodal")); }
            }
            chats.push(Chat { role: Some(model_role), parts });
        } else if message.role == Role::System {
            let previous_is_user = chats.last().is_some_and(|chat| chat.role == Some("user"));
            if previous_is_user {
                let chat = chats.last_mut().expect("checked");
                let appended = js::join(&[&js::text("\nsystem:", budget)?, &message.content], "", budget)?;
                let current = match chat.parts.first().and_then(|part| js::get(part, "text")) {
                    Some(J::Str(text)) => text.copy(budget)?,
                    _ => return Err(BuildError::Unsupported("system-after-non-text")),
                };
                js::set_str(&mut chat.parts[0], "text", J::Str(js::concat(&current, &appended, budget)?), budget)?;
            } else {
                chats.push(Chat { role: Some("user"), parts: vec![text_part(js::concat(&js::text("system:", budget)?, &message.content, budget)?, budget)?] });
            }
        } else if matches!(message.role, Role::Assistant | Role::User) {
            chats.push(Chat { role: Some(model_role), parts: vec![text_part(message.content.copy(budget)?, budget)?] });
        } else {
            chats.push(Chat { role: Some("user"), parts: vec![text_part(js::concat(&js::text("function:", budget)?, &message.content, budget)?, budget)?] });
        }
    }

    // `<tool_call>` markers become a functionCall and a functionResponse chat after the chat that held them.
    let marker = js::utf16("<tool_call>");
    let mut at: isize = 0;
    while (at as usize) < chats.len() {
        let index = at as usize;
        if chats[index].parts.iter().skip(1).any(|part| truthy_text(part).is_some_and(|text| js::find_units(&js::units(text), &marker, 0).is_some())) {
            return Err(BuildError::Unsupported("tool-call-in-later-part"));
        }
        let Some(text) = chats[index].parts.first().and_then(truthy_text).map(|text| text.copy(budget)).transpose()? else { at += 1; continue };
        let all = js::units(&text);
        if js::find_units(&all, &marker, 0).is_none() { at += 1; continue; }
        let found = tool_call_matches(&all);
        if found.is_empty() { at += 1; continue; }
        let role = chats[index].role;
        let mut segments: Vec<Segment> = Vec::new();
        let mut last = 0usize;
        for (start, inner, end, stop) in found {
            if start > last { segments.push(Segment::Text { content: js::from_units(&all[last..start], budget)?.trim(budget)?, role }); }
            if let Some(decoded) = decode_tool_call(&js::from_units(&all[inner..end], budget)?)? {
                if call.tools.iter().any(|tool| tool.name.equals(&decoded.name)) { segments.push(Segment::Call(Box::new(decoded))); }
            }
            last = stop;
        }
        if last < all.len() { segments.push(Segment::Text { content: js::from_units(&all[last..], budget)?.trim(budget)?, role }); }
        let kept = if matches!(segments.first(), Some(Segment::Text { .. })) {
            let Segment::Text { content, .. } = segments.remove(0) else { unreachable!() };
            content
        } else { js::text("", budget)? };
        let remove_current = kept.trim(budget)?.len_units() == 0;
        js::set_str(&mut chats[index].parts[0], "text", J::Str(kept), budget)?;
        let mut insert = index + 1;
        for segment in segments {
            match segment {
                Segment::Text { content, role } => {
                    if content.trim(budget)?.len_units() != 0 {
                        chats.insert(insert, Chat { role, parts: vec![text_part(content, budget)?] });
                        insert += 1;
                    }
                }
                Segment::Call(decoded) => {
                    let (model_chat, response_chat) = tool_chat(&decoded, budget)?;
                    chats.insert(insert, model_chat);
                    chats.insert(insert + 1, response_chat);
                    insert += 2;
                }
            }
        }
        if remove_current {
            chats.remove(index);
            at = insert as isize - 2;
        } else {
            at = insert as isize - 1;
        }
        at += 1;
    }

    // Consecutive chats of one role are merged, from the end.
    let mut index = chats.len();
    while index > 1 {
        index -= 1;
        if chats[index].role != chats[index - 1].role { continue; }
        let current = chats.remove(index);
        let previous = &mut chats[index - 1];
        let mut rest = current.parts;
        let both_text = match (previous.parts.last().and_then(truthy_text), rest.first().and_then(truthy_text)) {
            (Some(left), Some(right)) => Some(js::join(&[left, &js::text("\n\n", budget)?, right], "", budget)?),
            _ => None,
        };
        if let Some(joined) = both_text {
            let last = previous.parts.len() - 1;
            js::set_str(&mut previous.parts[last], "text", J::Str(joined), budget)?;
            rest.remove(0);
        }
        previous.parts.extend(rest);
    }

    let mut categories: Vec<&str> = vec![
        "HARM_CATEGORY_SEXUALLY_EXPLICIT", "HARM_CATEGORY_HATE_SPEECH", "HARM_CATEGORY_HARASSMENT", "HARM_CATEGORY_DANGEROUS_CONTENT",
        "HARM_CATEGORY_CIVIC_INTEGRITY",
    ];
    if has(flags, LlmFlag::NoCivilIntegrity) { categories.remove(4); }
    let threshold = if has(flags, LlmFlag::GeminiBlockOff) { "OFF" } else { "BLOCK_NONE" };
    let mut safety = Vec::new();
    for category in categories {
        let mut entry = js::new_object();
        js::set_str(&mut entry, "category", s(category, budget)?, budget)?;
        js::set_str(&mut entry, "threshold", s(threshold, budget)?, budget)?;
        safety.push(entry);
    }

    let mut wanted = vec!["temperature", "top_p", "top_k", "presence_penalty", "frequency_penalty"];
    if has(flags, LlmFlag::GeminiThinking) { wanted.extend(["thinking_tokens", "reasoning_effort"]); }
    let mut names = Vec::new();
    for name in wanted {
        if call.model.parameters.iter().any(|parameter| parameter.equals_ascii(name.as_bytes())) { names.push(js::text(name, budget)?); }
    }
    let mut generation = js::new_object();
    js::set_str(&mut generation, "maxOutputTokens", J::Num(inputs.max_tokens), budget)?;
    for (name, value) in params::apply_parameters_with(&names, inputs.sampling, &call.mode, &call.model.id, true, budget)? {
        let renamed = [("top_p", "topP"), ("top_k", "topK"), ("presence_penalty", "presencePenalty"), ("frequency_penalty", "frequencyPenalty"),
            ("thinking_tokens", "thinkingBudget"), ("reasoning_effort", "thinkingConfig.thinkingLevel")]
            .iter().find(|(from, _)| name.equals_ascii(from.as_bytes())).map_or_else(|| name.copy(budget), |(_, to)| js::text(to, budget))?;
        params::set_object_value(&mut generation, &renamed, js::from_value(&value, budget)?, budget)?;
    }

    let mut declarations = Vec::new();
    for tool in &call.tools {
        let mut declaration = js::new_object();
        js::set_str(&mut declaration, "name", J::Str(tool.name.copy(budget)?), budget)?;
        js::set_str(&mut declaration, "description", J::Str(tool.description.copy(budget)?), budget)?;
        js::set_str(&mut declaration, "parameters", simplify_schema(&js::from_value(&tool.input_schema, budget)?, budget)?, budget)?;
        declarations.push(declaration);
    }
    let no_tools = declarations.is_empty();

    let mut contents = Vec::new();
    for chat in chats { contents.push(chat_value(chat, budget)?); }
    let mut body = js::new_object();
    js::set_str(&mut body, "contents", J::Arr(contents), budget)?;
    js::set_str(&mut body, "generation_config", generation, budget)?;
    js::set_str(&mut body, "safetySettings", J::Arr(safety), budget)?;
    let mut instruction = js::new_object();
    js::set_str(&mut instruction, "parts", J::Arr(vec![text_part(system_prompt.copy(budget)?, budget)?]), budget)?;
    js::set_str(&mut body, "systemInstruction", instruction, budget)?;
    let mut tools = js::new_object();
    js::set_str(&mut tools, "functionDeclarations", J::Arr(declarations), budget)?;
    js::set_str(&mut body, "tools", tools, budget)?;

    if has(flags, LlmFlag::GeminiThinking) {
        let config = js::get_mut(&mut body, "generation_config").expect("set above");
        match js::get(config, "thinkingBudget").map(|value| js::clone_j(value, budget)).transpose()? {
            Some(thinking_budget) if !matches!(thinking_budget, J::Undefined) => {
                let mut thinking = js::new_object();
                js::set_str(&mut thinking, "thinkingBudget", thinking_budget, budget)?;
                js::set_str(&mut thinking, "includeThoughts", J::Bool(true), budget)?;
                js::set_str(config, "thinkingConfig", thinking, budget)?;
                js::delete(config, "thinkingBudget");
            }
            _ => {
                if let Some(thinking) = js::get_mut(config, "thinkingConfig").filter(|value| js::truthy(value)) {
                    let minimal = matches!(js::get(thinking, "thinkingLevel"), Some(J::Str(level)) if level.equals_ascii(b"minimal"));
                    if minimal && has(flags, LlmFlag::GeminiThinkingNoMinimal) { js::set_str(thinking, "thinkingLevel", s("low", budget)?, budget)?; }
                    js::set_str(thinking, "includeThoughts", J::Bool(true), budget)?;
                }
            }
        }
    }
    if system_prompt.len_units() == 0 { js::delete(&mut body, "systemInstruction"); }

    let mut stream = call.stream;
    let modalities = |kinds: [&str; 2], budget: &Budget| -> Result<J, Error> { Ok(J::Arr(vec![s(kinds[0], budget)?, s(kinds[1], budget)?])) };
    if has(flags, LlmFlag::HasAudioOutput) {
        let config = js::get_mut(&mut body, "generation_config").expect("set above");
        js::set_str(config, "responseModalities", modalities(["TEXT", "AUDIO"], budget)?, budget)?;
        stream = false;
    }
    if inputs.image_response || has(flags, LlmFlag::HasImageOutput) {
        let config = js::get_mut(&mut body, "generation_config").expect("set above");
        js::set_str(config, "responseModalities", modalities(["TEXT", "IMAGE"], budget)?, budget)?;
        stream = false;
    }

    let mut headers = js::new_object();
    js::set_str(&mut headers, "Content-Type", s("application/json", budget)?, budget)?;
    if settings.vision_high {
        let config = js::get_mut(&mut body, "generation_config").expect("set above");
        js::set_str(config, "mediaResolution", s("MEDIA_RESOLUTION_MEDIUM", budget)?, budget)?;
    }
    if vertex {
        let bearer = inputs.bearer.ok_or(BuildError::Unsupported("vertex-token-missing"))?;
        js::set_str(&mut headers, "Authorization", J::Str(js::concat(&js::text("Bearer ", budget)?, bearer, budget)?), budget)?;
    }

    if settings.json_schema_enabled || call.schema.is_some() { return Err(BuildError::Unsupported("json-schema")); }

    let api_key = match inputs.key.filter(|key| key.len_units() != 0) { Some(key) => key, None => &settings.access_token };
    let endpoint_name = if stream { "streamGenerateContent" } else { "generateContent" };
    let url: String = if let Some(custom) = inputs.custom_url.filter(|url| url.len_units() != 0) {
        let mut base = utf8(custom)?;
        if !base.ends_with('/') { base.push('/'); }
        if base.contains(['?', '#']) || !safe_segment(internal_id) { return Err(BuildError::Unsupported("google-custom-url")); }
        let joined = href(&format!("{base}models/{}:{endpoint_name}", utf8(internal_id)?))?;
        let mut url = joined;
        url.push_str(&format!("?key={}", form_encode(api_key)?));
        if stream { url.push_str("&alt=sse"); }
        url
    } else if vertex {
        let endpoint = if stream { "streamGenerateContent?alt=sse" } else { "generateContent" };
        let region = if vertex_global_only(internal_id) { js::text("global", budget)? } else { settings.vertex_region.copy(budget)? };
        if !safe_segment(&settings.project_id) || !safe_segment(&region) || !safe_segment(internal_id) { return Err(BuildError::Unsupported("google-url-form")); }
        let (project, region, id) = (utf8(&settings.project_id)?, utf8(&region)?, utf8(internal_id)?);
        if region == "global" {
            format!("https://aiplatform.googleapis.com/v1/projects/{project}/locations/{region}/publishers/google/models/{id}:{endpoint}")
        } else {
            format!("https://{region}-aiplatform.googleapis.com/v1/projects/{project}/locations/{region}/publishers/google/models/{id}:{endpoint}")
        }
    } else if !safe_segment(internal_id) {
        return Err(BuildError::Unsupported("google-url-form"));
    } else if stream {
        format!("https://generativelanguage.googleapis.com/v1beta/models/{}:streamGenerateContent?key={}&alt=sse", utf8(internal_id)?, raw_key(api_key)?)
    } else {
        format!("https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}", utf8(internal_id)?, raw_key(api_key)?)
    };

    // `will return error if functionDeclarations is empty`
    if no_tools { js::set_str(&mut body, "tools", J::Undefined, budget)?; }
    params::apply_additional_parameters(&mut body, &mut headers, &settings.additional_params, budget)?;

    let bytes = wire::stringify(&body, budget)?.ok_or(BuildError::Unsupported("undefined-body"))?;
    let spec = HttpRequestSpec {
        method: HttpMethod::Post,
        url: url.clone(),
        headers: wire_headers(&headers, ResponseKind::Sse)?,
        body: bytes,
        response: if stream { ResponseKind::Sse } else { ResponseKind::Buffered },
    };
    Ok(BuiltRequest { spec, raw_url: url, body, headers })
}

fn inline_data(modal: &Multimodal) -> Result<&Text, BuildError> {
    match &modal.data { MultimodalData::Inline(data) => Ok(data), MultimodalData::Asset(_) => Err(BuildError::Unsupported("multimodal-asset-reference")) }
}
