//! The glue between the owner (transport, MCP, alerts) and the builders: the mapping of buffered replies to
//! [`ProviderFailure`], and the answer to a response that asks for tools. Target resolution, the `JSON.parse` messages,
//! the message bound, stream-start failures and decline settlement are shared with the other families in
//! [`crate::common::owner`] and re-exported here.

pub use crate::common::owner::*;
use super::request::ToolResponsePart;
use super::response::{parse_buffered, Buffered, ParseContext, ToolUse};
use super::BuildError;
use crate::common::js;
use risunest_ir::provider::{FailureCategory, ProviderFailure};
use risunest_ir::{Budget, Error, Text, Value};

#[derive(Debug)]
pub struct BufferedOutcome {
    pub result: Buffered,
    /// Set when the outcome is a failure.
    pub failure: Option<ProviderFailure>,
}

/// Reads one buffered reply. A response with `tool_use` blocks is not an outcome: [`BufferedOutcome::result`] is then
/// [`Buffered::ToolUse`] and the owner runs [`answer_tool_use`].
pub fn read_buffered(reply: &BufferedReply<'_>, context: &ParseContext<'_>, budget: &Budget) -> Result<BufferedOutcome, BuildError> {
    let classified = classify(reply, budget)?;
    let result = parse_buffered(classified.ok, classified.data(), context, budget)?;
    let failure = match &result {
        Buffered::Fail { text, .. } => {
            let message = match (&classified.category, &classified.message) {
                (FailureCategory::Cancelled, _) => None,
                (_, Some(message)) => Some(bound_message(message, budget)?),
                (_, None) => Some(bound_message(text, budget)?),
            };
            Some(buffered_failure(classified.category.clone(), classified.status, message))
        }
        _ => None,
    };
    Ok(BufferedOutcome { result, failure })
}

/// What running a tool gives: the reply parts, or the message of the error `callTool` threw; the outer error is a budget
/// failure.
pub type ToolRun = Result<Result<Vec<ToolResponsePart>, Text>, Error>;
/// What encoding a call for `rememberToolUsage` gives, with the same two error levels.
pub type ToolCode = Result<Result<Text, Text>, Error>;

/// What the tool round of one response produced.
#[derive(Debug, Default)]
pub struct ToolAnswer {
    /// One reply per `tool_use` block, in order, for [`super::request::push_tool_round`].
    pub replies: Vec<Vec<ToolResponsePart>>,
    /// `arg.additionalOutput += code` for every call when `rememberToolUsage` is on, concatenated.
    pub remembered: Option<Text>,
}

/// The loop over a response's `tool_use` blocks. `run` is `callTool(name, input)`; `encode` is `encodeToolCall` for
/// `rememberToolUsage`. Either one throwing ends the request in TS, so `Err(text)` becomes [`BuildError::Thrown`].
pub fn answer_tool_use(
    round: &ToolUse, remember: bool,
    run: &mut dyn FnMut(&Text, &Value) -> ToolRun,
    encode: &mut dyn FnMut(&super::response::ToolUseCall, &[ToolResponsePart]) -> ToolCode,
    budget: &Budget,
) -> Result<ToolAnswer, BuildError> {
    let mut answer = ToolAnswer::default();
    let mut remembered = js::text("", budget)?;
    for call in &round.calls {
        let parts = match run(&call.name, &call.input)? { Ok(parts) => parts, Err(error) => return Err(BuildError::Thrown(error)) };
        if remember {
            match encode(call, &parts)? { Ok(code) => remembered.append(&code)?, Err(error) => return Err(BuildError::Thrown(error)) }
        }
        answer.replies.push(parts);
    }
    if remember { answer.remembered = Some(remembered); }
    Ok(answer)
}
