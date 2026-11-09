//! The buffered response handling of `requestHTTPResponsesAPI` (`responses.ts:378-585`) and the text extraction the stream
//! shares. A response that asks for tools is declined; the owner runs it through the TS path.

use super::js::{self, J};
use super::BuildError;
use crate::common::jsprop::{read_optional, stringify, to_text, truthy, Failed, Prop};
use crate::common::owner::{bound_message, buffered_failure, classify, BufferedReply};
use crate::common::schema::extract_json;
use risunest_ir::provider::{FailureCategory, ProviderFailure};
use risunest_ir::{Budget, Text, Value};

/// What the response handling reads besides the body.
pub struct ResponsesContext<'a> {
    /// `arg.extractJson` run through `risuChatParser`, when `arg.extractJson && (db.jsonSchemaEnabled || arg.schema)` holds.
    pub extract_json: Option<&'a Text>,
    /// `language.errors.httpError`.
    pub http_error_prefix: &'a Text,
}

#[derive(Debug)]
pub enum ResponsesOutcome {
    /// `{ type: 'success', result }`. Reasoning is inline as `<Thoughts>` text.
    Success(Text),
    /// `{ type: 'fail', result }`.
    Fail(Text),
}

#[derive(Debug)]
pub struct ResponsesBuffered {
    pub result: ResponsesOutcome,
    /// Set when the outcome is a failure.
    pub failure: Option<ProviderFailure>,
}

fn declined(failed: Failed) -> BuildError {
    match failed {
        Failed::Decline(name) => BuildError::Unsupported(name),
        Failed::Budget(error) => BuildError::Budget(error),
        Failed::Thrown => BuildError::Unsupported("thrown"),
    }
}

fn prop<'a>(value: Prop<'a>, key: &str) -> Prop<'a> { read_optional(value, key).unwrap_or(None) }

fn is_type(value: Prop<'_>, name: &str) -> bool { matches!(prop(value, "type"), Some(J::Str(text)) if text.equals_ascii(name.as_bytes())) }

/// `collectResponsesReasoningText(value)`.
fn collect_reasoning(value: Prop<'_>, output: &mut Vec<Text>, budget: &Budget) -> Result<(), BuildError> {
    let Some(value) = value.filter(|value| js::truthy(value)) else { return Ok(()) };
    match value {
        J::Str(text) => output.push(text.copy(budget)?),
        J::Arr(items) => for item in items { collect_reasoning(Some(item), output, budget)?; },
        J::Obj(_) => for key in ["text", "summary_text", "reasoning_text", "reasoning", "summary"] {
            if let Some(J::Str(text)) = prop(Some(value), key) { output.push(text.copy(budget)?); }
        },
        _ => {}
    }
    Ok(())
}

/// `extractResponsesReasoningTexts(item)`.
fn reasoning_texts(item: Prop<'_>, budget: &Budget) -> Result<Vec<Text>, BuildError> {
    if !is_type(item, "reasoning") { return Ok(Vec::new()); }
    let mut texts = Vec::new();
    collect_reasoning(prop(item, "summary"), &mut texts, budget)?;
    collect_reasoning(prop(item, "content"), &mut texts, budget)?;
    for key in ["text", "summary_text", "reasoning_text", "reasoning"] {
        if let Some(J::Str(text)) = prop(item, key) { texts.push(text.copy(budget)?); }
    }
    Ok(texts.into_iter().filter(|text| text.len_units() != 0).collect())
}

/// `data?.output ?? []` for iteration.
fn output_items(data: Prop<'_>) -> Result<&[J], BuildError> {
    match prop(data, "output") {
        None | Some(J::Null | J::Undefined) => Ok(&[]),
        Some(J::Arr(items)) => Ok(items),
        Some(_) => Err(BuildError::Unsupported("response-output-not-array")),
    }
}

/// `extractResponsesText(data, arg)`.
pub(crate) fn extract_text(data: Prop<'_>, format: Option<&Text>, budget: &Budget) -> Result<Text, BuildError> {
    let (mut texts, mut refusals, mut thoughts): (Vec<Text>, Vec<Text>, Vec<Text>) = (Vec::new(), Vec::new(), Vec::new());
    let top = match prop(data, "output_text") { Some(J::Str(text)) => Some(text), _ => None };
    if let Some(text) = top { texts.push(text.copy(budget)?); }
    for item in output_items(data)? {
        thoughts.extend(reasoning_texts(Some(item), budget)?);
        if !is_type(Some(item), "message") { continue; }
        let parts: &[J] = match prop(Some(item), "content") {
            None | Some(J::Null | J::Undefined) => &[],
            Some(J::Arr(parts)) => parts,
            Some(_) => return Err(BuildError::Unsupported("response-content-not-array")),
        };
        for part in parts {
            if top.is_none() && is_type(Some(part), "output_text") && truthy(prop(Some(part), "text")) {
                let Some(J::Str(text)) = prop(Some(part), "text") else { return Err(BuildError::Unsupported("response-text-not-text")) };
                texts.push(text.copy(budget)?);
            }
            if is_type(Some(part), "refusal") && truthy(prop(Some(part), "refusal")) {
                let Some(J::Str(text)) = prop(Some(part), "refusal") else { return Err(BuildError::Unsupported("response-text-not-text")) };
                refusals.push(text.copy(budget)?);
            }
        }
    }
    if !refusals.is_empty() && texts.is_empty() {
        let list: Vec<&Text> = refusals.iter().collect();
        return Ok(js::join(&list, "\n", budget)?);
    }
    let list: Vec<&Text> = texts.iter().collect();
    let mut result = js::join(&list, "\n", budget)?;
    if !thoughts.is_empty() && !js::starts_with(&result, "<Thoughts>") {
        let list: Vec<&Text> = thoughts.iter().collect();
        let mut shown = js::text("<Thoughts>\n\n", budget)?;
        shown.append(&js::join(&list, "\n\n", budget)?)?;
        shown.append(&js::text("\n\n</Thoughts>\n", budget)?)?;
        shown.append(&result)?;
        result = shown;
    }
    match format { Some(format) => extract_json(&result, format, budget), None => Ok(result) }
}

/// Whether `extractResponsesFunctionCalls(data)` finds a call.
pub(crate) fn has_function_calls(data: Prop<'_>) -> Result<bool, BuildError> {
    Ok(output_items(data)?.iter().any(|item| is_type(Some(item), "function_call") && truthy(prop(Some(item), "name")) && truthy(prop(Some(item), "call_id"))))
}

fn reason_text(data: Prop<'_>, budget: &Budget) -> Result<Option<Text>, BuildError> {
    let reason = prop(prop(data, "incomplete_details"), "reason");
    if !truthy(reason) { return Ok(None); }
    let mut shown = js::text("Incomplete response: ", budget)?;
    shown.append(&to_text(reason.expect("truthy"), budget).map_err(declined)?)?;
    Ok(Some(shown))
}

fn fail(text: Text) -> Result<ResponsesOutcome, BuildError> { Ok(ResponsesOutcome::Fail(text)) }

/// The response of `globalFetch`: `ok` is `res.ok`, `data` is `res.data`.
pub fn parse_buffered(ok: bool, data: &Value, context: &ResponsesContext<'_>, budget: &Budget) -> Result<ResponsesOutcome, BuildError> {
    let dat = js::from_value(data, budget)?;
    if !ok {
        let mut text = context.http_error_prefix.copy(budget)?;
        text.append(&stringify(&dat, budget)?)?;
        return fail(text);
    }
    let root = Some(&dat);
    let failed = matches!(prop(root, "status"), Some(J::Str(status)) if status.equals_ascii(b"failed"));
    if failed || truthy(prop(root, "error")) {
        let error = prop(root, "error").filter(|value| !matches!(value, J::Null | J::Undefined)).unwrap_or(&dat);
        return fail(stringify(error, budget)?);
    }
    if is_status(root, "incomplete") {
        let result = extract_text(root, context.extract_json, budget)?;
        let reason = reason_text(root, budget)?.unwrap_or(js::text("Incomplete response", budget)?);
        if result.len_units() == 0 { return fail(reason); }
        let mut shown = reason;
        shown.append(&js::text("\n", budget)?)?;
        shown.append(&result)?;
        return fail(shown);
    }
    if has_function_calls(root)? { return Err(BuildError::Unsupported("tool-round")); }
    let result = extract_text(root, context.extract_json, budget)?;
    if result.len_units() == 0 {
        return fail(match reason_text(root, budget)? { Some(reason) => reason, None => stringify(&dat, budget)? });
    }
    Ok(ResponsesOutcome::Success(result))
}

fn is_status(root: Prop<'_>, name: &str) -> bool { matches!(prop(root, "status"), Some(J::Str(status)) if status.equals_ascii(name.as_bytes())) }

/// Reads one buffered reply: classifies it as `globalFetch` reports it, parses it and derives the failure.
pub fn read_buffered(reply: &BufferedReply<'_>, context: &ResponsesContext<'_>, budget: &Budget) -> Result<ResponsesBuffered, BuildError> {
    let classified = classify(reply, budget)?;
    let result = parse_buffered(classified.ok, classified.data(), context, budget)?;
    let failure = match &result {
        ResponsesOutcome::Fail(text) => {
            let message = match (&classified.category, &classified.message) {
                (FailureCategory::Cancelled, _) => None,
                (_, Some(message)) => Some(bound_message(message, budget)?),
                (_, None) => Some(bound_message(text, budget)?),
            };
            Some(buffered_failure(classified.category.clone(), classified.status, message))
        }
        ResponsesOutcome::Success(_) => None,
    };
    Ok(ResponsesBuffered { result, failure })
}
