//! The response handling of `requestCohere`: `res.data.text` on success, `JSON.stringify(res.data)` otherwise.

use crate::common::js::{self, J};
use crate::common::jsprop::{read_optional, stringify, truthy, Prop};
use crate::common::owner::{bound_message, buffered_failure, classify, BufferedReply};
use crate::common::BuildError;
use risunest_ir::provider::{FailureCategory, ProviderFailure};
use risunest_ir::{Budget, Text};

#[derive(Debug)]
pub enum CohereOutcome {
    /// `{ type: 'success', result }`.
    Success(Text),
    /// `{ type: 'fail', result }`.
    Fail(Text),
}

#[derive(Debug)]
pub struct CohereBuffered {
    pub result: CohereOutcome,
    /// Set when the outcome is a failure.
    pub failure: Option<ProviderFailure>,
}

/// Reads one buffered reply the way `globalFetch` reports it and `requestCohere` handles it. A `text` that is truthy but
/// not a string would be returned as it is; it is declined.
pub fn read_buffered(reply: &BufferedReply<'_>, budget: &Budget) -> Result<CohereBuffered, BuildError> {
    let classified = classify(reply, budget)?;
    let data = js::from_value(classified.data(), budget)?;
    let fail = |data: &J| -> Result<CohereOutcome, BuildError> { Ok(CohereOutcome::Fail(stringify(data, budget)?)) };
    let result = if !classified.ok {
        fail(&data)?
    } else {
        let text: Prop<'_> = read_optional(Some(&data), "text").unwrap_or(None);
        match text {
            Some(J::Str(text)) if text.len_units() != 0 => CohereOutcome::Success(text.copy(budget)?),
            other if !truthy(other) => fail(&data)?,
            _ => return Err(BuildError::Unsupported("response-text-not-text")),
        }
    };
    let failure = match &result {
        CohereOutcome::Fail(text) => {
            let message = match (&classified.category, &classified.message) {
                (FailureCategory::Cancelled, _) => None,
                (_, Some(message)) => Some(bound_message(message, budget)?),
                (_, None) => Some(bound_message(text, budget)?),
            };
            Some(buffered_failure(classified.category.clone(), classified.status, message))
        }
        CohereOutcome::Success(_) => None,
    };
    Ok(CohereBuffered { result, failure })
}
