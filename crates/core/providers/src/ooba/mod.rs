//! The text-generation-webui formats: `LLMFormat.Ooba` (`requestOoba`, the OpenAI-style `/v1/completions` endpoint) in
//! [`completions`] and `LLMFormat.OobaLegacy` (`requestOobaLegacy`, the `/api/v1/generate` endpoint, `mancer`) in
//! [`legacy`] (`src/ts/process/request/request.ts`). Both are buffered; the legacy WebSocket stream is declined.

pub mod completions;
pub mod legacy;

#[cfg(test)]
mod replay;

use crate::common::js::{self, J};
use crate::common::jsprop::stringify;
use crate::common::owner::{bound_message, buffered_failure, Classified};
use crate::common::BuildError;
use crate::text_completion::stop_strings;
use risunest_ir::provider::{FailureCategory, ProviderFailure};
use risunest_ir::{Budget, Text, Value};

/// The database fields the two formats read.
#[derive(Debug)]
pub struct OobaSettings {
    /// `db.textgenWebUIBlockingURL`.
    pub blocking_url: Text,
    /// `db.ooba`, read with JavaScript property semantics (a missing field is `undefined`).
    pub ooba: Value,
    /// `db.ooba.formating.userPrefix`; `None` when it is `undefined`.
    pub user_prefix: Option<Text>,
    /// `db.username`.
    pub username: Text,
    /// `db.localStopStrings`.
    pub local_stop_strings: Option<Vec<Text>>,
    /// `db.top_p`.
    pub top_p: f64,
    /// `db.PresensePenalty` and `db.frequencyPenalty`, unscaled.
    pub presence_penalty: f64,
    pub frequency_penalty: f64,
    /// `db.maxResponse` and `db.temperature` (unscaled), which the legacy body reads directly.
    pub max_response: f64,
    pub temperature: f64,
    /// `db.reverseProxyOobaArgs`.
    pub ooba_args: Value,
    /// `db.mancerHeader`.
    pub mancer_header: Text,
    /// `getAdditionalParameters(aiModel)`.
    pub additional_params: Vec<(Text, Text)>,
}

/// What `requestChatDataMain` takes from `arg` besides the [`ProviderCall`](risunest_ir::provider::ProviderCall).
pub struct OobaInputs<'a> {
    /// `targ.aiModel`.
    pub ai_model: &'a Text,
    /// `applyChatTemplate(formated)`, rendered by the template stage.
    pub prompt: &'a Text,
    /// `arg.maxTokens ?? db.maxResponse`.
    pub max_tokens: f64,
    /// `arg.temperature ?? db.temperature / 100`.
    pub temperature: f64,
    /// `arg.PresensePenalty` and `arg.frequencyPenalty`.
    pub presence_penalty: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub settings: &'a OobaSettings,
}

/// The stop list both formats send: `getStopStrings(false)`, or `db.localStopStrings` when it is set. A nonempty local
/// list runs every entry through `risuChatParser` (CBS) in TS and is declined (`local-stop-strings`).
fn stop_list(settings: &OobaSettings, budget: &Budget) -> Result<J, BuildError> {
    let list = match &settings.local_stop_strings {
        None => stop_strings(settings.user_prefix.as_ref(), &settings.username, budget)?,
        Some(local) if local.is_empty() => Vec::new(),
        Some(_) => return Err(BuildError::Unsupported("local-stop-strings")),
    };
    Ok(J::Arr(list.into_iter().map(|entry| entry.map_or(J::Undefined, J::Str)).collect()))
}

#[derive(Debug)]
pub enum OobaOutcome {
    /// `{ type: 'success', result }`.
    Success(Text),
    /// `{ type: 'fail', result }`.
    Fail(Text),
}

#[derive(Debug)]
pub struct OobaBuffered {
    pub result: OobaOutcome,
    /// Set when the outcome is a failure.
    pub failure: Option<ProviderFailure>,
}

/// `language.errors.httpError + JSON.stringify(res.data)` for a reply that is not `ok`.
fn http_failure(classified: &Classified<'_>, http_error: &Text, budget: &Budget) -> Result<OobaBuffered, BuildError> {
    let data = js::from_value(classified.data(), budget)?;
    let text = js::concat(http_error, &stringify(&data, budget)?, budget)?;
    let message = match (&classified.category, &classified.message) {
        (FailureCategory::Cancelled, _) => None,
        (_, Some(message)) => Some(bound_message(message, budget)?),
        (_, None) => Some(bound_message(&text, budget)?),
    };
    Ok(OobaBuffered { result: OobaOutcome::Fail(text), failure: Some(buffered_failure(classified.category.clone(), classified.status, message)) })
}
