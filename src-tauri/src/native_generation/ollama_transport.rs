use super::{
    ollama_decode::{self as decode, Cancellation, Chunk, ChunkSource, DecodeError, DecodeLimits, Pipeline},
    prepare::PreparedOllamaTurn,
};
use risunest_ir::{self as ir, Budget, Charge, Error as CompatError, Text, Utf8Decoder, Value};
use async_trait::async_trait;
use reqwest::{header::{HeaderValue, ACCEPT, ACCEPT_ENCODING, CONTENT_TYPE, USER_AGENT}, Response, Url, Version};
use serde::{Serialize, ser::{SerializeSeq, Serializer}};
use std::{io, mem::size_of, sync::Arc};

#[derive(Clone, Copy, Serialize)]
#[serde(untagged)]
pub(crate) enum Think { Boolean(bool), Level(ThinkLevel) }
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ThinkLevel { Low, Medium, High }

pub(crate) struct CapturedOllamaConfig<'a> {
    pub host: &'a str,
    pub model: &'a str,
    pub streaming: bool,
    pub think: Option<Think>,
    // Raw navigator metadata, not the SDK's attempted browser-controlled UA value.
    pub browser_user_agent: &'a str,
}
#[derive(Clone, Copy)]
pub(crate) struct TransportLimits {
    pub decode: DecodeLimits,
    pub max_request_bytes: usize,
    pub max_url_bytes: usize,
    pub max_user_agent_bytes: usize,
    pub max_buffered_bytes: usize,
    pub max_line_bytes: usize,
    pub max_output_bytes: usize,
    // Per publication, not cumulative stream output. Retained allocations and
    // operation work/JSON/input counters remain shared across every publication.
    pub max_output_value_units: usize,
}
impl TransportLimits {
    fn validate(&self) -> Result<(), Failure> {
        self.decode.validate()?;
        if self.max_request_bytes == 0 || self.max_url_bytes == 0 || self.max_user_agent_bytes == 0
            || self.max_buffered_bytes == 0 || self.max_line_bytes == 0 || self.max_output_bytes == 0 || self.max_output_value_units == 0
            || self.max_output_bytes > 4 * 1024 * 1024 { return Err(Failure::Limit("invalid-transport-limits")); }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage { Request, Headers, Body, BufferedJson, StreamJson, SdkIterator, Transform, Credit, Offer }
#[derive(Debug)]
pub(crate) enum Failure {
    Cancelled,
    ConsumerCancelled,
    Limit(&'static str),
    Compat { stage: Stage, error: CompatError },
    Decode(DecodeError),
    Network { stage: Stage },
    Type { stage: Stage, operation: &'static str },
    Sdk { stage: Stage, message: Text },
    Http { status_code: u16, error: Value, message: Text, body_failure: Option<BodyFailure> },
    Consumer { stage: Stage },
    DeliveryUnknown,
    InvalidHost,
    Userinfo,
    Allocation,
}
#[derive(Debug, Clone, Copy)]
pub(crate) enum BodyFailure { Reader, Codec, Json, Property }
impl From<DecodeError> for Failure {
    fn from(value: DecodeError) -> Self {
        match value { DecodeError::Cancelled => Self::Cancelled, DecodeError::Limit(kind) => Self::Limit(kind), other => Self::Decode(other) }
    }
}
fn at(stage: Stage, error: CompatError) -> Failure {
    match error { CompatError::Cancelled => Failure::Cancelled, CompatError::Limit(kind) => Failure::Limit(kind),
        CompatError::Allocation => Failure::Allocation, other => Failure::Compat { stage, error: other } }
}
fn text(value: &str, budget: &Budget, stage: Stage) -> Result<Text, Failure> { Text::from_str(value, budget).map_err(|error| at(stage, error)) }
fn empty(budget: &Budget, stage: Stage) -> Result<Value, Failure> { Ok(Value::Text(text("", budget, stage)?)) }

pub(crate) struct Snapshot { pub sequence: u64, pub value: Value, _charge: Charge }
impl std::fmt::Debug for Snapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { formatter.debug_struct("Snapshot").field("sequence", &self.sequence).finish_non_exhaustive() }
}
#[derive(Debug, Clone, Copy)]
pub(crate) enum ConsumerStop { Cancelled, Failed }
#[derive(Debug, Clone, Copy)]
pub(crate) enum OfferResult { Accepted, Rejected(ConsumerStop), Unknown }
#[async_trait]
pub(crate) trait OutputSink: Send {
    async fn credit(&mut self) -> Result<(), ConsumerStop>;
    // Accepted may be returned only after the consumer's own durable receipt.
    // Dropping this future does not establish that the offer was rolled back.
    async fn offer(&mut self, snapshot: Arc<Snapshot>) -> OfferResult;
}
#[derive(Debug)]
pub(crate) enum Settlement { Normal, Cancelled(decode::CancelOrigin), ConsumerCancelled, Failed(Failure) }
#[derive(Debug)]
pub(crate) struct Outcome {
    pub settlement: Settlement,
    pub last_accepted: Option<Arc<Snapshot>>,
    pub in_flight: Option<Arc<Snapshot>>,
}
// Logical public-value units, not heap bytes or a wire encoding. Every node
// costs one; keys cost one plus their UTF-16 length; text adds its UTF-16 length.
fn output_value_units(value: &Value, maximum: usize, budget: &Budget) -> Result<usize, Failure> {
    fn add(total: &mut usize, amount: usize, maximum: usize) -> Result<(), Failure> {
        *total = total.checked_add(amount).filter(|value| *value <= maximum).ok_or(Failure::Limit("output-value-units"))?;
        Ok(())
    }
    fn visit(value: &Value, depth: usize, total: &mut usize, maximum: usize, budget: &Budget) -> Result<(), Failure> {
        budget.work(1).map_err(|error| at(Stage::Transform, error))?;
        if depth > 128 { return Err(Failure::Limit("output-value-depth")); }
        add(total, 1, maximum)?;
        match value {
            Value::Text(value) => add(total, value.len_units(), maximum)?,
            Value::Array(_) => for child in value.array().expect("array branch") { visit(child, depth + 1, total, maximum, budget)?; },
            Value::Object(_) => for (key, child) in value.object().expect("object branch") {
                budget.work(1).map_err(|error| at(Stage::Transform, error))?;
                add(total, 1, maximum)?; add(total, key.len_units(), maximum)?;
                visit(child, depth + 1, total, maximum, budget)?;
            },
            Value::Undefined | Value::Null | Value::Bool(_) | Value::Number(_) => (),
        }
        Ok(())
    }
    let mut total = 0;
    visit(value, 0, &mut total, maximum, budget)?;
    Ok(total)
}
struct Delivery<'a> {
    sink: &'a mut dyn OutputSink, cancel: &'a Cancellation, budget: &'a Budget,
    last: Option<Arc<Snapshot>>, in_flight: Option<Arc<Snapshot>>, next: u64,
}
impl Delivery<'_> {
    async fn publish(&mut self, value: Value, limits: TransportLimits) -> Result<(), Failure> {
        self.budget.check().map_err(|error| at(Stage::Credit, error))?;
        output_value_units(&value, limits.max_output_value_units, self.budget)?;
        if let Value::Text(value) = &value {
            if value.utf8_accounting_len(self.budget).map_err(|error| at(Stage::Transform, error))? > limits.max_output_bytes {
                return Err(Failure::Limit("formatted-output-bytes"));
            }
        }
        let charge = self.budget.reserve_owned(size_of::<Snapshot>() + 2 * size_of::<usize>()).map_err(|error| at(Stage::Credit, error))?;
        let candidate = Arc::new(Snapshot { sequence: self.next, value, _charge: charge });
        tokio::select! { biased;
            _ = self.cancel.cancelled() => return Err(Failure::Cancelled),
            result = self.sink.credit() => match result {
                Ok(()) => (), Err(ConsumerStop::Cancelled) => return Err(Failure::ConsumerCancelled),
                Err(ConsumerStop::Failed) => return Err(Failure::Consumer { stage: Stage::Credit }),
            }
        }
        if self.cancel.is_cancelled() { return Err(Failure::Cancelled); }
        self.in_flight = Some(candidate.clone());
        let accepted = tokio::select! { biased;
            _ = self.cancel.cancelled() => return Err(Failure::Cancelled),
            value = self.sink.offer(candidate) => value,
        };
        match accepted {
            OfferResult::Accepted => {
                self.last = self.in_flight.take();
                self.next = self.next.checked_add(1).ok_or(Failure::Limit("snapshot-sequence"))?;
                if self.cancel.is_cancelled() { return Err(Failure::Cancelled); }
                Ok(())
            }
            OfferResult::Rejected(reason) => {
                self.in_flight = None;
                Err(match reason { ConsumerStop::Cancelled => Failure::ConsumerCancelled, ConsumerStop::Failed => Failure::Consumer { stage: Stage::Offer } })
            }
            OfferResult::Unknown => Err(Failure::DeliveryUnknown),
        }
    }
}

#[derive(Serialize)]
struct WireMessage<'a> { role: &'a str, content: &'a str }
struct WireMessages<'a>(&'a PreparedOllamaTurn);
impl Serialize for WireMessages<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(None)?;
        for message in &self.0.messages {
            if matches!(message.role.as_str(), "assistant" | "user" | "system") {
                sequence.serialize_element(&WireMessage { role: &message.role, content: &message.content })?;
            }
        }
        sequence.end()
    }
}
#[derive(Serialize)]
struct WireBody<'a> {
    model: &'a str, messages: WireMessages<'a>, stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<Think>,
}
struct BodyWriter<'a> { data: Option<&'a mut Vec<u8>>, length: usize, maximum: usize, budget: &'a Budget, error: Option<Failure> }
impl io::Write for BodyWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = (|| {
            self.budget.work(bytes.len()).map_err(|error| at(Stage::Request, error))?;
            let length = self.length.checked_add(bytes.len()).filter(|length| *length <= self.maximum).ok_or(Failure::Limit("request-bytes"))?;
            if let Some(data) = self.data.as_mut() {
                if length > data.capacity() { return Err(Failure::Limit("request-capacity")); }
                data.extend_from_slice(bytes);
            }
            self.length = length;
            Ok(())
        })();
        if let Err(error) = result { self.error = Some(error); return Err(io::Error::other("bounded request serialization stopped")); }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}
fn request_body(prepared: &PreparedOllamaTurn, config: &CapturedOllamaConfig<'_>, limits: TransportLimits, budget: &Budget) -> Result<(Vec<u8>, Charge, Option<Charge>), Failure> {
    let body = WireBody { model: config.model, messages: WireMessages(prepared), stream: config.streaming, think: config.think };
    let mut count = BodyWriter { data: None, length: 0, maximum: limits.max_request_bytes, budget, error: None };
    if serde_json::to_writer(&mut count, &body).is_err() { return Err(count.error.unwrap_or(Failure::Type { stage: Stage::Request, operation: "serialize-request" })); }
    let charge = budget.reserve_owned(count.length).map_err(|error| at(Stage::Request, error))?;
    let mut data = Vec::new();
    data.try_reserve_exact(count.length).map_err(|_| Failure::Allocation)?;
    let extra = if data.capacity() > count.length { Some(budget.reserve_owned(data.capacity() - count.length).map_err(|error| at(Stage::Request, error))?) } else { None };
    let mut output = BodyWriter { data: Some(&mut data), length: 0, maximum: count.length, budget, error: None };
    if serde_json::to_writer(&mut output, &body).is_err() { return Err(output.error.unwrap_or(Failure::Type { stage: Stage::Request, operation: "serialize-request" })); }
    drop(output);
    Ok((data, charge, extra))
}
// The bytes `execute` sends for `prepared`.
#[cfg(test)]
pub(crate) fn request_bytes(prepared: &PreparedOllamaTurn, config: &CapturedOllamaConfig<'_>, limits: TransportLimits, compat: ir::Limits) -> Vec<u8> {
    let budget = Budget::new(compat, Arc::new(std::sync::atomic::AtomicBool::new(false))).expect("request budget");
    match request_body(prepared, config, limits, &budget) {
        Ok((bytes, ..)) => bytes,
        Err(_) => panic!("request body"),
    }
}

pub(super) fn endpoint(host: &str, limits: TransportLimits, budget: &Budget) -> Result<(Url, Charge), Failure> {
    if host.len() > limits.max_url_bytes { return Err(Failure::Limit("url-bytes")); }
    // URL parsing and the HTTP engine have opaque workspace. This reservation
    // covers our bounded normalization strings; it is not an RSS guarantee.
    let capacity = host.len().checked_mul(8).and_then(|value| value.checked_add(1024)).ok_or(Failure::Limit("url-bytes"))?;
    let charge = budget.reserve_owned(capacity).map_err(|error| at(Stage::Request, error))?;
    budget.work(host.len()).map_err(|error| at(Stage::Request, error))?;
    let explicit = host.contains("://") || host.starts_with(':');
    let input = if host.is_empty() { "http://127.0.0.1:11434".to_owned() }
        else if host.starts_with(':') { format!("http://127.0.0.1{host}") }
        else if !explicit { format!("http://{host}") } else { host.to_owned() };
    let mut url = Url::parse(&input).map_err(|_| Failure::InvalidHost)?;
    if !matches!(url.scheme(), "http" | "https") { return Err(Failure::InvalidHost); }
    if !url.username().is_empty() { return Err(Failure::Userinfo); }
    url.set_password(None).map_err(|_| Failure::InvalidHost)?;
    let port = url.port().unwrap_or(if !explicit && !host.is_empty() { 11434 } else if url.scheme() == "https" { 443 } else { 80 });
    url.set_port(Some(port)).map_err(|_| Failure::InvalidHost)?;
    url.set_query(None); url.set_fragment(None);
    let mut path = url.path().to_owned();
    if path.ends_with('/') { path.pop(); }
    path.push_str("/api/chat");
    url.set_path(&path);
    if url.as_str().len() > limits.max_url_bytes { return Err(Failure::Limit("url-bytes")); }
    Ok((url, charge))
}
fn user_agent(value: &str, maximum: usize) -> Result<HeaderValue, Failure> {
    if value.len() > maximum { return Err(Failure::Limit("user-agent-bytes")); }
    // Actual Chromium wire uses the raw browser UA, not ollama-js's attempted
    // browser-controlled header. Other WebViews require the same evidence gate.
    HeaderValue::from_str(value).map_err(|_| Failure::Type { stage: Stage::Request, operation: "header-value" })
}
struct HttpSource { response: Response, pending: Option<Chunk>, received: u64, maximum: u64 }
#[async_trait]
impl ChunkSource for HttpSource {
    async fn pull(&mut self, maximum: usize, cancel: &Cancellation, budget: &Budget) -> Result<Option<Chunk>, DecodeError> {
        if let Some(pending) = &mut self.pending {
            if pending.len() != 0 { return Ok(Some(pending.take(maximum))); }
            self.pending = None;
        }
        let bytes = tokio::select! { biased;
            _ = cancel.cancelled() => return Err(DecodeError::Cancelled),
            bytes = self.response.chunk() => bytes.map_err(|_| DecodeError::Reader("HTTP body read failed".to_owned()))?,
        };
        let Some(bytes) = bytes else { return Ok(None) };
        self.received = self.received.checked_add(bytes.len() as u64).filter(|value| *value <= self.maximum).ok_or(DecodeError::Limit("encoded-bytes"))?;
        // The borrowed HTTP frame remains live during this charged copy. Its
        // length reservation accounts overlap, without claiming private capacity.
        let transient = budget.reserve_owned(bytes.len())?;
        let mut pending = Chunk::copy(&bytes, budget)?;
        drop(bytes); drop(transient);
        let result = pending.take(maximum);
        self.pending = Some(pending);
        Ok(Some(result))
    }
}
fn pipeline(response: Response, limits: TransportLimits, budget: &Budget) -> Result<Pipeline, Failure> {
    let chain = decode::encoding_chain(response.headers(), limits.decode, budget)?;
    struct ChargedSource { source: HttpSource, _charge: Charge }
    let source_charge = budget.reserve_owned(size_of::<ChargedSource>()).map_err(|error| at(Stage::Headers, error))?;
    #[async_trait]
    impl ChunkSource for ChargedSource {
        async fn pull(&mut self, maximum: usize, cancel: &Cancellation, budget: &Budget) -> Result<Option<Chunk>, DecodeError> { self.source.pull(maximum, cancel, budget).await }
    }
    Ok(Pipeline::new(Box::new(ChargedSource { source: HttpSource { response, pending: None, received: 0, maximum: limits.decode.max_encoded_bytes }, _charge: source_charge }), chain, limits.decode, budget.clone())?)
}
async fn read_text(mut pipeline: Pipeline, maximum: usize, cancel: &Cancellation, budget: &Budget) -> Result<Text, Failure> {
    let mut decoder = Utf8Decoder::new();
    let mut output = text("", budget, Stage::Body)?;
    let mut length = 0usize;
    loop {
        let part = pipeline.pull(cancel).await?;
        let bytes = part.as_ref().map_or(&[][..], Chunk::bytes);
        let decoded = decoder.decode(bytes, part.is_none(), budget).map_err(|error| at(Stage::Body, error))?;
        let unicode = decoded.as_unicode().expect("network decoder emits Unicode");
        length = length.checked_add(unicode.len()).filter(|length| *length <= maximum).ok_or(Failure::Limit("buffered-body-bytes"))?;
        output.append(&decoded).map_err(|error| at(Stage::Body, error))?;
        if part.is_none() { return Ok(output); }
    }
}
fn latin1(bytes: &[u8], budget: &Budget) -> Result<Text, Failure> {
    let mut output = text("", budget, Stage::Headers)?;
    for byte in bytes {
        budget.work(1).map_err(|error| at(Stage::Headers, error))?;
        let mut storage = [0; 4];
        output.append(&text(char::from(*byte).encode_utf8(&mut storage), budget, Stage::Headers)?)
            .map_err(|error| at(Stage::Headers, error))?;
    }
    Ok(output)
}
struct ResponseMetadata { status: u16, reason: Text, json: bool }
fn response_metadata(response: &Response, limits: TransportLimits, budget: &Budget) -> Result<ResponseMetadata, Failure> {
    let mut bytes = 0usize;
    for (name, value) in response.headers() {
        bytes = bytes.checked_add(name.as_str().len()).and_then(|length| length.checked_add(value.as_bytes().len())).ok_or(Failure::Limit("response-header-bytes"))?;
        if bytes > limits.decode.max_header_bytes { return Err(Failure::Limit("response-header-bytes")); }
    }
    budget.work(bytes).map_err(|error| at(Stage::Headers, error))?;
    let reason = if matches!(response.version(), Version::HTTP_2 | Version::HTTP_3) {
        text("", budget, Stage::Headers)?
    } else if let Some(reason) = response.extensions().get::<hyper::ext::ReasonPhrase>() {
        if reason.as_bytes().len() > limits.decode.max_header_bytes { return Err(Failure::Limit("status-text-bytes")); }
        latin1(reason.as_bytes(), budget)?
    } else { text(response.status().canonical_reason().unwrap_or(""), budget, Stage::Headers)? };
    let json = response.headers().get_all(CONTENT_TYPE).iter().any(|value|
        value.as_bytes().windows(b"application/json".len()).any(|part| part == b"application/json"));
    Ok(ResponseMetadata { status: response.status().as_u16(), reason, json })
}
fn fallback_message(metadata: &ResponseMetadata, budget: &Budget) -> Result<Text, Failure> {
    let mut output = text(&format!("Error {}: ", metadata.status), budget, Stage::Headers)?;
    output.append(&metadata.reason).map_err(|error| at(Stage::Headers, error))?;
    Ok(output)
}
fn caught_body_failure(failure: &Failure) -> Option<BodyFailure> {
    match failure {
        Failure::Decode(DecodeError::Reader(_)) | Failure::Network { stage: Stage::Body } => Some(BodyFailure::Reader),
        Failure::Decode(DecodeError::Codec { .. } | DecodeError::NoProgress(_)) => Some(BodyFailure::Codec),
        Failure::Compat { error: CompatError::Syntax { .. }, .. } => Some(BodyFailure::Json),
        Failure::Type { operation: "http-error-property", .. } => Some(BodyFailure::Property),
        _ => None,
    }
}
async fn http_failure(body: Pipeline, metadata: ResponseMetadata, limits: TransportLimits, cancel: &Cancellation, budget: &Budget) -> Result<Failure, Failure> {
    let fallback = fallback_message(&metadata, budget)?;
    let read = read_text(body, limits.max_buffered_bytes, cancel, budget).await;
    let chosen = match read {
        Ok(value) if metadata.json => {
            let unicode = value.as_unicode().expect("network decoder emits Unicode");
            match ir::parse_json(unicode, budget) {
                Ok(Value::Null) => Err(Failure::Type { stage: Stage::Headers, operation: "http-error-property" }),
                Ok(mut value) => Ok(value.take(b"error", budget).map_err(|error| at(Stage::Headers, error))?.filter(Value::truthy)),
                Err(error) => Err(at(Stage::BufferedJson, error)),
            }
        }
        Ok(value) => Ok((value.len_units() != 0).then_some(Value::Text(value))),
        Err(error) => Err(error),
    };
    let (error, body_failure) = match chosen {
        Ok(Some(error)) => (error, None),
        Ok(None) => (Value::Text(fallback), None),
        Err(error) => {
            let Some(cause) = caught_body_failure(&error) else { return Err(error) };
            // checkOk catches body/JSON failures but still throws ResponseError.
            // A resource or cancellation failure never enters that fallback.
            (Value::Text(fallback), Some(cause))
        }
    };
    let message = error.to_text(budget).map_err(|error| at(Stage::Headers, error))?;
    Ok(Failure::Http { status_code: metadata.status, error, message, body_failure })
}
fn nullish(value: Option<Value>, budget: &Budget, stage: Stage) -> Result<Value, Failure> {
    match value { None | Some(Value::Null) => empty(budget, stage), Some(value) => Ok(value) }
}
fn message_field<'a>(message: Option<&'a Value>, key: &[u8], budget: &Budget) -> Result<Option<&'a Value>, Failure> {
    match message { None | Some(Value::Null) => Ok(None), Some(value) => value.get(key, budget).map_err(|error| at(Stage::Transform, error)) }
}
fn format_thinking(thinking: Value, content: Value, budget: &Budget) -> Result<Value, Failure> {
    if !thinking.truthy() { return Ok(content); }
    let mut output = text("<Thoughts>\n", budget, Stage::Transform)?;
    output.append(&thinking.to_text(budget).map_err(|error| at(Stage::Transform, error))?).map_err(|error| at(Stage::Transform, error))?;
    output.append(&text("\n</Thoughts>\n\n", budget, Stage::Transform)?).map_err(|error| at(Stage::Transform, error))?;
    output.append(&content.to_text(budget).map_err(|error| at(Stage::Transform, error))?).map_err(|error| at(Stage::Transform, error))?;
    Ok(Value::Text(output))
}
fn markers(prepared: &PreparedOllamaTurn, budget: &Budget, mut visit: impl FnMut(Text) -> Result<(), Failure>) -> Result<(), Failure> {
    for marker in ["system note:", "system:", "system note：", "system："] { visit(text(marker, budget, Stage::Transform)?)?; }
    for name in std::iter::once(prepared.character_name.as_str()).chain(std::iter::once(prepared.user_name.as_str()))
        .chain(prepared.messages.iter().filter_map(|message| message.name.as_deref())) {
        budget.work(1).map_err(|error| at(Stage::Transform, error))?;
        if name.is_empty() { continue; }
        for suffix in [":", "：", ": ", "： "] {
            let mut marker = text(name, budget, Stage::Transform)?;
            marker.append(&text(suffix, budget, Stage::Transform)?).map_err(|error| at(Stage::Transform, error))?;
            visit(marker)?;
        }
    }
    Ok(())
}
fn unstringlize(value: Value, prepared: &PreparedOllamaTurn, budget: &Budget) -> Result<Value, Failure> {
    match value {
        Value::Text(value) => {
            let mut earliest = None;
            markers(prepared, budget, |marker| {
                if let Some(index) = value.index_of(&marker, budget).map_err(|error| at(Stage::Transform, error))? {
                    earliest = Some(earliest.map_or(index, |previous: usize| previous.min(index)));
                }
                Ok(())
            })?;
            if let Some(index) = earliest {
                let prefix = value.prefix(index, budget).map_err(|error| at(Stage::Transform, error))?;
                Ok(Value::Text(prefix.trim(budget).map_err(|error| at(Stage::Transform, error))?))
            } else { Ok(Value::Text(value)) }
        }
        value @ Value::Array(_) => {
            let values = value.array().expect("array branch");
            markers(prepared, budget, |marker| {
                for value in values {
                    budget.work(1).map_err(|error| at(Stage::Transform, error))?;
                    if let Value::Text(value) = value {
                        budget.work(value.len_units().min(marker.len_units())).map_err(|error| at(Stage::Transform, error))?;
                        if value.equals(&marker) { return Err(Failure::Type { stage: Stage::Transform, operation: "substring" }); }
                    }
                }
                Ok(())
            })?;
            Ok(value)
        }
        _ => Err(Failure::Type { stage: Stage::Transform, operation: "indexOf" }),
    }
}
fn buffered_value(mut response: Value, prepared: &PreparedOllamaTurn, budget: &Budget) -> Result<Value, Failure> {
    if matches!(response, Value::Null) { return Err(Failure::Type { stage: Stage::Transform, operation: "message-get" }); }
    let mut message = response.take(b"message", budget).map_err(|error| at(Stage::Transform, error))?.unwrap_or(Value::Null);
    let thinking = nullish(message.take(b"thinking", budget).map_err(|error| at(Stage::Transform, error))?, budget, Stage::Transform)?;
    let content = nullish(message.take(b"content", budget).map_err(|error| at(Stage::Transform, error))?, budget, Stage::Transform)?;
    unstringlize(format_thinking(thinking, content, budget)?, prepared, budget)
}

struct StreamState { decoder: Utf8Decoder, line: Text, line_bytes: usize, thinking: Text, content: Text }
impl StreamState {
    fn new(budget: &Budget) -> Result<Self, Failure> {
        Ok(Self { decoder: Utf8Decoder::new(), line: text("", budget, Stage::StreamJson)?, line_bytes: 0,
            thinking: text("", budget, Stage::Transform)?, content: text("", budget, Stage::Transform)? })
    }
    fn append_line(&mut self, part: &str, limits: TransportLimits, budget: &Budget) -> Result<(), Failure> {
        self.line_bytes = self.line_bytes.checked_add(part.len()).filter(|length| *length <= limits.max_line_bytes).ok_or(Failure::Limit("ndjson-line-bytes"))?;
        self.line.append(&text(part, budget, Stage::StreamJson)?).map_err(|error| at(Stage::StreamJson, error))
    }
    fn frame(&mut self, budget: &Budget) -> Result<Option<(Value, bool)>, Failure> {
        let line = std::mem::replace(&mut self.line, text("", budget, Stage::StreamJson)?);
        self.line_bytes = 0;
        let value = match ir::parse_json(line.as_unicode().expect("network line is Unicode"), budget) {
            Ok(value) => value,
            Err(CompatError::Syntax { .. }) => return Ok(None),
            Err(error) => return Err(at(Stage::StreamJson, error)),
        };
        if !value.is_object() { return Err(Failure::Type { stage: Stage::SdkIterator, operation: "error-in-message" }); }
        if let Some(error) = value.get(b"error", budget).map_err(|error| at(Stage::SdkIterator, error))? {
            return Err(Failure::Sdk { stage: Stage::SdkIterator, message: error.to_text(budget).map_err(|error| at(Stage::SdkIterator, error))? });
        }
        let message = value.get(b"message", budget).map_err(|error| at(Stage::Transform, error))?;
        if let Some(thinking) = message_field(message, b"thinking", budget)?.filter(|value| !matches!(value, Value::Null)) {
            self.thinking.append(&thinking.to_text(budget).map_err(|error| at(Stage::Transform, error))?).map_err(|error| at(Stage::Transform, error))?;
        }
        if let Some(content) = message_field(message, b"content", budget)?.filter(|value| !matches!(value, Value::Null)) {
            self.content.append(&content.to_text(budget).map_err(|error| at(Stage::Transform, error))?).map_err(|error| at(Stage::Transform, error))?;
        }
        let done = value.get(b"done", budget).map_err(|error| at(Stage::SdkIterator, error))?.is_some_and(Value::truthy);
        let success = match value.get(b"status", budget).map_err(|error| at(Stage::SdkIterator, error))? {
            Some(Value::Text(status)) => { budget.work(7).map_err(|error| at(Stage::SdkIterator, error))?; status.equals_ascii(b"success") }
            _ => false,
        };
        let output = if self.thinking.len_units() == 0 {
            Value::Text(self.content.copy(budget).map_err(|error| at(Stage::Transform, error))?)
        } else {
            let mut output = text("<Thoughts>\n", budget, Stage::Transform)?;
            output.append(&self.thinking).map_err(|error| at(Stage::Transform, error))?;
            output.append(&text("\n</Thoughts>\n\n", budget, Stage::Transform)?).map_err(|error| at(Stage::Transform, error))?;
            output.append(&self.content).map_err(|error| at(Stage::Transform, error))?;
            Value::Text(output)
        };
        Ok(Some((output, done || success)))
    }
    async fn emit(&mut self, body: &mut Option<Pipeline>, delivery: &mut Delivery<'_>, limits: TransportLimits) -> Result<bool, Failure> {
        let Some((value, terminal)) = self.frame(delivery.budget)? else { return Ok(false) };
        if terminal { drop(body.take()); }
        delivery.publish(value, limits).await?;
        Ok(terminal)
    }
}
async fn stream(body: Pipeline, delivery: &mut Delivery<'_>, limits: TransportLimits) -> Result<(), Failure> {
    let mut body = Some(body);
    let mut state = StreamState::new(delivery.budget)?;
    loop {
        let part = body.as_mut().expect("body retained until terminal").pull(delivery.cancel).await?;
        let eof = part.is_none();
        let decoded = state.decoder.decode(part.as_ref().map_or(&[][..], Chunk::bytes), eof, delivery.budget).map_err(|error| at(Stage::Body, error))?;
        for segment in decoded.as_unicode().expect("network decoder emits Unicode").split_inclusive('\n') {
            let newline = segment.ends_with('\n');
            state.append_line(if newline { &segment[..segment.len() - 1] } else { segment }, limits, delivery.budget)?;
            if newline && state.emit(&mut body, delivery, limits).await? { return Ok(()); }
        }
        if eof {
            if state.line_bytes != 0 && state.emit(&mut body, delivery, limits).await? { return Ok(()); }
            return Err(Failure::Sdk { stage: Stage::SdkIterator,
                message: text("Did not receive done or success response in stream.", delivery.budget, Stage::SdkIterator)? });
        }
    }
}

async fn execute(prepared: &PreparedOllamaTurn, config: &CapturedOllamaConfig<'_>, limits: TransportLimits, delivery: &mut Delivery<'_>) -> Result<(), Failure> {
    limits.validate()?;
    delivery.budget.check().map_err(|error| at(Stage::Request, error))?;
    let (url, _url_charge) = endpoint(config.host, limits, delivery.budget)?;
    if config.browser_user_agent.len() > limits.max_user_agent_bytes { return Err(Failure::Limit("user-agent-bytes")); }
    let _agent_charge = delivery.budget.reserve_owned(config.browser_user_agent.len()).map_err(|error| at(Stage::Request, error))?;
    let agent = user_agent(config.browser_user_agent, limits.max_user_agent_bytes)?;
    let (bytes, _body_charge, _extra_body_charge) = request_body(prepared, config, limits, delivery.budget)?;
    #[cfg(all(test, target_os = "windows"))]
    crate::ollama_ui_verification::adapters::observe(crate::ollama_ui_verification::controller::Kind::RequestFormatted);
    let redirect = reqwest::redirect::Policy::custom(|attempt| {
        if !matches!(attempt.url().scheme(), "http" | "https") || !attempt.url().username().is_empty() || attempt.url().password().is_some() {
            attempt.error("Redirect URL is not fetchable")
        } else { reqwest::redirect::Policy::limited(20).redirect(attempt) }
    });
    let client = crate::platform_tls::client_builder().no_gzip().no_brotli().no_deflate().no_zstd()
        .retry(reqwest::retry::never()).redirect(redirect).build().map_err(|_| Failure::Network { stage: Stage::Request })?;
    let request = client.post(url).header(CONTENT_TYPE, "application/json").header(ACCEPT, "application/json")
        .header(USER_AGENT, agent).header(ACCEPT_ENCODING, "gzip, deflate, br, zstd").body(bytes);
    let response = tokio::select! { biased;
        _ = delivery.cancel.cancelled() => return Err(Failure::Cancelled),
        response = request.send() => response.map_err(|_| Failure::Network { stage: Stage::Headers })?,
    };
    let metadata = response_metadata(&response, limits, delivery.budget)?;
    let body = pipeline(response, limits, delivery.budget)?;
    if !(200..300).contains(&metadata.status) {
        return Err(http_failure(body, metadata, limits, delivery.cancel, delivery.budget).await?);
    }
    if config.streaming { return stream(body, delivery, limits).await; }
    let decoded = read_text(body, limits.max_buffered_bytes, delivery.cancel, delivery.budget).await?;
    #[cfg(all(test, target_os = "windows"))]
    crate::ollama_ui_verification::adapters::observe(crate::ollama_ui_verification::controller::Kind::BufferDecoded);
    let value = ir::parse_json(decoded.as_unicode().expect("network body is Unicode"), delivery.budget).map_err(|error| at(Stage::BufferedJson, error))?;
    #[cfg(all(test, target_os = "windows"))]
    crate::ollama_ui_verification::adapters::observe(crate::ollama_ui_verification::controller::Kind::JsonParsed);
    drop(decoded);
    let value = buffered_value(value, prepared, delivery.budget)?;
    #[cfg(all(test, target_os = "windows"))]
    crate::ollama_ui_verification::adapters::observe(crate::ollama_ui_verification::controller::Kind::ResponseTransformed);
    delivery.publish(value, limits).await
}

pub(crate) struct TransportContext { budget: Budget, cancel: Cancellation }
impl TransportContext {
    pub fn new(limits: ir::Limits, cancel: Cancellation) -> Result<Self, Failure> {
        let budget = Budget::new(limits, cancel.flag()).map_err(|error| at(Stage::Request, error))?;
        Ok(Self { budget, cancel })
    }
}
pub(crate) async fn generate(prepared: &PreparedOllamaTurn, config: &CapturedOllamaConfig<'_>, limits: TransportLimits,
    context: &TransportContext, sink: &mut dyn OutputSink) -> Outcome {
    let cancel = &context.cancel;
    let mut delivery = Delivery { sink, cancel, budget: &context.budget, last: None, in_flight: None, next: 1 };
    let result = execute(prepared, config, limits, &mut delivery).await;
    let settlement = match result {
        Ok(()) => Settlement::Normal,
        Err(Failure::Cancelled) => Settlement::Cancelled(cancel.origin()),
        Err(Failure::ConsumerCancelled) => Settlement::ConsumerCancelled,
        Err(failure) => Settlement::Failed(failure),
    };
    Outcome { settlement, last_accepted: delivery.last, in_flight: delivery.in_flight }
}
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::ollama_decode::{tests as wire, Coding};
    use std::sync::{atomic::{AtomicBool, Ordering}, Arc};
    use tokio::sync::Notify;

    fn limits() -> TransportLimits {
        TransportLimits { decode: wire::limits(), max_request_bytes: 65536, max_url_bytes: 4096, max_user_agent_bytes: 4096,
            max_buffered_bytes: 65536, max_line_bytes: 65536, max_output_bytes: 65536, max_output_value_units: 65536 }
    }
    fn prepared() -> PreparedOllamaTurn {
        PreparedOllamaTurn { messages: Vec::new(), input_tokens: 0, output_tokens: 1, max_context: 4096, last_memory: None,
            character_name: "Synthetic".into(), user_name: "Earlier".into() }
    }
    fn parse(input: &str, budget: &Budget) -> Value { ir::parse_json(input, budget).unwrap() }
    fn string(value: &Value) -> Option<&str> { match value { Value::Text(value) => value.as_unicode(), _ => None } }

    #[test]
    fn buffered_shape_coercion_and_earliest_speaker_match_the_actual_adapter() {
        let cancel = Cancellation::new(); let budget = wire::budget(&cancel); let prepared = prepared();
        let value = buffered_value(parse(r#"{"message":{"content":" kept Earlier: first system: later "}}"#, &budget), &prepared, &budget).unwrap();
        assert_eq!(string(&value), Some("kept"));
        let value = buffered_value(parse(r#"{"message":{"content":[1,null]}}"#, &budget), &prepared, &budget).unwrap();
        assert_eq!(value.array().unwrap().len(), 2);
        assert!(matches!(buffered_value(parse(r#"{"message":{"content":["system:"]}}"#, &budget), &prepared, &budget), Err(Failure::Type { operation: "substring", .. })));
        assert!(matches!(buffered_value(parse(r#"{"message":{"content":42}}"#, &budget), &prepared, &budget), Err(Failure::Type { operation: "indexOf", .. })));
        let value = buffered_value(parse(r#"{"message":{"thinking":[],"content":false}}"#, &budget), &prepared, &budget).unwrap();
        assert_eq!(string(&value), Some("<Thoughts>\n\n</Thoughts>\n\nfalse"));
        assert!(matches!(buffered_value(Value::Null, &prepared, &budget), Err(Failure::Type { operation: "message-get", .. })));
    }

    #[test]
    fn streamed_presence_primitive_and_surrogate_rules_remain_distinct() {
        let cancel = Cancellation::new(); let budget = wire::budget(&cancel); let mut state = StreamState::new(&budget).unwrap();
        state.append_line(r#"{"error":0,"done":true}"#, limits(), &budget).unwrap();
        assert!(matches!(state.frame(&budget), Err(Failure::Sdk { message, .. }) if message.as_unicode() == Some("0")));
        state.append_line("null", limits(), &budget).unwrap();
        assert!(matches!(state.frame(&budget), Err(Failure::Type { operation: "error-in-message", .. })));
        state.append_line(r#"{"message":{"content":"\ud800"}}"#, limits(), &budget).unwrap();
        let (first, done) = state.frame(&budget).unwrap().unwrap(); assert!(!done);
        assert!(matches!(first, Value::Text(value) if value.units().eq([0xd800])));
        state.append_line(r#"{"message":{"content":"\udc00"},"done":[]}"#, limits(), &budget).unwrap();
        let (second, done) = state.frame(&budget).unwrap().unwrap(); assert!(done);
        assert_eq!(string(&second), Some("\u{10000}"));
        state.append_line("malformed", limits(), &budget).unwrap(); assert!(state.frame(&budget).unwrap().is_none());
        // The pending-line cap spans fragments of one line, then restarts with the next line.
        let mut line = limits(); line.max_line_bytes = 4;
        state.append_line("{}", line, &budget).unwrap(); state.append_line("  ", line, &budget).unwrap();
        assert!(matches!(state.append_line(" ", line, &budget), Err(Failure::Limit("ndjson-line-bytes"))));
        let mut state = StreamState::new(&budget).unwrap();
        state.append_line("{}  ", line, &budget).unwrap();
        assert!(state.frame(&budget).unwrap().is_some());
        state.append_line("null", line, &budget).unwrap();
    }

    enum Gate { Accept, CreditHold, CreditFailure, OfferHold, Unknown }
    struct Sink { gate: Gate, entered: Arc<Notify>, accepted: Arc<AtomicBool> }
    #[async_trait]
    impl OutputSink for Sink {
        async fn credit(&mut self) -> Result<(), ConsumerStop> {
            match self.gate {
                Gate::CreditFailure => Err(ConsumerStop::Failed),
                Gate::CreditHold => { self.entered.notify_one(); std::future::pending().await }
                _ => Ok(()),
            }
        }
        async fn offer(&mut self, _: Arc<Snapshot>) -> OfferResult {
            match self.gate {
                Gate::OfferHold => { self.entered.notify_one(); std::future::pending().await }
                Gate::Unknown => OfferResult::Unknown,
                _ => { self.accepted.store(true, Ordering::Release); OfferResult::Accepted }
            }
        }
    }
    #[tokio::test]
    async fn terminal_releases_body_before_credit_and_cancellation_retains_only_offered_uncertainty() {
        let codecs = [("gzip", Coding::Gzip), ("deflate", Coding::Deflate), ("br", Coding::Brotli), ("zstd", Coding::Zstd), ("gzip, br", Coding::Gzip)];
        for (credit_hold, (header, coding)) in [true, false].into_iter().flat_map(|credit| codecs.map(|codec| (credit, codec))) {
            let cancel = Cancellation::new(); let budget = wire::budget(&cancel); let entered = Arc::new(Notify::new());
            let gate = if credit_hold { Gate::CreditHold } else { Gate::OfferHold };
            let mut sink = Sink { gate, entered: entered.clone(), accepted: Arc::new(AtomicBool::new(false)) };
            let mut encoded = wire::encode(coding, b"{\"message\":{\"content\":\"terminal\"},\"done\":true}\n");
            if header == "gzip, br" { encoded = wire::encode(Coding::Brotli, &encoded); }
            let (body, closed, _) = wire::pipeline(vec![wire::Action::Data(encoded), wire::Action::Hold(Arc::new(Notify::new()))], header, wire::limits(), &budget);
            let mut delivery = Delivery { sink: &mut sink, cancel: &cancel, budget: &budget, last: None, in_flight: None, next: 1 };
            {
                let pending = stream(body, &mut delivery, limits()); tokio::pin!(pending);
                tokio::select! {
                    result = &mut pending => panic!("held delivery settled early: {result:?}"),
                    _ = entered.notified() => (),
                }
                assert!(closed.load(Ordering::Acquire), "{header}: terminal body must be dropped before waiting for credit or offer acknowledgement");
                cancel.cancel();
                assert!(matches!(pending.await, Err(Failure::Cancelled)));
            }
            assert!(delivery.last.is_none());
            assert_eq!(delivery.in_flight.as_ref().map(|value| value.sequence), if credit_hold { None } else { Some(1) });
        }
    }
    #[tokio::test]
    async fn terminal_normal_requires_acceptance_and_sink_failure_never_claims_it() {
        for gate in [Gate::Accept, Gate::CreditFailure, Gate::Unknown] {
            let normal = matches!(gate, Gate::Accept); let unknown = matches!(gate, Gate::Unknown);
            let cancel = Cancellation::new(); let budget = wire::budget(&cancel);
            let mut sink = Sink { gate, entered: Arc::new(Notify::new()), accepted: Arc::new(AtomicBool::new(false)) };
            let (body, closed, _) = wire::pipeline(vec![wire::Action::Data(b"{\"done\":true}\n".to_vec()), wire::Action::Error], "", wire::limits(), &budget);
            let mut delivery = Delivery { sink: &mut sink, cancel: &cancel, budget: &budget, last: None, in_flight: None, next: 1 };
            let result = stream(body, &mut delivery, limits()).await;
            assert_eq!(result.is_ok(), normal); assert!(closed.load(Ordering::Acquire));
            assert_eq!(delivery.last.is_some(), normal); assert_eq!(delivery.in_flight.is_some(), unknown);
            if unknown { assert!(matches!(result, Err(Failure::DeliveryUnknown))); }
            else if !normal { assert!(matches!(result, Err(Failure::Consumer { stage: Stage::Credit }))); }
            cancel.cancel();
            assert_eq!(delivery.last.is_some(), normal, "a later event cannot rewrite settled acceptance");
        }
    }
    #[test]
    fn context_construction_ties_cpu_and_io_cancellation_to_one_owner_handle() {
        let cancel = Cancellation::new();
        let context = TransportContext::new(ir::Limits { max_owned_bytes: 4096, max_json_bytes: 1024,
            max_text_units: 1024, max_values: 64, max_depth: 8, max_work: 4096 }, cancel.clone()).unwrap();
        cancel.cancel_as(decode::CancelOrigin::Owner);
        assert!(context.cancel.is_cancelled());
        assert_eq!(context.cancel.origin(), decode::CancelOrigin::Owner);
        assert_eq!(context.budget.work(1).unwrap_err(), CompatError::Cancelled);
    }

    #[tokio::test]
    async fn previous_acceptance_survives_later_failure_and_simultaneously_ready_cancel() {
        use std::sync::atomic::AtomicU8;
        struct SelectiveSink { mode: Arc<AtomicU8>, entered: Arc<Notify>, release: Arc<Notify> }
        #[async_trait]
        impl OutputSink for SelectiveSink {
            async fn credit(&mut self) -> Result<(), ConsumerStop> {
                match self.mode.load(Ordering::Acquire) {
                    1 => Err(ConsumerStop::Failed),
                    2 => { self.entered.notify_one(); self.release.notified().await; Ok(()) }
                    _ => Ok(()),
                }
            }
            async fn offer(&mut self, _: Arc<Snapshot>) -> OfferResult {
                if self.mode.load(Ordering::Acquire) == 3 { self.entered.notify_one(); self.release.notified().await; }
                OfferResult::Accepted
            }
        }
        for mode in [1, 2, 3] {
            let cancel = Cancellation::new(); let budget = wire::budget(&cancel);
            let selected = Arc::new(AtomicU8::new(0)); let entered = Arc::new(Notify::new()); let release = Arc::new(Notify::new());
            let mut sink = SelectiveSink { mode: selected.clone(), entered: entered.clone(), release: release.clone() };
            let mut delivery = Delivery { sink: &mut sink, cancel: &cancel, budget: &budget, last: None, in_flight: None, next: 1 };
            delivery.publish(Value::Text(text("accepted", &budget, Stage::Transform).unwrap()), limits()).await.unwrap();
            selected.store(mode, Ordering::Release);
            let result;
            {
                let pending = delivery.publish(Value::Text(text("pending", &budget, Stage::Transform).unwrap()), limits()); tokio::pin!(pending);
                if mode == 1 { result = pending.await; }
                else {
                    tokio::select! { value = &mut pending => panic!("delivery unexpectedly settled: {value:?}"), _ = entered.notified() => () }
                    // Both branches are ready before polling again. Cancellation
                    // wins; an offered receipt remains uncertain, never replayed.
                    cancel.cancel(); release.notify_one();
                    result = pending.await;
                }
            }
            assert!(result.is_err());
            if mode == 1 { assert!(matches!(result, Err(Failure::Consumer { stage: Stage::Credit }))); }
            else { assert!(matches!(result, Err(Failure::Cancelled))); }
            assert_eq!(delivery.last.as_ref().unwrap().sequence, 1);
            assert_eq!(string(&delivery.last.as_ref().unwrap().value), Some("accepted"));
            assert_eq!(delivery.in_flight.as_ref().map(|value| value.sequence), if mode == 3 { Some(2) } else { None });
        }
    }
    #[test]
    fn public_value_units_count_nodes_keys_and_isolated_utf16_without_coercion() {
        let cancel = Cancellation::new(); let budget = wire::budget(&cancel);
        for (input, exact) in [(r#"[1,{"k":"\ud800"}]"#, 7), (r#"{"\ud800":[false]}"#, 5), (r#""\ud800""#, 2), ("[1,null]", 3)] {
            let value = parse(input, &budget);
            assert_eq!(output_value_units(&value, exact, &budget).unwrap(), exact);
            assert!(matches!(output_value_units(&value, exact - 1, &budget), Err(Failure::Limit("output-value-units"))));
            if let Value::Text(value) = &value { assert!(value.units().eq([0xd800])); assert!(value.as_unicode().is_none()); }
        }
    }
    #[tokio::test]
    async fn publication_units_apply_to_arrays_and_text_without_becoming_a_cumulative_counter() {
        let cancel = Cancellation::new(); let budget = wire::budget(&cancel);
        let mut sink = Sink { gate: Gate::Accept, entered: Arc::new(Notify::new()), accepted: Arc::new(AtomicBool::new(false)) };
        let mut delivery = Delivery { sink: &mut sink, cancel: &cancel, budget: &budget, last: None, in_flight: None, next: 1 };
        let mut tiny = limits(); tiny.max_output_bytes = 1; tiny.max_output_value_units = 2;
        for number in ["[1]", "[2]"] { delivery.publish(parse(number, &budget), tiny).await.unwrap(); }
        assert_eq!(delivery.last.as_ref().unwrap().sequence, 2);
        assert!(delivery.last.as_ref().unwrap().value.array().is_some());
        assert!(matches!(delivery.publish(parse("[1,null]", &budget), tiny).await, Err(Failure::Limit("output-value-units"))));
        assert_eq!(delivery.last.as_ref().unwrap().sequence, 2); assert!(delivery.in_flight.is_none());
        tiny.max_output_value_units = 3;
        delivery.publish(parse("[1,null]", &budget), tiny).await.unwrap();
        assert_eq!(delivery.last.as_ref().unwrap().value.array().unwrap().len(), 2);
        assert_eq!(delivery.last.as_ref().unwrap().sequence, 3);
        tiny.max_output_bytes = 3; tiny.max_output_value_units = 2;
        delivery.publish(parse(r#""\ud800""#, &budget), tiny).await.unwrap();
        assert!(matches!(&delivery.last.as_ref().unwrap().value, Value::Text(value) if value.units().eq([0xd800])));
        tiny.max_output_bytes = 2;
        assert!(matches!(delivery.publish(parse(r#""\ud800""#, &budget), tiny).await, Err(Failure::Limit("formatted-output-bytes"))));
        assert_eq!(delivery.last.as_ref().unwrap().sequence, 4);
    }

    #[test]
    fn endpoint_normalizes_sdk_hosts_and_rejects_unfetchable_urls_before_any_request() {
        let cancel = Cancellation::new(); let budget = wire::budget(&cancel);
        for (host, expected) in [
            ("", "http://127.0.0.1:11434/api/chat"),
            (":8080", "http://127.0.0.1:8080/api/chat"),
            ("example.test", "http://example.test:11434/api/chat"),
            ("example.test/base/", "http://example.test:11434/base/api/chat"),
            ("https://example.test", "https://example.test/api/chat"),
            ("http://example.test:8080/base//?query=1#fragment", "http://example.test:8080/base//api/chat"),
            // The SDK formats credentials only from a username, so a lone password is dropped.
            ("http://:secret@example.test", "http://example.test/api/chat"),
        ] {
            assert_eq!(endpoint(host, limits(), &budget).unwrap().0.as_str(), expected, "{host:?}");
        }
        assert!(matches!(endpoint("http://user:secret@example.test", limits(), &budget), Err(Failure::Userinfo)));
        assert!(matches!(endpoint("http://user@example.test", limits(), &budget), Err(Failure::Userinfo)));
        assert!(matches!(endpoint("ftp://example.test", limits(), &budget), Err(Failure::InvalidHost)));
        let mut exact = limits(); exact.max_url_bytes = "http://example.test:11434/api/chat".len();
        assert!(endpoint("example.test", exact, &budget).is_ok());
        exact.max_url_bytes -= 1;
        assert!(matches!(endpoint("example.test", exact, &budget), Err(Failure::Limit("url-bytes"))));
    }

    #[test]
    fn response_metadata_maps_reason_bytes_by_protocol_and_bounds_headers() {
        let cancel = Cancellation::new(); let budget = wire::budget(&cancel);
        let response = |version: Version, reason: Option<&[u8]>, content_type: &str| {
            let mut response = hyper::Response::builder().status(500).version(version).header(CONTENT_TYPE, content_type).body(Vec::<u8>::new()).unwrap();
            if let Some(reason) = reason { response.extensions_mut().insert(hyper::ext::ReasonPhrase::try_from(reason).unwrap()); }
            Response::from(response)
        };
        let units = |metadata: ResponseMetadata| metadata.reason.units().collect::<Vec<_>>();
        // HTTP/1 reason bytes become one code unit each; an empty phrase stays empty.
        let latin1 = response_metadata(&response(Version::HTTP_11, Some(b"Caf\xe9"), "application/json"), limits(), &budget).unwrap();
        assert!(latin1.json); assert_eq!(latin1.status, 500);
        assert_eq!(units(latin1), [0x43, 0x61, 0x66, 0xe9]);
        assert!(units(response_metadata(&response(Version::HTTP_11, Some(b""), "text/plain"), limits(), &budget).unwrap()).is_empty());
        let canonical = response_metadata(&response(Version::HTTP_11, None, "Application/JSON"), limits(), &budget).unwrap();
        assert!(!canonical.json);
        assert_eq!(canonical.reason.as_unicode(), Some("Internal Server Error"));
        // HTTP/2 and HTTP/3 carry no reason phrase, so none is synthesized or kept.
        for version in [Version::HTTP_2, Version::HTTP_3] {
            assert!(units(response_metadata(&response(version, Some(b"Synthetic status"), "application/json"), limits(), &budget).unwrap()).is_empty());
            assert!(units(response_metadata(&response(version, None, "application/json"), limits(), &budget).unwrap()).is_empty());
        }
        let headers = "content-type".len() + "application/json".len();
        let mut exact = limits(); exact.decode.max_header_bytes = headers;
        assert!(response_metadata(&response(Version::HTTP_11, None, "application/json"), exact, &budget).is_ok());
        exact.decode.max_header_bytes -= 1;
        assert!(matches!(response_metadata(&response(Version::HTTP_11, None, "application/json"), exact, &budget), Err(Failure::Limit("response-header-bytes"))));
    }

    #[tokio::test]
    async fn buffered_text_removes_bom_replaces_invalid_bytes_and_bounds_decoded_bytes() {
        // The UTF-8 BOM and an invalid byte straddle chunks; the cap counts decoded UTF-8 bytes.
        for (maximum, accepted) in ["ok\u{fffd}".len(), "ok\u{fffd}".len() - 1].into_iter().zip([true, false]) {
            let cancel = Cancellation::new(); let budget = wire::budget(&cancel);
            let actions = vec![wire::Action::Data(vec![0xef, 0xbb]), wire::Action::Data(vec![0xbf, b'o', b'k', 0xff])];
            let (body, closed, _) = wire::pipeline(actions, "", wire::limits(), &budget);
            let result = read_text(body, maximum, &cancel, &budget).await;
            assert!(closed.load(Ordering::Acquire));
            if accepted { assert_eq!(result.unwrap().as_unicode(), Some("ok\u{fffd}")); }
            else { assert!(matches!(result, Err(Failure::Limit("buffered-body-bytes"))), "{result:?}"); }
        }
    }

    #[tokio::test]
    async fn http_failure_follows_check_ok_and_propagates_only_resource_and_cancel_failures() {
        use wire::Action::{Data, Error as ReadError};
        const FALLBACK: &str = "Error 500: Synthetic status";
        // (body, coding, JSON content type, message, error is the fallback text, caught body failure)
        let cases: [(Vec<wire::Action>, &str, bool, &str, bool, Option<&str>); 11] = [
            (vec![Data(br#"{"error":"value"}"#.to_vec())], "", true, "value", false, None),
            (vec![Data(br#"{"error":42}"#.to_vec())], "", true, "42", false, None),
            (vec![Data(br#"{"error":false}"#.to_vec())], "", true, FALLBACK, true, None),
            (vec![], "", true, FALLBACK, true, Some("json")),
            (vec![Data(b"invalid".to_vec())], "", true, FALLBACK, true, Some("json")),
            (vec![Data(b"null".to_vec())], "", true, FALLBACK, true, Some("property")),
            (vec![Data(br#"{"error":"plain"}"#.to_vec())], "", false, r#"{"error":"plain"}"#, false, None),
            (vec![], "", false, FALLBACK, true, None),
            (vec![Data(vec![0xef, 0xbb, 0xbf, b'p', b'l', b'a', b'i', b'n', 0xff])], "", false, "plain\u{fffd}", false, None),
            (vec![Data(b"{".to_vec()), ReadError], "", true, FALLBACK, true, Some("reader")),
            (vec![Data(vec![0xff; 32])], "gzip", true, FALLBACK, true, Some("codec")),
        ];
        for (index, (actions, coding, json, message, fallback, caught)) in cases.into_iter().enumerate() {
            let cancel = Cancellation::new(); let budget = wire::budget(&cancel);
            let metadata = ResponseMetadata { status: 500, reason: text("Synthetic status", &budget, Stage::Headers).unwrap(), json };
            let (body, closed, _) = wire::pipeline(actions, coding, wire::limits(), &budget);
            let failure = http_failure(body, metadata, limits(), &cancel, &budget).await.unwrap();
            assert!(closed.load(Ordering::Acquire), "case {index}");
            let Failure::Http { status_code, error, message: actual, body_failure } = failure else { panic!("case {index}: {failure:?}") };
            assert_eq!(status_code, 500, "case {index}");
            assert_eq!(actual.as_unicode(), Some(message), "case {index}");
            if fallback { assert_eq!(string(&error), Some(FALLBACK), "case {index}"); }
            let caught_name = body_failure.map(|value| match value {
                BodyFailure::Reader => "reader", BodyFailure::Codec => "codec", BodyFailure::Json => "json", BodyFailure::Property => "property" });
            assert_eq!(caught_name, caught, "case {index}");
        }
        // The SDK keeps a non-string error value as thrown; only the message is coerced.
        let cancel = Cancellation::new(); let budget = wire::budget(&cancel);
        let metadata = ResponseMetadata { status: 500, reason: text("", &budget, Stage::Headers).unwrap(), json: true };
        let (body, _, _) = wire::pipeline(vec![Data(br#"{"error":42}"#.to_vec())], "", wire::limits(), &budget);
        assert!(matches!(http_failure(body, metadata, limits(), &cancel, &budget).await, Ok(Failure::Http { error: Value::Number(value), .. }) if value == 42.0));
        // A resource limit or cancellation is never converted into the ResponseError fallback.
        let mut tiny = limits(); tiny.max_buffered_bytes = 4;
        let metadata = ResponseMetadata { status: 500, reason: text("", &budget, Stage::Headers).unwrap(), json: false };
        let (body, _, _) = wire::pipeline(vec![Data(b"too long".to_vec())], "", wire::limits(), &budget);
        assert!(matches!(http_failure(body, metadata, tiny, &cancel, &budget).await, Err(Failure::Limit("buffered-body-bytes"))));
        let entered = Arc::new(Notify::new());
        let metadata = ResponseMetadata { status: 500, reason: text("", &budget, Stage::Headers).unwrap(), json: true };
        let (body, closed, _) = wire::pipeline(vec![Data(b"{".to_vec()), wire::Action::Hold(entered.clone())], "", wire::limits(), &budget);
        let pending = http_failure(body, metadata, limits(), &cancel, &budget); tokio::pin!(pending);
        tokio::select! { result = &mut pending => panic!("held error body settled early: {result:?}"), _ = entered.notified() => () }
        cancel.cancel();
        assert!(matches!(pending.await, Err(Failure::Cancelled)));
        assert!(closed.load(Ordering::Acquire));
    }
}