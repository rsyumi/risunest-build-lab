//! `tokenizeGoogleCloud` (`src/ts/tokenizer.ts`): the Gemini `countTokens` API, with Gemma for non-200 responses.

use super::{Memo, PluginMemoLimits};
use risunest_ir::provider::{HttpMethod, HttpRequestSpec, ResponseKind};
use risunest_ir::stage::{HostReply, HostRequest, StageError};
use risunest_ir::{json, num, Budget, OwnedVec, Text, Utf8Decoder, Value};
use std::borrow::Cow;
use std::sync::{Arc, Mutex, PoisonError};

// The largest length `ToIndex` accepts, `2 ** 53 - 1`.
const MAX_INDEX: f64 = 9_007_199_254_740_991.0;

/// What `new Uint32Array(totalTokens)` makes of one answer: the array's length, or the `RangeError` it throws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Total { Length(u64), Invalid }
impl Total {
    pub(crate) fn length(self) -> Result<u64, StageError> {
        match self {
            Self::Length(length) => Ok(length),
            Self::Invalid => Err(StageError::Invalid(Cow::Borrowed("google-cloud-token-length"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answer { Api(Total), Gemma }

/// The `googleCloudTokenizedCache` of `tokenizeGoogleCloud`: parsed HTTP 200 `totalTokens`, keyed by the text
/// followed by the model's `internalID`, as TS concatenates them. TS consults it before every request whether or not
/// `useTokenizerCaching` is on, and keeps every answer for the session; this memo has the bounds of
/// [`PluginMemoLimits`], so a text it drops reaches the API again where TS would reuse it. Clones share one memo: an
/// owner that keeps one for the session and hands it to every [`super::Counter`] and [`super::encode::Encoder`] asks
/// the API as often as TS does.
#[derive(Clone)]
pub struct GoogleCloudMemo(Arc<Mutex<Memo<Total>>>);
impl GoogleCloudMemo {
    pub fn new() -> Self { Self::with_limits(PluginMemoLimits::default()) }
    pub fn with_limits(limits: PluginMemoLimits) -> Self { Self(Arc::new(Mutex::new(Memo::new(limits)))) }
}
impl Default for GoogleCloudMemo {
    fn default() -> Self { Self::new() }
}

/// What `tokenizeGoogleCloud` reads besides the text. `getModelInfo` stays with the owner.
pub struct GoogleCloud {
    /// `getModelInfo(db.aiModel).internalID`, as a template literal prints it.
    pub internal_id: Text,
    /// `db.google?.accessToken` as a template literal prints it; `None` prints `undefined`.
    pub access_token: Option<Text>,
    pub memo: GoogleCloudMemo,
}

// A `fetch` URL is a USVString, so an isolated surrogate reaches the request as U+FFFD.
fn usv(text: &Text, budget: &Budget) -> Result<String, StageError> {
    budget.work(text.len_units())?;
    let _charge = budget.reserve_owned(text.len_units().saturating_mul(3))?;
    Ok(match text.as_unicode() {
        Some(value) => value.to_owned(),
        None => char::decode_utf16(text.units()).map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER)).collect(),
    })
}

fn object(key: &str, value: Value, budget: &Budget) -> Result<Value, StageError> {
    let mut object = Value::Object(OwnedVec::new(budget)?);
    object.define(Text::from_str(key, budget)?, value, budget)?;
    Ok(object)
}
fn array(value: Value, budget: &Budget) -> Result<Value, StageError> {
    let mut values = OwnedVec::new(budget)?;
    values.push(value)?;
    Ok(Value::Array(values))
}

// `new Uint32Array(json.totalTokens)` for a value that is not an object: `ToIndex` of its number.
fn total(value: Option<&Value>, budget: &Budget) -> Result<Total, StageError> {
    let number = match value {
        None | Some(Value::Undefined | Value::Null) => 0.0,
        Some(Value::Bool(value)) => f64::from(u8::from(*value)),
        Some(Value::Number(value)) => *value,
        Some(Value::Text(value)) => num::to_number(value, budget)?,
        // TS reads an array or object as the array-like it copies into the typed array.
        Some(Value::Array(_) | Value::Object(_)) => return Err(StageError::Unsupported(Cow::Borrowed("google-cloud-token-list"))),
    };
    let integer = if number.is_nan() { 0.0 } else { number.trunc() };
    Ok(if (0.0..=MAX_INDEX).contains(&integer) { Total::Length(integer as u64) } else { Total::Invalid })
}

impl GoogleCloud {
    // `text + model.internalID`.
    fn key(&self, text: &Text, budget: &Budget) -> Result<Vec<u16>, StageError> {
        let length = text.len_units().saturating_add(self.internal_id.len_units());
        budget.work(length)?;
        let _charge = budget.reserve_owned(length.saturating_mul(2))?;
        Ok(text.units().chain(self.internal_id.units()).collect())
    }

    /// The memo's answer for `text`, or `None` when TS would ask the API.
    pub(crate) fn cached(&self, text: &Text, budget: &Budget) -> Result<Option<Total>, StageError> {
        let mut memo = self.memo.0.lock().unwrap_or_else(PoisonError::into_inner);
        if !memo.keeps(text.len_units().saturating_add(self.internal_id.len_units())) { return Ok(None); }
        let key = self.key(text, budget)?;
        Ok(memo.get(&key))
    }

    /// The `countTokens` POST: the URL as the template literal builds it, no header but `Content-Type`, and the body
    /// `JSON.stringify({contents: [{parts: [{text}]}]})`.
    pub(crate) fn request(&self, text: &Text, budget: &Budget) -> Result<HostRequest, StageError> {
        let model = usv(&self.internal_id, budget)?;
        let key = match self.access_token.as_ref() { Some(token) => usv(token, budget)?, None => "undefined".to_owned() };
        let url = format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:countTokens?key={key}");
        let part = object("text", Value::Text(text.copy(budget)?), budget)?;
        let content = object("parts", array(part, budget)?, budget)?;
        let request = object("contents", array(content, budget)?, budget)?;
        let body = json::stringify(&request, budget)?.ok_or(StageError::Invalid(Cow::Borrowed("google-cloud-body")))?;
        // `stringify` writes isolated surrogates as escapes, so the body is always Unicode.
        let body = body.as_unicode().ok_or(StageError::Invalid(Cow::Borrowed("google-cloud-body")))?;
        budget.work(body.len())?;
        Ok(HostRequest::Http(HttpRequestSpec {
            method: HttpMethod::Post,
            url,
            headers: vec![("Content-Type".to_owned(), "application/json".to_owned())],
            body: body.as_bytes().to_vec(),
            response: ResponseKind::Buffered,
        }))
    }

    /// What TS makes of the reply to [`GoogleCloud::request`] for `text`: a 200 reads `totalTokens` from `res.json()`
    /// into the memo, any other status takes the Gemma path, and a thrown `fetch` is the error TS throws.
    pub(crate) fn answer(&self, text: &Text, reply: HostReply, budget: &Budget) -> Result<Answer, StageError> {
        let body = match reply {
            HostReply::Http { status: 200, body } => body,
            HostReply::Http { .. } => return Ok(Answer::Gemma),
            HostReply::Failed { message } => return Err(StageError::Host(message.units().collect())),
            HostReply::Cancelled => return Err(StageError::Cancelled),
            HostReply::Unsupported(name) => return Err(StageError::Unsupported(name)),
            _ => return Err(StageError::Invalid(Cow::Borrowed("google-cloud-reply"))),
        };
        // `res.json()`: UTF-8 with the BOM removed and malformed bytes replaced, then `JSON.parse`.
        let decoded = Utf8Decoder::new().decode(&body, true, budget)?;
        let parsed = json::parse_json_text(&decoded, budget)?;
        // `json.totalTokens` throws a TypeError on null and reads `undefined` from any other non-object.
        if matches!(parsed, Value::Null) { return Err(StageError::Invalid(Cow::Borrowed("google-cloud-response"))); }
        let total = total(parsed.get(b"totalTokens", budget)?, budget)?;
        // TS stores the answer before it builds the array, so a length that throws is reused and throws again.
        let key = self.key(text, budget)?;
        let cost = key.len();
        self.memo.0.lock().unwrap_or_else(PoisonError::into_inner).set(key, total, cost);
        Ok(Answer::Api(total))
    }
}
