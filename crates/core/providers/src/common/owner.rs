//! Owner glue every family shares: target resolution (`requestChatDataMain`), the `JSON.parse` messages, the bound on
//! `ProviderFailure::sdk_message`, stream-start failures, and the rule for what a declined input becomes once a
//! request is under way.

use super::js;
use super::BuildError;
use risunest_ir::provider::{FailureCategory, FailureStage, ModelDescriptor, ProviderFailure};
use risunest_ir::{Budget, Error, Text, Value};

/// The most UTF-16 units `ProviderFailure::sdk_message` keeps.
pub const SDK_MESSAGE_LIMIT_UNITS: usize = 2048;

// ---------------------------------------------------------------------------------------------------------------
// Target resolution (`requestChatDataMain`, `request.ts:479-492`).

/// A `db.customModels` entry, reduced to what target resolution reads.
pub struct CustomModelRef<'a> {
    pub id: &'a Text,
    pub url: Option<&'a Text>,
    pub key: Option<&'a Text>,
}

pub struct TargetSource<'a> {
    /// `targ.aiModel`.
    pub ai_model: &'a Text,
    pub proxy_key: &'a Text,
    pub force_replace_url: &'a Text,
    pub custom_proxy_request_model: &'a Text,
    pub custom_models: &'a [CustomModelRef<'a>],
}

/// `targ.key`, `targ.customURL` and the `modelInfo.internalID` override.
#[derive(Debug, Default)]
pub struct Target {
    pub key: Option<Text>,
    pub custom_url: Option<Text>,
    /// Set for `reverse_proxy`: `db.customProxyRequestModel`.
    pub internal_id: Option<Text>,
}

impl Target {
    /// Applies the `internalID` override to the descriptor the builder reads.
    pub fn apply(&self, model: &mut ModelDescriptor, budget: &Budget) -> Result<(), Error> {
        if let Some(internal) = &self.internal_id { model.internal_id = Some(internal.copy(budget)?); }
        Ok(())
    }
}

pub fn resolve_target(source: &TargetSource<'_>, budget: &Budget) -> Result<Target, Error> {
    let mut target = Target::default();
    if source.ai_model.equals_ascii(b"reverse_proxy") {
        target.internal_id = Some(source.custom_proxy_request_model.copy(budget)?);
        target.custom_url = Some(source.force_replace_url.copy(budget)?);
        target.key = Some(source.proxy_key.copy(budget)?);
    }
    if js::starts_with(source.ai_model, "xcustom:::") {
        let found = source.custom_models.iter().find(|model| model.id.equals(source.ai_model));
        target.custom_url = found.and_then(|model| model.url).map(|url| url.copy(budget)).transpose()?;
        target.key = found.and_then(|model| model.key).map(|key| key.copy(budget)).transpose()?;
    }
    Ok(target)
}


// ---------------------------------------------------------------------------------------------------------------
// JSON.parse messages.

/// `String(error)` of the `SyntaxError` V8 throws for `JSON.parse(source)`. Only the messages that depend on nothing
/// but the first unexpected character are modelled; anything else is declined.
pub fn json_parse_error(source: &Text, budget: &Budget) -> Result<Text, BuildError> {
    let units = js::units(source);
    let is_space = |unit: u16| matches!(unit, 9 | 10 | 13 | 32);
    let start = units.iter().position(|&unit| !is_space(unit));
    let length = units.len();
    // The index of the first unit V8 cannot continue from, or `None` when the input ends first.
    let culprit = match start {
        None => None,
        Some(at) => {
            let unit = units[at];
            let literal: Option<&str> = match unit { 0x74 => Some("true"), 0x66 => Some("false"), 0x6e => Some("null"), _ => None };
            let value_start = matches!(unit, 0x7b | 0x5b | 0x2d | 0x22) || (0x30..=0x39).contains(&unit);
            if let Some(word) = literal {
                let word = js::utf16(word);
                match (0..word.len()).find(|&offset| units.get(at + offset) != Some(&word[offset])) {
                    Some(offset) if at + offset < length => Some(at + offset),
                    Some(_) => None,
                    None => return Err(BuildError::Unsupported("json-parse-message")),
                }
            } else if value_start || (0xd800..=0xdfff).contains(&unit) {
                if units == js::utf16("[object Object]") {
                    return Ok(js::text("SyntaxError: \"[object Object]\" is not valid JSON", budget)?);
                }
                return Err(BuildError::Unsupported("json-parse-message"));
            } else { Some(at) }
        }
    };
    let message = match culprit {
        None => "Unexpected end of JSON input".to_owned(),
        Some(at) => {
            let unit = units[at];
            if (0xd800..=0xdfff).contains(&unit) { return Err(BuildError::Unsupported("json-parse-message")); }
            let token = String::from_utf16(&[unit]).map_err(|_| BuildError::Unsupported("json-parse-message"))?;
            let slice = |from: usize, to: usize| String::from_utf16_lossy(&units[from..to]);
            let context = if length < 21 { format!("\"{}\"", slice(0, length)) }
                else if at < 10 { format!("\"{}\"...", slice(0, at + 10)) }
                else if at < length - 10 { format!("...\"{}\"...", slice(at - 10, at + 10)) }
                else { format!("...\"{}\"", slice(at - 10, length)) };
            format!("Unexpected token '{token}', {context} is not valid JSON")
        }
    };
    Ok(js::text(&format!("SyntaxError: {message}"), budget)?)
}

/// `error.message` of a `String(error)` text such as `TypeError: fetch failed`.
pub fn error_message(rendered: &Text, budget: &Budget) -> Result<Text, Error> {
    let units = js::units(rendered);
    match js::find_units(&units, &js::utf16(": "), 0) {
        Some(at) => js::from_units(&units[at + 2..], budget),
        None => rendered.copy(budget),
    }
}

/// `text.trim()` cut to [`SDK_MESSAGE_LIMIT_UNITS`] without splitting a surrogate pair.
pub fn bound_message(text: &Text, budget: &Budget) -> Result<Text, Error> {
    let trimmed = text.trim(budget)?;
    let units = js::units(&trimmed);
    if units.len() <= SDK_MESSAGE_LIMIT_UNITS { return Ok(trimmed); }
    let mut end = SDK_MESSAGE_LIMIT_UNITS;
    if (0xd800..0xdc00).contains(&units[end - 1]) { end -= 1; }
    js::from_units(&units[..end], budget)
}

// ---------------------------------------------------------------------------------------------------------------
// Streams.

/// The body of a non-event-stream response as `Response.text()` reads it: UTF-8 with a leading byte order mark removed
/// and invalid sequences replaced.
pub fn response_text(bytes: &[u8], budget: &Budget) -> Result<Text, Error> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    js::text(&String::from_utf8_lossy(bytes), budget)
}

/// A stream whose first response was not an event stream fails with the body text.
pub fn stream_start_failure(status: u16, body: &[u8], budget: &Budget) -> Result<(Text, ProviderFailure), Error> {
    let shown = response_text(body, budget)?;
    let (category, stage) = if status != 200 { (FailureCategory::Http, FailureStage::Body) } else { (FailureCategory::Decode, FailureStage::Headers) };
    let failure = ProviderFailure { category, stage: Some(stage), http_status: Some(status), sdk_message: Some(bound_message(&shown, budget)?), body_failure: None };
    Ok((shown, failure))
}

// ---------------------------------------------------------------------------------------------------------------
// Declines.

/// How far a request has got, which decides what a declined input may become.
#[derive(Debug, Clone, Copy, Default)]
pub struct Progress {
    /// A snapshot reached the caller.
    pub emitted: bool,
    /// A tool ran.
    pub tools_ran: bool,
}

/// A request that cannot continue and cannot be handed to the TS path any more.
#[derive(Debug)]
pub struct Terminal {
    /// The decline name.
    pub reason: &'static str,
    pub failure: ProviderFailure,
}

#[derive(Debug)]
pub enum Settled {
    /// Nothing has been shown or run: the owner sends the request through the TS path.
    FallBack(&'static str),
    /// The request is under way: it ends with this failure.
    Terminal(Terminal),
    /// TS throws; the text is the error message.
    Thrown(Text),
    /// TS returns `{ type: 'fail', result }`.
    Fail(Text),
    Budget(Error),
}

pub fn settle(error: BuildError, progress: Progress, stage: FailureStage) -> Settled {
    match error {
        BuildError::Unsupported(reason) if !progress.emitted && !progress.tools_ran => Settled::FallBack(reason),
        BuildError::Unsupported(reason) => Settled::Terminal(Terminal {
            reason,
            failure: ProviderFailure { category: FailureCategory::Compat, stage: Some(stage), http_status: None, sdk_message: None, body_failure: None },
        }),
        BuildError::Fail(text) => Settled::Fail(text),
        BuildError::Thrown(text) => Settled::Thrown(text),
        BuildError::Budget(error) => Settled::Budget(error),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Buffered replies.

/// What `globalFetch` hands back to a buffered request, before the JSON is read.
pub enum BufferedReply<'a> {
    /// The body was JSON.
    Json { status: u16, data: &'a Value },
    /// `response.json()` threw: `globalFetch` answers `{ ok: false, data: "SyntaxError: ...", status: 400 }` whatever the
    /// HTTP status was.
    NotJson { status: u16, body: &'a Text },
    /// `fetch` threw; `error` is `${error}`.
    Transport { error: &'a Text },
    /// `data: 'aborted'`.
    Aborted,
}

/// A buffered reply read the way `globalFetch` reports it: `res.ok`, `res.data` and the failure category.
pub struct Classified<'a> {
    pub ok: bool,
    pub status: Option<u16>,
    pub category: FailureCategory,
    /// The `sdk_message` when it is not derived from the failure text: the error of a transport failure or of a body that
    /// was not JSON.
    pub message: Option<Text>,
    data: ClassifiedData<'a>,
}

enum ClassifiedData<'a> { Borrowed(&'a Value), Owned(Value) }

impl Classified<'_> {
    /// `res.data`.
    pub fn data(&self) -> &Value {
        match &self.data { ClassifiedData::Borrowed(value) => value, ClassifiedData::Owned(value) => value }
    }
}

pub fn classify<'a>(reply: &'a BufferedReply<'a>, budget: &Budget) -> Result<Classified<'a>, BuildError> {
    Ok(match reply {
        BufferedReply::Json { status, data } => {
            let ok = (200..300).contains(status);
            Classified { ok, status: Some(*status), category: if ok { FailureCategory::Decode } else { FailureCategory::Http }, message: None, data: ClassifiedData::Borrowed(data) }
        }
        BufferedReply::NotJson { status, body } => {
            let rendered = json_parse_error(body, budget)?;
            let message = error_message(&rendered, budget)?;
            Classified { ok: false, status: Some(*status), category: FailureCategory::Decode, message: Some(message), data: ClassifiedData::Owned(Value::Text(rendered)) }
        }
        BufferedReply::Transport { error } => Classified {
            ok: false, status: None, category: FailureCategory::Network, message: Some(error_message(error, budget)?),
            data: ClassifiedData::Owned(Value::Text(error.copy(budget)?)),
        },
        BufferedReply::Aborted => Classified {
            ok: false, status: None, category: FailureCategory::Cancelled, message: None, data: ClassifiedData::Owned(Value::Text(js::text("aborted", budget)?)),
        },
    })
}

/// The failure a buffered request reports.
pub fn buffered_failure(category: FailureCategory, status: Option<u16>, message: Option<Text>) -> ProviderFailure {
    ProviderFailure { category, stage: Some(FailureStage::BufferedJson), http_status: status, sdk_message: message, body_failure: None }
}
