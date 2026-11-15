//! The summary requests of SupaMemory (`summarize`, `src/ts/process/memory/supaMemory.ts:185-283`), HypaV2 (`summary`,
//! `hypav2.ts:38-143`) and HypaV3 (`summarize`, `hypav3.ts:1730-1790`), and the mapping of the host's replies to what
//! each function returns or throws. The `distilbart` summarizer and HypaV3's local models are declined when the
//! service is built, before any request.
//!
//! The OpenAI completions request is [`HostRequest::Http`] as `globalFetch` sends it through the Tauri HTTP plugin
//! (`src/ts/globalApi.svelte.ts:786-800`): an [`HostReply::Http`] reply is read as `response.json()` reads it, and
//! [`HostReply::Failed`] carries `${error}` of what the fetch threw. A key whose `Authorization` value the plugin
//! cannot send is declined when the service is built. The auxiliary model is [`HostRequest::Llm`]; its
//! [`HostReply::Failed`] carries the message of the error `requestChatData` threw. Only HypaV3 passes an abort signal,
//! so only its summary is ever answered [`HostReply::Cancelled`] for a user abort.
use crate::hypa_v2::{SummaryOutput, SummaryService};
use crate::hypa_v3::sub_model_summary;
use risunest_ir::chat::{MessageRole, PromptMessage, Role};
use risunest_ir::chat_ml::parse_chat_ml;
use risunest_ir::provider::{HttpMethod, HttpRequestSpec, OutputShape, ProviderOutput, ResponseKind};
use risunest_ir::stage::{HostReply, HostRequest, StageError, Step, SubRequest, TextContext, TextProcessor};
use risunest_ir::{json, Budget, Error, OwnedVec, Text, Utf8Decoder, Value};
use std::borrow::Cow;

const COMPLETIONS_URL: &str = "https://api.openai.com/v1/completions";
const SUPA_PROMPT: &str = "[Summarize the ongoing role story, It must also remove redundancy and unnecessary text and content from the output to reduce tokens for gpt3 and other sublanguage models]\n";
const HYPA_V2_PROMPT: &str = "[Summarize the ongoing role story, It must also remove redundancy and unnecessary text and content from the output.]\n";
const HYPA_V3_PROMPT: &str = "[Summarize the ongoing role story, It must also remove redundancy and unnecessary text and content from the output.]";
const HTTP_FAILURE: &str = "SupaMemory: HTTP: ";
/// The `SyntaxError` text for a malformed body whose V8 message depends on more than its first unexpected character.
const UNMODELLED_JSON_ERROR: &str = "SyntaxError: the response is not valid JSON";

fn text(value: &str, budget: &Budget) -> Result<Text, StageError> { Ok(Text::from_str(value, budget)?) }
fn units(value: &str) -> Vec<u16> { value.encode_utf16().collect() }
fn invalid(name: &'static str) -> StageError { StageError::Invalid(Cow::Borrowed(name)) }
fn decline(name: &'static str) -> StageError { StageError::Unsupported(Cow::Borrowed(name)) }
fn concat(parts: &[&Text], budget: &Budget) -> Result<Text, StageError> {
    let mut result = text("", budget)?;
    for part in parts { result.append(part)?; }
    Ok(result)
}
fn prefixed(prefix: &str, value: &Text, budget: &Budget) -> Result<Text, StageError> { concat(&[&text(prefix, budget)?, value], budget) }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode { Supa, HypaV2, HypaV3 }

enum Route {
    /// `requestChatData(..., 'memory')`.
    SubModel,
    /// `POST https://api.openai.com/v1/completions` with the `Authorization` value the plugin sends.
    Completions { model: &'static str, authorization: String },
}

#[derive(Clone, Copy)]
enum Pending { Llm, Http }

/// One mode's summary function over the settings captured when the policy starts. `P` is the `risuChatParser` pass
/// `parseChatML` runs over each part of a ChatML prompt (`src/ts/parser/chatML.ts:55`), with no options.
pub struct Summary<P> {
    mode: Mode,
    route: Route,
    /// The prompt after each mode's default rule.
    prompt: Text,
    processor: P,
    pending: Option<Pending>,
}

/// `db.supaModelType` as both summary functions branch on it: `distilbart` runs the local summarizer, `subModel` the
/// auxiliary model, and any other value an OpenAI completions model, whose key must make a sendable header.
fn route(model_type: &Text, key: Option<&Text>, budget: &Budget) -> Result<Route, StageError> {
    if model_type.equals_ascii(b"distilbart") { return Err(decline("supa-model-distilbart")); }
    if model_type.equals_ascii(b"subModel") { return Ok(Route::SubModel); }
    let model = if model_type.equals_ascii(b"curie") { "text-curie-001" }
        else if model_type.equals_ascii(b"instruct35") { "gpt-3.5-turbo-instruct" }
        else { "text-davinci-003" };
    Ok(Route::Completions { model, authorization: authorization(key, budget)? })
}

/// `'Bearer ' + db.supaMemoryKey` as the Tauri HTTP plugin sends it: `new Headers` converts the value to a ByteString
/// (throwing above 0xFF, lone surrogates included), strips leading and trailing HTTP whitespace and throws for NUL,
/// LF or CR, and the plugin's `HeaderValue::from_str` (`crates/tauri-plugin-http/src/commands.rs`) then rejects the
/// other controls except tab. A value TS cannot send is declined, so TS keeps the request with its own error text.
fn authorization(key: Option<&Text>, budget: &Budget) -> Result<String, StageError> {
    let mut value = units("Bearer ");
    match key { Some(key) => value.extend(key.units()), None => value.extend(units("undefined")) }
    budget.work(value.len())?;
    let space = |unit: &u16| matches!(*unit, 0x09 | 0x0a | 0x0d | 0x20);
    let start = value.iter().position(|unit| !space(unit)).unwrap_or(value.len());
    let end = value.iter().rposition(|unit| !space(unit)).map_or(start, |last| last + 1);
    let sent = &value[start..end];
    if value.iter().any(|&unit| unit > 0xff) || sent.iter().any(|&unit| (unit < 0x20 && unit != 0x09) || unit == 0x7f) {
        return Err(decline("supa-key-header"));
    }
    Ok(sent.iter().map(|&unit| char::from(unit as u8)).collect())
}

impl<P: TextProcessor> Summary<P> {
    /// SupaMemory's summary: `model_type` is `db.supaModelType`, `prompt` is `db.supaMemoryPrompt` and `key` is
    /// `db.supaMemoryKey`. An empty prompt selects the default.
    pub fn supa(model_type: &Text, prompt: &Text, key: Option<&Text>, processor: P, budget: &Budget) -> Result<Self, StageError> {
        let prompt = if prompt.len_units() == 0 { text(SUPA_PROMPT, budget)? } else { prompt.copy(budget)? };
        Ok(Self { mode: Mode::Supa, route: route(model_type, key, budget)?, prompt, processor, pending: None })
    }

    /// HypaV2's summary over the same settings as [`Summary::supa`], with its own default prompt and the `supaPrompt`
    /// memo on every auxiliary-model message.
    pub fn hypa_v2(model_type: &Text, prompt: &Text, key: Option<&Text>, processor: P, budget: &Budget) -> Result<Self, StageError> {
        let prompt = if prompt.len_units() == 0 { text(HYPA_V2_PROMPT, budget)? } else { prompt.copy(budget)? };
        Ok(Self { mode: Mode::HypaV2, route: route(model_type, key, budget)?, prompt, processor, pending: None })
    }

    /// HypaV3's `summarize(messages, false, signal)` with the preset's `summarizationModel` and `summarizationPrompt`;
    /// a prompt that trims to nothing selects the default. Local models are declined.
    pub fn hypa_v3(model: &Text, prompt: &Text, processor: P, budget: &Budget) -> Result<Self, StageError> {
        if !model.equals_ascii(b"subModel") { return Err(decline("hypa-v3-summarization-model")); }
        let prompt = if prompt.trim(budget)?.len_units() == 0 { text(HYPA_V3_PROMPT, budget)? } else { prompt.copy(budget)? };
        Ok(Self { mode: Mode::HypaV3, route: Route::SubModel, prompt, processor, pending: None })
    }

    /// `supaPrompt.replaceAll('{{slot}}', input)` parsed as ChatML, else the batch as a user message and the prompt as a
    /// system message.
    fn messages(&mut self, input: &Text, budget: &Budget) -> Result<Vec<PromptMessage>, StageError> {
        let replaced = replace_slot(&self.prompt, input, budget)?;
        let mut messages = match parse_chat_ml(&replaced, budget)? {
            Some(parts) => {
                let mut messages = Vec::new();
                for part in parts {
                    budget.work(1)?;
                    let output = self.processor.process(&Text::from_units(&part.content, budget)?, &TextContext::default(), budget)?;
                    // `parseChatML` reads CBS without `runVar`, so no variable can change.
                    if !output.effects.is_empty() { return Err(invalid("summary-chat-ml-effects")); }
                    let mut message = PromptMessage::new(part.role, output.text);
                    let mut thoughts = Vec::new();
                    for thought in &part.thoughts { thoughts.push(Text::from_units(thought, budget)?); }
                    message.thoughts = Some(thoughts);
                    messages.push(message);
                }
                messages
            }
            None => vec![PromptMessage::new(Role::User, input.copy(budget)?), PromptMessage::new(Role::System, self.prompt.copy(budget)?)],
        };
        if self.mode == Mode::HypaV2 {
            for message in &mut messages { message.memo = Some(text("supaPrompt", budget)?); }
        }
        Ok(messages)
    }

    fn completions(&self, model: &str, authorization: &str, input: &Text, budget: &Budget) -> Result<HttpRequestSpec, StageError> {
        let prompt = concat(&[input, &text("\n\n", budget)?, &self.prompt, &text("\n\nOutput:", budget)?], budget)?;
        let mut body = Value::Object(OwnedVec::new(budget)?);
        body.define(text("model", budget)?, Value::Text(text(model, budget)?), budget)?;
        body.define(text("prompt", budget)?, Value::Text(prompt), budget)?;
        body.define(text("max_tokens", budget)?, Value::Number(600.0), budget)?;
        body.define(text("temperature", budget)?, Value::Number(0.0), budget)?;
        let serialized = json::stringify(&body, budget)?.ok_or_else(|| invalid("summary-body"))?;
        // `JSON.stringify` escapes isolated surrogates, so the body is always well-formed.
        let body = serialized.as_unicode().ok_or_else(|| invalid("summary-body-unicode"))?.as_bytes().to_vec();
        let headers = vec![("Content-Type".to_owned(), "application/json".to_owned()), ("Authorization".to_owned(), authorization.to_owned())];
        Ok(HttpRequestSpec { method: HttpMethod::Post, url: COMPLETIONS_URL.to_owned(), headers, body, response: ResponseKind::Buffered })
    }

    /// What each mode makes of the `requestChatData` result.
    fn chat_reply(&self, reply: HostReply, budget: &Budget) -> Result<SummaryOutput, StageError> {
        let result = match reply {
            HostReply::Llm(Ok(output)) => ChatResult::from_output(output)?,
            // The owner sets `sdk_message` to the `result` TS shows for the failure.
            HostReply::Llm(Err(failure)) => ChatResult::Fail(failure.sdk_message.ok_or_else(|| invalid("summary-failure-text"))?),
            reply => return thrown(reply),
        };
        match self.mode {
            Mode::Supa | Mode::HypaV2 => match result {
                ChatResult::Success(result) => Ok(SummaryOutput::Success(result)),
                other => Ok(SummaryOutput::Failed(prefixed(HTTP_FAILURE, &other.into_text(budget)?, budget)?)),
            },
            Mode::HypaV3 => match result {
                ChatResult::Success(result) => sub_model_summary(&result, budget),
                ChatResult::Fail(result) => Ok(SummaryOutput::Failed(prefixed("Error: ", &result, budget)?)),
                ChatResult::Streamed | ChatResult::Multiline(_) => Ok(SummaryOutput::Failed(text("Error: Unexpected response type", budget)?)),
            },
        }
    }

    /// The completions branch after `globalFetch` resolved.
    fn http_reply(&self, reply: HostReply, budget: &Budget) -> Result<SummaryOutput, StageError> {
        let fetched = match reply {
            HostReply::Http { status, body } => {
                let body = Utf8Decoder::new().decode(&body, true, budget)?;
                match json::parse_json_text(&body, budget) {
                    Ok(data) => Fetched { ok: (200..300).contains(&status), data, status },
                    // `response.json()` threw inside `fetchWithTauri`, which answers status 400 whatever the response was.
                    Err(Error::Syntax { .. }) => Fetched { ok: false, data: Value::Text(json_parse_error(&body, budget)?), status: 400 },
                    Err(error) => return Err(error.into()),
                }
            }
            HostReply::Failed { message } => Fetched { ok: false, data: Value::Text(message), status: 400 },
            reply => return thrown(reply),
        };
        if fetched.ok {
            match completion_text(&fetched.data, budget)? {
                Completion::Text(result) => return Ok(SummaryOutput::Success(result)),
                Completion::Thrown(error) => return Ok(SummaryOutput::Failed(prefixed(HTTP_FAILURE, &text(error, budget)?, budget)?)),
                Completion::Falsy => {}
            }
        }
        // SupaMemory reports `JSON.stringify(da.data)`, HypaV2 the whole `da`.
        let reported = match self.mode {
            Mode::HypaV2 => {
                let Fetched { ok, data, status } = fetched;
                let mut da = Value::Object(OwnedVec::new(budget)?);
                da.define(text("ok", budget)?, Value::Bool(ok), budget)?;
                da.define(text("data", budget)?, data, budget)?;
                // The response headers are not part of the reply; HypaV2 only logs this text.
                da.define(text("headers", budget)?, Value::Object(OwnedVec::new(budget)?), budget)?;
                da.define(text("status", budget)?, Value::Number(f64::from(status)), budget)?;
                da
            }
            _ => fetched.data,
        };
        let serialized = json::stringify(&reported, budget)?.ok_or_else(|| invalid("summary-error-data"))?;
        Ok(SummaryOutput::Failed(prefixed(HTTP_FAILURE, &serialized, budget)?))
    }
}

impl<P: TextProcessor + Send> SummaryService for Summary<P> {
    fn summarize(&mut self, input: Text, budget: &Budget) -> Result<Step<SummaryOutput>, StageError> {
        if self.pending.is_some() { return Err(invalid("summary-pending")); }
        let (request, pending) = match &self.route {
            Route::SubModel => {
                let messages = self.messages(&input, budget)?;
                let mut options = Value::Object(OwnedVec::new(budget)?);
                options.define(text("bias", budget)?, Value::Object(OwnedVec::new(budget)?), budget)?;
                options.define(text("useStreaming", budget)?, Value::Bool(false), budget)?;
                options.define(text("noMultiGen", budget)?, Value::Bool(true), budget)?;
                (HostRequest::Llm(SubRequest { mode: text("memory", budget)?, messages, options }), Pending::Llm)
            }
            Route::Completions { model, authorization } => (HostRequest::Http(self.completions(model, authorization, &input, budget)?), Pending::Http),
        };
        budget.check()?;
        self.pending = Some(pending);
        Ok(Step::NeedsHost(request))
    }

    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<SummaryOutput>, StageError> {
        let pending = self.pending.take().ok_or_else(|| invalid("summary-not-pending"))?;
        let output = match pending {
            Pending::Llm => self.chat_reply(reply, budget)?,
            Pending::Http => self.http_reply(reply, budget)?,
        };
        Ok(Step::Done(output))
    }
}

/// A request that threw, was cancelled or that the host does not serve.
fn thrown<T>(reply: HostReply) -> Result<T, StageError> {
    match reply {
        HostReply::Failed { message } => Err(StageError::Host(message.units().collect())),
        HostReply::Cancelled => Err(StageError::Cancelled),
        HostReply::Unsupported(name) => Err(StageError::Unsupported(name)),
        _ => Err(invalid("summary-reply")),
    }
}

/// `requestChatData`'s `{ type, result }`.
enum ChatResult { Success(Text), Fail(Text), Streamed, Multiline(Vec<(MessageRole, Text)>) }
impl ChatResult {
    fn from_output(output: ProviderOutput) -> Result<Self, StageError> {
        let ProviderOutput { mut texts, shape, .. } = output;
        Ok(match shape {
            OutputShape::Text if texts.len() == 1 => Self::Success(texts.pop().expect("one text")),
            OutputShape::Streamed if texts.len() == 1 => Self::Streamed,
            OutputShape::Multiline(roles) if roles.len() == texts.len() => Self::Multiline(roles.into_iter().zip(texts).collect()),
            _ => return Err(invalid("summary-reply-shape")),
        })
    }
    /// `'' + result`: the stream as `String` names it, the pairs as `Array.prototype.toString` joins them.
    fn into_text(self, budget: &Budget) -> Result<Text, StageError> {
        Ok(match self {
            Self::Success(result) | Self::Fail(result) => result,
            Self::Streamed => text("[object ReadableStream]", budget)?,
            Self::Multiline(pairs) => {
                let mut joined = text("", budget)?;
                let comma = text(",", budget)?;
                for (index, (role, value)) in pairs.iter().enumerate() {
                    budget.work(1)?;
                    if index != 0 { joined.append(&comma)?; }
                    joined.append(&text(if *role == MessageRole::User { "user" } else { "char" }, budget)?)?;
                    joined.append(&comma)?;
                    joined.append(value)?;
                }
                joined
            }
        })
    }
}

/// `globalFetch`'s `{ ok, data, status }`.
struct Fetched { ok: bool, data: Value, status: u16 }

enum Completion { Text(Text), Falsy, Thrown(&'static str) }

/// `(await da.data)?.choices[0]?.text?.trim()`: the trimmed text, a falsy result, or the `TypeError` it throws.
fn completion_text(data: &Value, budget: &Budget) -> Result<Completion, StageError> {
    // A primitive or an array has no own `choices`, `0` or `text` that JSON could give it, so each reads `undefined`.
    let choices = match data {
        Value::Undefined | Value::Null => return Ok(Completion::Falsy),
        data => data.get(b"choices", budget)?,
    };
    let first = match choices {
        None | Some(Value::Undefined) => return Ok(Completion::Thrown("TypeError: Cannot read properties of undefined (reading '0')")),
        Some(Value::Null) => return Ok(Completion::Thrown("TypeError: Cannot read properties of null (reading '0')")),
        Some(Value::Array(items)) => items.as_slice().first(),
        Some(object @ Value::Object(_)) => object.get(b"0", budget)?,
        // A string's character has no `text`.
        Some(_) => None,
    };
    let result = match first {
        Some(object @ Value::Object(_)) => object.get(b"text", budget)?,
        _ => None,
    };
    match result {
        None | Some(Value::Undefined | Value::Null) => Ok(Completion::Falsy),
        Some(Value::Text(result)) => {
            let trimmed = result.trim(budget)?;
            Ok(if trimmed.len_units() == 0 { Completion::Falsy } else { Completion::Text(trimmed) })
        }
        Some(_) => Ok(Completion::Thrown("TypeError: (intermediate value)?.choices[0]?.text?.trim is not a function")),
    }
}

/// `prompt.replaceAll('{{slot}}', input)`, whose string replacement expands `$$`, `$&`, `` $` `` and `$'`
/// (GetSubstitution with no captures; `$1` and `$<` stay as written).
fn replace_slot(prompt: &Text, input: &Text, budget: &Budget) -> Result<Vec<u16>, StageError> {
    let source: Vec<u16> = prompt.units().collect();
    let replacement: Vec<u16> = input.units().collect();
    let slot = units("{{slot}}");
    budget.work(source.len().saturating_add(replacement.len()))?;
    let mut positions = Vec::new();
    let mut from = 0;
    while from + slot.len() <= source.len() {
        budget.work(1)?;
        if source[from..].starts_with(&slot) { positions.push(from); from += slot.len(); } else { from += 1; }
    }
    let mut output = Vec::new();
    let mut last = 0;
    for &at in &positions {
        output.extend_from_slice(&source[last..at]);
        let mut index = 0;
        while index < replacement.len() {
            budget.work(1)?;
            let unit = replacement[index];
            let next = replacement.get(index + 1).copied();
            match (unit, next) {
                (0x24, Some(0x24)) => { output.push(0x24); index += 2; }
                (0x24, Some(0x26)) => { output.extend_from_slice(&slot); index += 2; }
                (0x24, Some(0x60)) => { output.extend_from_slice(&source[..at]); index += 2; }
                (0x24, Some(0x27)) => { output.extend_from_slice(&source[at + slot.len()..]); index += 2; }
                _ => { output.push(unit); index += 1; }
            }
        }
        last = at + slot.len();
    }
    output.extend_from_slice(&source[last..]);
    budget.work(output.len())?;
    Ok(output)
}

/// `String(error)` of the `SyntaxError` V8 throws for `JSON.parse(source)`, for the messages that depend on nothing
/// but the first character it cannot continue from; any other malformed body reads [`UNMODELLED_JSON_ERROR`].
fn json_parse_error(source: &Text, budget: &Budget) -> Result<Text, StageError> {
    let source: Vec<u16> = source.units().collect();
    budget.work(source.len())?;
    let length = source.len();
    let unmodelled = || text(UNMODELLED_JSON_ERROR, budget);
    let is_space = |unit: u16| matches!(unit, 9 | 10 | 13 | 32);
    // The index of the first unit V8 cannot continue from, or `None` when the input ends first.
    let culprit = match source.iter().position(|&unit| !is_space(unit)) {
        None => None,
        Some(at) => {
            let unit = source[at];
            let literal = match unit { 0x74 => Some("true"), 0x66 => Some("false"), 0x6e => Some("null"), _ => None };
            let value_start = matches!(unit, 0x7b | 0x5b | 0x2d | 0x22) || (0x30..=0x39).contains(&unit);
            if let Some(word) = literal {
                let word = units(word);
                match (0..word.len()).find(|&offset| source.get(at + offset) != Some(&word[offset])) {
                    Some(offset) if at + offset < length => Some(at + offset),
                    Some(_) => None,
                    None => return unmodelled(),
                }
            } else if value_start { return unmodelled(); } else { Some(at) }
        }
    };
    let mut message = units("SyntaxError: ");
    match culprit {
        None => message.extend(units("Unexpected end of JSON input")),
        Some(at) => {
            if (0xd800..=0xdfff).contains(&source[at]) { return unmodelled(); }
            message.extend(units("Unexpected token '"));
            message.push(source[at]);
            message.extend(units("', "));
            let (from, to, before, after) = if length < 21 { (0, length, false, false) }
                else if at < 10 { (0, at + 10, false, true) }
                else if at < length - 10 { (at - 10, at + 10, true, true) }
                else { (at - 10, length, true, false) };
            if before { message.extend(units("...")); }
            message.push(0x22);
            message.extend_from_slice(&source[from..to]);
            message.push(0x22);
            if after { message.extend(units("...")); }
            message.extend(units(" is not valid JSON"));
        }
    }
    Ok(Text::from_units(&message, budget)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::provider::{FailureCategory, ProviderFailure};
    use risunest_ir::stage::{Effect, TextOutput};
    use risunest_ir::Limits;
    use std::sync::{atomic::AtomicBool, Arc};

    fn budget() -> Budget {
        Budget::new(Limits { max_owned_bytes: 16 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
            max_values: 100_000, max_depth: 64, max_work: 10_000_000 }, Arc::new(AtomicBool::new(false))).unwrap()
    }
    fn t(value: &str, b: &Budget) -> Text { Text::from_str(value, b).unwrap() }

    /// Returns each text unchanged and records what it was given.
    #[derive(Default)]
    struct Echo { seen: Vec<(String, i64, bool)>, effect: bool }
    impl TextProcessor for Echo {
        fn process(&mut self, input: &Text, context: &TextContext, budget: &Budget) -> Result<TextOutput, StageError> {
            self.seen.push((input.as_unicode().unwrap().to_owned(), context.chat_index, context.run_var));
            let mut effects = Vec::new();
            if self.effect { effects.push(Effect::ChatField { field: t("x", budget), value: Value::Undefined }); }
            Ok(TextOutput { text: prefixed("cbs:", input, budget)?, effects })
        }
        fn apply(&mut self, _: &Effect, _: &Budget) -> Result<(), StageError> { Ok(()) }
    }

    fn supa(model: &str, prompt: &str, key: Option<&str>, b: &Budget) -> Summary<Echo> { supa_text(model, &t(prompt, b), key, b) }
    fn supa_text(model: &str, prompt: &Text, key: Option<&str>, b: &Budget) -> Summary<Echo> {
        Summary::supa(&t(model, b), prompt, key.map(|key| t(key, b)).as_ref(), Echo::default(), b).unwrap()
    }
    fn hypa_v2(model: &str, prompt: &str, b: &Budget) -> Summary<Echo> {
        Summary::hypa_v2(&t(model, b), &t(prompt, b), Some(&t("k", b)), Echo::default(), b).unwrap()
    }
    fn hypa_v3(prompt: &str, b: &Budget) -> Summary<Echo> { Summary::hypa_v3(&t("subModel", b), &t(prompt, b), Echo::default(), b).unwrap() }
    fn request(service: &mut Summary<Echo>, input: &str, b: &Budget) -> HostRequest {
        match service.summarize(t(input, b), b).unwrap() { Step::NeedsHost(request) => request, Step::Done(_) => panic!("no request") }
    }
    fn messages(request: &HostRequest, b: &Budget) -> String {
        let HostRequest::Llm(sub) = request else { panic!("auxiliary model request") };
        let values: Vec<String> = sub.messages.iter().map(|message| json::stringify(&message.to_value(b).unwrap(), b).unwrap().unwrap().as_unicode().unwrap().to_owned()).collect();
        values.join("|")
    }
    fn output(service: &mut Summary<Echo>, reply: HostReply, b: &Budget) -> Result<(bool, String), StageError> {
        match service.resume(reply, b)? {
            Step::Done(SummaryOutput::Success(value)) => Ok((true, String::from_utf16(&value.units().collect::<Vec<_>>()).unwrap())),
            Step::Done(SummaryOutput::Failed(value)) => Ok((false, String::from_utf16(&value.units().collect::<Vec<_>>()).unwrap())),
            Step::NeedsHost(_) => panic!("one round trip"),
        }
    }
    fn llm(texts: &[&str], shape: OutputShape, b: &Budget) -> HostReply {
        HostReply::Llm(Ok(ProviderOutput { texts: texts.iter().map(|value| t(value, b)).collect(), shape, thoughts: Vec::new(),
            tool_calls: Vec::new(), usage: None, finish: None }))
    }
    fn fail(message: Option<&str>, b: &Budget) -> HostReply {
        HostReply::Llm(Err(ProviderFailure { category: FailureCategory::Http, stage: None, http_status: Some(500),
            sdk_message: message.map(|message| t(message, b)), body_failure: None }))
    }
    fn http(status: u16, body: &str) -> HostReply { HostReply::Http { status, body: body.as_bytes().to_vec() } }
    /// The reply mapped by a fresh completions request.
    fn completed(mode: &str, reply: HostReply, b: &Budget) -> (bool, String) {
        let mut service = if mode == "supa" { supa("instruct35", "", Some("k"), b) } else { hypa_v2("instruct35", "", b) };
        request(&mut service, "user: a", b);
        output(&mut service, reply, b).unwrap()
    }
    fn chatted(service: &mut Summary<Echo>, reply: HostReply, b: &Budget) -> Result<(bool, String), StageError> {
        request(service, "user: a", b);
        output(service, reply, b)
    }

    #[test]
    fn completions_request_is_the_body_and_headers_global_fetch_sends() {
        let b = budget();
        let prompt = Text::from_units(&[0x53, 0x75, 0x6d, 0x20, 0xd800, 0x2e], &b).unwrap();
        for (model, sent) in [("instruct35", "gpt-3.5-turbo-instruct"), ("curie", "text-curie-001"), ("davinci", "text-davinci-003"), ("none", "text-davinci-003")] {
            let HostRequest::Http(spec) = request(&mut supa_text(model, &prompt, Some("sk-1"), &b), "user: \"a\"\n\u{1f600}", &b) else { panic!("HTTP request") };
            assert_eq!(spec.url, "https://api.openai.com/v1/completions");
            assert_eq!(spec.method, HttpMethod::Post);
            assert_eq!(spec.response, ResponseKind::Buffered);
            assert_eq!(spec.headers, [("Content-Type".to_owned(), "application/json".to_owned()), ("Authorization".to_owned(), "Bearer sk-1".to_owned())]);
            assert_eq!(String::from_utf8(spec.body).unwrap(), format!(
                "{{\"model\":\"{sent}\",\"prompt\":\"user: \\\"a\\\"\\n\u{1f600}\\n\\nSum \\ud800.\\n\\nOutput:\",\"max_tokens\":600,\"temperature\":0}}"));
        }
        // An absent key is concatenated as `undefined`; an empty prompt is each mode's default.
        let HostRequest::Http(spec) = request(&mut supa("davinci", "", None, &b), "x", &b) else { panic!() };
        assert_eq!(spec.headers[1].1, "Bearer undefined");
        assert!(String::from_utf8(spec.body).unwrap().contains("x\\n\\n[Summarize the ongoing role story, It must also remove redundancy and unnecessary text and content from the output to reduce tokens for gpt3 and other sublanguage models]\\n\\n\\nOutput:"));
        let HostRequest::Http(spec) = request(&mut hypa_v2("curie", "", &b), "x", &b) else { panic!() };
        assert!(String::from_utf8(spec.body).unwrap().contains("x\\n\\n[Summarize the ongoing role story, It must also remove redundancy and unnecessary text and content from the output.]\\n\\n\\nOutput:"));
    }

    #[test]
    fn authorization_is_the_value_the_plugin_sends_and_an_unsendable_key_declines() {
        let b = budget();
        let sent = |key: &Text| match Summary::supa(&t("davinci", &b), &t("", &b), Some(key), Echo::default(), &b) {
            Ok(mut service) => { let HostRequest::Http(spec) = request(&mut service, "x", &b) else { panic!() }; Ok(spec.headers[1].1.clone()) }
            Err(error) => Err(error),
        };
        // `new Headers` strips HTTP whitespace from both ends; Latin-1 and an inner tab are sent as they are.
        for (key, value) in [("", "Bearer"), ("sk-1\n", "Bearer sk-1"), (" \t\r\n", "Bearer"), ("sk 1\t", "Bearer sk 1"), ("a\tb", "Bearer a\tb"), ("\u{e9}", "Bearer \u{e9}")] {
            assert_eq!(sent(&t(key, &b)).unwrap(), value, "{key:?}");
        }
        // Inner NUL, LF, CR or another control, a unit above 0xFF and a lone surrogate cannot be sent.
        let mut keys: Vec<Text> = ["sk\n1", "sk\r1", "sk\u{0}1", "sk\u{1}1", "sk\u{7f}1", "\u{d55c}", "\u{1f600}"].iter().map(|key| t(key, &b)).collect();
        keys.push(Text::from_units(&[0x73, 0xd800], &b).unwrap());
        for key in &keys {
            assert!(matches!(sent(key), Err(StageError::Unsupported(name)) if name == "supa-key-header"), "{:?}", key.units().collect::<Vec<_>>());
        }
        assert!(matches!(Summary::hypa_v2(&t("instruct35", &b), &t("", &b), Some(&t("\u{d55c}", &b)), Echo::default(), &b), Err(StageError::Unsupported(name)) if name == "supa-key-header"));
        // The auxiliary model does not send the key.
        assert!(Summary::supa(&t("subModel", &b), &t("", &b), Some(&t("sk\n1", &b)), Echo::default(), &b).is_ok());
    }

    #[test]
    fn auxiliary_model_request_carries_the_prompt_pair_or_the_parsed_chat_ml() {
        let b = budget();
        let HostRequest::Llm(sub) = request(&mut supa("subModel", "", None, &b), "user: a", &b) else { panic!() };
        assert_eq!(sub.mode.as_unicode(), Some("memory"));
        assert_eq!(json::stringify(&sub.options, &b).unwrap().unwrap().as_unicode(), Some("{\"bias\":{},\"useStreaming\":false,\"noMultiGen\":true}"));
        assert_eq!(messages(&request(&mut supa("subModel", "P", None, &b), "user: a", &b), &b),
            "{\"role\":\"user\",\"content\":\"user: a\"}|{\"role\":\"system\",\"content\":\"P\"}");
        assert_eq!(messages(&request(&mut hypa_v2("subModel", "", &b), "user: a", &b), &b), format!(
            "{{\"role\":\"user\",\"content\":\"user: a\",\"memo\":\"supaPrompt\"}}|{{\"role\":\"system\",\"content\":{},\"memo\":\"supaPrompt\"}}",
            json::stringify(&Value::Text(t(HYPA_V2_PROMPT, &b)), &b).unwrap().unwrap().as_unicode().unwrap()));
        // A prompt of whitespace is HypaV3's default; any other prompt is used as written.
        assert_eq!(messages(&request(&mut hypa_v3(" \n ", &b), "user: a", &b), &b), format!(
            "{{\"role\":\"user\",\"content\":\"user: a\"}}|{{\"role\":\"system\",\"content\":\"{HYPA_V3_PROMPT}\"}}"));
        assert_eq!(messages(&request(&mut hypa_v3(" P ", &b), "user: a", &b), &b), "{\"role\":\"user\",\"content\":\"user: a\"}|{\"role\":\"system\",\"content\":\" P \"}");
        // The slot is replaced before the ChatML split; each part goes through the CBS pass with no options and keeps its
        // thoughts.
        let prompt = "<|im_start|>system\nS <Thoughts>t</Thoughts><|im_end|><|im_start|>user\n[{{slot}}]<|im_end|>";
        let mut service = hypa_v2("subModel", prompt, &b);
        let sent = request(&mut service, "$$ a", &b);
        assert_eq!(service.processor.seen, [("S ".to_owned(), -1, false), ("[$ a]".to_owned(), -1, false)]);
        assert_eq!(messages(&sent, &b), "{\"role\":\"system\",\"content\":\"cbs:S \",\"memo\":\"supaPrompt\",\"thoughts\":[\"t\"]}|{\"role\":\"user\",\"content\":\"cbs:[$ a]\",\"memo\":\"supaPrompt\",\"thoughts\":[]}");
        // A string `replaceAll` expands `$$`, `$&`, `` $` `` and `$'` at each match, and keeps `$1`, `$<` and a lone `$`.
        let replaced = replace_slot(&t("A{{slot}}B{{slot}}C", &b), &t("$$|$&|$`|$'|$1|$<|$", &b), &b).unwrap();
        assert_eq!(String::from_utf16(&replaced).unwrap(), "A$|{{slot}}|A|B{{slot}}C|$1|$<|$B$|{{slot}}|A{{slot}}B|C|$1|$<|$C");
        assert_eq!(String::from_utf16(&replace_slot(&t("{{slot}", &b), &t("x", &b), &b).unwrap()).unwrap(), "{{slot}");
        let mut writes = supa("subModel", "<|im_start|>user\n{{slot}}", None, &b);
        writes.processor.effect = true;
        assert!(matches!(writes.summarize(t("a", &b), &b), Err(StageError::Invalid(name)) if name == "summary-chat-ml-effects"));
    }

    #[test]
    fn declines_come_at_construction_and_one_request_is_pending_at_a_time() {
        let b = budget();
        assert!(matches!(Summary::supa(&t("distilbart", &b), &t("", &b), None, Echo::default(), &b), Err(StageError::Unsupported(name)) if name == "supa-model-distilbart"));
        assert!(matches!(Summary::hypa_v2(&t("distilbart", &b), &t("", &b), None, Echo::default(), &b), Err(StageError::Unsupported(_))));
        assert!(matches!(Summary::hypa_v3(&t("Qwen3-1.7B", &b), &t("", &b), Echo::default(), &b), Err(StageError::Unsupported(name)) if name == "hypa-v3-summarization-model"));
        let mut service = supa("subModel", "", None, &b);
        assert!(matches!(service.resume(llm(&["a"], OutputShape::Text, &b), &b), Err(StageError::Invalid(name)) if name == "summary-not-pending"));
        request(&mut service, "a", &b);
        assert!(matches!(service.summarize(t("b", &b), &b), Err(StageError::Invalid(name)) if name == "summary-pending"));
        assert_eq!(output(&mut service, llm(&["  S  "], OutputShape::Text, &b), &b).unwrap(), (true, "  S  ".to_owned()));
        request(&mut service, "b", &b);
    }

    #[test]
    fn auxiliary_model_replies_map_to_each_mode_result() {
        let b = budget();
        let multiline = || llm(&["a", "b"], OutputShape::Multiline(vec![MessageRole::User, MessageRole::Char]), &b);
        for mode in ["supa", "hypa-v2"] {
            let service = || if mode == "supa" { supa("subModel", "", None, &b) } else { hypa_v2("subModel", "", &b) };
            assert_eq!(chatted(&mut service(), llm(&[""], OutputShape::Text, &b), &b).unwrap(), (true, String::new()));
            assert_eq!(chatted(&mut service(), fail(Some("quota"), &b), &b).unwrap(), (false, "SupaMemory: HTTP: quota".to_owned()));
            assert_eq!(chatted(&mut service(), llm(&["x"], OutputShape::Streamed, &b), &b).unwrap(), (false, "SupaMemory: HTTP: [object ReadableStream]".to_owned()));
            assert_eq!(chatted(&mut service(), multiline(), &b).unwrap(), (false, "SupaMemory: HTTP: user,a,char,b".to_owned()));
        }
        let v3 = || hypa_v3("", &b);
        assert_eq!(chatted(&mut v3(), llm(&[" <Thoughts>x</Thoughts> S "], OutputShape::Text, &b), &b).unwrap(), (true, "S".to_owned()));
        assert_eq!(chatted(&mut v3(), llm(&[" \n"], OutputShape::Text, &b), &b).unwrap(), (false, "Error: Empty summary returned".to_owned()));
        assert_eq!(chatted(&mut v3(), fail(Some("quota"), &b), &b).unwrap(), (false, "Error: quota".to_owned()));
        assert_eq!(chatted(&mut v3(), llm(&["x"], OutputShape::Streamed, &b), &b).unwrap(), (false, "Error: Unexpected response type".to_owned()));
        assert_eq!(chatted(&mut v3(), multiline(), &b).unwrap(), (false, "Error: Unexpected response type".to_owned()));
        // A thrown request, a cancel and an unserved request end the summary; a reply TS cannot produce is invalid.
        for mut service in [supa("subModel", "", None, &b), hypa_v3("", &b)] {
            assert!(matches!(chatted(&mut service, HostReply::Failed { message: t("down", &b) }, &b), Err(StageError::Host(message)) if message == units("down")));
            assert!(matches!(chatted(&mut service, HostReply::Cancelled, &b), Err(StageError::Cancelled)));
            assert!(matches!(chatted(&mut service, HostReply::Unsupported(Cow::Borrowed("m")), &b), Err(StageError::Unsupported(name)) if name == "m"));
            assert!(matches!(chatted(&mut service, fail(None, &b), &b), Err(StageError::Invalid(name)) if name == "summary-failure-text"));
            assert!(matches!(chatted(&mut service, llm(&["a", "b"], OutputShape::Text, &b), &b), Err(StageError::Invalid(name)) if name == "summary-reply-shape"));
            assert!(matches!(chatted(&mut service, http(200, "{}"), &b), Err(StageError::Invalid(name)) if name == "summary-reply"));
        }
    }

    #[test]
    fn completions_replies_read_as_global_fetch_and_the_optional_chain_read_them() {
        let b = budget();
        let supa_text = |reply| completed("supa", reply, &b);
        assert_eq!(supa_text(http(200, "\u{feff}{\"choices\":[{\"text\":\"  S \\n\"}]}")), (true, "S".to_owned()));
        assert_eq!(supa_text(http(429, "{\"error\":{\"message\":\"rate\"}}")), (false, "SupaMemory: HTTP: {\"error\":{\"message\":\"rate\"}}".to_owned()));
        assert_eq!(supa_text(http(200, "{\"choices\":[{\"text\":\" \"}]}")), (false, "SupaMemory: HTTP: {\"choices\":[{\"text\":\" \"}]}".to_owned()));
        assert_eq!(supa_text(http(200, "null")), (false, "SupaMemory: HTTP: null".to_owned()));
        for (body, error) in [("{}", "undefined"), ("[]", "undefined"), ("5", "undefined"), ("{\"choices\":null}", "null")] {
            assert_eq!(supa_text(http(200, body)), (false, format!("SupaMemory: HTTP: TypeError: Cannot read properties of {error} (reading '0')")));
        }
        for body in ["{\"choices\":\"abc\"}", "{\"choices\":[]}", "{\"choices\":[null]}", "{\"choices\":[\"t\"]}", "{\"choices\":{\"0\":{}}}"] {
            assert_eq!(supa_text(http(200, body)), (false, format!("SupaMemory: HTTP: {body}")));
        }
        assert_eq!(supa_text(http(200, "{\"choices\":{\"0\":{\"text\":\"O\"}}}")), (true, "O".to_owned()));
        for body in ["{\"choices\":[{\"text\":5}]}", "{\"choices\":[{\"text\":{}}]}"] {
            assert_eq!(supa_text(http(200, body)), (false, "SupaMemory: HTTP: TypeError: (intermediate value)?.choices[0]?.text?.trim is not a function".to_owned()));
        }
        // A body that is not JSON and a fetch that threw become `data` strings, which `JSON.stringify` quotes.
        assert_eq!(supa_text(http(200, "<!DOCTYPE html><p>")), (false, "SupaMemory: HTTP: \"SyntaxError: Unexpected token '<', \\\"<!DOCTYPE html><p>\\\" is not valid JSON\"".to_owned()));
        assert_eq!(supa_text(http(502, " ")), (false, "SupaMemory: HTTP: \"SyntaxError: Unexpected end of JSON input\"".to_owned()));
        assert_eq!(supa_text(http(200, "{bad")), (false, format!("SupaMemory: HTTP: \"{UNMODELLED_JSON_ERROR}\"")));
        assert_eq!(supa_text(HostReply::Failed { message: t("TypeError: fetch \"failed\"", &b) }), (false, "SupaMemory: HTTP: \"TypeError: fetch \\\"failed\\\"\"".to_owned()));
        // HypaV2 reports the whole `globalFetch` result, without the response headers.
        assert_eq!(completed("hypa-v2", http(500, "{\"e\":1}"), &b), (false, "SupaMemory: HTTP: {\"ok\":false,\"data\":{\"e\":1},\"headers\":{},\"status\":500}".to_owned()));
        assert_eq!(completed("hypa-v2", http(200, "{\"choices\":[]}"), &b), (false, "SupaMemory: HTTP: {\"ok\":true,\"data\":{\"choices\":[]},\"headers\":{},\"status\":200}".to_owned()));
        assert_eq!(completed("hypa-v2", http(200, "x"), &b), (false, "SupaMemory: HTTP: {\"ok\":false,\"data\":\"SyntaxError: Unexpected token 'x', \\\"x\\\" is not valid JSON\",\"headers\":{},\"status\":400}".to_owned()));
        assert_eq!(completed("hypa-v2", http(200, "{\"choices\":[{\"text\":\"S\"}]}"), &b), (true, "S".to_owned()));
        for mut service in [supa("davinci", "", None, &b), hypa_v2("davinci", "", &b)] {
            request(&mut service, "a", &b);
            assert!(matches!(service.resume(HostReply::Cancelled, &b), Err(StageError::Cancelled)));
            request(&mut service, "a", &b);
            assert!(matches!(service.resume(llm(&["a"], OutputShape::Text, &b), &b), Err(StageError::Invalid(name)) if name == "summary-reply"));
        }
    }

    #[test]
    fn json_parse_messages_follow_v8_for_the_first_unexpected_character() {
        let b = budget();
        let message = |source: &str| json_parse_error(&t(source, &b), &b).unwrap().as_unicode().unwrap().to_owned();
        assert_eq!(message(""), "SyntaxError: Unexpected end of JSON input");
        assert_eq!(message("tru"), "SyntaxError: Unexpected end of JSON input");
        assert_eq!(message("nul!"), "SyntaxError: Unexpected token '!', \"nul!\" is not valid JSON");
        assert_eq!(message("<html><body>Bad gateway</body></html>"), "SyntaxError: Unexpected token '<', \"<html><bod\"... is not valid JSON");
        assert_eq!(message("0123456789abcdefghijkl x"), UNMODELLED_JSON_ERROR);
        assert_eq!(message("          abcdefghijklmnopqrstuvwxyz"), "SyntaxError: Unexpected token 'a', ...\"          abcdefghij\"... is not valid JSON");
        assert_eq!(message("                    Z"), "SyntaxError: Unexpected token 'Z', ...\"          Z\" is not valid JSON");
        assert_eq!(message("null x"), UNMODELLED_JSON_ERROR);
    }
}
