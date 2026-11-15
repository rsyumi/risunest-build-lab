//! `LLMFormat.Horde` (`requestHorde`, `src/ts/process/request/request.ts`) as a sans-IO machine: the request that
//! queues the job on Stable Horde, the status request every two seconds, and the job's first generation cut at the first
//! speaker chunk as its reply. The preview mode returns a fixed error text and builds no request.
//!
//! The owner drives it. [`HordeSession::start`] gives the create request; every [`Step::Send`] is answered with
//! [`HordeSession::replied`]; every [`Step::Wait`] is answered with [`HordeSession::waited`] once the delay has passed.
//! TS uses the plain `fetch`: the create request carries the abort signal and the headers as built, the status requests
//! carry nothing, and a transport error or an unreadable JSON body throws out of `requestHorde`. The poll loop has no
//! timeout and does not look at the abort signal. When the job is not possible, TS starts a `DELETE` it never awaits;
//! [`Step::Done`] carries it as `detached`.
//!
//! Declines (`BuildError::Unsupported`) after the create request has been answered leave a job on the server, so the
//! owner must treat them as terminal (a request that has done work), never as a fall back to the TS path.

#[cfg(test)]
mod replay;

use crate::common::built::{wire_headers, BuiltRequest};
use crate::common::js::{self, J};
use crate::common::jsprop::{read, to_text, truthy, Failed};
use crate::common::owner::{error_message, json_parse_error, response_text};
use crate::common::params;
use crate::common::wire;
use crate::common::BuildError;
use crate::text_completion::unstringlize_chat;
use risunest_ir::chat::PromptMessage;
use risunest_ir::provider::{HttpMethod, HttpRequestSpec, LlmFormat, ProviderCall, ResponseKind};
use risunest_ir::{json, Budget, Error, Text};

/// What the preview mode returns for Horde instead of a request.
pub const PREVIEW_RESULT: &str = "{\"error\":\"Preview body is not supported for Horde\"}";
pub const CREATE_URL: &str = "https://stablehorde.net/api/v2/generate/text/async";
pub const STATUS_URL: &str = "https://stablehorde.net/api/v2/generate/text/status/";
/// The delay before every status request.
pub const POLL_INTERVAL_MS: u32 = 2000;

/// The database fields `requestHorde` reads. The sampling values come from the database directly, not through
/// `applyParameters`.
pub struct HordeSettings {
    /// `db.hordeConfig.apiKey`.
    pub api_key: Text,
    /// `db.maxContext`, `db.maxResponse`, `db.temperature`, `db.top_k` and `db.top_p`.
    pub max_context: f64,
    pub max_response: f64,
    pub temperature: f64,
    pub top_k: f64,
    pub top_p: f64,
    /// `getAdditionalParameters(aiModel)`.
    pub additional_params: Vec<(Text, Text)>,
}

impl std::fmt::Debug for HordeSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("HordeSettings").field("api_key", &"<redacted>").field("max_context", &self.max_context)
            .field("max_response", &self.max_response).field("additional_params", &self.additional_params.len()).finish()
    }
}

pub struct HordeInputs<'a> {
    /// `targ.aiModel`, such as `horde:::<model>`.
    pub ai_model: &'a Text,
    /// `applyChatTemplate(formated)`; the template runs in the template stage.
    pub prompt: &'a Text,
    pub settings: &'a HordeSettings,
}

/// `aiModel.split(":::")[1]`, `None` when there is no `:::`.
fn model_name(ai_model: &Text, budget: &Budget) -> Result<Option<Text>, Error> {
    let units = js::units(ai_model);
    let separator = js::utf16(":::");
    let Some(at) = js::find_units(&units, &separator, 0) else { return Ok(None) };
    let start = at + separator.len();
    let end = js::find_units(&units, &separator, start).unwrap_or(units.len());
    Ok(Some(js::from_units(&units[start..end], budget)?))
}

/// Builds the create request `requestHorde` sends. A model id without `:::` (such as `reverse_proxy` set to the Horde
/// format) throws in TS before any request.
pub fn build_request(call: &ProviderCall, inputs: &HordeInputs<'_>, budget: &Budget) -> Result<BuiltRequest, BuildError> {
    if call.model.format != LlmFormat::Horde { return Err(BuildError::Unsupported("format")); }
    let Some(model) = model_name(inputs.ai_model, budget)? else {
        return Err(BuildError::Thrown(js::text("Cannot read properties of undefined (reading 'trim')", budget)?));
    };
    let settings = inputs.settings;
    let mut generation = js::new_object();
    js::set_str(&mut generation, "n", J::Num(1.0), budget)?;
    js::set_str(&mut generation, "max_context_length", J::Num(settings.max_context + 100.0), budget)?;
    js::set_str(&mut generation, "max_length", J::Num(settings.max_response), budget)?;
    js::set_str(&mut generation, "singleline", J::Bool(false), budget)?;
    js::set_str(&mut generation, "temperature", J::Num(settings.temperature / 100.0), budget)?;
    js::set_str(&mut generation, "top_k", J::Num(settings.top_k), budget)?;
    js::set_str(&mut generation, "top_p", J::Num(settings.top_p), budget)?;
    let mut body = js::new_object();
    js::set_str(&mut body, "prompt", J::Str(inputs.prompt.copy(budget)?), budget)?;
    js::set_str(&mut body, "params", generation, budget)?;
    js::set_str(&mut body, "trusted_workers", J::Bool(false), budget)?;
    js::set_str(&mut body, "workerslow_workers", J::Bool(true), budget)?;
    js::set_str(&mut body, "_blacklist", J::Bool(false), budget)?;
    js::set_str(&mut body, "dry_run", J::Bool(false), budget)?;
    if !model.equals_ascii(b"auto") {
        let space = js::text(" ", budget)?;
        let models = vec![
            J::Str(model.copy(budget)?), J::Str(model.trim(budget)?), J::Str(js::concat(&space, &model, budget)?), J::Str(js::concat(&model, &space, budget)?),
        ];
        js::set_str(&mut body, "models", J::Arr(models), budget)?;
    }
    let api_key = if settings.api_key.len_units() > 2 { settings.api_key.copy(budget)? } else { js::text("0000000000", budget)? };
    let mut headers = js::new_object();
    js::set_str(&mut headers, "content-type", js::str_value("application/json", budget)?, budget)?;
    js::set_str(&mut headers, "apikey", J::Str(api_key), budget)?;
    params::apply_additional_parameters(&mut body, &mut headers, &settings.additional_params, budget)?;
    let bytes = wire::stringify(&body, budget)?.ok_or(BuildError::Unsupported("undefined-body"))?;
    // The plain `fetch` receives the headers as built.
    let wire = wire_headers(&headers, ResponseKind::Sse)?;
    let spec = HttpRequestSpec { method: HttpMethod::Post, url: CREATE_URL.to_owned(), headers: wire, body: bytes, response: ResponseKind::Buffered };
    Ok(BuiltRequest { spec, raw_url: CREATE_URL.to_owned(), body, headers })
}

/// A request of the machine. `abortable` is whether TS passes the abort signal to `fetch`.
#[derive(Debug)]
pub struct HordeRequest {
    pub spec: HttpRequestSpec,
    pub abortable: bool,
}

/// How a request ended.
#[derive(Debug, Clone, Copy)]
pub enum HordeReply<'a> {
    Http { status: u16, body: &'a [u8] },
    /// `fetch` threw, or the abort signal ended it. `error` is `String(error)`, such as `TypeError: fetch failed`.
    Transport { error: &'a Text },
}

#[derive(Debug)]
pub enum HordeOutcome {
    /// `{ type: 'success', result }`.
    Success(Text),
    /// `{ type: 'fail', result }`, with `noRetry: true` when `no_retry` is set.
    Fail { result: Text, no_retry: bool },
}

/// What the owner does next.
#[derive(Debug)]
pub enum Step {
    /// Send the request and answer with [`HordeSession::replied`].
    Send(HordeRequest),
    /// Wait `ms` and answer with [`HordeSession::waited`].
    Wait { ms: u32 },
    /// `requestHorde` returns. `detached` is a request TS starts and never awaits: its reply and its errors are ignored.
    Done { outcome: HordeOutcome, detached: Option<HttpRequestSpec> },
}

#[derive(Debug, PartialEq, Eq)]
enum Phase { Creating, Sleeping, Status, Finished }

/// What the reply cleanup reads: the messages after `reformater`, the character name and `getUserName()`.
pub struct ReplyContext<'a> {
    pub formated: &'a [PromptMessage],
    /// `currentChar?.name ?? ''`.
    pub char_name: &'a Text,
    /// `getUserName()`.
    pub user_name: &'a Text,
}

/// One job from its create request to its end.
#[derive(Debug)]
pub struct HordeSession {
    url: String,
    warn: Text,
    phase: Phase,
}

fn thrown(message: &str, budget: &Budget) -> BuildError {
    match js::text(message, budget) { Ok(text) => BuildError::Thrown(text), Err(error) => error.into() }
}

/// `await response.json()`: a body that is not JSON throws the `SyntaxError` of `JSON.parse`.
fn parsed(body: &[u8], budget: &Budget) -> Result<J, BuildError> {
    let text = response_text(body, budget)?;
    match json::parse(&String::from_utf16_lossy(&text.units().collect::<Vec<_>>()), budget) {
        Ok(value) => Ok(js::from_value(&value, budget)?),
        Err(Error::Syntax { .. }) => Err(BuildError::Thrown(error_message(&json_parse_error(&text, budget)?, budget)?)),
        Err(other) => Err(other.into()),
    }
}

fn decline(failed: Failed) -> BuildError { failed.into() }

impl HordeSession {
    /// The create request of `built`, which [`build_request`] made.
    pub fn start(built: &BuiltRequest, budget: &Budget) -> Result<(Self, Step), BuildError> {
        let session = Self { url: String::new(), warn: js::text("", budget)?, phase: Phase::Creating };
        Ok((session, Step::Send(HordeRequest { spec: built.spec.clone(), abortable: true })))
    }

    fn finish(&mut self, outcome: HordeOutcome, detached: Option<HttpRequestSpec>) -> Step {
        self.phase = Phase::Finished;
        Step::Done { outcome, detached }
    }

    fn sleep(&mut self) -> Step { self.phase = Phase::Sleeping; Step::Wait { ms: POLL_INTERVAL_MS } }

    /// The delay has passed: the status request follows.
    pub fn waited(&mut self) -> Result<Step, BuildError> {
        if self.phase != Phase::Sleeping { return Err(BuildError::Unsupported("horde-step-order")); }
        self.phase = Phase::Status;
        let spec = HttpRequestSpec { method: HttpMethod::Get, url: self.url.clone(), headers: Vec::new(), body: Vec::new(), response: ResponseKind::Buffered };
        Ok(Step::Send(HordeRequest { spec, abortable: false }))
    }

    /// The reply to the last [`Step::Send`].
    pub fn replied(&mut self, reply: HordeReply<'_>, context: &ReplyContext<'_>, budget: &Budget) -> Result<Step, BuildError> {
        match self.phase {
            Phase::Creating => self.created(reply, budget),
            Phase::Status => self.status(reply, context, budget),
            Phase::Sleeping | Phase::Finished => Err(BuildError::Unsupported("horde-step-order")),
        }
    }

    fn created(&mut self, reply: HordeReply<'_>, budget: &Budget) -> Result<Step, BuildError> {
        let (status, body) = match reply {
            HordeReply::Transport { error } => return Err(BuildError::Thrown(error_message(error, budget)?)),
            HordeReply::Http { status, body } => (status, body),
        };
        if status != 202 {
            let result = response_text(body, budget)?;
            return Ok(self.finish(HordeOutcome::Fail { result, no_retry: false }, None));
        }
        let root = parsed(body, budget)?;
        let message = read(Some(&root), "message").map_err(|_| thrown("Cannot read properties of null (reading 'message')", budget))?;
        if truthy(message) {
            let mut warn = js::text("with ", budget)?;
            warn.append(&to_text(message.unwrap_or(&J::Undefined), budget).map_err(decline)?)?;
            self.warn = warn;
        }
        // `"…/status/" + json.id`. Real ids are UUIDs; an id that `fetch` would rewrite or read as another part of the URL
        // is declined.
        let id = read(Some(&root), "id").map_err(|_| thrown("Cannot read properties of null (reading 'id')", budget))?;
        let id = to_text(id.unwrap_or(&J::Undefined), budget).map_err(decline)?;
        let id = id.as_unicode().ok_or(BuildError::Unsupported("id-form"))?;
        if !id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-') { return Err(BuildError::Unsupported("id-form")); }
        self.url = format!("{STATUS_URL}{id}");
        Ok(self.sleep())
    }

    fn status(&mut self, reply: HordeReply<'_>, context: &ReplyContext<'_>, budget: &Budget) -> Result<Step, BuildError> {
        // The status code is not looked at.
        let body = match reply {
            HordeReply::Transport { error } => return Err(BuildError::Thrown(error_message(error, budget)?)),
            HordeReply::Http { body, .. } => body,
        };
        let root = parsed(body, budget)?;
        let possible = read(Some(&root), "is_possible").map_err(|_| thrown("Cannot read properties of null (reading 'is_possible')", budget))?;
        if !truthy(possible) {
            let mut result = js::text("Response not possible", budget)?;
            result.append(&self.warn)?;
            let cancel = HttpRequestSpec { method: HttpMethod::Delete, url: self.url.clone(), headers: Vec::new(), body: Vec::new(), response: ResponseKind::Buffered };
            return Ok(self.finish(HordeOutcome::Fail { result, no_retry: true }, Some(cancel)));
        }
        let done = read(Some(&root), "done").map_err(decline_thrown)?;
        let generations = read(Some(&root), "generations").map_err(decline_thrown)?;
        let first = match generations { Some(J::Arr(items)) if truthy(done) => items.first(), _ => None };
        let Some(first) = first else { return Ok(self.sleep()) };
        let text = read(Some(first), "text").map_err(|_| thrown("Cannot read properties of null (reading 'text')", budget))?;
        let text = match text {
            None | Some(J::Null) | Some(J::Undefined) => js::text("", budget)?,
            Some(J::Str(text)) => text.copy(budget)?,
            // `unstringlizeChat` calls `text.indexOf`.
            Some(_) => return Err(BuildError::Unsupported("result-text-not-text")),
        };
        let cleaned = unstringlize_chat(&text, context.formated, context.char_name, context.user_name, budget)?;
        Ok(self.finish(HordeOutcome::Success(cleaned), None))
    }
}

fn decline_thrown(_: crate::common::jsprop::Thrown) -> BuildError { BuildError::Unsupported("thrown") }
