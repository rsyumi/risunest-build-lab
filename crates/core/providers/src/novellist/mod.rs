//! `LLMFormat.NovelList` (`requestNovelList`, `src/ts/process/request/request.ts`): the NovelList (tringpt) `api`
//! request with the AIN-formatted chat as its text (`stringlizeAINChat`), and the reply split into speaker lines
//! (`unstringlizeAIN`, a `multiline` result).

use crate::common::built::{finish, BuiltRequest};
use crate::common::js::{self, J};
use crate::common::jsprop::{read, to_text, truthy, Failed};
use crate::common::owner::{bound_message, buffered_failure, classify, BufferedReply};
use crate::common::params;
use crate::common::wire;
use crate::common::BuildError;
use crate::text_completion::{field, unstringlizer_chunks, ChunkMode};
use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::provider::{FailureCategory, LlmFormat, ProviderCall, ProviderFailure};
use risunest_ir::{Budget, Error, Text, Value};

#[cfg(test)]
mod replay;

/// `api_server_url + '/api'`; the preview always shows it, even when a custom URL is sent to.
pub const DEFAULT_URL: &str = "https://api.tringpt.com//api";

const OPEN: u16 = 0x300c;
const CLOSE: u16 = 0x300d;

/// The database fields `requestNovelList` reads.
pub struct NovelListSettings {
    /// `db.novellistAPI`.
    pub api_key: Text,
    /// `db.ainconfig`, read with JavaScript property semantics.
    pub ainconfig: Value,
    /// `getAdditionalParameters(aiModel)`.
    pub additional_params: Vec<(Text, Text)>,
}

impl std::fmt::Debug for NovelListSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("NovelListSettings").field("api_key", &"<redacted>").field("additional_params", &self.additional_params.len()).finish()
    }
}

/// What `requestChatDataMain` takes from `arg` besides the [`ProviderCall`]. `call.bias` is `arg.biasString`.
pub struct NovelListInputs<'a> {
    /// `targ.aiModel`.
    pub ai_model: &'a Text,
    /// `arg.customURL`.
    pub custom_url: Option<&'a Text>,
    /// `arg.maxTokens ?? db.maxResponse`.
    pub max_tokens: f64,
    /// `arg.temperature ?? db.temperature / 100`.
    pub temperature: f64,
    /// `getCurrentCharacter()?.name ?? ''`.
    pub char_name: &'a Text,
    /// `getUserName()`.
    pub user_name: &'a Text,
    /// `arg.continue`.
    pub continued: bool,
    pub settings: &'a NovelListSettings,
}

fn is_line_terminator(unit: u16) -> bool { matches!(unit, 10 | 13 | 0x2028 | 0x2029) }

/// `formatToAIN(name, content)`: straight quotes become alternating corner brackets, and every `「..」` on one line
/// becomes its own `name 「..」` part.
fn format_to_ain(name: &[u16], content: &[u16]) -> Vec<Vec<u16>> {
    let mut quotes = 0;
    let content: Vec<u16> = content.iter().map(|&unit| if unit == 34 { quotes += 1; if quotes % 2 == 1 { OPEN } else { CLOSE } } else { unit }).collect();
    let mut parts = Vec::new();
    let (mut last_end, mut at) = (0, 0);
    // `/「(.*?)」/g`: a match needs its closing bracket before the next line terminator.
    while at < content.len() {
        if content[at] == OPEN {
            let close = content[at + 1..].iter().position(|&unit| unit == CLOSE || is_line_terminator(unit)).map(|offset| at + 1 + offset);
            if let Some(end) = close.filter(|&end| content[end] == CLOSE) {
                if at != last_end { parts.push(content[last_end..at].to_vec()); }
                let mut inside = name.to_vec();
                inside.extend(js::utf16(" \u{300c}"));
                inside.extend_from_slice(&content[at + 1..end]);
                inside.push(CLOSE);
                parts.push(inside);
                last_end = end + 1;
                at = end + 1;
                continue;
            }
        }
        at += 1;
    }
    if last_end < content.len() { parts.push(content[last_end..].to_vec()); }
    parts
}

/// `stringlizeAINChat(formated, char, continued)`. Without `continued`, TS appends `+ "\n\n<char> 「"`, a unary plus
/// that is always `NaN`.
pub fn stringlize_ain_chat(formated: &[PromptMessage], char_name: &Text, user_name: &Text, continued: bool, budget: &Budget) -> Result<Text, Error> {
    let mut lines: Vec<Vec<u16>> = Vec::new();
    for form in formated {
        budget.work(form.content.len_units().saturating_add(1))?;
        let new_chat = form.memo.as_ref().is_some_and(|memo| memo.len_units() != 0 && js::starts_with(memo, "newChat"));
        if new_chat || form.content.equals_ascii(b"[Start a new chat]") {
            lines.push(js::utf16("[\u{65b0}\u{3057}\u{3044}\u{30c1}\u{30e3}\u{30c3}\u{30c8}\u{306e}\u{59cb}\u{307e}\u{308a}]"));
            continue;
        }
        let content = js::units(&form.content);
        match form.role {
            Role::System => lines.push(content),
            Role::User => lines.extend(format_to_ain(&js::units(user_name), &content)),
            Role::Assistant => lines.extend(format_to_ain(&js::units(form.name.as_ref().unwrap_or(char_name)), &content)),
            Role::Function => match form.name.as_ref().filter(|name| name.len_units() != 0) {
                Some(name) => lines.extend(format_to_ain(&js::units(name), &content)),
                None => lines.push(content),
            },
        }
    }
    let mut output: Vec<u16> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if index != 0 { output.extend(js::utf16("\n\n")); }
        output.extend_from_slice(line);
    }
    output.extend(js::utf16(if continued { " \u{300c}" } else { "NaN" }));
    js::from_units(&output, budget)
}

/// Builds the request `requestNovelList` sends. The request is always buffered.
pub fn build_request(call: &ProviderCall, inputs: &NovelListInputs<'_>, budget: &Budget) -> Result<BuiltRequest, BuildError> {
    if call.model.format != LlmFormat::NovelList { return Err(BuildError::Unsupported("format")); }
    let config = &inputs.settings.ainconfig;
    let mut body = js::new_object();
    let text = stringlize_ain_chat(&call.messages, inputs.char_name, inputs.user_name, inputs.continued, budget)?;
    js::set_str(&mut body, "text", J::Str(text), budget)?;
    js::set_str(&mut body, "length", J::Num(inputs.max_tokens), budget)?;
    js::set_str(&mut body, "temperature", J::Num(inputs.temperature), budget)?;
    for name in ["top_p", "top_k", "rep_pen", "top_a", "rep_pen_slope", "rep_pen_range", "typical_p", "badwords"] {
        js::set_str(&mut body, name, field(config, name, budget)?, budget)?;
    }
    let model = if inputs.ai_model.equals_ascii(b"novellist_damsel") { "damsel" } else { "supertrin" };
    js::set_str(&mut body, "model", js::str_value(model, budget)?, budget)?;
    let stop = to_text(&field(config, "stoptokens", budget)?, budget).map_err(BuildError::from)?;
    js::set_str(&mut body, "stoptokens", J::Str(js::concat(&js::text("\u{300c}", budget)?, &stop, budget)?), budget)?;
    let (mut texts, mut values): (Vec<&Text>, Vec<Text>) = (Vec::new(), Vec::new());
    for (text, bias) in &call.bias {
        texts.push(text);
        values.push(Value::Number(*bias).to_text(budget)?);
    }
    let joined = |parts: &[&Text], separator: &str| -> Result<J, Error> {
        Ok(if parts.is_empty() { J::Undefined } else { J::Str(js::join(parts, separator, budget)?) })
    };
    js::set_str(&mut body, "logit_bias", joined(&texts, "<<|>>")?, budget)?;
    js::set_str(&mut body, "logit_bias_values", joined(&values.iter().collect::<Vec<_>>(), "|")?, budget)?;

    let mut headers = js::new_object();
    js::set_str(&mut headers, "Authorization", J::Str(js::concat(&js::text("Bearer ", budget)?, &inputs.settings.api_key, budget)?), budget)?;
    js::set_str(&mut headers, "Content-Type", js::str_value("application/json", budget)?, budget)?;
    params::apply_additional_parameters(&mut body, &mut headers, &inputs.settings.additional_params, budget)?;
    let url = match inputs.custom_url { Some(url) => crate::common::built::utf8(url)?, None => DEFAULT_URL.to_owned() };
    let mut built = finish(url, body, &headers, false, budget)?;
    built.raw_url = DEFAULT_URL.to_owned();
    Ok(built)
}

/// The speaker of one `multiline` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker { Char, User }

#[derive(Debug)]
pub enum NovelListOutcome {
    /// `{ type: 'multiline', result: [role, text][] }`.
    Multiline(Vec<(Speaker, Text)>),
    /// `{ type: 'fail', result }`. A reply that is not `ok` fails with `res.data` as it is, which need not be a string.
    Fail(Value),
}

#[derive(Debug)]
pub struct NovelListBuffered {
    pub result: NovelListOutcome,
    /// Set when the outcome is a failure.
    pub failure: Option<ProviderFailure>,
}

/// `extractAINOutputStrings`: `(speaker chunk, content)` pairs, `None` for narration.
fn extract_ain_output(input: &[u16], characters: &[Vec<u16>], budget: &Budget) -> Result<Vec<(Option<usize>, Vec<u16>)>, Error> {
    let trim = |units: &[u16]| -> Result<Vec<u16>, Error> { Ok(js::units(&js::from_units(units, budget)?.trim(budget)?)) };
    let mut results = Vec::new();
    let mut remaining = input;
    while !remaining.is_empty() {
        budget.work(remaining.len().saturating_mul(characters.len().max(1)))?;
        let mut found: Option<(usize, usize)> = None;
        for (which, name) in characters.iter().enumerate() {
            let mut needle = name.clone();
            needle.push(OPEN);
            if let Some(index) = js::find_units(remaining, &needle, 0) {
                if found.is_none_or(|(_, best)| index < best) { found = Some((which, index)); }
            }
        }
        let Some((which, index)) = found else {
            results.push((None, trim(remaining)?));
            break;
        };
        if index > 0 { results.push((None, trim(&remaining[..index])?)); }
        let start = index + characters[which].len() + 1;
        match remaining[index + characters[which].len()..].iter().position(|&unit| unit == CLOSE).map(|offset| index + characters[which].len() + offset) {
            None => { results.push((Some(which), trim(&remaining[start.min(remaining.len())..])?)); break; }
            Some(end) => { results.push((Some(which), trim(&remaining[start..end])?)); remaining = &remaining[end + 1..]; }
        }
    }
    Ok(results)
}

/// `unstringlizeAIN(data, formated, char)`.
pub fn unstringlize_ain(data: &Text, formated: &[PromptMessage], char_name: &Text, user_name: &Text, budget: &Budget) -> Result<Vec<(Speaker, Text)>, Error> {
    let (chunks, ext) = unstringlizer_chunks(formated, char_name, user_name, ChunkMode::Ain, budget)?;
    let mut text = js::units(char_name);
    text.extend(js::utf16(" \u{300c}"));
    text.extend(js::units(data));
    for chunk in &ext {
        if text.ends_with(chunk) { text.truncate(text.len() - chunk.len()); }
    }
    let user = js::units(user_name);
    let mut result: Vec<(Speaker, Vec<u16>)> = Vec::new();
    for (speaker, content) in extract_ain_output(&text, &chunks, budget)? {
        match speaker {
            None => match result.last_mut() {
                None => result.push((Speaker::Char, content)),
                Some((_, last)) => { last.push(10); last.extend(content); }
            },
            Some(which) => {
                let name = js::units(&js::from_units(&chunks[which], budget)?.trim(budget)?);
                let mut quoted = vec![OPEN];
                quoted.extend(content);
                quoted.push(CLOSE);
                result.push((if name == user { Speaker::User } else { Speaker::Char }, quoted));
            }
        }
    }
    result.into_iter().map(|(speaker, units)| Ok((speaker, js::from_units(&units, budget)?))).collect()
}

/// What the reply reader needs besides the reply.
pub struct ReplyContext<'a> {
    pub formated: &'a [PromptMessage],
    pub char_name: &'a Text,
    pub user_name: &'a Text,
}

/// Reads one buffered reply the way `globalFetch` reports it and `requestNovelList` handles it. A truthy `error` that is
/// not a string throws at `.replace` (`error-not-text`); an unreadable `data` or `data.data` throws (`thrown`); a string
/// `data.data` is declined (`data-not-array`).
pub fn read_buffered(reply: &BufferedReply<'_>, context: &ReplyContext<'_>, budget: &Budget) -> Result<NovelListBuffered, BuildError> {
    let classified = classify(reply, budget)?;
    let data = js::from_value(classified.data(), budget)?;
    let failed = |result: Value, shown: &Text| -> Result<NovelListBuffered, BuildError> {
        let message = match (&classified.category, &classified.message) {
            (FailureCategory::Cancelled, _) => None,
            (_, Some(message)) => Some(bound_message(message, budget)?),
            (_, None) => Some(bound_message(shown, budget)?),
        };
        Ok(NovelListBuffered { result: NovelListOutcome::Fail(result), failure: Some(buffered_failure(classified.category.clone(), classified.status, message)) })
    };
    if !classified.ok {
        let shown = match &data { J::Str(text) => text.copy(budget)?, other => crate::common::jsprop::stringify(other, budget)? };
        return failed(wire::to_value(&data, budget)?, &shown);
    }
    let error = read(Some(&data), "error").map_err(Failed::from)?;
    if truthy(error) {
        let Some(J::Str(error)) = error else { return Err(BuildError::Unsupported("error-not-text")) };
        let replaced = js::replace_first(error, "token", &js::text("api key", budget)?, budget)?;
        return failed(Value::Text(replaced.copy(budget)?), &replaced);
    }
    let items = read(Some(&data), "data").map_err(Failed::from)?;
    // A string `data` would be indexed by character.
    if matches!(items, Some(J::Str(_))) { return Err(BuildError::Unsupported("data-not-array")); }
    let first = read(items, "0").map_err(Failed::from)?;
    let text = match first { None => js::text("undefined", budget)?, Some(value) => to_text(value, budget).map_err(BuildError::from)? };
    let lines = unstringlize_ain(&text, context.formated, context.char_name, context.user_name, budget)?;
    Ok(NovelListBuffered { result: NovelListOutcome::Multiline(lines), failure: None })
}
