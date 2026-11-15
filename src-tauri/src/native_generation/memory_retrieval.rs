use super::memory_http::{Operation, arc_bytes};
use risunest_ir::{Budget, Charge, OwnedVec, Text};
use risunest_ir::stage::{HostReply, StageError, Step};
use risunest_memory::{hanurai::{Retrieval, RetrievalOutput, ScoredDocument}, hypa_v1::{EmbeddingBatch,
    EmbeddingInput, EmbeddingPurpose, EmbeddingRequest, EmbeddingService, EmbeddingTarget, EmbeddingVector,
    HypaV1Retrieval, ImmediateCache}};
use risunest_providers::embeddings::{self, CapturedConfig, Failure, Input, Output, Purpose, RemoteTarget, Service};
use std::{mem::size_of, sync::{Arc, atomic::{AtomicUsize, Ordering}}};

pub(crate) struct CandidateCount {
    pub(crate) rows: AtomicUsize,
    _charge: Charge,
    operation: Operation,
}
impl CandidateCount {
    pub(crate) fn new(operation: &Operation) -> Result<Arc<Self>, Failure> {
        let charge = operation.budget().reserve_owned(arc_bytes::<Self>()?)?;
        Ok(Arc::new(Self { rows: AtomicUsize::new(0), _charge: charge, operation: operation.clone() }))
    }
    pub(crate) fn record(&self, rows: usize) -> Result<(), Failure> {
        self.operation.budget().work(1)?;
        self.rows.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| current.checked_add(rows))
            .map_err(|_| Failure::Limit("native-http-candidate-count"))?;
        Ok(())
    }
    pub(crate) fn reserve_result(&self) -> Result<(Charge, usize), Failure> {
        let bytes = self.rows.load(Ordering::Relaxed).checked_mul(size_of::<ScoredDocument>())
            .ok_or(Failure::Limit("native-http-result-bytes"))?;
        Ok((self.operation.budget().reserve_owned(bytes)?, bytes))
    }
}

pub(crate) fn terminal_failure(error: Failure) -> StageError {
    match error {
        Failure::Invalid(kind) => StageError::Invalid(kind.into()),
        Failure::Unsupported(kind) => StageError::Unsupported(kind.into()),
        Failure::Limit(kind) => StageError::Limit(kind.into()),
        Failure::Cancelled => StageError::Cancelled,
        _ => StageError::Invalid("native-memory-failure-class".into()),
    }
}

fn host_units(text: &Text, budget: &Budget) -> Result<(StageError, Charge), StageError> {
    budget.work(text.len_units())?;
    let bytes = text.len_units().checked_mul(size_of::<u16>()).ok_or_else(|| StageError::Limit("owned-bytes".into()))?;
    let mut charge = budget.reserve_owned(bytes)?;
    let mut units = Vec::new();
    units.try_reserve_exact(text.len_units()).map_err(|_| StageError::Limit("allocation".into()))?;
    if units.capacity() > text.len_units() {
        charge = budget.reserve_owned(units.capacity().checked_mul(size_of::<u16>()).ok_or_else(|| StageError::Limit("owned-bytes".into()))?)?;
    }
    units.extend(text.units());
    Ok((StageError::Host(units), charge))
}

pub(crate) struct Adapter<C> {
    capture: C,
    pending: Option<(Service, Purpose)>,
    terminal: bool,
    failure: Option<Failure>,
    host_charge: Option<Charge>,
    candidates: Arc<CandidateCount>,
}
impl<C: Fn(&Budget) -> Result<CapturedConfig, Failure>> Adapter<C> {
    pub(crate) fn new(capture: C, candidates: Arc<CandidateCount>) -> Self {
        Self { capture, pending: None, terminal: false, failure: None, host_charge: None, candidates }
    }
    pub(crate) fn take_failure(&mut self) -> Option<Failure> { self.failure.take() }
    fn failure(&mut self, error: Failure, budget: &Budget) -> StageError {
        self.terminal = true;
        self.pending = None;
        let result = match &error {
            Failure::Remote { diagnostic, .. } | Failure::Decode { diagnostic } | Failure::Transport { diagnostic } => host_units(diagnostic, budget),
            Failure::Invalid("embedding-custom-url") => Text::from_str("Embedding custom URL is empty", budget)
                .map_err(StageError::from).and_then(|text| host_units(&text, budget)),
            _ => return terminal_failure(error),
        };
        match result {
            Ok((stage, charge)) => { self.host_charge = Some(charge); self.failure = Some(error); stage }
            Err(error) => error,
        }
    }
    pub(crate) fn transfer(mut output: Output, budget: &Budget) -> Result<EmbeddingBatch, StageError> {
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
                if purpose == Purpose::Document { self.candidates.record(output.as_slice().len()).map_err(|error| self.failure(error, budget))?; }
                match Self::transfer(output, budget) {
                    Ok(output) => Ok(Step::Done(output)),
                    Err(error) => { self.terminal = true; Err(error) }
                }
            }
            Err(error) => Err(self.failure(error, budget)),
        }
    }
}
impl<C: Fn(&Budget) -> Result<CapturedConfig, Failure>> EmbeddingService for Adapter<C> {
    fn embed(&mut self, request: EmbeddingRequest, budget: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        budget.check()?;
        if self.terminal || self.pending.is_some() { return Err(self.failure(Failure::Invalid("native-http-adapter-pending"), budget)); }
        let config = (self.capture)(budget).map_err(|error| self.failure(error, budget))?;
        let target = RemoteTarget::capture(request.target.model(), None, Some(request.target.custom_endpoint()), None, budget)
            .map_err(|error| self.failure(error, budget))?;
        let purpose = match request.purpose { EmbeddingPurpose::Document => Purpose::Document, EmbeddingPurpose::Query => Purpose::Query };
        let input = match request.input { EmbeddingInput::Scalar(text) => Input::Scalar(text), EmbeddingInput::Batch(texts) => Input::Batch(texts) };
        let mut service = Service::new();
        let result = service.run(embeddings::Request { target, config, purpose, input }, budget);
        self.settle(service, purpose, result, budget)
    }
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        budget.check()?;
        if self.terminal { return Err(self.failure(Failure::Invalid("native-http-adapter-terminal"), budget)); }
        let Some((mut service, purpose)) = self.pending.take() else { return Err(self.failure(Failure::Invalid("native-http-adapter-resume"), budget)); };
        let result = service.resume(reply, budget);
        self.settle(service, purpose, result, budget)
    }
}

/// Synchronous policy dependency. The four memory policies consume each score vector before querying again.
pub(crate) struct PolicyRetrieval<C, F> {
    inner: HypaV1Retrieval<C, Adapter<F>>,
    prepared: bool,
    allowance: Option<(Charge, usize)>,
    excess: Option<(Charge, usize)>,
    host_charge: Option<Charge>,
    candidates: Arc<CandidateCount>,
}
impl<C: ImmediateCache, F: Fn(&Budget) -> Result<CapturedConfig, Failure>> PolicyRetrieval<C, F> {
    pub(crate) fn new(target: EmbeddingTarget, cache: C, capture: F, cache_batch: usize, operation: &Operation) -> Result<Self, StageError> {
        let candidates = CandidateCount::new(operation).map_err(terminal_failure)?;
        let inner = HypaV1Retrieval::new(target, cache, Adapter::new(capture, candidates.clone()), cache_batch)?;
        Ok(Self { inner, prepared: false, allowance: None, excess: None, host_charge: None, candidates })
    }
    fn accept(&mut self, result: Result<Step<RetrievalOutput>, StageError>, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        budget.check()?;
        match result {
            Ok(Step::Done(RetrievalOutput::Scored(values))) => {
                let bytes = values.capacity().checked_mul(size_of::<ScoredDocument>()).ok_or_else(|| StageError::Limit("native-http-result-bytes".into()))?;
                let admitted = self.allowance.as_ref().ok_or_else(|| StageError::Invalid("native-memory-score-state".into()))?.1;
                let extra = bytes.saturating_sub(admitted);
                if extra > self.excess.as_ref().map_or(0, |(_, bytes)| *bytes) { self.excess = Some((budget.reserve_owned(extra)?, extra)); }
                Ok(Step::Done(RetrievalOutput::Scored(values)))
            }
            Err(StageError::Invalid(kind)) if matches!(kind.as_ref(), "hypa-embedding-vector" | "hypa-query-vector") => {
                let text = Text::from_str("Embedding response has no vector", budget)?;
                let (error, charge) = host_units(&text, budget)?;
                self.host_charge = Some(charge);
                Err(error)
            }
            other => other,
        }
    }
}
impl<C: ImmediateCache, F: Fn(&Budget) -> Result<CapturedConfig, Failure>> Retrieval for PolicyRetrieval<C, F> {
    fn prepare(&mut self, documents: Vec<Text>, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        budget.check()?;
        if self.prepared { return Err(StageError::Invalid("hypa-v1-state".into())); }
        self.prepared = true;
        self.candidates.record(documents.len()).map_err(terminal_failure)?;
        let result = self.inner.prepare(documents, budget);
        self.accept(result, budget)
    }
    fn query(&mut self, query: Text, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        budget.check()?;
        if self.allowance.is_none() { self.allowance = Some(self.candidates.reserve_result().map_err(terminal_failure)?); }
        let result = self.inner.query(query, budget);
        self.accept(result, budget)
    }
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        budget.check()?;
        let result = self.inner.resume(reply, budget);
        self.accept(result, budget)
    }
}
