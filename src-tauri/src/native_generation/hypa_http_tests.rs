use super::ollama_decode::{self as decode, Cancellation, Chunk, ChunkSource, DecodeError, DecodeLimits, Pipeline};
use bytes::Bytes;
use reqwest::{header::{self, HeaderMap, HeaderValue}, Method, Response};
use risunest_ir::{Budget, Charge, OwnedVec, Text};
use risunest_ir::provider::{HttpMethod, HttpRequestSpec, ResponseKind};
use risunest_ir::stage::{HostReply, HostRequest, StageError, Step};
use risunest_memory::{hanurai::{Retrieval, RetrievalOutput}, hypa_v1::{EmbeddingBatch, EmbeddingInput, EmbeddingPurpose, EmbeddingRequest, EmbeddingService, EmbeddingVector}};
use risunest_providers::embeddings::{self, CapturedConfig, Failure, Input, Output, Purpose, RemoteTarget, Service};
use std::{future::{Future, Ready, ready}, mem::size_of, pin::Pin,
    sync::{Arc, Condvar, Mutex, atomic::{AtomicUsize, Ordering}}, task::{Context, Poll}};
use tokio::sync::{Notify, Semaphore};

#[path = "hypa_http_ownership_tests.rs"]
mod ownership;
#[path = "../../tests/support/hypa_http_plugin_tests.rs"]
mod plugin;
#[path = "hypa_http_cases_tests.rs"]
mod cases;
#[path = "hypa_http_parser_measurement_tests.rs"]
mod parser_measurement;
use ownership::{Buffer, Flight, Observed, Operation, SharedFailure};

#[derive(Clone, Copy)]
struct Limits {
    url_bytes: usize,
    header_bytes: usize,
    response_bytes: usize,
    decode_owned: usize,
    decode_work: usize,
    copy_quantum: usize,
    decode: DecodeLimits,
}

fn decode_failure(error: DecodeError) -> Failure {
    match error {
        DecodeError::Cancelled => Failure::Cancelled,
        DecodeError::Limit(kind) => Failure::Limit(kind),
        DecodeError::Allocation => Failure::Limit("allocation"),
        _ => Failure::Invalid("native-http-decode"),
    }
}

fn stage_failure(error: Failure, diagnostic: &SharedFailure) -> StageError {
    match error {
        Failure::Invalid(kind) => StageError::Invalid(kind.into()),
        Failure::Unsupported(kind) => StageError::Unsupported(kind.into()),
        Failure::Limit(kind) => StageError::Limit(kind.into()),
        Failure::Cancelled => StageError::Cancelled,
        value => { diagnostic.set(value); StageError::Invalid("embedding-provider-failure".into()) }
    }
}

fn transport(message: &'static str, budget: &Budget) -> Failure {
    match Text::from_str(message, budget) { Ok(diagnostic) => Failure::Transport { diagnostic }, Err(error) => error.into() }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PauseAt { BeforeParse, AfterParse }

struct ParseBarrier {
    at: PauseAt,
    entered: Notify,
    submitted: Notify,
    released: (Mutex<bool>, Condvar),
}
impl ParseBarrier {
    fn new(at: PauseAt) -> Arc<Self> { Arc::new(Self { at, entered: Notify::new(), submitted: Notify::new(), released: (Mutex::new(false), Condvar::new()) }) }
    fn pause(&self, at: PauseAt) {
        if self.at != at { return; }
        self.entered.notify_one();
        let (lock, wake) = &self.released;
        let mut released = lock.lock().unwrap();
        while !*released { released = wake.wait(released).unwrap(); }
    }
    fn release(&self) { let (lock, wake) = &self.released; *lock.lock().unwrap() = true; wake.notify_all(); }
}

// Fields drop in order; the operation holds the slot permit, so it comes last to free the slot after the charges.
struct ParsedUrl {
    url: url::Url,
    _state: Charge,
    _operation: Operation,
}

// Fields drop in order; the operation holds the slot permit, so it comes last to free the slot after the charges.
struct ParseInput {
    raw: String,
    barrier: Option<Arc<ParseBarrier>>,
    _raw: Charge,
    state: Charge,
    operation: Operation,
}

async fn parse_url(raw: String, maximum: usize, operation: &Operation, barrier: Option<Arc<ParseBarrier>>) -> Result<ParsedUrl, Failure> {
    operation.check()?;
    if raw.len() > maximum { return Err(Failure::Limit("native-http-url-bytes")); }
    let raw_charge = operation.budget().reserve_owned(raw.capacity())?;
    let state = operation.budget().reserve_owned(size_of::<ParseInput>().max(size_of::<ParsedUrl>()))?;
    let submitted = barrier.clone();
    let input = ParseInput { raw, operation: operation.clone(), barrier, _raw: raw_charge, state };
    let mut handle = tokio::task::spawn_blocking(move || {
        input.operation.check()?;
        if let Some(barrier) = &input.barrier { barrier.pause(PauseAt::BeforeParse); }
        input.operation.check()?;
        let parsed = url::Url::parse(&input.raw).map_err(|_| Failure::Invalid("native-http-url"));
        if let Some(barrier) = &input.barrier { barrier.pause(PauseAt::AfterParse); }
        input.operation.check()?;
        let url = parsed?;
        if !url.username().is_empty() || url.password().is_some() { return Err(Failure::Invalid("native-http-url-credentials")); }
        if !matches!(url.scheme(), "http" | "https") { return Err(Failure::Unsupported("native-http-experiment-scheme")); }
        Ok(ParsedUrl { url, _operation: input.operation, _state: input.state })
    });
    if let Some(submitted) = submitted { submitted.submitted.notify_one(); }
    let joined = tokio::select! { biased;
        _ = operation.cancel().cancelled() => {
            handle.abort();
            let _ = handle.await;
            return Err(Failure::Cancelled);
        },
        result = &mut handle => result,
    };
    operation.check()?;
    joined.map_err(|_| Failure::Invalid("native-http-parser-join"))?
}

type PullResult = Result<Option<Chunk>, DecodeError>;
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

struct HttpSource {
    response: Response,
    pending: Option<Bytes>,
    pending_charge: Option<Charge>,
    position: usize,
    received: u64,
    maximum: u64,
    tail_pending: Option<Arc<Notify>>,
    _operation: Operation,
    _charge: Charge,
}

impl HttpSource {
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
                if let Some(pending) = &self.tail_pending { pending.notify_one(); }
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

struct DecoderOwner {
    pipeline: Option<Pipeline>,
    budget: Option<Budget>,
    body_progress: Option<Arc<Notify>>,
    tail_pending: Option<Arc<Notify>>,
    _reserve: Charge,
}
impl DecoderOwner {
    fn new(operation: &Operation, limits: Limits) -> Result<Self, Failure> {
        let reserve = operation.budget().reserve_owned(limits.decode_owned)?;
        let budget = Budget::new(risunest_ir::Limits { max_owned_bytes: limits.decode_owned, max_json_bytes: 1,
            max_text_units: 1, max_values: 1, max_depth: 1, max_work: limits.decode_work }, operation.cancel().flag()).map_err(DecodeError::from).map_err(decode_failure)?;
        Ok(Self { pipeline: None, budget: Some(budget), body_progress: None, tail_pending: None, _reserve: reserve })
    }
    async fn collect(&mut self, response: Response, gzip: bool, operation: &Operation, limits: Limits) -> Result<Buffer, Failure> {
        let budget = self.budget.as_ref().expect("decoder owner budget");
        let source_charge = budget.reserve_owned(size_of::<HttpSource>() + size_of::<Ready<PullResult>>()).map_err(DecodeError::from).map_err(decode_failure)?;
        let source = HttpSource { response, pending: None, pending_charge: None, position: 0, received: 0,
            maximum: limits.decode.max_encoded_bytes, tail_pending: self.tail_pending.clone(), _operation: operation.clone(), _charge: source_charge };
        self.pipeline = Some(Pipeline::native_plugin(Box::new(source), gzip, limits.decode, budget.clone()).map_err(decode_failure)?);
        let mut output = Buffer::new(operation, limits.response_bytes, limits.copy_quantum)?;
        let result = async {
            while let Some(part) = self.pipeline.as_mut().unwrap().pull(operation.cancel()).await.map_err(decode_failure)? {
                output.append(part.bytes())?;
                drop(part);
                if let Some(progress) = &self.body_progress { progress.notify_one(); }
            }
            operation.check()
        }.await;
        self.pipeline = None;
        result?;
        Ok(output)
    }
}
impl Drop for DecoderOwner {
    fn drop(&mut self) { self.pipeline = None; self.budget = None; }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase { Ready, Running(u64), Response(u64), Terminal }

struct ReplyEnvelope {
    status: u16,
    body: Option<Vec<u8>>,
    generation: u64,
    operation: Operation,
    _charge: Charge,
    _extra: Option<Charge>,
}

struct Executor {
    operation: Operation,
    decoder: DecoderOwner,
    limits: Limits,
    phase: Phase,
    next: u64,
    origin: HeaderValue,
    _origin: Option<Bytes>,
    parser_barrier: Option<Arc<ParseBarrier>>,
}

impl Executor {
    fn new(operation: Operation, limits: Limits, origin: String) -> Result<Self, Failure> {
        limits.decode.validate().map_err(decode_failure)?;
        if limits.copy_quantum == 0 || limits.url_bytes == 0 || limits.header_bytes == 0 { return Err(Failure::Invalid("native-http-limits")); }
        if origin.len() > limits.header_bytes { return Err(Failure::Limit("native-http-header-bytes")); }
        let origin_bytes = ownership::header_payload(origin, &operation)?;
        let origin = HeaderValue::from_maybe_shared(origin_bytes.clone()).map_err(|_| Failure::Invalid("native-http-origin"))?;
        let decoder = DecoderOwner::new(&operation, limits)?;
        Ok(Self { operation, decoder, limits, phase: Phase::Ready, next: 0, origin, _origin: Some(origin_bytes), parser_barrier: None })
    }

    async fn execute(&mut self, request: HttpRequestSpec) -> Result<ReplyEnvelope, Failure> {
        if self.phase != Phase::Ready { return Err(Failure::Invalid("native-http-dispatch-state")); }
        self.next = self.next.checked_add(1).ok_or(Failure::Limit("native-http-generation"))?;
        let generation = self.next;
        self.phase = Phase::Running(generation);
        let mut flight = Flight::new(&self.operation);
        let result = self.exchange(request).await;
        flight.settle();
        match result {
            Ok((status, output)) => {
                if let Err(error) = self.operation.check() { self.phase = Phase::Terminal; return Err(error); }
                let (body, charge, extra, operation) = output.into_parts();
                self.phase = Phase::Response(generation);
                Ok(ReplyEnvelope { status, body: Some(body), generation, operation, _charge: charge, _extra: extra })
            }
            Err(error) => { self.phase = Phase::Terminal; Err(error) }
        }
    }

    fn resume<T>(&mut self, mut reply: ReplyEnvelope, resume: impl FnOnce(HostReply) -> T) -> Result<T, Failure> {
        if !self.operation.same(&reply.operation) || self.phase != Phase::Response(reply.generation) {
            return Err(Failure::Invalid("native-http-reply-owner"));
        }
        self.operation.check()?;
        self.phase = Phase::Ready;
        let result = resume(HostReply::Http { status: reply.status, body: reply.body.take().unwrap() });
        self.operation.check()?;
        drop(reply);
        Ok(result)
    }

    async fn exchange(&mut self, request: HttpRequestSpec) -> Result<(u16, Buffer), Failure> {
        if request.method != HttpMethod::Post || request.response != ResponseKind::Buffered {
            return Err(Failure::Unsupported("native-http-embedding-request"));
        }
        let operation = &self.operation;
        let parsed = parse_url(request.url, self.limits.url_bytes, operation, self.parser_barrier.clone()).await?;
        let meta = operation.budget().reserve_owned(ownership::header_map_bytes(6)?)?;
        let mut headers = HeaderMap::with_capacity(6);
        let mut header_bytes = self.origin.as_bytes().len().checked_add(
            "originuser-agenttauri-plugin-http/2.8.0accept-encodinggzipaccept*/*".len()
        ).filter(|n| *n <= self.limits.header_bytes).ok_or(Failure::Limit("native-http-header-bytes"))?;
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
        headers.insert(header::ORIGIN, self.origin.clone());
        headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
        headers.insert(header::ACCEPT, HeaderValue::from_static("*/*"));
        let body_len = request.body.len();
        let body = ownership::payload(request.body, 0, body_len, operation)?;
        let mut wire = reqwest::Request::new(Method::POST, parsed.url);
        *wire.headers_mut() = headers;
        *wire.body_mut() = Some(reqwest::Body::from(body));
        let client = crate::platform_tls::client_builder().no_proxy().no_gzip().no_brotli().no_deflate().no_zstd()
            .redirect(reqwest::redirect::Policy::limited(10)).retry(reqwest::retry::never()).build()
            .map_err(|_| transport("HTTP client could not be created", operation.budget()))?;
        operation.check()?;
        let response = tokio::select! { biased;
            _ = operation.cancel().cancelled() => return Err(Failure::Cancelled),
            response = client.execute(wire) => response.map_err(|_| transport("HTTP request failed", operation.budget()))?,
        };
        drop(meta);
        operation.check()?;
        let status = response.status().as_u16();
        let gzip = response.headers().get(header::CONTENT_ENCODING).is_some_and(|value| value.as_bytes() == b"gzip");
        response_headers(response.headers(), gzip, self.limits.header_bytes, operation.budget())?;
        if !(200..=599).contains(&status) { return Err(transport("HTTP response could not be constructed", operation.budget())); }
        if matches!(status, 204 | 205 | 304) {
            drop(response);
            return Ok((status, Buffer::new(operation, self.limits.response_bytes, self.limits.copy_quantum)?));
        }
        let output = self.decoder.collect(response, gzip, operation, self.limits).await.map_err(|error| match error {
            Failure::Invalid("native-http-decode") => transport("HTTP response body could not be read", operation.budget()),
            other => other,
        })?;
        Ok((status, output))
    }
}

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

struct CandidateCount {
    rows: AtomicUsize,
    operation: Operation,
    _charge: Charge,
}
impl CandidateCount {
    fn new(operation: &Operation) -> Result<Arc<Self>, Failure> {
        let charge = operation.budget().reserve_owned(ownership::arc_bytes::<Self>()?)?;
        Ok(Arc::new(Self { rows: AtomicUsize::new(0), operation: operation.clone(), _charge: charge }))
    }
    fn record(&self, rows: usize) -> Result<(), Failure> {
        self.operation.budget().work(1)?;
        self.rows.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| current.checked_add(rows))
            .map_err(|_| Failure::Limit("native-http-candidate-count"))?;
        Ok(())
    }
    fn reserve_result(&self) -> Result<(Charge, usize), Failure> {
        // Hypa appends each document-output row, then at most one cache entry per input key.
        let bytes = self.rows.load(Ordering::Relaxed).checked_mul(size_of::<risunest_memory::hanurai::ScoredDocument>())
            .ok_or(Failure::Limit("native-http-result-bytes"))?;
        Ok((self.operation.budget().reserve_owned(bytes)?, bytes))
    }
}

struct Adapter<C> {
    capture: C,
    pending: Option<(Service, Purpose)>,
    terminal: bool,
    diagnostic: Arc<SharedFailure>,
    candidates: Arc<CandidateCount>,
}

impl<C: Fn(&Budget) -> Result<CapturedConfig, Failure>> Adapter<C> {
    fn new(capture: C, diagnostic: Arc<SharedFailure>, candidates: Arc<CandidateCount>) -> Self {
        Self { capture, pending: None, terminal: false, diagnostic, candidates }
    }
    fn failure(&mut self, error: Failure) -> StageError {
        self.terminal = true;
        self.pending = None;
        stage_failure(error, &self.diagnostic)
    }
    fn transfer(mut output: Output, budget: &Budget) -> Result<EmbeddingBatch, StageError> {
        let mut result = OwnedVec::new(budget)?;
        for slot in output.as_mut_slice() {
            budget.work(1)?;
            result.push(slot.take().map(EmbeddingVector::F64))?;
        }
        Ok(result)
    }
    fn settle(&mut self, service: Service, purpose: Purpose, result: Result<Step<Output>, Failure>, budget: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        match result {
            Ok(Step::NeedsHost(request)) => { self.pending = Some((service, purpose)); Ok(Step::NeedsHost(request)) }
            Ok(Step::Done(output)) => {
                if purpose == Purpose::Document { self.candidates.record(output.as_slice().len()).map_err(|error| self.failure(error))?; }
                match Self::transfer(output, budget) {
                    Ok(output) => Ok(Step::Done(output)),
                    Err(error) => { self.terminal = true; Err(error) }
                }
            }
            Err(error) => Err(self.failure(error)),
        }
    }
}

impl<C: Fn(&Budget) -> Result<CapturedConfig, Failure>> EmbeddingService for Adapter<C> {
    fn embed(&mut self, request: EmbeddingRequest, budget: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        if self.terminal || self.pending.is_some() { return Err(self.failure(Failure::Invalid("native-http-adapter-pending"))); }
        let config = (self.capture)(budget).map_err(|error| self.failure(error))?;
        let target = RemoteTarget::capture(request.target.model(), None, Some(request.target.custom_endpoint()), None, budget)
            .map_err(|error| self.failure(error))?;
        let purpose = match request.purpose { EmbeddingPurpose::Document => Purpose::Document, EmbeddingPurpose::Query => Purpose::Query };
        let input = match request.input { EmbeddingInput::Scalar(text) => Input::Scalar(text), EmbeddingInput::Batch(texts) => Input::Batch(texts) };
        let mut service = Service::new();
        let result = service.run(embeddings::Request { target, config, purpose, input }, budget);
        self.settle(service, purpose, result, budget)
    }
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        if self.terminal { return Err(self.failure(Failure::Invalid("native-http-adapter-terminal"))); }
        let Some((mut service, purpose)) = self.pending.take() else { return Err(self.failure(Failure::Invalid("native-http-adapter-resume"))); };
        let result = service.resume(reply, budget);
        self.settle(service, purpose, result, budget)
    }
}

struct RetrievalResult {
    output: RetrievalOutput,
    _outer: Charge,
    _excess: Option<Charge>,
}
struct PendingResult {
    output: RetrievalOutput,
    reservation: Option<(Charge, usize)>,
}
impl RetrievalResult {
    fn adopt(output: RetrievalOutput, budget: &Budget, reservation: Option<(Charge, usize)>) -> Result<Self, Failure> {
        let mut pending = PendingResult { output, reservation };
        if pending.reservation.is_none() && !matches!(pending.output, RetrievalOutput::Prepared) { return Err(Failure::Invalid("native-http-preparation-output")); }
        let bytes = match &pending.output {
            RetrievalOutput::Prepared => 0,
            RetrievalOutput::Scored(values) => values.capacity().checked_mul(size_of::<risunest_memory::hanurai::ScoredDocument>())
                .ok_or(Failure::Limit("native-http-result-bytes"))?,
        };
        let admitted = pending.reservation.as_ref().map_or(0, |(_, bytes)| *bytes);
        let excess = if bytes > admitted { Some(budget.reserve_owned(bytes - admitted)?) } else { None };
        let charge = match pending.reservation.take() { Some((charge, _)) => charge, None => budget.reserve_owned(0)? };
        Ok(Self { output: pending.output, _outer: charge, _excess: excess })
    }
}

struct WorkerSession<R> {
    retrieval: R,
    executor: Executor,
}

struct PrepareInput {
    documents: Option<Vec<Text>>,
    _charge: Option<Charge>,
}
enum Turn {
    Prepare(PrepareInput),
    Query(Option<Text>),
    ResumeHttp(Option<ReplyEnvelope>),
    ResumeFailed(Option<Text>),
}
impl Turn {
    fn apply<R: Retrieval>(&mut self, session: &mut WorkerSession<R>, budget: &Budget, diagnostic: &SharedFailure) -> Result<Step<RetrievalOutput>, StageError> {
        match self {
            Self::Prepare(input) => session.retrieval.prepare(input.documents.take().unwrap(), budget),
            Self::Query(query) => session.retrieval.query(query.take().unwrap(), budget),
            Self::ResumeHttp(reply) => {
                let retrieval = &mut session.retrieval;
                session.executor.resume(reply.take().unwrap(), |reply| retrieval.resume(reply, budget))
                    .map_err(|error| stage_failure(error, diagnostic))?
            }
            Self::ResumeFailed(message) => session.retrieval.resume(HostReply::Failed { message: message.take().unwrap() }, budget),
        }
    }
}

enum WorkerResult { Done(RetrievalResult), Http(HttpRequestSpec) }
// Payloads precede the session and independent leases, including on an unreceived output.
struct WorkerOutput<R> {
    result: Option<Result<WorkerResult, StageError>>,
    session: Option<WorkerSession<R>>,
    reservation: Option<(Charge, usize)>,
    handoff: Option<Charge>,
    _operation: Operation,
}
struct WorkerTask<R> {
    turn: Turn,
    session: Option<WorkerSession<R>>,
    reservation: Option<(Charge, usize)>,
    handoff: Option<Charge>,
    operation: Operation,
    diagnostic: Arc<SharedFailure>,
}
impl<R: Retrieval> WorkerTask<R> {
    fn run(mut self) -> WorkerOutput<R> {
        let budget = self.operation.budget();
        let step = match self.operation.check() {
            Ok(()) => self.turn.apply(self.session.as_mut().unwrap(), budget, &self.diagnostic),
            Err(error) => Err(stage_failure(error, &self.diagnostic)),
        };
        let result = match self.operation.check() {
            Err(error) => { drop(step); Err(stage_failure(error, &self.diagnostic)) }
            Ok(()) => match step {
                Err(error) => Err(error),
                Ok(Step::Done(output)) => RetrievalResult::adopt(output, budget, self.reservation.take())
                    .map(WorkerResult::Done).map_err(|error| stage_failure(error, &self.diagnostic)),
                Ok(Step::NeedsHost(HostRequest::Http(request))) => Ok(WorkerResult::Http(request)),
                Ok(Step::NeedsHost(_)) => Err(StageError::Unsupported("native-http-host-request".into())),
            },
        };
        WorkerOutput { result: Some(result), session: self.session.take(), reservation: self.reservation.take(),
            handoff: self.handoff.take(), _operation: self.operation.clone() }
    }
}

struct NativeRetrieval<R> {
    session: Option<WorkerSession<R>>,
    diagnostic: Arc<SharedFailure>,
    candidates: Arc<CandidateCount>,
    operation: Operation,
}
impl<R: Retrieval + Send + 'static> NativeRetrieval<R> {
    fn new(retrieval: R, executor: Executor, diagnostic: Arc<SharedFailure>, candidates: Arc<CandidateCount>) -> Self {
        let operation = executor.operation.clone();
        Self { session: Some(WorkerSession { retrieval, executor }), diagnostic, candidates, operation }
    }
    fn terminal(&mut self, error: StageError) -> Observed<Result<RetrievalResult, StageError>> {
        self.session = None;
        Observed::new(Err(error), self.diagnostic.take(), &self.operation)
    }
    fn handoff_bytes(&self) -> Result<usize, Failure> {
        size_of::<WorkerTask<R>>().checked_add(size_of::<WorkerOutput<R>>()).ok_or(Failure::Limit("native-http-worker-bytes"))
    }
    fn handoff(&self) -> Result<Charge, Failure> {
        // Task bookkeeping is inherited; both application-owned move containers are admitted here.
        Ok(self.operation.budget().reserve_owned(self.handoff_bytes()?)?)
    }
    async fn prepare(&mut self, documents: Vec<Text>) -> Observed<Result<RetrievalResult, StageError>> {
        let mut flight = Flight::new(&self.operation);
        let mut input = PrepareInput { documents: Some(documents), _charge: None };
        let admission = (|| {
            self.operation.check()?;
            let documents = input.documents.as_ref().unwrap();
            self.candidates.record(documents.len())?;
            let bytes = documents.capacity().checked_mul(size_of::<Text>()).ok_or(Failure::Limit("native-http-input-bytes"))?;
            input._charge = Some(self.operation.budget().reserve_owned(bytes)?);
            self.handoff()
        })();
        match admission {
            Ok(handoff) => self.drive(Turn::Prepare(input), None, handoff, flight).await,
            Err(error) => { drop(input); flight.settle(); self.terminal(stage_failure(error, &self.diagnostic)) }
        }
    }
    async fn query(&mut self, query: Text) -> Observed<Result<RetrievalResult, StageError>> {
        let mut flight = Flight::new(&self.operation);
        let admission = (|| {
            self.operation.check()?;
            // Independent of other retained query outputs, before worker submission or HTTP.
            Ok::<_, Failure>((self.candidates.reserve_result()?, self.handoff()?))
        })();
        match admission {
            Ok((reservation, handoff)) => self.drive(Turn::Query(Some(query)), Some(reservation), handoff, flight).await,
            Err(error) => { drop(query); flight.settle(); self.terminal(stage_failure(error, &self.diagnostic)) }
        }
    }
    async fn worker(&mut self, turn: Turn, reservation: Option<(Charge, usize)>, handoff: Charge) -> Result<WorkerOutput<R>, StageError> {
        // Keep rejected inputs before their charge even when the session was already abandoned.
        let mut task = WorkerTask { turn, session: None, reservation, handoff: Some(handoff),
            operation: self.operation.clone(), diagnostic: self.diagnostic.clone() };
        task.session = Some(self.session.take().ok_or_else(|| StageError::Invalid("native-http-retrieval-terminal".into()))?);
        let mut handle = tokio::task::spawn_blocking(move || task.run());
        let joined = tokio::select! {
            biased;
            _ = self.operation.cancel().cancelled() => { handle.abort(); handle.await }
            result = &mut handle => result,
        };
        if let Err(error) = self.operation.check() {
            drop(joined);
            return Err(stage_failure(error, &self.diagnostic));
        }
        joined.map_err(|_| StageError::Invalid("native-http-worker-join".into()))
    }
    async fn drive(&mut self, mut turn: Turn, mut reservation: Option<(Charge, usize)>, mut handoff: Charge,
        mut flight: Flight) -> Observed<Result<RetrievalResult, StageError>>
    {
        loop {
            let mut output = match self.worker(turn, reservation, handoff).await {
                Ok(output) => output,
                Err(error) => { flight.settle(); return self.terminal(error); }
            };
            if let Err(error) = self.operation.check() {
                drop(output);
                flight.settle();
                return self.terminal(stage_failure(error, &self.diagnostic));
            }
            // A pending HTTP request remains backed by the restored pure-service session.
            self.session = output.session.take();
            let request = match output.result.take().unwrap() {
                Err(error) => { drop(output); flight.settle(); return self.terminal(error); }
                Ok(WorkerResult::Done(result)) => {
                    flight.settle();
                    return Observed::new(Ok(result), self.diagnostic.take(), &self.operation);
                }
                Ok(WorkerResult::Http(request)) => request,
            };
            turn = match self.session.as_mut().unwrap().executor.execute(request).await {
                Ok(reply) => Turn::ResumeHttp(Some(reply)),
                Err(Failure::Transport { diagnostic }) => Turn::ResumeFailed(Some(diagnostic)),
                Err(error) => { drop(output); flight.settle(); return self.terminal(stage_failure(error, &self.diagnostic)); }
            };
            reservation = output.reservation.take();
            handoff = output.handoff.take().unwrap();
        }
    }
}
