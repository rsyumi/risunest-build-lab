//! The responses of `requestGoogle` (`google.ts:584-962`): the buffered reply, the failure of a stream that did not start,
//! and the retry of an overloaded model. A reply that asks for tools, carries a thought signature to save or holds
//! inline media is declined; the owner runs those through the TS path.

use super::BuildError;
use crate::common::js::{self, J};
use crate::common::jsprop::{nullish, read, read_optional, stringify, truthy, Thrown};
use crate::common::owner::{bound_message, buffered_failure, json_parse_error, response_text, error_message, stream_start_failure};
use crate::common::wire;
use risunest_ir::provider::{FailureCategory, ProviderFailure};
use risunest_ir::{json, Budget, Error, Text, Value};

/// What the reply handling reads besides the body.
#[derive(Debug, Clone, Copy)]
pub struct ReplyContext {
    /// The request is a `VertexAIGemini` one, which is never retried.
    pub vertex: bool,
    /// `arg.abortSignal.aborted` when the reply arrived.
    pub aborted: bool,
    /// `db.antiServerOverloads`.
    pub anti_server_overloads: bool,
    /// `arg.saveSignatures`.
    pub save_signatures: bool,
}

/// One `bodyIntercepterStore` event `meta_gemini`: the owner passes it to the registered interceptors.
#[derive(Debug)]
pub struct Meta {
    pub usage_metadata: Value,
    pub model_status: Value,
    pub prompt_feedback: Value,
}

#[derive(Debug)]
pub enum Buffered {
    /// `{ type: 'success', result }`; reasoning is inline as `<Thoughts>` text, as TS returns it.
    Success { text: Text, meta: Vec<Meta> },
    /// `{ type: 'fail', result }`. `server_error` is `failByServerError`.
    Fail { text: Text, server_error: bool, failure: ProviderFailure },
    /// `RESOURCE_EXHAUSTED` with `antiServerOverloads` on: TS sends the identical request again, whatever the number of
    /// earlier attempts.
    Retry,
}

fn thrown(message: &str, budget: &Budget) -> BuildError {
    match js::text(message, budget) { Ok(text) => BuildError::Thrown(text), Err(error) => BuildError::Budget(error) }
}

pub(crate) fn contains(text: &Text, needle: &str) -> bool { js::find_units(&js::units(text), &js::utf16(needle), 0).is_some() }

/// `fallBackGemini`.
fn fall_back(original: Text, ctx: &ReplyContext, status: u16, budget: &Budget) -> Result<Buffered, BuildError> {
    if ctx.anti_server_overloads && !ctx.aborted && !ctx.vertex { return Ok(Buffered::Retry); }
    let failure = buffered_failure(FailureCategory::Http, Some(status), Some(bound_message(&original, budget)?));
    Ok(Buffered::Fail { text: original, server_error: true, failure })
}

/// A stream whose first response was not a 200.
pub fn read_stream_start(status: u16, body: &[u8], ctx: &ReplyContext, budget: &Budget) -> Result<Buffered, BuildError> {
    let (text, failure) = stream_start_failure(status, body, budget)?;
    if contains(&text, "RESOURCE_EXHAUSTED") { return fall_back(text, ctx, status, budget); }
    Ok(Buffered::Fail { text, server_error: false, failure })
}

fn string_of(value: &J, budget: &Budget) -> Result<Text, BuildError> {
    let bytes = wire::stringify(value, budget)?.ok_or(BuildError::Unsupported("data-undefined"))?;
    Ok(js::text(std::str::from_utf8(&bytes).map_err(|_| BuildError::Unsupported("json-utf8"))?, budget)?)
}

/// The reply to a request sent without streaming.
pub fn read_buffered(status: u16, body: &[u8], ctx: &ReplyContext, budget: &Budget) -> Result<Buffered, BuildError> {
    let text = response_text(body, budget)?;
    if !(200..300).contains(&status) {
        // `JSON.stringify(await res.text())`.
        let quoted = string_of(&J::Str(text.copy(budget)?), budget)?;
        if contains(&quoted, "RESOURCE_EXHAUSTED") { return fall_back(quoted, ctx, status, budget); }
        let mut result = quoted;
        result.append(&js::text(&format!(" (status: {status})"), budget)?)?;
        let failure = buffered_failure(FailureCategory::Http, Some(status), Some(bound_message(&result, budget)?));
        return Ok(Buffered::Fail { text: result, server_error: false, failure });
    }
    let source = String::from_utf16_lossy(&js::units(&text));
    let parsed = match json::parse(&source, budget) {
        Ok(value) => value,
        Err(Error::Syntax { .. }) => return Err(BuildError::Thrown(error_message(&json_parse_error(&text, budget)?, budget)?)),
        Err(other) => return Err(other.into()),
    };
    let data = js::from_value(&parsed, budget)?;
    let items: Vec<&J> = match &data { J::Arr(items) => items.iter().collect(), other => vec![other] };

    let mut parts: Vec<(Text, bool)> = Vec::new();
    let mut calls = false;
    let mut meta = Vec::new();
    for item in items {
        let parts_value = item_parts(item).map_err(|Thrown| BuildError::Unsupported("thrown"))?;
        if truthy(parts_value) {
            let list: &[J] = match parts_value {
                Some(J::Arr(list)) => list,
                // `parts.length` is `undefined` for a number or a boolean, and a string only holds characters.
                Some(J::Str(_) | J::Num(_) | J::Bool(_)) => &[],
                _ => return Err(BuildError::Unsupported("parts-not-array")),
            };
            for part in list {
                if nullish(Some(part)) { return Err(thrown("Cannot read properties of null (reading 'text')", budget)); }
                let text_value = read(Some(part), "text").map_err(|Thrown| BuildError::Unsupported("thrown"))?;
                if truthy(text_value) {
                    let Some(J::Str(shown)) = text_value else { return Err(BuildError::Unsupported("part-text-not-text")) };
                    let thought = truthy(read(Some(part), "thought").map_err(|Thrown| BuildError::Unsupported("thrown"))?);
                    parts.push((shown.copy(budget)?, thought));
                }
                let signature = truthy(read(Some(part), "thoughtSignature").map_err(|Thrown| BuildError::Unsupported("thrown"))?);
                if signature && ctx.save_signatures { return Err(BuildError::Unsupported("signature-response")); }
                if truthy(read(Some(part), "inlineData").map_err(|Thrown| BuildError::Unsupported("thrown"))?) { return Err(BuildError::Unsupported("inline-data-response")); }
                if truthy(read(Some(part), "functionCall").map_err(|Thrown| BuildError::Unsupported("thrown"))?) { calls = true; }
            }
        }
        let usage = read_optional(Some(item), "usageMetadata").map_err(|Thrown| BuildError::Unsupported("thrown"))?;
        if truthy(usage) {
            let field = |name: &str| -> Result<Value, BuildError> {
                match read_optional(Some(item), name).map_err(|Thrown| BuildError::Unsupported("thrown"))? {
                    Some(value) => Ok(wire::to_value(value, budget)?),
                    None => Ok(Value::Undefined),
                }
            };
            meta.push(Meta { usage_metadata: field("usageMetadata")?, model_status: field("modelStatus")?, prompt_feedback: field("promptFeedback")? });
        }
    }
    if calls { return Err(BuildError::Unsupported("tool-round")); }

    let (mut thoughts, mut content) = (Vec::<&Text>::new(), Vec::<&Text>::new());
    for (shown, thought) in &parts { if *thought { thoughts.push(shown); } else { content.push(shown); } }
    let joined_thoughts = js::join(&thoughts, "\n\n", budget)?;
    let joined_content = js::join(&content, "\n\n", budget)?;
    let mut result = js::text("", budget)?;
    if joined_thoughts.len_units() != 0 {
        result.append(&js::text("<Thoughts>\n\n", budget)?)?;
        result.append(&joined_thoughts)?;
        result.append(&js::text("\n\n</Thoughts>\n\n", budget)?)?;
    }
    result.append(&joined_content)?;
    if result.len_units() == 0 {
        let mut shown = js::text("Got empty response: ", budget)?;
        shown.append(&stringify(&data, budget)?)?;
        let failure = buffered_failure(FailureCategory::Decode, Some(status), Some(bound_message(&shown, budget)?));
        return Ok(Buffered::Fail { text: shown, server_error: false, failure });
    }
    Ok(Buffered::Success { text: result, meta })
}

/// `data?.candidates?.[0]?.content?.parts`.
fn item_parts(item: &J) -> Result<Option<&J>, Thrown> {
    let candidates = read_optional(Some(item), "candidates")?;
    let first = read_optional(candidates, "0")?;
    let content = read_optional(first, "content")?;
    read_optional(content, "parts")
}
