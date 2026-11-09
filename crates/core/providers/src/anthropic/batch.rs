//! `db.claudeBatching` (`anthropic.ts:623-886`) as a sans-IO machine: the request that creates the batch, the poll loop that
//! asks for its status every three seconds, the cancel request after an abort, and the results read back as JSON lines.
//!
//! The owner drives it. [`BatchSession::start`] gives the create request; every [`Step::Send`] is answered with
//! [`BatchSession::replied`]; every [`Step::Wait`] is answered with [`BatchSession::waited`] once the delay has passed,
//! which is where the owner says whether the request was aborted meanwhile. Both calls take the clock (`Date.now()`) at
//! that moment, so no timer or clock lives here. A transport error, and a request cut short by the abort signal, are
//! reported as [`BatchReply::Transport`]; the poll loop of TS swallows both and polls again.
//!
//! Declines (`BuildError::Unsupported`) after the create request has been answered leave a batch on the server, so the
//! owner must treat them as terminal (a request that has done work), never as a fall back to the TS path.

use super::response::render_content;
use super::BuildError;
use crate::common::built::{utf8, wire_headers, BuiltRequest};
use crate::common::js::{self, J};
use crate::common::jsprop::{read, read_optional, to_text, truthy, Failed, Prop};
use crate::common::owner::{error_message, json_parse_error, response_text};
use crate::common::wire;
use risunest_ir::provider::{HttpMethod, HttpRequestSpec, ResponseKind};
use risunest_ir::{json, Budget, Error, Text};

/// The delay before every status request.
pub const POLL_INTERVAL_MS: u32 = 3000;
/// `24 * 60 * 60 * 1000 + 600 * 1000`: the poll loop gives up when more than this has passed since the batch was created.
pub const TIMEOUT_MS: i64 = 87_000_000;

/// A request of the machine. `abortable` is whether TS passes the abort signal to `fetch`; the cancel request and every
/// request after the cancel never carry it.
#[derive(Debug)]
pub struct BatchRequest {
    pub spec: HttpRequestSpec,
    pub abortable: bool,
}

/// How a request ended.
#[derive(Debug, Clone, Copy)]
pub enum BatchReply<'a> {
    Http { status: u16, body: &'a [u8] },
    /// `fetch` threw, or the abort signal ended it. `error` is `String(error)`, such as `TypeError: fetch failed`.
    Transport { error: &'a Text },
}

/// What the owner does next.
#[derive(Debug)]
pub enum Step {
    /// Send the request and answer with [`BatchSession::replied`].
    Send(BatchRequest),
    /// Wait `ms` and answer with [`BatchSession::waited`]. The first one means the batch exists and the stream begins.
    Wait { ms: u32 },
    /// The request ends with `{ type: 'fail', result }` before any stream exists.
    Rejected(Text),
    /// The stream ends: the snapshot `{ "0": text }` when there is one, then `controller.close()`.
    Done(Option<Text>),
    /// The stream ends with `controller.error(new Error(message))`; the error text is `Error: message`, or `Error` when the
    /// message is empty.
    Error(Text),
}

#[derive(Debug, PartialEq, Eq)]
enum Phase { Creating, Sleeping, Cancelling, Status, Results, Finished }

/// One batch from its create request to its end.
#[derive(Debug)]
pub struct BatchSession {
    base_url: String,
    headers: Vec<(String, String)>,
    id: Option<String>,
    start_ms: i64,
    cancel_requested: bool,
    phase: Phase,
}

fn request(method: HttpMethod, url: String, headers: &[(String, String)], body: Vec<u8>, abortable: bool) -> Step {
    Step::Send(BatchRequest { spec: HttpRequestSpec { method, url, headers: headers.to_vec(), body, response: ResponseKind::Buffered }, abortable })
}

/// `Response.json()` / `Response.text()`: UTF-8 without a leading byte order mark, invalid sequences replaced.
fn decoded(body: &[u8], budget: &Budget) -> Result<Text, Error> { response_text(body, budget) }

impl BatchSession {
    /// The create request for the batch that holds the request `built` describes. `custom_id` is the `v4()` TS draws.
    pub fn start(built: &BuiltRequest, custom_id: &Text, budget: &Budget) -> Result<(Self, Step), BuildError> {
        let mut params = js::clone_j(&built.body, budget)?;
        if !matches!(js::get(&params, "stream"), None | Some(J::Undefined)) { js::delete(&mut params, "stream"); }
        let mut entry = js::new_object();
        js::set_str(&mut entry, "custom_id", J::Str(custom_id.copy(budget)?), budget)?;
        js::set_str(&mut entry, "params", params, budget)?;
        let mut body = js::new_object();
        js::set_str(&mut body, "requests", J::Arr(vec![entry]), budget)?;
        let bytes = wire::stringify(&body, budget)?.ok_or(BuildError::Unsupported("undefined-body"))?;
        // `fetchNative` receives the URL and the headers as built.
        let headers = wire_headers(&built.headers, ResponseKind::Sse)?;
        let base_url = built.raw_url.clone();
        let session = Self { base_url: base_url.clone(), headers: headers.clone(), id: None, start_ms: 0, cancel_requested: false, phase: Phase::Creating };
        let step = request(HttpMethod::Post, format!("{base_url}/batches"), &headers, bytes, true);
        Ok((session, step))
    }

    /// Whether the batch exists and the stream has begun.
    pub fn is_streaming(&self) -> bool { !matches!(self.phase, Phase::Creating) && self.id.is_some() }

    fn url(&self, suffix: &str) -> String { format!("{}/batches/{}{suffix}", self.base_url, self.id.as_deref().unwrap_or_default()) }

    fn finish(&mut self, step: Step) -> Step { self.phase = Phase::Finished; step }

    fn sleep(&mut self) -> Step { self.phase = Phase::Sleeping; Step::Wait { ms: POLL_INTERVAL_MS } }

    /// The delay has passed. `aborted` is `abortSignal?.aborted` now.
    pub fn waited(&mut self, aborted: bool, now_ms: i64, budget: &Budget) -> Result<Step, BuildError> {
        if self.phase != Phase::Sleeping { return Err(BuildError::Unsupported("batch-step-order")); }
        if aborted && !self.cancel_requested {
            self.cancel_requested = true;
            self.phase = Phase::Cancelling;
            return Ok(request(HttpMethod::Post, self.url("/cancel"), &self.headers, b"{}".to_vec(), false));
        }
        self.poll_or_time_out(now_ms, budget)
    }

    fn poll_or_time_out(&mut self, now_ms: i64, budget: &Budget) -> Result<Step, BuildError> {
        if now_ms - self.start_ms > TIMEOUT_MS {
            let message = js::text("Claude batch request timed out after 24 hours", budget)?;
            return Ok(self.finish(Step::Error(message)));
        }
        self.phase = Phase::Status;
        Ok(request(HttpMethod::Get, self.url(""), &self.headers, Vec::new(), !self.cancel_requested))
    }

    /// The reply to the last [`Step::Send`].
    pub fn replied(&mut self, reply: BatchReply<'_>, now_ms: i64, budget: &Budget) -> Result<Step, BuildError> {
        match self.phase {
            Phase::Creating => self.created(reply, now_ms, budget),
            // A failed cancel request is ignored, and so is its answer.
            Phase::Cancelling => self.poll_or_time_out(now_ms, budget),
            Phase::Status => self.status(reply, budget),
            Phase::Results => self.results(reply, budget),
            Phase::Sleeping | Phase::Finished => Err(BuildError::Unsupported("batch-step-order")),
        }
    }

    fn created(&mut self, reply: BatchReply<'_>, now_ms: i64, budget: &Budget) -> Result<Step, BuildError> {
        let (status, body) = match reply {
            BatchReply::Transport { error } => return Err(BuildError::Thrown(error_message(error, budget)?)),
            BatchReply::Http { status, body } => (status, body),
        };
        let text = decoded(body, budget)?;
        if status != 200 { return Ok(self.finish(Step::Rejected(text))); }
        let parsed = match json::parse(&String::from_utf16_lossy(&text.units().collect::<Vec<_>>()), budget) {
            Ok(value) => value,
            Err(Error::Syntax { .. }) => return Err(BuildError::Thrown(error_message(&json_parse_error(&text, budget)?, budget)?)),
            Err(other) => return Err(other.into()),
        };
        let root = js::from_value(&parsed, budget)?;
        let id = match read(Some(&root), "id") {
            Ok(id) => id,
            Err(_) => return Err(BuildError::Thrown(js::text("Cannot read properties of null (reading 'id')", budget)?)),
        };
        if !truthy(id) {
            let message = js::text("No results URL returned from Claude batch request", budget)?;
            return Ok(self.finish(Step::Rejected(message)));
        }
        // The id goes into three URLs. Real ids are `msgbatch_` and letters and digits; anything `fetch` would rewrite or
        // read as another part of the URL is declined, and so is an id that is not text.
        let Some(J::Str(id)) = id else { return Err(BuildError::Unsupported("batch-id-not-text")) };
        let id = utf8(id).map_err(|_| BuildError::Unsupported("batch-id-form"))?;
        if !id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-') { return Err(BuildError::Unsupported("batch-id-form")); }
        self.id = Some(id);
        self.start_ms = now_ms;
        Ok(self.sleep())
    }

    fn status(&mut self, reply: BatchReply<'_>, budget: &Budget) -> Result<Step, BuildError> {
        let (status, body) = match reply {
            // Swallowed by the `catch` around the loop body.
            BatchReply::Transport { .. } => return Ok(self.sleep()),
            BatchReply::Http { status, body } => (status, body),
        };
        let text = decoded(body, budget)?;
        if status != 200 { return Ok(self.finish(Step::Error(text))); }
        let ended = match json::parse(&String::from_utf16_lossy(&text.units().collect::<Vec<_>>()), budget) {
            Ok(value) => {
                let root = js::from_value(&value, budget)?;
                // `null.processing_status` throws, which the loop swallows like a value that is not `'ended'`.
                matches!(js::get(&root, "processing_status"), Some(J::Str(found)) if found.equals_ascii(b"ended"))
            }
            Err(Error::Syntax { .. }) => false,
            Err(other) => return Err(other.into()),
        };
        if !ended { return Ok(self.sleep()); }
        self.phase = Phase::Results;
        Ok(request(HttpMethod::Get, self.url("/results"), &self.headers, Vec::new(), !self.cancel_requested))
    }

    fn results(&mut self, reply: BatchReply<'_>, budget: &Budget) -> Result<Step, BuildError> {
        let (status, body) = match reply {
            BatchReply::Transport { .. } => return Ok(self.sleep()),
            BatchReply::Http { status, body } => (status, body),
        };
        let text = decoded(body, budget)?;
        if status != 200 { return Ok(self.finish(Step::Error(text))); }
        let source = String::from_utf16_lossy(&text.units().collect::<Vec<_>>());
        let mut items: Vec<J> = Vec::new();
        for line in source.split('\n') {
            if js::text(line, budget)?.trim(budget)?.len_units() == 0 { continue; }
            match json::parse(line, budget) {
                // A line that is the JSON `null` is dropped like one that does not parse.
                Ok(risunest_ir::Value::Null) | Err(Error::Syntax { .. }) => {}
                Ok(value) => items.push(js::from_value(&value, budget)?),
                Err(other) => return Err(other.into()),
            }
        }
        for item in &items {
            match item_outcome(item, budget)? {
                Item::Next => {}
                Item::Swallowed => return Ok(self.sleep()),
                Item::Done(snapshot) => return Ok(self.finish(Step::Done(snapshot))),
                Item::Error(message) => return Ok(self.finish(Step::Error(message))),
            }
        }
        Ok(self.sleep())
    }
}

enum Item {
    /// Not a result this loop acts on.
    Next,
    /// An exception the loop swallows: nothing after it in the same reply is read.
    Swallowed,
    Done(Option<Text>),
    Error(Text),
}

fn decline(failed: Failed) -> BuildError {
    match failed {
        Failed::Decline(name) => BuildError::Unsupported(name),
        Failed::Budget(error) => BuildError::Budget(error),
        Failed::Thrown => BuildError::Unsupported("thrown"),
    }
}

fn is(value: Prop<'_>, name: &str) -> bool { matches!(value, Some(J::Str(found)) if found.equals_ascii(name.as_bytes())) }

/// One line of the results, which TS reads with `batchData?.result?.type`.
fn item_outcome(item: &J, budget: &Budget) -> Result<Item, BuildError> {
    let result = read_optional(Some(item), "result").map_err(|_| BuildError::Unsupported("thrown"))?;
    let kind = read_optional(result, "type").map_err(|_| BuildError::Unsupported("thrown"))?;
    if is(kind, "succeeded") {
        // `batchData.result.message.content ?? []`
        let Ok(message) = read(result, "message") else { return Ok(Item::Swallowed) };
        let Ok(contents) = read(message, "content") else { return Ok(Item::Swallowed) };
        let blocks: &[J] = match contents {
            None | Some(J::Null | J::Undefined) => &[],
            Some(J::Arr(items)) => items,
            // `for (const content of 'text')` visits characters, none of which has a `type`.
            Some(J::Str(_)) => &[],
            // Not iterable.
            Some(_) => return Ok(Item::Swallowed),
        };
        // `content.type` throws for a `null` entry, before anything is enqueued.
        if blocks.iter().any(|block| matches!(block, J::Null | J::Undefined)) { return Ok(Item::Swallowed); }
        return Ok(Item::Done(Some(render_content(blocks, budget)?)));
    }
    if is(kind, "errored") {
        let batch_error = read(result, "error").map_err(|_| BuildError::Unsupported("thrown"))?;
        let inner = read_optional(batch_error, "error").map_err(|_| BuildError::Unsupported("thrown"))?;
        let message = read_optional(inner, "message").map_err(|_| BuildError::Unsupported("thrown"))?;
        let text = if truthy(message) {
            let kind = read(inner, "type").map_err(|_| BuildError::Unsupported("thrown"))?;
            let mut text = to_text(kind.unwrap_or(&J::Undefined), budget).map_err(decline)?;
            text.append(&js::text(": ", budget)?)?;
            text.append(&to_text(message.unwrap_or(&J::Undefined), budget).map_err(decline)?)?;
            text
        } else {
            // `JSON.stringify(undefined)` is `undefined`, which `new Error` reads as no message.
            match batch_error {
                None => js::text("", budget)?,
                Some(value) => {
                    let bytes = wire::stringify(value, budget)?.ok_or(BuildError::Unsupported("undefined-body"))?;
                    js::text(std::str::from_utf8(&bytes).map_err(|_| BuildError::Unsupported("json-utf8"))?, budget)?
                }
            }
        };
        return Ok(Item::Error(text));
    }
    if is(kind, "canceled") { return Ok(Item::Done(None)); }
    if is(kind, "expired") { return Ok(Item::Error(js::text("Claude batch request expired", budget)?)); }
    Ok(Item::Next)
}
