//! `requestOobaLegacy`: the text-generation-webui `/api/v1/generate` request built from `db.ooba`, and
//! `results[0].text` cut at the first speaker chunk as its reply. The streaming variant talks over a WebSocket and is
//! declined (`websocket-stream`).

use super::{http_failure, stop_list, OobaBuffered, OobaInputs, OobaOutcome};
use crate::common::built::{finish, BuiltRequest};
use crate::common::js::{self, J};
use crate::common::jsprop::read;
use crate::common::owner::{classify, BufferedReply};
use crate::common::params;
use crate::common::BuildError;
use crate::text_completion::{field, unstringlize_chat};
use risunest_ir::chat::PromptMessage;
use risunest_ir::provider::{LlmFormat, ProviderCall};
use risunest_ir::{Budget, Text};

/// `url.replace(/\/api.*/, "/api/v1/generate")`: from the first `/api` up to the next line terminator.
fn generate_url(source: &Text, budget: &Budget) -> Result<Text, BuildError> {
    let units = js::units(source);
    let Some(at) = js::find_units(&units, &js::utf16("/api"), 0) else { return Ok(source.copy(budget)?) };
    let end = units[at..].iter().position(|&unit| matches!(unit, 10 | 13 | 0x2028 | 0x2029)).map_or(units.len(), |offset| at + offset);
    let mut output = units[..at].to_vec();
    output.extend(js::utf16("/api/v1/generate"));
    output.extend_from_slice(&units[end..]);
    Ok(js::from_units(&output, budget)?)
}

/// Builds the request `requestOobaLegacy` sends. A streaming request opens a WebSocket in TS and is declined.
pub fn build_request(call: &ProviderCall, inputs: &OobaInputs<'_>, budget: &Budget) -> Result<BuiltRequest, BuildError> {
    if call.model.format != LlmFormat::OobaLegacy { return Err(BuildError::Unsupported("format")); }
    if call.stream { return Err(BuildError::Unsupported("websocket-stream")); }
    let settings = inputs.settings;
    let url = generate_url(&settings.blocking_url, budget)?;
    let stop = stop_list(settings, budget)?;
    let mut body = js::new_object();
    let field = |body: &mut J, name: &str| -> Result<(), BuildError> {
        let value = field(&settings.ooba, name, budget)?;
        Ok(js::set_str(body, name, value, budget)?)
    };
    js::set_str(&mut body, "max_new_tokens", J::Num(settings.max_response), budget)?;
    field(&mut body, "do_sample")?;
    js::set_str(&mut body, "temperature", J::Num(settings.temperature / 100.0), budget)?;
    for name in ["top_p", "typical_p", "repetition_penalty", "encoder_repetition_penalty", "top_k", "min_length", "no_repeat_ngram_size",
        "num_beams", "penalty_alpha", "length_penalty"] {
        field(&mut body, name)?;
    }
    js::set_str(&mut body, "early_stopping", J::Bool(false), budget)?;
    js::set_str(&mut body, "truncation_length", J::Num(inputs.max_tokens), budget)?;
    field(&mut body, "ban_eos_token")?;
    js::set_str(&mut body, "stopping_strings", stop, budget)?;
    js::set_str(&mut body, "seed", J::Num(-1.0), budget)?;
    field(&mut body, "add_bos_token")?;
    js::set_str(&mut body, "topP", J::Num(settings.top_p), budget)?;
    js::set_str(&mut body, "prompt", J::Str(inputs.prompt.copy(budget)?), budget)?;

    let mut headers = js::new_object();
    if !inputs.ai_model.equals_ascii(b"textgen_webui") {
        js::set_str(&mut headers, "X-API-KEY", J::Str(settings.mancer_header.copy(budget)?), budget)?;
    }
    params::apply_additional_parameters(&mut body, &mut headers, &settings.additional_params, budget)?;
    finish(crate::common::built::utf8(&url)?, body, &headers, false, budget)
}

/// What the reply cleanup reads: the messages after `reformater`, the character name and `getUserName()`.
pub struct ReplyContext<'a> {
    /// `language.errors.httpError`.
    pub http_error: &'a Text,
    pub formated: &'a [PromptMessage],
    /// `getCurrentCharacter()?.name ?? ''`.
    pub char_name: &'a Text,
    /// `getUserName()`.
    pub user_name: &'a Text,
}

/// Reads one buffered reply the way `globalFetch` reports it and `requestOobaLegacy` handles it. TS catches the
/// exception of an unreadable `results[0]` or a non-string `text` and fails with its message; both are declined
/// (`result-shape`, `result-text-not-text`).
pub fn read_buffered(reply: &BufferedReply<'_>, context: &ReplyContext<'_>, budget: &Budget) -> Result<OobaBuffered, BuildError> {
    let classified = classify(reply, budget)?;
    if !classified.ok { return http_failure(&classified, context.http_error, budget); }
    let data = js::from_value(classified.data(), budget)?;
    let text = read(Some(&data), "results").and_then(|results| read(results, "0")).and_then(|first| read(first, "text"))
        .map_err(|_| BuildError::Unsupported("result-shape"))?;
    let text = match text {
        None | Some(J::Null) | Some(J::Undefined) => js::text("", budget)?,
        Some(J::Str(text)) => text.copy(budget)?,
        Some(_) => return Err(BuildError::Unsupported("result-text-not-text")),
    };
    let cleaned = unstringlize_chat(&text, context.formated, context.char_name, context.user_name, budget)?;
    Ok(OobaBuffered { result: OobaOutcome::Success(cleaned), failure: None })
}
