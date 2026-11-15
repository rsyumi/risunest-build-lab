//! The response handling of the Mistral branch: `choices[0].message.content` on success, the `httpError` text with the
//! server's error message or the whole body otherwise.

use crate::common::js::{self, J};
use crate::common::jsprop::{read, read_optional, stringify, to_text, truthy, Failed, Prop};
use crate::common::owner::{bound_message, buffered_failure, classify, BufferedReply};
use crate::common::BuildError;
use risunest_ir::provider::{FailureCategory, ProviderFailure};
use risunest_ir::{Budget, Text};

#[derive(Debug)]
pub enum MistralOutcome {
    /// `{ type: 'success', result }`.
    Success(Text),
    /// `{ type: 'fail', result }`.
    Fail(Text),
}

#[derive(Debug)]
pub struct MistralBuffered {
    pub result: MistralOutcome,
    /// Set when the outcome is a failure.
    pub failure: Option<ProviderFailure>,
}

/// `dat.choices[0].message.content`; every property read on a missing value throws, which the caller turns into a failure.
fn first_content<'a>(data: &'a J) -> Result<Prop<'a>, crate::common::jsprop::Thrown> {
    let choices = read(Some(data), "choices")?;
    let choice = read(choices, "0")?;
    let message = read(choice, "message")?;
    read(message, "content")
}

fn http_failure(prefix: &Text, shown: &Text, budget: &Budget) -> Result<Text, BuildError> {
    let mut text = prefix.copy(budget)?;
    text.append(shown)?;
    Ok(text)
}

/// Reads one buffered reply the way `globalFetch` reports it and the Mistral branch handles it. A `content` that is not
/// a string or nullish would be returned as it is; it is declined. `http_error_prefix` is `language.errors.httpError`.
pub fn read_buffered(reply: &BufferedReply<'_>, http_error_prefix: &Text, budget: &Budget) -> Result<MistralBuffered, BuildError> {
    let classified = classify(reply, budget)?;
    let data = js::from_value(classified.data(), budget)?;
    let result = if classified.ok {
        match first_content(&data) {
            Ok(None | Some(J::Null | J::Undefined)) => MistralOutcome::Success(js::text("", budget)?),
            Ok(Some(J::Str(text))) => MistralOutcome::Success(text.copy(budget)?),
            Ok(Some(_)) => return Err(BuildError::Unsupported("response-content-not-text")),
            Err(_) => MistralOutcome::Fail(http_failure(http_error_prefix, &stringify(&data, budget)?, budget)?),
        }
    } else {
        // `dat.error && dat.error.message`: reading `error` of `null` throws and is not caught.
        let error: Prop<'_> = match read(Some(&data), "error") {
            Ok(error) => error,
            Err(_) => return Err(BuildError::Thrown(js::text("Cannot read properties of null (reading 'error')", budget)?)),
        };
        let message = if truthy(error) { read_optional(error, "message").unwrap_or(None) } else { None };
        if truthy(message) {
            let shown = to_text(message.expect("truthy"), budget).map_err(|failed| match failed {
                Failed::Budget(error) => BuildError::Budget(error),
                _ => BuildError::Unsupported("thrown"),
            })?;
            MistralOutcome::Fail(http_failure(http_error_prefix, &shown, budget)?)
        } else {
            MistralOutcome::Fail(http_failure(http_error_prefix, &stringify(&data, budget)?, budget)?)
        }
    };
    let failure = match &result {
        MistralOutcome::Fail(text) => {
            let message = match (&classified.category, &classified.message) {
                (FailureCategory::Cancelled, _) => None,
                (_, Some(message)) => Some(bound_message(message, budget)?),
                (_, None) => Some(bound_message(text, budget)?),
            };
            Some(buffered_failure(classified.category.clone(), classified.status, message))
        }
        MistralOutcome::Success(_) => None,
    };
    Ok(MistralBuffered { result, failure })
}
