//! `LLMFormat.NovelAI` (`requestNovelAI`, `src/ts/process/request/request.ts`): the NovelAI `ai/generate` request
//! with the chat as NovelAI text (`stringlizeNAIChat`, `src/ts/process/models/nai.ts`), and `output` cut at the first
//! speaker chunk as its reply. The preview mode returns a fixed error text and builds no request.

pub mod data;

#[cfg(test)]
mod replay;

use crate::common::built::{finish, BuiltRequest};
use crate::common::js::{self, J};
use crate::common::jsprop::{read, stringify, truthy, Failed};
use crate::common::owner::{bound_message, buffered_failure, classify, BufferedReply};
use crate::common::params;
use crate::common::BuildError;
use crate::text_completion::{field, unstringlize_chat};
use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::provider::{FailureCategory, LlmFormat, ProviderCall, ProviderFailure};
use risunest_ir::{Budget, Error, Text, Value};

/// What the preview mode returns for NovelAI instead of a request.
pub const PREVIEW_RESULT: &str = "{\"error\":\"This model is not supported in preview mode\"}";

/// The database fields `requestNovelAI` reads.
pub struct NovelAiSettings {
    /// `db.novelai.token`.
    pub token: Text,
    /// `db.NAIsettings`, read with JavaScript property semantics.
    pub nai_settings: Value,
    /// `db.NAIadventure` and `db.NAIappendName`.
    pub adventure: bool,
    pub append_name: bool,
    /// `getAdditionalParameters(aiModel)`.
    pub additional_params: Vec<(Text, Text)>,
}

impl std::fmt::Debug for NovelAiSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("NovelAiSettings").field("token", &"<redacted>").field("adventure", &self.adventure)
            .field("append_name", &self.append_name).field("additional_params", &self.additional_params.len()).finish()
    }
}

/// What `requestChatDataMain` takes from `arg` besides the [`ProviderCall`]. `call.bias` is `arg.biasString`.
pub struct NovelAiInputs<'a> {
    /// `targ.aiModel`.
    pub ai_model: &'a Text,
    /// `arg.key`: the reverse proxy or custom model key, when there is one.
    pub key: Option<&'a Text>,
    /// `arg.maxTokens ?? db.maxResponse`.
    pub max_tokens: f64,
    /// `arg.temperature ?? db.temperature / 100`.
    pub temperature: f64,
    /// `tokenizeNum(text)` of every `call.bias` entry, in order; tokenization runs in the tokenizer stage.
    pub bias_tokens: &'a [Vec<f64>],
    /// `getCurrentCharacter()?.name ?? ''`.
    pub char_name: &'a Text,
    /// `getUserName()`.
    pub user_name: &'a Text,
    /// `arg.continue`.
    pub continued: bool,
    pub settings: &'a NovelAiSettings,
}

/// `value.replaceAll("\\n", "\n") || fallback`.
fn unescaped_or(value: &J, fallback: &str, budget: &Budget) -> Result<Vec<u16>, BuildError> {
    let J::Str(value) = value else { return Err(BuildError::Unsupported("nai-settings-text")) };
    let units = js::units(value);
    let mut output = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        if units[index] == 92 && units.get(index + 1) == Some(&110) { output.push(10); index += 2; continue; }
        output.push(units[index]);
        index += 1;
    }
    budget.work(units.len().saturating_add(1))?;
    Ok(if output.is_empty() { js::utf16(fallback) } else { output })
}

/// `stringlizeNAIChat(formated, char, continued)`. A separator or starter that is not a string throws in TS
/// (`nai-settings-text`).
pub fn stringlize_nai_chat(
    formated: &[PromptMessage], inputs: &NovelAiInputs<'_>, budget: &Budget,
) -> Result<Text, BuildError> {
    let settings = inputs.settings;
    let separator = unescaped_or(&field(&settings.nai_settings, "seperator", budget)?, "\n", budget)?;
    let starter = unescaped_or(&field(&settings.nai_settings, "starter", budget)?, "\u{2042}", budget)?;
    let mut lines: Vec<Vec<u16>> = Vec::new();
    for form in formated {
        budget.work(form.content.len_units().saturating_add(1))?;
        let content = js::units(&form.content);
        let named = form.name.as_ref().is_some_and(|name| name.len_units() != 0);
        let line = if form.role == Role::System {
            let marker = form.memo.as_ref().is_some_and(|memo| memo.equals_ascii(b"NewChatExample") || memo.equals_ascii(b"NewChat"));
            if marker || form.content.equals_ascii(b"[Start a new chat]") { starter.clone() } else { content }
        } else if named || form.role == Role::Assistant {
            if !settings.append_name { content } else {
                let mut line = js::units(form.name.as_ref().unwrap_or(inputs.char_name));
                line.extend(js::utf16(": "));
                line.extend(content);
                line
            }
        } else if form.role == Role::User {
            let mut line = Vec::new();
            if settings.adventure { line.extend(js::utf16("> ")); }
            if settings.append_name { line.extend(js::units(inputs.user_name)); line.extend(js::utf16(": ")); }
            line.extend(content);
            line
        } else { content };
        lines.push(line);
    }
    let mut output: Vec<u16> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if index != 0 { output.extend_from_slice(&separator); }
        output.extend_from_slice(line);
    }
    if !inputs.continued {
        output.extend_from_slice(&separator);
        output.extend(js::units(inputs.char_name));
        output.push(58);
    }
    Ok(js::from_units(&output, budget)?)
}

fn numbers(values: &[u32]) -> J { J::Arr(values.iter().map(|&value| J::Num(f64::from(value))).collect()) }

/// `gen[name] ?? fallback`.
fn or_default(settings: &Value, name: &str, fallback: f64, budget: &Budget) -> Result<J, Error> {
    Ok(match field(settings, name, budget)? { J::Undefined | J::Null => J::Num(fallback), value => value })
}

/// Builds the request `requestNovelAI` sends. The request is always buffered.
pub fn build_request(call: &ProviderCall, inputs: &NovelAiInputs<'_>, budget: &Budget) -> Result<BuiltRequest, BuildError> {
    if call.model.format != LlmFormat::NovelAI { return Err(BuildError::Unsupported("format")); }
    if inputs.bias_tokens.len() != call.bias.len() { return Err(BuildError::Unsupported("bias-tokens")); }
    let prompt = stringlize_nai_chat(&call.messages, inputs, budget)?;
    let settings = inputs.settings;
    let generation = &settings.nai_settings;

    let mut bias = Vec::new();
    for ((_, value), tokens) in call.bias.iter().zip(inputs.bias_tokens) {
        let mut entry = js::new_object();
        js::set_str(&mut entry, "sequence", J::Arr(tokens.iter().map(|&token| J::Num(token)).collect()), budget)?;
        js::set_str(&mut entry, "bias", J::Num(*value), budget)?;
        js::set_str(&mut entry, "ensure_sequence_finish", J::Bool(false), budget)?;
        js::set_str(&mut entry, "generate_once", J::Bool(true), budget)?;
        bias.push(entry);
    }

    let mut payload = js::new_object();
    let mut put = |name: &str, value: J| js::set_str(&mut payload, name, value, budget);
    put("temperature", J::Num(inputs.temperature))?;
    put("max_length", J::Num(inputs.max_tokens))?;
    put("min_length", J::Num(1.0))?;
    for (name, key) in [("top_k", "topK"), ("top_p", "topP"), ("top_a", "topA"), ("tail_free_sampling", "tailFreeSampling"),
        ("repetition_penalty", "repetitionPenalty"), ("repetition_penalty_range", "repetitionPenaltyRange"),
        ("repetition_penalty_slope", "repetitionPenaltySlope"), ("repetition_penalty_frequency", "frequencyPenalty"),
        ("repetition_penalty_presence", "presencePenalty")] {
        put(name, field(generation, key, budget)?)?;
    }
    put("generate_until_sentence", J::Bool(true))?;
    put("use_cache", J::Bool(false))?;
    put("use_string", J::Bool(true))?;
    put("return_full_text", J::Bool(false))?;
    put("prefix", js::str_value(if settings.adventure { "theme_textadventure" } else { "vanilla" }, budget)?)?;
    put("order", numbers(&[6, 2, 3, 0, 4, 1, 5, 8]))?;
    put("typical_p", field(generation, "typicalp", budget)?)?;
    put("repetition_penalty_whitelist", numbers(&data::REPETITION_PENALTY_WHITELIST))?;
    put("stop_sequences", J::Arr(vec![numbers(&[49287]), numbers(&[49405])]))?;
    put("bad_words_ids", J::Arr(data::BAD_WORD_IDS.iter().map(|ids| numbers(ids)).collect()))?;
    put("logit_bias_exp", J::Arr(bias))?;
    put("mirostat_lr", or_default(generation, "mirostat_lr", 1.0, budget)?)?;
    put("mirostat_tau", or_default(generation, "mirostat_tau", 0.0, budget)?)?;
    put("cfg_scale", or_default(generation, "cfg_scale", 1.0, budget)?)?;
    put("cfg_uc", js::str_value("", budget)?)?;

    let kayra = inputs.ai_model.equals_ascii(b"novelai_kayra");
    let mut body = js::new_object();
    js::set_str(&mut body, "input", J::Str(prompt), budget)?;
    js::set_str(&mut body, "model", js::str_value(if kayra { "kayra-v1" } else { "clio-v1" }, budget)?, budget)?;
    js::set_str(&mut body, "parameters", payload, budget)?;
    let mut headers = js::new_object();
    let key = inputs.key.unwrap_or(&settings.token);
    js::set_str(&mut headers, "Authorization", J::Str(js::concat(&js::text("Bearer ", budget)?, key, budget)?), budget)?;
    params::apply_additional_parameters(&mut body, &mut headers, &settings.additional_params, budget)?;
    let url = if kayra { "https://text.novelai.net/ai/generate" } else { "https://api.novelai.net/ai/generate" };
    finish(url.to_owned(), body, &headers, false, budget)
}

#[derive(Debug)]
pub enum NovelAiOutcome {
    /// `{ type: 'success', result }`.
    Success(Text),
    /// `{ type: 'fail', result }`.
    Fail(Text),
}

#[derive(Debug)]
pub struct NovelAiBuffered {
    pub result: NovelAiOutcome,
    /// Set when the outcome is a failure.
    pub failure: Option<ProviderFailure>,
}

/// What the reply reader needs besides the reply.
pub struct ReplyContext<'a> {
    /// `language.errors.httpError`.
    pub http_error: &'a Text,
    pub formated: &'a [PromptMessage],
    pub char_name: &'a Text,
    pub user_name: &'a Text,
}

/// Reads one buffered reply the way `globalFetch` reports it and `requestNovelAI` handles it. An `ok` reply whose data
/// is `null` throws (`thrown`), and a truthy `output` that is not a string throws in `unstringlizeChat`
/// (`result-text-not-text`).
pub fn read_buffered(reply: &BufferedReply<'_>, context: &ReplyContext<'_>, budget: &Budget) -> Result<NovelAiBuffered, BuildError> {
    let classified = classify(reply, budget)?;
    let data = js::from_value(classified.data(), budget)?;
    let output = if classified.ok { read(Some(&data), "output").map_err(Failed::from)? } else { None };
    if !classified.ok || !truthy(output) {
        let text = js::concat(context.http_error, &stringify(&data, budget)?, budget)?;
        let message = match (&classified.category, &classified.message) {
            (FailureCategory::Cancelled, _) => None,
            (_, Some(message)) => Some(bound_message(message, budget)?),
            (_, None) => Some(bound_message(&text, budget)?),
        };
        let failure = buffered_failure(classified.category.clone(), classified.status, message);
        return Ok(NovelAiBuffered { result: NovelAiOutcome::Fail(text), failure: Some(failure) });
    }
    let Some(J::Str(output)) = output else { return Err(BuildError::Unsupported("result-text-not-text")) };
    let cleaned = unstringlize_chat(output, context.formated, context.char_name, context.user_name, budget)?;
    Ok(NovelAiBuffered { result: NovelAiOutcome::Success(cleaned), failure: None })
}
