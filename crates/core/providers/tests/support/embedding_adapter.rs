use risunest_ir::stage::{HostReply, StageError, Step};
use risunest_ir::{Budget, OwnedVec, Text};
use risunest_memory::hypa_v1::{EmbeddingBatch, EmbeddingInput, EmbeddingPurpose, EmbeddingRequest, EmbeddingService, EmbeddingVector};
use risunest_providers::embeddings::{CapturedConfig, Failure, Input, Output, Purpose, RemoteTarget, Request, Service};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct Settings { pub custom_model: Option<String>, pub custom_key: Option<String> }
pub struct Adapter {
    pub settings: Arc<Mutex<Settings>>,
    pub diagnostic: Arc<Mutex<Option<Failure>>>,
    pub purposes: Arc<Mutex<Vec<Purpose>>>,
    pub moves: Arc<Mutex<Vec<(usize, usize)>>>,
    pending: Option<Service>,
}
impl Adapter {
    pub fn new(settings: Arc<Mutex<Settings>>) -> Self {
        Self { settings, diagnostic: Arc::new(Mutex::new(None)), purposes: Arc::new(Mutex::new(Vec::new())), moves: Arc::new(Mutex::new(Vec::new())), pending: None }
    }
    fn failure(&mut self, error: Failure) -> StageError {
        match error {
            Failure::Invalid(kind) => StageError::Invalid(kind.into()), Failure::Unsupported(kind) => StageError::Unsupported(kind.into()),
            Failure::Limit(kind) => StageError::Limit(kind.into()), Failure::Cancelled => StageError::Cancelled,
            diagnostic => { *self.diagnostic.lock().unwrap() = Some(diagnostic); StageError::Invalid("embedding-provider-failure".into()) }
        }
    }
    fn transfer(&mut self, mut output: Output, b: &Budget) -> Result<EmbeddingBatch, StageError> {
        let mut result = OwnedVec::new(b)?;
        for slot in output.as_mut_slice() {
            b.work(1)?;
            let vector = slot.take();
            let before = vector.as_ref().map(|v| v.as_slice().as_ptr() as usize);
            result.push(vector.map(EmbeddingVector::F64))?;
            if let Some(before) = before {
                let Some(EmbeddingVector::F64(values)) = result.as_slice().last().unwrap() else { panic!() };
                self.moves.lock().unwrap().push((before, values.as_slice().as_ptr() as usize));
            }
        }
        Ok(result)
    }
    fn settle(&mut self, service: Service, result: Result<Step<Output>, Failure>, b: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        match result {
            Ok(Step::NeedsHost(request)) => { self.pending = Some(service); Ok(Step::NeedsHost(request)) }
            Ok(Step::Done(output)) => Ok(Step::Done(self.transfer(output, b)?)),
            Err(error) => Err(self.failure(error)),
        }
    }
}
impl EmbeddingService for Adapter {
    fn embed(&mut self, request: EmbeddingRequest, b: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        if self.pending.take().is_some() { return Err(StageError::Invalid("embedding-adapter-pending".into())); }
        let current = self.settings.lock().unwrap();
        let model = current.custom_model.as_ref().map(|s| Text::from_str(s, b)).transpose()?;
        let key = current.custom_key.as_ref().map(|s| Text::from_str(s, b)).transpose()?;
        let config = CapturedConfig::capture(model.as_ref(), key.as_ref(), None, None, b);
        drop(current);
        let config = config.map_err(|e| self.failure(e))?;
        let target = RemoteTarget::capture(request.target.model(), None, Some(request.target.custom_endpoint()), None, b).map_err(|e| self.failure(e))?;
        let purpose = match request.purpose { EmbeddingPurpose::Document => Purpose::Document, EmbeddingPurpose::Query => Purpose::Query };
        self.purposes.lock().unwrap().push(purpose);
        let input = match request.input { EmbeddingInput::Scalar(value) => Input::Scalar(value), EmbeddingInput::Batch(values) => Input::Batch(values) };
        let mut service = Service::new();
        let result = service.run(Request { target, config, purpose, input }, b);
        self.settle(service, result, b)
    }
    fn resume(&mut self, reply: HostReply, b: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        let mut service = self.pending.take().ok_or_else(|| StageError::Invalid("embedding-adapter-resume".into()))?;
        let result = service.resume(reply, b);
        self.settle(service, result, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::Limits;
    use std::mem::size_of;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn moving_vectors_admits_both_outer_containers_and_releases_failures() {
        const ROWS: usize = 64;
        for generous in [false, true] {
            let b = Budget::new(Limits { max_owned_bytes: 1 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
                max_values: 10_000, max_depth: 16, max_work: 1_000_000 }, Arc::new(AtomicBool::new(false))).unwrap();
            let mut input = OwnedVec::new(&b).unwrap();
            for _ in 0..ROWS { let mut vector = OwnedVec::new(&b).unwrap(); vector.push(1.00000001).unwrap(); input.push(Some(vector)).unwrap(); }
            let headroom = if generous { 3 * ROWS * size_of::<Option<EmbeddingVector>>() } else { size_of::<Option<EmbeddingVector>>() };
            let held = b.limits().max_owned_bytes - b.owned_bytes() - headroom;
            let caller = b.reserve_owned(held).unwrap();
            let mut adapter = Adapter::new(Arc::new(Mutex::new(Settings::default())));
            let result = adapter.transfer(input, &b);
            if generous {
                let output = result.as_ref().unwrap();
                assert_eq!(output.as_slice().len(), ROWS);
                for row in output.as_slice() { let Some(EmbeddingVector::F64(values)) = row else { panic!() }; assert_eq!(values.as_slice(), [1.00000001]); }
                let moves = adapter.moves.lock().unwrap();
                assert_eq!(moves.len(), ROWS);
                assert!(moves.iter().all(|(from, to)| from == to));
            } else { assert!(matches!(result, Err(StageError::Limit(ref kind)) if kind == "owned-bytes")); }
            drop(result); drop(adapter);
            assert_eq!(b.owned_bytes(), Budget::context_bytes() + held);
            drop(caller);
            assert_eq!(b.owned_bytes(), Budget::context_bytes());
        }
    }
}
