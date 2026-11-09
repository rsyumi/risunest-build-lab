//! The buffered response handling of `requestClaudeHTTP` (`anthropic.ts:1020-1218`).

use super::BuildError;
use crate::common::js::{self, J};
use crate::common::jsprop::{nullish, read, to_text, truthy, Failed, Thrown};
use crate::common::schema::extract_json;
use crate::common::wire;
use risunest_ir::{Budget, Text, Value};

/// What the buffered parser reads besides the response body.
pub struct ParseContext<'a> {
    /// `db.jsonSchema` run through `risuChatParser`, when `arg.extractJson && db.jsonSchemaEnabled` holds; `None`
    /// otherwise.
    pub extract_json: Option<&'a Text>,
    /// `arg.additionalOutput`: the `rememberToolUsage` codes collected so far, put in front of a successful result.
    pub additional_output: &'a Text,
    /// The reply of a Bedrock request, which `requestClaude` reads on its own: no tool round, and a `content` that is not
    /// a list is iterated or rejected as JavaScript does.
    pub bedrock: bool,
}

/// A `tool_use` block of the response.
#[derive(Debug)]
pub struct ToolUseCall {
    /// `content.name`, passed to `callTool`.
    pub name: Text,
    /// `content.input`.
    pub input: Value,
    /// `content.id` when it is text; the `rememberToolUsage` code carries it.
    pub id: Option<Text>,
}

/// A response that asks for tools: the owner calls each tool in order, and
/// [`super::request::push_tool_round`] builds the follow-up request from `blocks` and the replies.
#[derive(Debug)]
pub struct ToolUse {
    /// Every content block of the response, verbatim.
    pub(crate) blocks: Vec<J>,
    /// The `tool_use` blocks in order.
    pub calls: Vec<ToolUseCall>,
}

#[derive(Debug)]
pub enum Buffered {
    /// `{ type: 'success', result }`. Reasoning is inline as `<Thoughts>` text, as TS returns it.
    Success(Text),
    /// `{ type: 'fail', result }`. `server_error` is `failByServerError`: the text mentions an overload.
    Fail { text: Text, server_error: bool },
    ToolUse(ToolUse),
}

fn stringify(value: &J, budget: &Budget) -> Result<Text, BuildError> {
    let bytes = wire::stringify(value, budget)?.ok_or(BuildError::Unsupported("data-undefined"))?;
    Ok(js::text(std::str::from_utf8(&bytes).map_err(|_| BuildError::Unsupported("json-utf8"))?, budget)?)
}

pub(crate) fn overloaded(text: &Text) -> bool {
    let all = js::units(text);
    let needle = js::utf16("overload");
    (0..all.len()).any(|start| all[start..].len() >= needle.len() && all[start..start + needle.len()].iter().zip(&needle).all(|(unit, expected)| {
        *unit < 128 && (*unit as u8).to_ascii_lowercase() as u16 == *expected
    }))
}

fn failure(value: &J, budget: &Budget) -> Result<Buffered, BuildError> {
    let text = stringify(value, budget)?;
    let server_error = overloaded(&text);
    Ok(Buffered::Fail { text, server_error })
}

fn decline(failed: Failed) -> BuildError {
    match failed {
        Failed::Decline(name) => BuildError::Unsupported(name),
        Failed::Budget(error) => BuildError::Budget(error),
        Failed::Thrown => BuildError::Unsupported("thrown"),
    }
}

fn thrown_text(message: &str, budget: &Budget) -> BuildError {
    match js::text(message, budget) {
        Ok(text) => BuildError::Thrown(text),
        Err(error) => BuildError::Budget(error),
    }
}

fn thrown_read(what: &str, budget: &Budget) -> BuildError {
    match js::text(&format!("Cannot read properties of {what}"), budget) {
        Ok(text) => BuildError::Thrown(text),
        Err(error) => BuildError::Budget(error),
    }
}

/// The text a list of content blocks renders to: text as it is, thinking inside `<Thoughts>` tags.
pub(crate) fn render_content(contents: &[J], budget: &Budget) -> Result<Text, BuildError> {
    let mut result = js::text("", budget)?;
    let mut thinking = false;
    let kind = |content: &J| -> Result<Option<&'static str>, BuildError> {
        match content {
            J::Null | J::Undefined => Err(BuildError::Unsupported("content-entry-null")),
            J::Obj(_) => Ok(["text", "thinking", "redacted_thinking"].into_iter().find(|name| matches!(js::get(content, "type"), Some(J::Str(text)) if text.equals_ascii(name.as_bytes())))),
            _ => Ok(None),
        }
    };
    for content in contents {
        match kind(content)? {
            Some("text") => {
                if thinking { result.append(&js::text("</Thoughts>\n\n", budget)?)?; thinking = false; }
                let shown = to_text(js::get(content, "text").unwrap_or(&J::Undefined), budget).map_err(decline)?;
                result.append(&shown)?;
            }
            Some("thinking") => {
                if !thinking { result.append(&js::text("<Thoughts>\n", budget)?)?; thinking = true; }
                match js::get(content, "thinking") {
                    None | Some(J::Null | J::Undefined) => {}
                    Some(value) => result.append(&to_text(value, budget).map_err(decline)?)?,
                }
            }
            Some("redacted_thinking") => {
                if !thinking { result.append(&js::text("<Thoughts>\n", budget)?)?; thinking = true; }
                result.append(&js::text("\n{{redacted_thinking}}\n", budget)?)?;
            }
            _ => {}
        }
    }
    if thinking { result.append(&js::text("</Thoughts>\n\n", budget)?)?; }
    Ok(result)
}

/// The response of a request sent with `globalFetch`: `ok` is TS `res.ok`, `data` is `res.data` (the parsed JSON body,
/// or the `${error}` text of a transport failure).
pub fn parse_buffered(ok: bool, data: &Value, context: &ParseContext<'_>, budget: &Budget) -> Result<Buffered, BuildError> {
    let parsed = parse_inner(ok, data, context, budget)?;
    // The Bedrock branch returns its failures without `failByServerError`.
    Ok(match parsed {
        Buffered::Fail { text, .. } if context.bedrock => Buffered::Fail { text, server_error: false },
        other => other,
    })
}

fn parse_inner(ok: bool, data: &Value, context: &ParseContext<'_>, budget: &Budget) -> Result<Buffered, BuildError> {
    let dat = js::from_value(data, budget)?;
    if !ok { return failure(&dat, budget); }
    let root = Some(&dat);
    if nullish(root) {
        let what = if matches!(dat, J::Undefined) { "undefined (reading 'error')" } else { "null (reading 'error')" };
        return Err(thrown_read(what, budget));
    }
    let error = read(root, "error").map_err(|Thrown| BuildError::Unsupported("thrown"))?;
    if truthy(error) { return failure(error.expect("truthy"), budget); }
    let contents = read(root, "content").map_err(|Thrown| BuildError::Unsupported("thrown"))?;
    let blocks: &[J] = match contents {
        None | Some(J::Undefined | J::Null) => return failure(&dat, budget),
        Some(value) if !truthy(Some(value)) => return failure(&dat, budget),
        Some(J::Arr(items)) if items.is_empty() => return failure(&dat, budget),
        Some(J::Arr(items)) => items,
        // `for (const content of 'text')` visits characters, none of which has a `type`.
        Some(J::Str(_)) if context.bedrock => &[],
        Some(_) if context.bedrock => return Err(thrown_text("contents is not iterable", budget)),
        Some(_) => return Err(BuildError::Unsupported("content-not-array")),
    };
    if context.bedrock {
        if blocks.iter().any(|block| matches!(block, J::Null | J::Undefined)) {
            return Err(thrown_read("null (reading 'type')", budget));
        }
        let rendered = render_content(blocks, budget)?;
        let body = match context.extract_json {
            Some(format) => extract_json(&rendered, format, budget)?,
            None => rendered,
        };
        return Ok(Buffered::Success(body));
    }
    // `contents.some((v) => v.type === 'tool_use')` throws on a null entry.
    let mut calls = Vec::new();
    for block in blocks {
        if matches!(block, J::Null | J::Undefined) { return Err(BuildError::Unsupported("content-entry-null")); }
        if matches!(js::get(block, "type"), Some(J::Str(text)) if text.equals_ascii(b"tool_use")) {
            let name = match js::get(block, "name") { Some(J::Str(name)) => name.copy(budget)?, _ => return Err(BuildError::Unsupported("tool-use-name-not-text")) };
            let input = match js::get(block, "input") { Some(input) => wire::to_value(input, budget)?, None => Value::Undefined };
            let id = match js::get(block, "id") { Some(J::Str(id)) => Some(id.copy(budget)?), _ => None };
            calls.push(ToolUseCall { name, input, id });
        }
    }
    if !calls.is_empty() {
        let blocks = blocks.iter().map(|block| js::clone_j(block, budget)).collect::<Result<Vec<_>, _>>()?;
        return Ok(Buffered::ToolUse(ToolUse { blocks, calls }));
    }
    let rendered = render_content(blocks, budget)?;
    let body = match context.extract_json {
        Some(format) => extract_json(&rendered, format, budget)?,
        None => rendered,
    };
    let mut result = context.additional_output.copy(budget)?;
    result.append(&body)?;
    Ok(Buffered::Success(result))
}
