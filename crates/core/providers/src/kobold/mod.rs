//! `LLMFormat.Kobold` (`requestKobold`, `src/ts/process/request/request.ts`): a buffered KoboldAI `generate` request
//! with the rendered chat template as its prompt, and `results[0].text` as its reply.

use crate::common::built::{finish, utf8, BuiltRequest};
use crate::common::js::{self, J};
use crate::common::jsprop::{read, stringify, Failed};
use crate::common::owner::{bound_message, buffered_failure, classify, BufferedReply};
use crate::common::params;
use crate::common::settings::SamplingSettings;
use crate::common::url::href;
use crate::common::BuildError;
use risunest_ir::provider::{FailureCategory, LlmFormat, ProviderCall, ProviderFailure};
use risunest_ir::{Budget, Text};

#[cfg(test)]
mod replay;

/// The database fields `requestKobold` reads.
#[derive(Debug)]
pub struct KoboldSettings {
    /// `db.koboldURL`.
    pub kobold_url: Text,
    /// `db.maxContext`.
    pub max_context: f64,
    /// `getAdditionalParameters(aiModel)`.
    pub additional_params: Vec<(Text, Text)>,
}

/// What `requestChatDataMain` takes from `arg` besides the [`ProviderCall`].
pub struct KoboldInputs<'a> {
    /// `targ.aiModel`; it is also the `modelId` of `applyParameters`.
    pub ai_model: &'a Text,
    /// `applyChatTemplate(formated)`, rendered by the template stage.
    pub prompt: &'a Text,
    /// `arg.maxTokens ?? db.maxResponse`.
    pub max_tokens: f64,
    pub sampling: &'a SamplingSettings,
    pub settings: &'a KoboldSettings,
}

/// `new URL(db.koboldURL)` with `pathname = 'api/v1/generate'` when its path is shorter than three units.
fn generate_url(source: &Text) -> Result<String, BuildError> {
    let parsed = href(&utf8(source)?)?;
    let authority = parsed.find("://").map(|at| at + 3).ok_or(BuildError::Unsupported("url-form"))?;
    let path_start = parsed[authority..].find('/').map(|at| authority + at).ok_or(BuildError::Unsupported("url-form"))?;
    let path_end = parsed[path_start..].find(['?', '#']).map_or(parsed.len(), |at| path_start + at);
    if path_end - path_start >= 3 { return Ok(parsed); }
    Ok(format!("{}/api/v1/generate{}", &parsed[..path_start], &parsed[path_end..]))
}

/// Builds the request `requestKobold` sends. `call.messages` are only read by the template stage; `call.stream` is not
/// read, the request is always buffered.
pub fn build_request(call: &ProviderCall, inputs: &KoboldInputs<'_>, budget: &Budget) -> Result<BuiltRequest, BuildError> {
    if call.model.format != LlmFormat::Kobold { return Err(BuildError::Unsupported("format")); }
    let url = generate_url(&inputs.settings.kobold_url)?;
    let mut body = js::new_object();
    js::set_str(&mut body, "prompt", J::Str(inputs.prompt.copy(budget)?), budget)?;
    js::set_str(&mut body, "max_length", J::Num(inputs.max_tokens), budget)?;
    js::set_str(&mut body, "max_context_length", J::Num(inputs.settings.max_context), budget)?;
    js::set_str(&mut body, "n", J::Num(1.0), budget)?;
    let names: Vec<Text> = ["temperature", "top_p", "repetition_penalty", "top_k", "top_a"].iter().map(|name| js::text(name, budget)).collect::<Result<_, _>>()?;
    for (name, value) in params::apply_parameters(&names, inputs.sampling, &call.mode, inputs.ai_model, budget)? {
        let renamed = if name.equals_ascii(b"repetition_penalty") { js::text("rep_pen", budget)? } else { name };
        js::set(&mut body, renamed, js::from_value(&value, budget)?, budget)?;
    }
    let mut headers = js::new_object();
    js::set_str(&mut headers, "content-type", js::str_value("application/json", budget)?, budget)?;
    params::apply_additional_parameters(&mut body, &mut headers, &inputs.settings.additional_params, budget)?;
    finish(url, body, &headers, false, budget)
}

#[derive(Debug)]
pub enum KoboldOutcome {
    /// `{ type: 'success', result }`.
    Success(Text),
    /// `{ type: 'fail', result, noRetry: true }`: the owner must not retry it.
    Fail(Text),
}

#[derive(Debug)]
pub struct KoboldBuffered {
    pub result: KoboldOutcome,
    /// Set when the outcome is a failure.
    pub failure: Option<ProviderFailure>,
}

/// Reads one buffered reply the way `globalFetch` reports it and `requestKobold` handles it. A `results[0].text` that is
/// not a string would be returned as it is, and a missing `results[0]` throws; both are declined (`result-text-not-text`,
/// `thrown`).
pub fn read_buffered(reply: &BufferedReply<'_>, budget: &Budget) -> Result<KoboldBuffered, BuildError> {
    let classified = classify(reply, budget)?;
    let data = js::from_value(classified.data(), budget)?;
    if !classified.ok {
        let text = match &data { J::Str(text) => text.copy(budget)?, other => stringify(other, budget)? };
        let message = match (&classified.category, &classified.message) {
            (FailureCategory::Cancelled, _) => None,
            (_, Some(message)) => Some(bound_message(message, budget)?),
            (_, None) => Some(bound_message(&text, budget)?),
        };
        let failure = buffered_failure(classified.category.clone(), classified.status, message);
        return Ok(KoboldBuffered { result: KoboldOutcome::Fail(text), failure: Some(failure) });
    }
    let first = read(Some(&data), "results").and_then(|results| read(results, "0")).and_then(|first| read(first, "text"));
    match first.map_err(Failed::from)? {
        Some(J::Str(text)) => Ok(KoboldBuffered { result: KoboldOutcome::Success(text.copy(budget)?), failure: None }),
        _ => Err(BuildError::Unsupported("result-text-not-text")),
    }
}
