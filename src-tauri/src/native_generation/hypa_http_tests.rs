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
use super::memory_http::{self as product_http, Limits, Phase, ReplyEnvelope, ParsedUrl, ParseTask, PullResult};
use super::memory_retrieval::{self as product_retrieval, CandidateCount, PolicyRetrieval};

fn isolate_loopback_test(qualified: &str) -> bool {
    const CHILD: &str = "RISUNEST_MEMORY_LOOPBACK_CHILD";
    let exact = qualified.split_once("::").expect("qualified test name").1;
    if std::env::var(CHILD).as_deref() == Ok(exact) { return false; }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child.args(["--exact", exact, "--nocapture", "--test-threads=1"]);
    for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy", "REQUEST_METHOD"] { child.env_remove(name); }
    child.env("NO_PROXY", "*,127.0.0.0/8,::1").env("no_proxy", "*,127.0.0.0/8,::1").env(CHILD, exact);
    let output = child.output().expect("isolated exact-test child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "isolated {exact}: {stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    let summary = stdout.lines().rev().find(|line| line.starts_with("test result:"));
    assert!(summary.is_some_and(|line| line.starts_with("test result: ok. 1 passed; 0 failed;")), "exact child did not execute {exact}: {stdout}");
    print!("{stdout}");
    true
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

async fn parse_url(raw: String, maximum: usize, operation: &Operation, barrier: Option<Arc<ParseBarrier>>) -> Result<ParsedUrl, Failure> {
    parse_url_with_stop(raw, maximum, operation, barrier, None).await
}
async fn parse_url_with_stop(raw: String, maximum: usize, operation: &Operation, barrier: Option<Arc<ParseBarrier>>, stop: Option<&Cancellation>) -> Result<ParsedUrl, Failure> {
    let task = ParseTask::new(raw, maximum, operation, stop)?;
    let submitted = barrier.clone();
    let handle = tokio::task::spawn_blocking(move || {
        if let Some(barrier) = &barrier { barrier.pause(PauseAt::BeforeParse); }
        let parsed = task.run();
        if let Some(barrier) = &barrier { barrier.pause(PauseAt::AfterParse); }
        parsed
    });
    if let Some(submitted) = submitted { submitted.submitted.notify_one(); }
    product_http::settle_parse(handle, operation, stop).await
}

struct HttpSource {
    inner: product_http::HttpSource,
    tail_pending: Option<Arc<Notify>>,
}
impl ChunkSource for HttpSource {
    fn pull<'life0, 'life1, 'life2, 'async_trait>(&'life0 mut self, maximum: usize,
        cancel: &'life1 Cancellation, budget: &'life2 Budget)
        -> Pin<Box<dyn Future<Output = PullResult> + Send + 'async_trait>>
        where 'life0: 'async_trait, 'life1: 'async_trait, 'life2: 'async_trait, Self: 'async_trait
    { self.inner.pull(maximum, cancel, budget) }
    fn poll_native_finish(&mut self, cx: &mut Context<'_>, cancel: &Cancellation, budget: &Budget) -> Poll<Result<(), DecodeError>> {
        let result = self.inner.poll_native_finish(cx, cancel, budget);
        if result.is_pending() { if let Some(pending) = &self.tail_pending { pending.notify_one(); } }
        result
    }
}

#[derive(Default)]
struct DecoderObservation { body_progress: Option<Arc<Notify>>, tail_pending: Option<Arc<Notify>> }
async fn collect_observed(decoder: &mut product_http::DecoderOwner, response: Response, gzip: bool, operation: &Operation,
    limits: Limits, observation: &DecoderObservation, stop: Option<&Cancellation>) -> Result<Buffer, Failure>
{
    decoder.pipeline = None;
    let mut output = Buffer::new(operation, limits.response_bytes, limits.copy_quantum)?;
    let source = product_http::HttpSource::new(response, operation, decoder.source_budget(), limits.decode.max_encoded_bytes)?;
    decoder.start(Box::new(HttpSource { inner: source, tail_pending: observation.tail_pending.clone() }), gzip, limits)?;
    let result = async {
        while decoder.collect_step(&mut output, operation, stop).await? {
            if let Some(progress) = &observation.body_progress { progress.notify_one(); }
        }
        operation.check()
    }.await;
    decoder.pipeline = None;
    result?;
    Ok(output)
}
struct DecoderOwner { inner: product_http::DecoderOwner }
impl std::ops::Deref for DecoderOwner { type Target = product_http::DecoderOwner; fn deref(&self) -> &Self::Target { &self.inner } }
impl std::ops::DerefMut for DecoderOwner { fn deref_mut(&mut self) -> &mut Self::Target { &mut self.inner } }
impl DecoderOwner {
    fn new(operation: &Operation, limits: Limits) -> Result<Self, Failure> { Ok(Self { inner: product_http::DecoderOwner::new(operation, limits)? }) }
    async fn collect(&mut self, response: Response, gzip: bool, operation: &Operation, limits: Limits) -> Result<Buffer, Failure> {
        self.inner.collect(response, gzip, operation, limits, None).await
    }
}

struct Executor {
    inner: product_http::Executor,
    parser_barrier: Option<Arc<ParseBarrier>>,
    observation: DecoderObservation,
}
impl std::ops::Deref for Executor { type Target = product_http::Executor; fn deref(&self) -> &Self::Target { &self.inner } }
impl std::ops::DerefMut for Executor { fn deref_mut(&mut self) -> &mut Self::Target { &mut self.inner } }
impl Executor {
    fn new(operation: Operation, limits: Limits) -> Result<Self, Failure> {
        Ok(Self { inner: product_http::Executor::new(operation, limits)?, parser_barrier: None, observation: DecoderObservation::default() })
    }
    async fn execute(&mut self, request: HttpRequestSpec) -> Result<ReplyEnvelope, Failure> { self.execute_with_stop(request, None).await }
    async fn execute_with_stop(&mut self, mut request: HttpRequestSpec, stop: Option<&Cancellation>) -> Result<ReplyEnvelope, Failure> {
        if self.parser_barrier.is_none() && self.observation.body_progress.is_none() && self.observation.tail_pending.is_none() {
            return self.inner.execute(request, stop).await;
        }
        let (generation, mut flight) = self.inner.begin()?;
        let operation = self.inner.operation.clone();
        let result = async {
            product_http::Executor::admit_shape(&request)?;
            let parsed = parse_url_with_stop(std::mem::take(&mut request.url), self.inner.limits.url_bytes, &operation, self.parser_barrier.clone(), stop).await?;
            let incoming = self.inner.send(request, parsed, stop).await?;
            let product_http::Incoming { status, response, gzip } = incoming;
            let Some(response) = response else { return Ok((status, Buffer::new(&operation, self.inner.limits.response_bytes, self.inner.limits.copy_quantum)?)); };
            let output = collect_observed(&mut self.inner.decoder, response, gzip, &operation, self.inner.limits, &self.observation, stop).await
                .map_err(|error| match error { Failure::Invalid("native-http-decode") => transport("HTTP response body could not be read", operation.budget()), other => other })?;
            Ok((status, output))
        }.await;
        flight.settle();
        self.inner.publish(generation, result, stop)
    }
    fn resume<T>(&mut self, reply: ReplyEnvelope, resume: impl FnOnce(HostReply) -> T) -> Result<T, Failure> { self.inner.resume(reply, resume) }
}

struct Adapter<C> { inner: product_retrieval::Adapter<C>, diagnostic: Arc<SharedFailure> }
impl<C: Fn(&Budget) -> Result<CapturedConfig, Failure>> Adapter<C> {
    fn new(capture: C, diagnostic: Arc<SharedFailure>, candidates: Arc<CandidateCount>) -> Self { Self { inner: product_retrieval::Adapter::new(capture, candidates), diagnostic } }
    fn observe(&mut self, result: Result<Step<EmbeddingBatch>, StageError>) -> Result<Step<EmbeddingBatch>, StageError> {
        if let Some(error) = self.inner.take_failure() { self.diagnostic.set(error); }
        result
    }
}
impl<C: Fn(&Budget) -> Result<CapturedConfig, Failure>> EmbeddingService for Adapter<C> {
    fn embed(&mut self, request: EmbeddingRequest, budget: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        let result = self.inner.embed(request, budget); self.observe(result)
    }
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        let result = self.inner.resume(reply, budget); self.observe(result)
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
        Observed::failed(error, self.diagnostic.take(), &self.operation)
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
