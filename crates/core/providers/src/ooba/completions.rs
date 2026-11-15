//! `requestOoba`: an OpenAI-style completion request to `/v1/completions` on `db.textgenWebUIBlockingURL`, with the
//! allowed `db.reverseProxyOobaArgs` merged in, and `choices[0].text` without `##\n` as its reply.

use super::{http_failure, stop_list, OobaBuffered, OobaInputs, OobaOutcome};
use crate::common::built::{finish, utf8, BuiltRequest};
use crate::common::js::{self, J};
use crate::common::jsprop::{read, Failed};
use crate::common::owner::{classify, BufferedReply};
use crate::common::params;
use crate::common::url::href;
use crate::common::BuildError;
use risunest_ir::provider::{LlmFormat, ProviderCall};
use risunest_ir::{Budget, Text, Value};

/// `OobaParams` (`src/ts/process/prompt.ts`): the `reverseProxyOobaArgs` keys copied into the body.
pub const OOBA_PARAMS: [&str; 33] = [
    "tokenizer", "min_p", "top_k", "repetition_penalty", "repetition_penalty_range", "typical_p", "tfs", "top_a", "epsilon_cutoff",
    "eta_cutoff", "guidance_scale", "negative_prompt", "penalty_alpha", "mirostat_mode", "mirostat_tau", "mirostat_eta", "temperature_last",
    "do_sample", "seed", "encoder_repetition_penalty", "no_repeat_ngram_size", "min_length", "num_beams", "length_penalty", "early_stopping",
    "truncation_length", "max_tokens_second", "custom_token_bans", "auto_max_new_tokens", "ban_eos_token", "add_bos_token",
    "skip_special_tokens", "grammar_string",
];

/// `new URL(url)` with `pathname = "/v1/completions"`; the query and fragment stay.
fn completions_url(source: &Text) -> Result<String, BuildError> {
    let parsed = href(&utf8(source)?)?;
    let authority = parsed.find("://").map(|at| at + 3).ok_or(BuildError::Unsupported("url-form"))?;
    let path_start = parsed[authority..].find('/').map(|at| authority + at).ok_or(BuildError::Unsupported("url-form"))?;
    let path_end = parsed[path_start..].find(['?', '#']).map_or(parsed.len(), |at| path_start + at);
    Ok(format!("{}/v1/completions{}", &parsed[..path_start], &parsed[path_end..]))
}

/// `arg.value || db.value / 100`.
fn penalty(argument: Option<f64>, stored: f64) -> f64 {
    match argument { Some(value) if value != 0.0 && !value.is_nan() => value, _ => stored / 100.0 }
}

/// Builds the request `requestOoba` sends. `call.stream` is not read: the request is always buffered.
pub fn build_request(call: &ProviderCall, inputs: &OobaInputs<'_>, budget: &Budget) -> Result<BuiltRequest, BuildError> {
    if call.model.format != LlmFormat::Ooba { return Err(BuildError::Unsupported("format")); }
    let settings = inputs.settings;
    let stop = stop_list(settings, budget)?;
    let mut body = js::new_object();
    js::set_str(&mut body, "prompt", J::Str(inputs.prompt.copy(budget)?), budget)?;
    js::set_str(&mut body, "presence_penalty", J::Num(penalty(inputs.presence_penalty, settings.presence_penalty)), budget)?;
    js::set_str(&mut body, "frequency_penalty", J::Num(penalty(inputs.frequency_penalty, settings.frequency_penalty)), budget)?;
    js::set_str(&mut body, "logit_bias", js::new_object(), budget)?;
    js::set_str(&mut body, "max_tokens", J::Num(inputs.max_tokens), budget)?;
    js::set_str(&mut body, "stop", stop, budget)?;
    js::set_str(&mut body, "temperature", J::Num(inputs.temperature), budget)?;
    js::set_str(&mut body, "top_p", J::Num(settings.top_p), budget)?;
    let url = completions_url(&settings.blocking_url)?;

    // A set, allowed argument replaces the body value; any other key removes a truthy body value.
    if let Value::Object(entries) = &settings.ooba_args {
        for (key, value) in entries.as_slice() {
            budget.work(1)?;
            let allowed = OOBA_PARAMS.iter().any(|name| key.equals_ascii(name.as_bytes()));
            if !matches!(value, Value::Undefined | Value::Null) && allowed {
                js::set(&mut body, key.copy(budget)?, js::from_value(value, budget)?, budget)?;
            } else if matches!(&body, J::Obj(own) if js::find(own, key).is_some_and(|at| js::truthy(&own[at].1))) {
                js::delete_text(&mut body, key);
            }
        }
    }

    let mut headers = js::new_object();
    params::apply_additional_parameters(&mut body, &mut headers, &settings.additional_params, budget)?;
    finish(url, body, &headers, false, budget)
}

/// Reads one buffered reply the way `globalFetch` reports it and `requestOoba` handles it. A missing `choices[0]`
/// throws (`thrown`), and a `text` that is neither nullish nor a string throws at `.replace` (`result-text-not-text`).
pub fn read_buffered(reply: &BufferedReply<'_>, http_error: &Text, budget: &Budget) -> Result<OobaBuffered, BuildError> {
    let classified = classify(reply, budget)?;
    if !classified.ok { return http_failure(&classified, http_error, budget); }
    let data = js::from_value(classified.data(), budget)?;
    let text = read(Some(&data), "choices").and_then(|choices| read(choices, "0")).and_then(|first| read(first, "text")).map_err(Failed::from)?;
    let text = match text {
        None | Some(J::Null) | Some(J::Undefined) => js::text("", budget)?,
        Some(J::Str(text)) => text.copy(budget)?,
        Some(_) => return Err(BuildError::Unsupported("result-text-not-text")),
    };
    // `text.replace(/##\n/g, '')`.
    let units = js::units(&text);
    let mut kept = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        if units[index..].starts_with(&[35, 35, 10]) { index += 3; continue; }
        kept.push(units[index]);
        index += 1;
    }
    Ok(OobaBuffered { result: OobaOutcome::Success(js::from_units(&kept, budget)?), failure: None })
}
