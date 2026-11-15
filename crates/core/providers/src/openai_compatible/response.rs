//! Buffered response handling of `requestHTTPOpenAI` (`requests.ts:656-887`) and the HTTP error mapping.

use super::js::{self, J};
use super::schema::extract_json;
use super::BuildError;
pub(crate) use crate::common::jsprop::*;
use risunest_ir::provider::LlmFlag;
use risunest_ir::{Budget, Error, Text, Value};

/// What the buffered parser reads besides the response body.
pub struct ParseContext<'a> {
    /// `modelInfo.flags`.
    pub flags: &'a [LlmFlag],
    /// `arg.multiGen`.
    pub multi_gen: bool,
    /// `arg.extractJson`, already run through `risuChatParser`, when `arg.extractJson && (db.jsonSchemaEnabled ||
    /// arg.schema)` holds; `None` otherwise.
    pub extract_json: Option<&'a Text>,
    /// `language.errors.httpError`.
    pub http_error_prefix: &'a Text,
}

/// One entry of `message.tool_calls` that passes the validity check TS applies before running it. `id` and `name`
/// keep the JSON value the model sent: TS forwards them untouched, so a number stays a number.
#[derive(Debug)]
pub struct ToolCallEntry {
    /// `toolCall.id`; `undefined` when the model sent none (the key is then left out of the reply message).
    pub(crate) id: J,
    pub(crate) name: J,
    pub arguments: Text,
}

impl ToolCallEntry {
    /// The name when it is a string; `tool.name === name` can match nothing else.
    pub fn tool_name(&self) -> Option<&Text> {
        match &self.name { J::Str(text) => Some(text), _ => None }
    }

    /// The id when it is a string.
    pub fn id_text(&self) -> Option<&Text> {
        match &self.id { J::Str(text) => Some(text), _ => None }
    }

    /// `'' + toolCall.function.name`, which the reply for an unknown tool appends.
    pub(crate) fn name_label(&self, budget: &Budget) -> Result<Text, BuildError> {
        to_text(&self.name, budget).map_err(|failed| match failed {
            Failed::Budget(error) => BuildError::Budget(error),
            _ => BuildError::Unsupported("tool-call-name-to-primitive"),
        })
    }
}

/// `dat.choices[0].message` after TS merged the tool calls of every choice into it.
#[derive(Debug)]
pub struct AssistantMessage(pub(crate) J);

#[derive(Debug)]
pub struct ToolCalls {
    /// `processTextResponse(dat)`; `Err` carries the failure text TS returns when that call throws after the tool
    /// round has run.
    pub text: Result<Text, Text>,
    pub assistant: AssistantMessage,
    pub calls: Vec<ToolCallEntry>,
}

/// The outcome of one buffered response, mirroring `requestDataResponse`.
#[derive(Debug)]
pub enum Buffered {
    /// `{ type: 'success', result }`. Reasoning is inline as `<Thoughts>` text, as TS returns it.
    Success(Text),
    /// `{ type: 'multiline', result }`: one text per choice.
    Multiline(Vec<Text>),
    /// The response asked for tool calls; the owner runs them and sends the follow-up request.
    ToolCalls(ToolCalls),
    /// `{ type: 'fail', result }`.
    Fail(Text),
}

/// `result.replace(/(.*)<\/think>/gms, ...)`: the text before the last `</think>` is the reasoning and is removed together
/// with the tag. Returns `(reasoning, rest)`.
pub(crate) fn split_think(value: &Text, budget: &Budget) -> Result<Option<(Text, Text)>, Error> {
    let all = js::units(value);
    let close = js::utf16("</think>");
    let mut last = None;
    let mut from = 0;
    while let Some(at) = js::find_units(&all, &close, from) { last = Some(at); from = at + 1; }
    let Some(at) = last else { return Ok(None) };
    Ok(Some((js::from_units(&all[..at], budget)?, js::from_units(&all[at + close.len()..], budget)?)))
}

/// `text.replace(/<think>/gms, '')`.
pub(crate) fn remove_think_open(value: &Text, budget: &Budget) -> Result<Text, Error> {
    let all = js::units(value);
    let open = js::utf16("<think>");
    let mut output = Vec::with_capacity(all.len());
    let mut at = 0;
    while at < all.len() {
        if all[at..].starts_with(&open) { at += open.len(); } else { output.push(all[at]); at += 1; }
    }
    js::from_units(&output, budget)
}

pub(crate) fn thoughts_block(reasoning: &Text, rest: &Text, budget: &Budget) -> Result<Text, Error> {
    let mut output = js::text("<Thoughts>\n", budget)?;
    output.append(reasoning)?;
    output.append(&js::text("\n</Thoughts>\n", budget)?)?;
    output.append(rest)?;
    Ok(output)
}

/// `processTextResponse(dat)`.
fn process_text_response(data: &J, context: &ParseContext<'_>, budget: &Budget) -> Result<Text, Failed> {
    let root = Some(data);
    let choices = if nullish(root) { None } else { Some(read(root, "choices")?) };
    let choice0 = match choices { None => None, Some(choices) => read(choices, "0")? };
    let own_text = read_optional(choice0, "text")?;
    if truthy(own_text) {
        let Some(J::Str(text)) = own_text else { return Err(Failed::Decline("choice-text-not-text")) };
        if let Some(format) = context.extract_json {
            // `extractJSON(JSON.parse(text), format)`: a parsed string behaves as the string; anything else is not text.
            let Some(source) = text.as_unicode() else { return Err(Failed::Decline("choice-text-isolated-surrogate")) };
            return match risunest_ir::json::parse(source, budget) {
                Ok(Value::Text(inner)) => Ok(extract_json(&inner, format, budget)?),
                Ok(_) => Err(Failed::Decline("extract-json-non-text")),
                Err(Error::Syntax { .. }) => Ok(text.copy(budget)?),
                Err(other) => Err(other.into()),
            };
        }
        return Ok(text.copy(budget)?);
    }
    let message = read(choice0, "message")?;
    let content = read(message, "content")?;
    if let Some(format) = context.extract_json {
        return match content {
            None | Some(J::Null) => Ok(js::text("", budget)?),
            Some(J::Str(text)) => Ok(extract_json(text, format, budget)?),
            Some(_) => Err(Failed::Decline("extract-json-non-text")),
        };
    }
    let mut result = match content {
        None | Some(J::Null) => js::text("", budget)?,
        Some(J::Str(text)) => text.copy(budget)?,
        Some(_) => return Err(Failed::Decline("message-content-not-text")),
    };
    let message_member = read_optional(choice0, "message")?;
    let direct = read_optional(choice0, "reasoning_content")?;
    let reasoning_field = if nullish(direct) { read_optional(message_member, "reasoning_content")? } else { direct };
    if context.flags.contains(&LlmFlag::DeepSeekThinkingOutput) && !truthy(reasoning_field) {
        if let Some((reasoning, rest)) = split_think(&result, budget)? {
            result = rest;
            if reasoning.len_units() != 0 {
                let reasoning = remove_think_open(&reasoning, budget)?;
                result = thoughts_block(&reasoning, &result, budget)?;
            }
        }
    }
    if let Some(field) = reasoning_field.filter(|value| js::truthy(value)) {
        if !js::starts_with(&result, "<Thoughts>") {
            result = thoughts_block(&to_text(field, budget)?, &result, budget)?;
        }
    }
    if let Some(reasoning) = read_optional(message_member, "reasoning")?.filter(|value| js::truthy(value)) {
        result = thoughts_block(&to_text(reasoning, budget)?, &result, budget)?;
    }
    Ok(result)
}

fn string_member(value: Prop<'_>, what: &'static str, budget: &Budget) -> Result<Option<Text>, Failed> {
    match value {
        None | Some(J::Undefined) | Some(J::Null) => Ok(None),
        Some(J::Str(text)) => Ok(Some(text.copy(budget)?)),
        Some(_) => Err(Failed::Decline(what)),
    }
}

/// `choices` as the array `for...of` and `.map` accept.
fn choice_list(dat: &J) -> Result<&[J], Failed> {
    match read(Some(dat), "choices")? {
        Some(J::Arr(items)) => Ok(items),
        _ => Err(Failed::Thrown),
    }
}

fn tool_call_entries(dat: &mut J, context: &ParseContext<'_>, budget: &Budget) -> Result<Option<ToolCalls>, Failed> {
    // `allToolCalls`: every choice's calls, in choice order.
    let mut all: Vec<J> = Vec::new();
    if truthy(read(Some(dat), "choices")?) {
        for choice in choice_list(dat)? {
            let message = read(Some(choice), "message")?;
            let calls = read_optional(message, "tool_calls")?;
            match calls {
                Some(J::Arr(items)) if !items.is_empty() => for item in items { all.push(js::clone_j(item, budget)?); },
                Some(J::Arr(_)) | None | Some(J::Null) | Some(J::Undefined) | Some(J::Bool(_)) | Some(J::Num(_)) | Some(J::Obj(_)) => {}
                Some(J::Str(text)) if text.len_units() == 0 => {}
                Some(J::Str(_)) => return Err(Failed::Decline("tool-calls-not-array")),
            }
        }
    }
    // `dat.choices[0].message.tool_calls = allToolCalls` when the first choice has a message.
    let has_message = {
        let first = read_optional(read_optional(Some(&*dat), "choices")?, "0")?;
        truthy(read_optional(first, "message")?)
    };
    if has_message && !all.is_empty() {
        let J::Obj(entries) = dat else { return Ok(None) };
        let choices = entries.iter_mut().find(|(name, _)| name.equals_ascii(b"choices")).map(|(_, value)| value);
        if let Some(J::Arr(items)) = choices {
            if let Some(J::Obj(first)) = items.first_mut() {
                if let Some((_, J::Obj(message))) = first.iter_mut().find(|(name, _)| name.equals_ascii(b"message")) {
                    let value = J::Arr(all);
                    match message.iter_mut().find(|(name, _)| name.equals_ascii(b"tool_calls")) {
                        Some((_, slot)) => *slot = value,
                        None => message.push((js::text("tool_calls", budget)?, value)),
                    }
                }
            }
        }
    }
    let first = read_optional(read_optional(Some(&*dat), "choices")?, "0")?;
    let message = read_optional(first, "message")?;
    let calls = read_optional(message, "tool_calls")?;
    let list = match calls {
        Some(J::Arr(items)) if !items.is_empty() => items,
        Some(J::Str(text)) if text.len_units() != 0 => return Err(Failed::Decline("tool-calls-not-array")),
        _ => return Ok(None),
    };
    let mut entries = Vec::new();
    for call in list {
        let function = read(Some(call), "function")?;
        if !truthy(function) { continue; }
        let name = read(function, "name")?;
        let arguments = read(function, "arguments")?;
        if !truthy(name) || nullish(arguments) { continue; }
        let id = match read(Some(call), "id")? { Some(id) => js::clone_j(id, budget)?, None => J::Undefined };
        let name = js::clone_j(name.expect("truthy"), budget)?;
        let Some(arguments) = string_member(arguments, "tool-call-arguments-not-text", budget)? else { continue };
        entries.push(ToolCallEntry { id, name, arguments });
    }
    let text = match process_text_response(dat, context, budget) {
        Ok(text) => Ok(text),
        Err(Failed::Thrown) => Err(()),
        Err(other) => return Err(other),
    };
    let assistant = AssistantMessage(js::clone_j(message.expect("tool calls imply a message"), budget)?);
    let text = match text {
        Ok(text) => Ok(text),
        Err(()) => Err(failure_with_dump(context, dat, budget)?),
    };
    Ok(Some(ToolCalls { text, assistant, calls: entries }))
}

fn failure_with_dump(context: &ParseContext<'_>, dat: &J, budget: &Budget) -> Result<Text, Failed> {
    Ok(js::concat(context.http_error_prefix, &stringify(dat, budget)?, budget)?)
}

fn buffered_ok(dat: &mut J, context: &ParseContext<'_>, budget: &Budget) -> Result<Buffered, Failed> {
    if let Some(calls) = tool_call_entries(dat, context, budget)? { return Ok(Buffered::ToolCalls(calls)); }
    if context.multi_gen && truthy(read(Some(dat), "choices")?) {
        let mut texts = Vec::new();
        for choice in choice_list(dat)? {
            let content = read(read(Some(choice), "message")?, "content")?;
            let text = match (content, context.extract_json) {
                (Some(J::Str(text)), Some(format)) => extract_json(text, format, budget)?,
                (None | Some(J::Null), Some(_)) | (None | Some(J::Null), None) => js::text("", budget)?,
                (Some(J::Str(text)), None) => text.copy(budget)?,
                (Some(_), _) => return Err(Failed::Decline("message-content-not-text")),
            };
            texts.push(text);
        }
        return Ok(Buffered::Multiline(texts));
    }
    Ok(Buffered::Success(process_text_response(dat, context, budget)?))
}

/// The response of a request sent with `globalFetch`: `ok` is TS `res.ok`, `data` is `res.data` (the parsed JSON body,
/// or the `${error}` text of a transport failure).
pub fn parse_buffered(ok: bool, data: &Value, context: &ParseContext<'_>, budget: &Budget) -> Result<Buffered, BuildError> {
    let mut dat = js::from_value(data, budget)?;
    if ok {
        return match buffered_ok(&mut dat, context, budget) {
            Ok(result) => Ok(result),
            Err(Failed::Thrown) => Ok(Buffered::Fail(failure_with_dump(context, &dat, budget)?)),
            Err(other) => Err(other.into()),
        };
    }
    let error = match read(Some(&dat), "error") {
        Ok(error) => error,
        Err(Thrown) => {
            let what = if matches!(dat, J::Undefined) { "undefined" } else { "null" };
            return Err(BuildError::Thrown(js::text(&format!("Cannot read properties of {what} (reading 'error')"), budget)?));
        }
    };
    if truthy(error) {
        if let Ok(message) = read(error, "message") {
            if truthy(message) {
                let rendered = to_text(message.expect("truthy"), budget)?;
                return Ok(Buffered::Fail(js::concat(context.http_error_prefix, &rendered, budget)?));
            }
        }
    }
    Ok(Buffered::Fail(failure_with_dump(context, &dat, budget)?))
}
