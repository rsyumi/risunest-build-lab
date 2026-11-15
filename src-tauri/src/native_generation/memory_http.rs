use super::ollama_decode::{self as decode, CancelOrigin, Cancellation, Chunk, ChunkSource, DecodeError, DecodeLimits, Pipeline};
use bytes::Bytes;
use reqwest::{header::{self, HeaderMap, HeaderValue}, Method, Response};
use risunest_ir::{Budget, Charge, Text};
use risunest_ir::provider::{HttpMethod, HttpRequestSpec, ResponseKind};
use risunest_ir::stage::HostReply;
use risunest_providers::embeddings::Failure;
use std::{alloc::Layout, future::{Future, Ready, ready}, mem::{align_of, size_of}, pin::Pin,
    sync::{Arc, atomic::{AtomicBool, AtomicU8, AtomicUsize}}, task::{Context, Poll}};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use self as ownership;

pub(crate) fn limit() -> Failure { Failure::Limit("native-http-owned-bytes") }

pub(crate) fn arc_bytes<T>() -> Result<usize, Failure> {
    Layout::new::<[usize; 2]>().extend(Layout::new::<T>()).map(|(layout, _)| layout.pad_to_align().size()).map_err(|_| limit())
}

struct OperationState {
    budget: Budget,
    cancel: Cancellation,
    _charge: Charge,
    _permit: OwnedSemaphorePermit,
    _liveness: Option<Arc<()>>,
}

#[derive(Clone)]
pub(crate) struct Operation(Arc<OperationState>);

impl Operation {
    pub fn new(slots: Arc<Semaphore>, limits: risunest_ir::Limits) -> Result<Self, Failure> {
        Self::new_inner(slots, limits, None)
    }
    pub fn new_with_liveness(slots: Arc<Semaphore>, limits: risunest_ir::Limits, witness: Arc<()>) -> Result<Self, Failure> {
        Self::new_inner(slots, limits, Some(witness))
    }
    fn new_inner(slots: Arc<Semaphore>, limits: risunest_ir::Limits, witness: Option<Arc<()>>) -> Result<Self, Failure> {
        let cancel = Cancellation::new();
        let budget = Budget::new(limits, cancel.flag())?;
        Self::admit_pair(slots, budget, cancel, witness)
    }
    #[cfg(test)]
    pub(crate) fn admit(slots: Arc<Semaphore>, budget: Budget, cancel: Cancellation) -> Result<Self, Failure> { Self::admit_pair(slots, budget, cancel, None) }
    fn admit_pair(slots: Arc<Semaphore>, budget: Budget, cancel: Cancellation, witness: Option<Arc<()>>) -> Result<Self, Failure> {
        budget.check()?;
        let permit = slots.try_acquire_owned().map_err(|_| Failure::Limit("native-http-active-operations"))?;
        let controls = arc_bytes::<AtomicBool>()?.checked_add(arc_bytes::<AtomicU8>()?)
            .and_then(|bytes| bytes.checked_add(arc_bytes::<Notify>().ok()?)).ok_or_else(limit)?;
        let witness_bytes = if witness.is_some() { arc_bytes::<()>()? } else { 0 };
        let charge = budget.reserve_owned(arc_bytes::<OperationState>()?.checked_add(controls)
            .and_then(|bytes| bytes.checked_add(witness_bytes)).ok_or_else(limit)?)?;
        Ok(Self(Arc::new(OperationState { budget, cancel, _permit: permit, _charge: charge, _liveness: witness })))
    }
    pub fn budget(&self) -> &Budget { &self.0.budget }
    pub fn cancel(&self) -> &Cancellation { &self.0.cancel }
    pub fn same(&self, other: &Self) -> bool { Arc::ptr_eq(&self.0, &other.0) }
    pub fn check(&self) -> Result<(), Failure> { self.budget().check().map_err(Into::into) }
}

pub(crate) struct Flight {
    operation: Operation,
    settled: bool,
}

impl Flight {
    pub fn new(operation: &Operation) -> Self { Self { operation: operation.clone(), settled: false } }
    pub fn settle(&mut self) { self.settled = true; }
}

impl Drop for Flight {
    fn drop(&mut self) {
        if !self.settled { self.operation.cancel().cancel_as(CancelOrigin::Owner); }
    }
}

struct Payload {
    data: Vec<u8>,
    first: usize,
    last: usize,
    _storage: Charge,
    _wrapper: Charge,
    _operation: Operation,
}

impl AsRef<[u8]> for Payload {
    fn as_ref(&self) -> &[u8] { &self.data[self.first..self.last] }
}

pub(crate) fn payload(data: Vec<u8>, first: usize, last: usize, operation: &Operation) -> Result<Bytes, Failure> {
    operation.check()?;
    if first > last || last > data.len() { return Err(Failure::Invalid("native-http-payload-range")); }
    let storage = operation.budget().reserve_owned(data.capacity())?;
    // bytes 1.12.1 uses repr(C) Owned<T> { AtomicUsize, T } for from_owner.
    let wrapper_size = Layout::new::<AtomicUsize>().extend(Layout::new::<Payload>())
        .map_err(|_| limit())?.0.pad_to_align().size();
    let wrapper = operation.budget().reserve_owned(wrapper_size)?;
    Ok(Bytes::from_owner(Payload { data, first, last, _storage: storage, _wrapper: wrapper, _operation: operation.clone() }))
}

pub(crate) fn header_payload(value: String, operation: &Operation) -> Result<Bytes, Failure> {
    for scalar in value.chars() {
        operation.budget().work(1)?;
        if scalar as u32 > 255 { return Err(Failure::Invalid("native-http-header-byte-string")); }
    }
    let bytes = value.as_bytes();
    let mut first = 0;
    let mut last = bytes.len();
    while first < last && matches!(bytes[first], b' ' | b'\t' | b'\r' | b'\n') { operation.budget().work(1)?; first += 1; }
    while last > first && matches!(bytes[last - 1], b' ' | b'\t' | b'\r' | b'\n') { operation.budget().work(1)?; last -= 1; }
    for byte in &bytes[first..last] {
        operation.budget().work(1)?;
        if matches!(byte, 0 | b'\r' | b'\n') { return Err(Failure::Invalid("native-http-header-value")); }
    }
    // Validate the plugin's from_str path while its transient copy is also charged.
    let transient = operation.budget().reserve_owned(last - first)?;
    let validated = HeaderValue::from_str(&value[first..last]).map_err(|_| Failure::Invalid("native-http-header-value"))?;
    drop(validated);
    drop(transient);
    payload(value.into_bytes(), first, last, operation)
}

pub(crate) fn header_map_bytes(entries: usize) -> Result<usize, Failure> {
    use reqwest::header::{HeaderName, HeaderValue};
    if entries == 0 { return Ok(0); }
    let raw = entries.checked_add(entries / 3).and_then(usize::checked_next_power_of_two).ok_or_else(limit)?;
    let usable = raw - raw / 4;
    // http 1.5.0 has u16 index/hash slots and buckets with hash, key, value,
    // and optional two-usize links. Include maximal padding at each field.
    let alignment = align_of::<HeaderName>().max(align_of::<HeaderValue>()).max(align_of::<usize>());
    let bucket = size_of::<u16>().checked_add(size_of::<HeaderName>())
        .and_then(|n| n.checked_add(size_of::<HeaderValue>()))
        .and_then(|n| n.checked_add(3 * size_of::<usize>()))
        .and_then(|n| n.checked_add(4 * (alignment - 1))).ok_or_else(limit)?;
    raw.checked_mul(2 * size_of::<u16>()).and_then(|n| usable.checked_mul(bucket).and_then(|b| n.checked_add(b))).ok_or_else(limit)
}

pub(crate) struct Buffer {
    data: Vec<u8>,
    charge: Charge,
    extra: Option<Charge>,
    operation: Operation,
    maximum: usize,
    quantum: usize,
}

impl Buffer {
    pub fn new(operation: &Operation, maximum: usize, quantum: usize) -> Result<Self, Failure> {
        if quantum == 0 { return Err(Failure::Invalid("native-http-copy-quantum")); }
        Ok(Self { data: Vec::new(), charge: operation.budget().reserve_owned(0)?, extra: None,
            operation: operation.clone(), maximum, quantum })
    }
    pub fn append(&mut self, bytes: &[u8]) -> Result<(), Failure> {
        let required = self.data.len().checked_add(bytes.len()).filter(|n| *n <= self.maximum)
            .ok_or(Failure::Limit("native-http-buffered-bytes"))?;
        if required > self.data.capacity() {
            let next = required.checked_next_power_of_two().unwrap_or(self.maximum).min(self.maximum);
            let charge = self.operation.budget().reserve_owned(next)?;
            let mut data = Vec::new();
            data.try_reserve_exact(next).map_err(|_| Failure::Limit("allocation"))?;
            let extra = if data.capacity() > next { Some(self.operation.budget().reserve_owned(data.capacity() - next)?) } else { None };
            let mut replacement = Self { data, charge, extra, operation: self.operation.clone(), maximum: self.maximum, quantum: self.quantum };
            for part in self.data.chunks(self.quantum) {
                self.operation.budget().work(part.len())?;
                replacement.data.extend_from_slice(part);
            }
            let previous = std::mem::replace(self, replacement);
            drop(previous);
        }
        for part in bytes.chunks(self.quantum) {
            self.operation.budget().work(part.len())?;
            self.data.extend_from_slice(part);
        }
        Ok(())
    }
    pub fn bytes(&self) -> &[u8] { &self.data }
    pub fn capacity(&self) -> usize { self.data.capacity() }
    pub fn into_parts(self) -> (Vec<u8>, Charge, Option<Charge>, Operation) {
        (self.data, self.charge, self.extra, self.operation)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) url_bytes: usize,
    pub(crate) header_bytes: usize,
    pub(crate) response_bytes: usize,
    pub(crate) decode_owned: usize,
    pub(crate) decode_work: usize,
    pub(crate) copy_quantum: usize,
    pub(crate) decode: DecodeLimits,
}

pub(crate) fn decode_failure(error: DecodeError) -> Failure {
    match error {
        DecodeError::Cancelled => Failure::Cancelled,
        DecodeError::Limit(kind) => Failure::Limit(kind),
        DecodeError::Allocation => Failure::Limit("allocation"),
        _ => Failure::Invalid("native-http-decode"),
    }
}

pub(crate) fn transport(message: &'static str, budget: &Budget) -> Failure {
    match Text::from_str(message, budget) { Ok(diagnostic) => Failure::Transport { diagnostic }, Err(error) => error.into() }
}


fn stop_check(operation: &Operation, stop: Option<&Cancellation>) -> Result<(), Failure> {
    operation.check()?;
    if stop.is_some_and(Cancellation::is_cancelled) { return Err(Failure::Cancelled); }
    Ok(())
}
async fn stopped(stop: Option<&Cancellation>) {
    match stop { Some(stop) => stop.cancelled().await, None => std::future::pending().await }
}

pub(crate) struct ParsedUrl {
    pub(crate) url: url::Url,
    _state: Charge,
    _operation: Operation,
}

pub(crate) struct ParseTask {
    raw: String,
    _raw: Charge,
    state: Charge,
    stop: Option<Cancellation>,
    operation: Operation,
}
impl ParseTask {
    pub(crate) fn new(raw: String, maximum: usize, operation: &Operation, stop: Option<&Cancellation>) -> Result<Self, Failure> {
        stop_check(operation, stop)?;
        if raw.len() > maximum { return Err(Failure::Limit("native-http-url-bytes")); }
        let raw_charge = operation.budget().reserve_owned(raw.capacity())?;
        let state = operation.budget().reserve_owned(size_of::<Self>().max(size_of::<ParsedUrl>()))?;
        Ok(Self { raw, _raw: raw_charge, state, stop: stop.cloned(), operation: operation.clone() })
    }
    pub(crate) fn run(self) -> Result<ParsedUrl, Failure> {
        stop_check(&self.operation, self.stop.as_ref())?;
        let url = url::Url::parse(&self.raw).map_err(|_| Failure::Invalid("native-http-url"))?;
        stop_check(&self.operation, self.stop.as_ref())?;
        if !url.username().is_empty() || url.password().is_some() { return Err(Failure::Invalid("native-http-url-credentials")); }
        if !matches!(url.scheme(), "http" | "https") { return Err(Failure::Unsupported("native-http-scheme")); }
        Ok(ParsedUrl { url, _state: self.state, _operation: self.operation })
    }
}
pub(crate) async fn settle_parse(mut handle: tokio::task::JoinHandle<Result<ParsedUrl, Failure>>, operation: &Operation,
    stop: Option<&Cancellation>) -> Result<ParsedUrl, Failure>
{
    let joined = tokio::select! { biased;
        _ = operation.cancel().cancelled() => { handle.abort(); let _ = handle.await; return Err(Failure::Cancelled); },
        _ = stopped(stop) => {
            handle.abort();
            let joined = handle.await;
            operation.check()?;
            if matches!(joined, Err(error) if !error.is_cancelled()) { return Err(Failure::Invalid("native-http-parser-join")); }
            return Err(Failure::Cancelled);
        },
        result = &mut handle => result,
    };
    operation.check()?;
    let parsed = joined.map_err(|_| Failure::Invalid("native-http-parser-join"))?;
    stop_check(operation, stop)?;
    parsed
}
pub(crate) async fn parse_url(raw: String, maximum: usize, operation: &Operation, stop: Option<&Cancellation>) -> Result<ParsedUrl, Failure> {
    let task = ParseTask::new(raw, maximum, operation, stop)?;
    settle_parse(tokio::task::spawn_blocking(move || task.run()), operation, stop).await
}

pub(crate) type PullResult = Result<Option<Chunk>, DecodeError>;
struct ChargedPull<F> { inner: Pin<Box<F>>, _charge: Charge }
impl<F: Future<Output = PullResult>> Future for ChargedPull<F> {
    type Output = PullResult;
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().inner.as_mut().poll(context)
    }
}

fn charged_pull<'a, F: Future<Output = PullResult> + Send + 'a>(future: F, budget: &Budget) -> Pin<Box<dyn Future<Output = PullResult> + Send + 'a>> {
    let bytes = size_of::<F>().checked_add(size_of::<ChargedPull<F>>());
    let admission = bytes.ok_or(DecodeError::Limit("owned-bytes")).and_then(|bytes| budget.reserve_owned(bytes).map_err(Into::into));
    match admission {
        Ok(charge) => Box::pin(ChargedPull { inner: Box::pin(future), _charge: charge }),
        // HttpSource reserves this one fallback frame while its exclusive pull borrow lives.
        Err(error) => Box::pin(ready(Err(error))),
    }
}

pub(crate) struct HttpSource {
    response: Response,
    pending: Option<Bytes>,
    pending_charge: Option<Charge>,
    position: usize,
    received: u64,
    maximum: u64,
    _charge: Charge,
    _operation: Operation,
}

impl HttpSource {
    pub(crate) fn new(response: Response, operation: &Operation, budget: &Budget, maximum: u64) -> Result<Self, Failure> {
        let charge = budget.reserve_owned(size_of::<Self>() + size_of::<Ready<PullResult>>())?;
        Ok(Self { response, pending: None, pending_charge: None, position: 0, received: 0, maximum,
            _operation: operation.clone(), _charge: charge })
    }
    async fn pull_inner(&mut self, maximum: usize, cancel: &Cancellation, budget: &Budget) -> PullResult {
        loop {
            budget.work(1)?;
            if let Some(bytes) = &self.pending {
                if self.position < bytes.len() {
                    let end = self.position.saturating_add(maximum).min(bytes.len());
                    let part = Chunk::copy(&bytes[self.position..end], budget)?;
                    self.position = end;
                    if self.position == bytes.len() { self.pending = None; self.pending_charge = None; }
                    return Ok(Some(part));
                }
                self.pending = None;
                self.pending_charge = None;
            }
            let incoming = tokio::select! { biased;
                _ = cancel.cancelled() => return Err(DecodeError::Cancelled),
                value = self.response.chunk() => value.map_err(|_| DecodeError::Reader(String::new()))?,
            };
            let Some(bytes) = incoming else { return Ok(None) };
            self.received = self.received.checked_add(bytes.len() as u64).filter(|n| *n <= self.maximum)
                .ok_or(DecodeError::Limit("encoded-bytes"))?;
            let charge = budget.reserve_owned(bytes.len())?;
            self.pending = Some(bytes);
            self.pending_charge = Some(charge);
            self.position = 0;
        }
    }
}

impl ChunkSource for HttpSource {
    fn pull<'life0, 'life1, 'life2, 'async_trait>(&'life0 mut self, maximum: usize,
        cancel: &'life1 Cancellation, budget: &'life2 Budget)
        -> Pin<Box<dyn Future<Output = PullResult> + Send + 'async_trait>>
        where 'life0: 'async_trait, 'life1: 'async_trait, 'life2: 'async_trait, Self: 'async_trait
    {
        charged_pull(self.pull_inner(maximum, cancel, budget), budget)
    }
    fn poll_native_finish(&mut self, cx: &mut Context<'_>, cancel: &Cancellation, budget: &Budget)
        -> Poll<Result<(), DecodeError>>
    {
        budget.check()?;
        if cancel.is_cancelled() { return Poll::Ready(Err(DecodeError::Cancelled)); }
        // Discard the current engine frame, including bytes not copied into intake quanta.
        self.pending = None;
        self.pending_charge = None;
        self.position = 0;
        budget.work(1)?;
        let incoming = {
            let mut read = std::pin::pin!(self.response.chunk());
            read.as_mut().poll(cx)
        };
        let incoming = match incoming {
            Poll::Pending => {
                return Poll::Pending;
            }
            Poll::Ready(value) => {
                budget.check()?;
                value.map_err(|_| DecodeError::Reader(String::new()))?
            }
        };
        budget.check()?;
        if let Some(bytes) = incoming {
            self.received = self.received.checked_add(bytes.len() as u64).filter(|n| *n <= self.maximum)
                .ok_or(DecodeError::Limit("encoded-bytes"))?;
            let charge = budget.reserve_owned(bytes.len())?;
            let empty = bytes.is_empty();
            drop(bytes);
            drop(charge);
            budget.check()?;
            if !empty { return Poll::Ready(Err(DecodeError::Reader(String::new()))); }
        }
        budget.check()?;
        Poll::Ready(Ok(()))
    }
}

pub(crate) struct DecoderOwner {
    pub(crate) pipeline: Option<Pipeline>,
    pub(crate) budget: Option<Budget>,
    _reserve: Charge,
}
impl DecoderOwner {
    pub(crate) fn new(operation: &Operation, limits: Limits) -> Result<Self, Failure> {
        let reserve = operation.budget().reserve_owned(limits.decode_owned)?;
        let budget = Budget::new(risunest_ir::Limits { max_owned_bytes: limits.decode_owned, max_json_bytes: 1,
            max_text_units: 1, max_values: 1, max_depth: 1, max_work: limits.decode_work }, operation.cancel().flag())
            .map_err(DecodeError::from).map_err(decode_failure)?;
        Ok(Self { pipeline: None, budget: Some(budget), _reserve: reserve })
    }
    pub(crate) fn start(&mut self, source: Box<dyn ChunkSource>, gzip: bool, limits: Limits) -> Result<(), Failure> {
        let budget = self.source_budget().clone();
        self.pipeline = Some(Pipeline::native_plugin(source, gzip, limits.decode, budget).map_err(decode_failure)?);
        Ok(())
    }
    pub(crate) fn source_budget(&self) -> &Budget { self.budget.as_ref().expect("decoder owner budget") }
    pub(crate) async fn collect_step(&mut self, output: &mut Buffer, operation: &Operation,
        stop: Option<&Cancellation>) -> Result<bool, Failure>
    {
        stop_check(operation, stop)?;
        let pulled = tokio::select! { biased;
            _ = operation.cancel().cancelled() => { return Err(Failure::Cancelled); },
            _ = stopped(stop) => { operation.check()?; return Err(Failure::Cancelled); },
            result = self.pipeline.as_mut().ok_or(Failure::Invalid("native-http-decoder-state"))?.pull(operation.cancel()) => result,
        }.map_err(decode_failure)?;
        stop_check(operation, stop)?;
        let Some(part) = pulled else { return Ok(false); };
        output.append(part.bytes())?;
        drop(part);
        Ok(true)
    }
    pub(crate) async fn collect(&mut self, response: Response, gzip: bool, operation: &Operation, limits: Limits,
        stop: Option<&Cancellation>) -> Result<Buffer, Failure>
    {
        self.pipeline = None;
        let mut output = Buffer::new(operation, limits.response_bytes, limits.copy_quantum)?;
        let source = HttpSource::new(response, operation, self.source_budget(), limits.decode.max_encoded_bytes)?;
        self.start(Box::new(source), gzip, limits)?;
        let result = async { while self.collect_step(&mut output, operation, stop).await? {} stop_check(operation, stop) }.await;
        self.pipeline = None;
        result?;
        Ok(output)
    }
}
impl Drop for DecoderOwner { fn drop(&mut self) { self.pipeline = None; self.budget = None; } }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase { Ready, Running(u64), Response(u64), Terminal }

pub(crate) struct ReplyEnvelope {
    reply: Option<HostReply>,
    pub(crate) generation: u64,
    stop: Option<Cancellation>,
    _charge: Option<Charge>,
    _extra: Option<Charge>,
    pub(crate) operation: Operation,
}

impl ReplyEnvelope {
    #[cfg(test)]
    pub(crate) fn synthetic(status: u16, buffer: Buffer, generation: u64) -> Self {
        let (body, charge, extra, operation) = buffer.into_parts();
        Self { reply: Some(HostReply::Http { status, body }), generation, stop: None, _charge: Some(charge), _extra: extra, operation }
    }
    #[cfg(test)]
    pub(crate) fn body(&self) -> Option<&Vec<u8>> { match &self.reply { Some(HostReply::Http { body, .. }) => Some(body), _ => None } }
}

pub(crate) struct Executor {
    pub(crate) decoder: DecoderOwner,
    pub(crate) limits: Limits,
    pub(crate) phase: Phase,
    next: u64,
    pub(crate) operation: Operation,
}
impl Executor {
    pub(crate) fn new(operation: Operation, limits: Limits) -> Result<Self, Failure> {
        limits.decode.validate().map_err(decode_failure)?;
        if limits.copy_quantum == 0 || limits.url_bytes == 0 || limits.header_bytes == 0 { return Err(Failure::Invalid("native-http-limits")); }
        let decoder = DecoderOwner::new(&operation, limits)?;
        Ok(Self { operation, decoder, limits, phase: Phase::Ready, next: 0 })
    }
    pub(crate) fn begin(&mut self) -> Result<(u64, Flight), Failure> {
        if self.phase != Phase::Ready { self.phase = Phase::Terminal; return Err(Failure::Invalid("native-http-dispatch-state")); }
        if let Err(error) = self.operation.check() { self.phase = Phase::Terminal; return Err(error); }
        self.next = match self.next.checked_add(1) { Some(next) => next, None => { self.phase = Phase::Terminal; return Err(Failure::Limit("native-http-generation")); } };
        self.phase = Phase::Running(self.next);
        Ok((self.next, Flight::new(&self.operation)))
    }
    pub(crate) fn publish(&mut self, generation: u64, result: Result<(u16, Buffer), Failure>,
        stop: Option<&Cancellation>) -> Result<ReplyEnvelope, Failure>
    {
        if self.phase != Phase::Running(generation) { return Err(Failure::Invalid("native-http-dispatch-state")); }
        self.phase = Phase::Terminal;
        self.operation.check()?;
        let result = if stop.is_some_and(Cancellation::is_cancelled) {
            match result {
                Err(error) if !matches!(&error, Failure::Transport { .. } | Failure::Cancelled |
                    Failure::Invalid("native-http-url" | "native-http-url-credentials" | "native-http-header-byte-string" | "native-http-header-value") |
                    Failure::Unsupported("native-http-scheme")) => Err(error),
                _ => Err(Failure::Cancelled),
            }
        } else { result };
        let (mut reply, charge, extra) = match result {
            Ok((status, output)) => {
                let (body, charge, extra, _) = output.into_parts();
                (HostReply::Http { status, body }, Some(charge), extra)
            }
            Err(error) => {
                let reply = match error {
                    Failure::Cancelled if self.operation.check().is_ok() && stop.is_some_and(Cancellation::is_cancelled) => HostReply::Cancelled,
                    Failure::Transport { diagnostic } => HostReply::Failed { message: diagnostic },
                    Failure::Invalid("native-http-url" | "native-http-url-credentials" | "native-http-header-byte-string" | "native-http-header-value") |
                    Failure::Unsupported("native-http-scheme") => HostReply::Failed { message: Text::from_str("HTTP request is invalid", self.operation.budget())? },
                    other => { self.phase = Phase::Terminal; return Err(other); }
                };
                (reply, None, None)
            }
        };
        self.operation.check()?;
        if stop.is_some_and(Cancellation::is_cancelled) { reply = HostReply::Cancelled; }
        self.phase = Phase::Response(generation);
        Ok(ReplyEnvelope { reply: Some(reply), generation, stop: stop.cloned(), _charge: charge, _extra: extra, operation: self.operation.clone() })
    }
    pub(crate) async fn execute(&mut self, request: HttpRequestSpec, stop: Option<&Cancellation>) -> Result<ReplyEnvelope, Failure> {
        let (generation, mut flight) = self.begin()?;
        let result = self.exchange(request, stop).await;
        flight.settle();
        self.publish(generation, result, stop)
    }
    pub(crate) fn resume<T>(&mut self, mut reply: ReplyEnvelope, resume: impl FnOnce(HostReply) -> T) -> Result<T, Failure> {
        if !self.operation.same(&reply.operation) || self.phase != Phase::Response(reply.generation) { self.phase = Phase::Terminal; return Err(Failure::Invalid("native-http-reply-owner")); }
        if let Err(error) = self.operation.check() { self.phase = Phase::Terminal; return Err(error); }
        self.phase = Phase::Ready;
        let value = reply.reply.take().ok_or(Failure::Invalid("native-http-reply-state"))?;
        let value = if reply.stop.as_ref().is_some_and(Cancellation::is_cancelled) { HostReply::Cancelled } else { value };
        let result = resume(value);
        if let Err(error) = self.operation.check() { self.phase = Phase::Terminal; return Err(error); }
        drop(reply);
        Ok(result)
    }
    async fn exchange(&mut self, mut request: HttpRequestSpec, stop: Option<&Cancellation>) -> Result<(u16, Buffer), Failure> {
        Self::admit_shape(&request)?;
        let parsed = parse_url(std::mem::take(&mut request.url), self.limits.url_bytes, &self.operation, stop).await?;
        let incoming = self.send(request, parsed, stop).await?;
        self.receive(incoming, stop).await
    }
    pub(crate) async fn receive(&mut self, incoming: Incoming, stop: Option<&Cancellation>) -> Result<(u16, Buffer), Failure> {
        let Incoming { status, response, gzip } = incoming;
        let Some(response) = response else { return Ok((status, Buffer::new(&self.operation, self.limits.response_bytes, self.limits.copy_quantum)?)); };
        let output = self.decoder.collect(response, gzip, &self.operation, self.limits, stop).await.map_err(|error| match error {
            Failure::Invalid("native-http-decode") => transport("HTTP response body could not be read", self.operation.budget()), other => other,
        })?;
        Ok((status, output))
    }
    pub(crate) fn admit_shape(request: &HttpRequestSpec) -> Result<(), Failure> {
        if request.method != HttpMethod::Post || request.response != ResponseKind::Buffered { return Err(Failure::Unsupported("native-http-embedding-request")); }
        Ok(())
    }
    pub(crate) async fn send(&mut self, request: HttpRequestSpec, parsed: ParsedUrl, stop: Option<&Cancellation>) -> Result<Incoming, Failure> {
        Self::admit_shape(&request)?;
        let operation = &self.operation;
        let meta = operation.budget().reserve_owned(ownership::header_map_bytes(6)?)?;
        let mut headers = HeaderMap::with_capacity(6);
        let mut header_bytes = "user-agenttauri-plugin-http/2.8.0accept-encodinggzipaccept*/*".len();
        if header_bytes > self.limits.header_bytes { return Err(Failure::Limit("native-http-header-bytes")); }
        for (name, value) in request.headers {
            operation.budget().work(name.len())?;
            let key = if name.eq_ignore_ascii_case("content-type") { header::CONTENT_TYPE }
                else if name.eq_ignore_ascii_case("authorization") { header::AUTHORIZATION }
                else { return Err(Failure::Unsupported("native-http-embedding-header")); };
            if headers.contains_key(&key) { return Err(Failure::Invalid("native-http-duplicate-request-header")); }
            header_bytes = header_bytes.checked_add(name.len()).and_then(|n| n.checked_add(value.len()))
                .filter(|n| *n <= self.limits.header_bytes).ok_or(Failure::Limit("native-http-header-bytes"))?;
            let value = HeaderValue::from_maybe_shared(ownership::header_payload(value, operation)?)
                .map_err(|_| Failure::Invalid("native-http-header-value"))?;
            headers.insert(key, value);
        }
        headers.insert(header::USER_AGENT, HeaderValue::from_static("tauri-plugin-http/2.8.0"));
        headers.insert(header::ACCEPT, HeaderValue::from_static("*/*"));
        headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
        let body_len = request.body.len();
        let body = ownership::payload(request.body, 0, body_len, operation)?;
        let mut wire = reqwest::Request::new(Method::POST, parsed.url);
        *wire.headers_mut() = headers;
        *wire.body_mut() = Some(reqwest::Body::from(body));
        let client = crate::platform_tls::client_builder().no_gzip().no_brotli().no_deflate().no_zstd()
            .redirect(reqwest::redirect::Policy::limited(10)).retry(reqwest::retry::never()).build()
            .map_err(|_| transport("HTTP client could not be created", operation.budget()))?;
        stop_check(operation, stop)?;
        let response = tokio::select! { biased;
            _ = operation.cancel().cancelled() => return Err(Failure::Cancelled),
            _ = stopped(stop) => { operation.check()?; return Err(Failure::Cancelled); },
            response = risunest_network_log::reqwest::execute(&client, wire, "native-memory-http") => response.map_err(|_| transport("HTTP request failed", operation.budget()))?,
        };
        drop(meta);
        stop_check(operation, stop)?;
        let status = response.status().as_u16();
        let gzip = response.headers().get(header::CONTENT_ENCODING).is_some_and(|value| value.as_bytes() == b"gzip");
        response_headers(response.headers(), gzip, self.limits.header_bytes, operation.budget())?;
        if !(200..=599).contains(&status) { return Err(transport("HTTP response could not be constructed", operation.budget())); }
        if matches!(status, 204 | 205 | 304) {
            drop(response);
            return Ok(Incoming { status, response: None, gzip });
        }
        Ok(Incoming { status, response: Some(response), gzip })
    }
}
pub(crate) struct Incoming { pub(crate) status: u16, pub(crate) response: Option<Response>, pub(crate) gzip: bool }

fn response_headers(headers: &HeaderMap, gzip: bool, maximum: usize, budget: &Budget) -> Result<(), Failure> {
    let mut total = 0usize;
    for (name, value) in headers {
        if gzip && (*name == header::CONTENT_ENCODING || *name == header::CONTENT_LENGTH) { continue; }
        total = total.checked_add(name.as_str().len()).and_then(|n| n.checked_add(value.as_bytes().len()))
            .filter(|n| *n <= maximum).ok_or(Failure::Limit("native-http-header-bytes"))?;
        budget.work(value.as_bytes().len())?;
        let value = std::str::from_utf8(value.as_bytes()).map_err(|_| transport("HTTP response headers are invalid", budget))?;
        for scalar in value.chars() {
            budget.work(1)?;
            if scalar as u32 > 255 { return Err(transport("HTTP response headers are invalid", budget)); }
        }
    }
    Ok(())
}
