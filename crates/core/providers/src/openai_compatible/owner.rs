//! The glue between the owner (transport, MCP, tokenizer, alerts) and the builders: the answer to a model's tool
//! calls, the composition of a buffered tool round, and the mapping of buffered replies to [`ProviderFailure`].
//! Target resolution, `JSON.parse` messages, the message bound, stream-start failures and decline settlement are shared
//! with the other families in [`crate::common::owner`] and re-exported here.

pub use crate::common::owner::*;
use super::js;
use super::request::ToolReply;
use super::response::{parse_buffered, Buffered, ParseContext, ToolCallEntry, ToolCalls};
use super::BuildError;
use risunest_ir::provider::{FailureCategory, ProviderFailure, ToolSpec};
use risunest_ir::{json, Budget, Error, Text, Value};

/// `db.genTime > 1 && aiModel.startsWith('gpt') && !arg.continue && !arg.noMultiGen`.
pub fn multi_gen(gen_time: f64, ai_model: &Text, continuing: bool, no_multi_gen: bool) -> bool {
    gen_time > 1.0 && js::starts_with(ai_model, "gpt") && !continuing && !no_multi_gen
}


// ---------------------------------------------------------------------------------------------------------------
// Buffered replies.

#[derive(Debug)]
pub struct BufferedOutcome {
    pub result: Buffered,
    /// Set when the outcome is a failure.
    pub failure: Option<ProviderFailure>,
}

/// The text a failure shows after the `httpError` prefix.
fn without_prefix(shown: &Text, prefix: &Text, budget: &Budget) -> Result<Text, Error> {
    let units = js::units(shown);
    let skip = prefix.len_units();
    js::from_units(&units[skip.min(units.len())..], budget)
}

pub fn read_buffered(reply: &BufferedReply<'_>, context: &ParseContext<'_>, budget: &Budget) -> Result<BufferedOutcome, BuildError> {
    let classified = classify(reply, budget)?;
    let (ok, status, data, category, message) = (classified.ok, classified.status, classified.data(), classified.category.clone(), classified.message.as_ref().map(|text| text.copy(budget)).transpose()?);
    let result = parse_buffered(ok, data, context, budget)?;
    let failure = match &result {
        Buffered::Fail(shown) => {
            let message = match (&category, message) {
                (FailureCategory::Cancelled, _) => None,
                (_, Some(message)) => Some(bound_message(&message, budget)?),
                (_, None) => Some(bound_message(&without_prefix(shown, context.http_error_prefix, budget)?, budget)?),
            };
            Some(buffered_failure(category, status, message))
        }
        _ => None,
    };
    Ok(BufferedOutcome { result, failure })
}

// ---------------------------------------------------------------------------------------------------------------
// Tool rounds.

/// What the host's tool call produced.
pub enum ToolRun {
    /// The `text` entries of the tool response.
    Texts(Vec<Text>),
    /// The tool threw; the text is `String(error)`.
    Threw(Text),
}

#[derive(Debug, Default)]
pub struct RoundReplies {
    pub replies: Vec<ToolReply>,
    /// `rememberToolUsage` codes, in call order.
    pub call_codes: Vec<Text>,
}

/// What `encodeToolCall` gives: the code, or the text of the error it threw.
pub type EncodeResult = Result<Result<Text, Text>, Error>;

/// The loop over a response's tool calls in `requestHTTPOpenAI` and `wrapToolStream`. `run` calls the tool with its
/// parsed arguments; `encode` is `encodeToolCall` for `rememberToolUsage` and answers `Err(text)` when it throws.
pub fn answer_tool_calls(
    calls: &[ToolCallEntry], tools: &[ToolSpec], remember: bool,
    run: &mut dyn FnMut(&Text, &Value) -> Result<ToolRun, Error>,
    encode: &mut dyn FnMut(&ToolCallEntry, &[Text]) -> EncodeResult,
    budget: &Budget,
) -> Result<RoundReplies, BuildError> {
    let mut output = RoundReplies::default();
    for call in calls {
        let parsed = if call.arguments.len_units() == 0 { Value::Object(risunest_ir::OwnedVec::new(budget)?) } else {
            let Some(source) = call.arguments.as_unicode() else { return Err(BuildError::Unsupported("tool-arguments-isolated-surrogate")) };
            match json::parse(source, budget) {
                Ok(value) => value,
                Err(Error::Syntax { .. }) => {
                    output.replies.push(ToolReply::failed(call, &json_parse_error(&call.arguments, budget)?, budget)?);
                    continue;
                }
                Err(other) => return Err(other.into()),
            }
        };
        if tools.is_empty() { continue; }
        let Some(tool) = call.tool_name().and_then(|name| tools.iter().find(|tool| tool.name.equals(name))) else {
            output.replies.push(ToolReply::no_tool(call, budget)?);
            continue;
        };
        match run(&tool.name, &parsed)? {
            ToolRun::Threw(error) => output.replies.push(ToolReply::failed(call, &error, budget)?),
            ToolRun::Texts(texts) if texts.is_empty() => output.replies.push(ToolReply::no_text(call, budget)?),
            ToolRun::Texts(texts) => {
                output.replies.push(ToolReply::text(call, &texts[0], budget)?);
                if remember {
                    match encode(call, &texts)? {
                        Ok(code) => output.call_codes.push(code),
                        Err(error) => output.replies.push(ToolReply::failed(call, &error, budget)?),
                    }
                }
            }
        }
    }
    Ok(output)
}

/// The value `requestHTTPOpenAI` returns after a tool round and the follow-up request: the model's text (left out
/// with `simplifiedToolUse`) and the remembered codes, then the follow-up's text. A failed follow-up leaves the first
/// part alone; a text that cannot be rendered turns the whole round into the failure TS reports.
pub fn compose_tool_round(
    round: &ToolCalls, call_codes: &[Text], simplified_tool_use: bool, follow_up: Buffered, budget: &Budget,
) -> Result<Buffered, Error> {
    let lead = match &round.text {
        Err(failure) => return Ok(Buffered::Fail(failure.copy(budget)?)),
        Ok(text) => text,
    };
    let codes: Vec<&Text> = call_codes.iter().collect();
    let mut result = if simplified_tool_use { js::text("", budget)? } else { js::concat(lead, &js::text("\n\n", budget)?, budget)? };
    result.append(&js::join(&codes, "\n\n", budget)?)?;
    Ok(match follow_up {
        Buffered::Fail(_) => Buffered::Success(result),
        Buffered::Success(rest) => {
            result.append(&js::text("\n\n", budget)?)?;
            result.append(&rest)?;
            Buffered::Success(result)
        }
        other => other,
    })
}
