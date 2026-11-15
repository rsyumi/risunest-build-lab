use risunest_ir::stage::{EmbedRequest, HostReply, HostRequest, StageError, Step};
use risunest_ir::{Budget, Limits, OwnedVec, Text};
use risunest_memory::hanurai::{Retrieval, RetrievalOutput};
use risunest_memory::hypa_v1::*;
use std::collections::VecDeque;
use std::mem::size_of;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};

fn budget(owned: usize, work: usize, cancel: Arc<AtomicBool>) -> Budget {
    Budget::new(Limits { max_owned_bytes: owned, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
        max_values: 100_000, max_depth: 128, max_work: work }, cancel).unwrap()
}
fn ample() -> Budget { budget(16 << 20, 10_000_000, Arc::new(AtomicBool::new(false))) }
fn text(value: &str, b: &Budget) -> Text { Text::from_str(value, b).unwrap() }
fn target(b: &Budget) -> EmbeddingTarget {
    EmbeddingTarget::capture(&text("custom", b), None, &text(" https://synthetic.invalid/v1 ", b), &text(" model ", b), b).unwrap()
}
fn vector(values: &[f64], b: &Budget) -> EmbeddingVector { let mut out = OwnedVec::new(b).unwrap(); out.extend(values).unwrap(); EmbeddingVector::F64(out) }
fn batch(values: &[Option<&[f64]>], b: &Budget) -> EmbeddingBatch {
    let mut out = OwnedVec::new(b).unwrap();
    for values in values { out.push(values.map(|v| vector(v, b))).unwrap(); }
    out
}
type Durable = Arc<Mutex<Vec<(CacheKey, Vec<u32>)>>>;
struct Cache { durable: Durable, cancel_after_write: Option<Arc<AtomicBool>>, reads: Arc<Mutex<usize>> }
impl ImmediateCache for Cache {
    fn read_batch(&mut self, _: &[CacheKey], b: &Budget) -> Result<OwnedVec<Option<CacheRow>>, CacheError> {
        *self.reads.lock().unwrap() += 1;
        OwnedVec::new(b).map_err(|e| CacheError::Terminal(e.into()))
    }
    fn write_batch(&mut self, _: &EmbeddingTarget, entries: &[CacheWrite], _: &Budget) -> Result<(), CacheError> {
        if entries.iter().any(|entry| entry.key.is_none() || entry.bits.as_slice().is_empty()) { return Err(CacheError::Unavailable); }
        self.durable.lock().unwrap().extend(entries.iter().map(|entry| (entry.key.unwrap(), entry.bits.as_slice().to_vec())));
        if let Some(cancel) = &self.cancel_after_write { cancel.store(true, Ordering::Relaxed); }
        Ok(())
    }
}
struct Service {
    resumes: Arc<Mutex<usize>>, results: VecDeque<EmbeddingBatch>, pending: Option<EmbeddingBatch>, yields: u8, remaining: u8,
    requests: Arc<Mutex<Vec<(EmbeddingPurpose, bool, Vec<Vec<u16>>)>>> ,
}
impl Service {
    fn leaf(b: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        Ok(Step::NeedsHost(HostRequest::Embed(EmbedRequest { model: text("synthetic-native-service-leaf", b), texts: Vec::new() })))
    }
}
impl EmbeddingService for Service {
    fn embed(&mut self, request: EmbeddingRequest, b: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        assert_eq!(request.target.model().as_unicode(), Some("custom"));
        assert_eq!(request.target.custom_model().as_unicode(), Some("model"));
        assert_eq!(request.target.custom_endpoint().as_unicode(), Some("https://synthetic.invalid/v1"));
        let (scalar, texts) = match request.input {
            EmbeddingInput::Scalar(value) => (true, vec![value.units().collect()]),
            EmbeddingInput::Batch(values) => (false, values.as_slice().iter().map(|value| value.units().collect()).collect()),
        };
        self.requests.lock().unwrap().push((request.purpose, scalar, texts));
        let result = self.results.pop_front().expect("unplanned embedding request");
        if self.yields == 0 { Ok(Step::Done(result)) }
        else { self.pending = Some(result); self.remaining = self.yields; Self::leaf(b) }
    }
    fn resume(&mut self, reply: HostReply, b: &Budget) -> Result<Step<EmbeddingBatch>, StageError> {
        *self.resumes.lock().unwrap() += 1;
        match reply {
            HostReply::Failed { message } => { self.pending = None; return Err(StageError::Host(message.units().collect())); }
            HostReply::Cancelled => { self.pending = None; return Err(StageError::Cancelled); }
            HostReply::Embeddings(_) => {}, _ => return Err(StageError::Invalid("synthetic-leaf-reply".into())),
        }
        self.remaining -= 1;
        if self.remaining > 0 { Self::leaf(b) } else { Ok(Step::Done(self.pending.take().unwrap())) }
    }
}
fn dependencies(results: Vec<EmbeddingBatch>, yields: u8) -> (Cache, Service, Durable) {
    let durable = Arc::new(Mutex::new(Vec::new()));
    let cache = Cache { durable: durable.clone(), cancel_after_write: None, reads: Arc::new(Mutex::new(0)) };
    let service = Service { resumes: Arc::new(Mutex::new(0)), results: results.into(), pending: None, yields, remaining: 0, requests: Arc::new(Mutex::new(Vec::new())) };
    (cache, service, durable)
}
fn complete<C: ImmediateCache, E: EmbeddingService>(stage: &mut HypaV1Retrieval<C, E>, mut step: Step<RetrievalOutput>, b: &Budget) -> RetrievalOutput {
    loop { match step { Step::Done(value) => return value, Step::NeedsHost(HostRequest::Embed(request)) => {
        assert_eq!(request.model.as_unicode(), Some("synthetic-native-service-leaf"));
        step = stage.resume(HostReply::Embeddings(Vec::new()), b).unwrap();
    }, _ => panic!("wrong leaf") } }
}
#[test]
fn yielding_service_preserves_purpose_shape_and_fresh_precision() {
    for yields in [0, 2] {
        let b = ample();
        let (cache, service, durable) = dependencies(vec![batch(&[Some(&[1.00000001]), Some(&[1.00000002])], &b), batch(&[Some(&[1.0])], &b)], yields);
        let requests = service.requests.clone();
        let mut stage = HypaV1Retrieval::new(target(&b), cache, service, 1).unwrap();
        let step = stage.prepare(vec![text("first", &b), text("second", &b)], &b).unwrap();
        assert!(matches!(complete(&mut stage, step, &b), RetrievalOutput::Prepared));
        assert_eq!(durable.lock().unwrap().iter().map(|(_, bits)| bits[0]).collect::<Vec<_>>(), vec![1f32.to_bits(); 2]);
        let step = stage.query(text("question", &b), &b).unwrap();
        let RetrievalOutput::Scored(results) = complete(&mut stage, step, &b) else { panic!() };
        assert_eq!(results.iter().map(|entry| entry.document.as_ref().unwrap().as_unicode().unwrap()).collect::<Vec<_>>(), ["second", "first"]);
        assert_eq!(results[0].score, 1.00000002);
        let requests = requests.lock().unwrap();
        assert_eq!((requests[0].0, requests[0].1, requests[0].2.len()), (EmbeddingPurpose::Document, false, 2));
        assert_eq!((requests[1].0, requests[1].1, requests[1].2.len()), (EmbeddingPurpose::Query, true, 1));
    }
}
#[test]
fn late_absent_vector_prevents_all_writes_but_late_missing_key_keeps_prior_batch() {
    for absent in [true, false] {
        let b = ample();
        let result = if absent { batch(&[Some(&[1.0]), Some(&[2.0]), None], &b) }
            else { batch(&[Some(&[1.0]), Some(&[2.0]), Some(&[3.0])], &b) };
        let (cache, service, durable) = dependencies(vec![result, batch(&[Some(&[1.0])], &b)], 0);
        let mut stage = HypaV1Retrieval::new(target(&b), cache, service, 2).unwrap();
        let result = stage.prepare(vec![text("first", &b), text("second", &b)], &b);
        if absent {
            assert!(matches!(result, Err(StageError::Invalid(_))));
            assert!(durable.lock().unwrap().is_empty());
            assert!(matches!(stage.query(text("q", &b), &b), Err(StageError::Invalid(_))));
        } else {
            assert!(matches!(result, Ok(Step::Done(RetrievalOutput::Prepared))));
            assert_eq!(durable.lock().unwrap().len(), 2);
            let Step::Done(RetrievalOutput::Scored(results)) = stage.query(text("q", &b), &b).unwrap() else { panic!() };
            assert!(results[0].document.is_none());
            assert_eq!(results[0].score, 3.0);
        }
    }
}
#[test]
fn terminal_dependencies_and_cancellation_do_not_undo_cache_writes_or_resume_again() {
    for cancel_after_write in [false, true] {
        let cancel = Arc::new(AtomicBool::new(false));
        let b = budget(16 << 20, 1_000_000, cancel.clone());
        let (mut cache, service, durable) = dependencies(vec![batch(&[Some(&[1.0])], &b), batch(&[Some(&[1.0])], &b)], 2);
        if cancel_after_write { cache.cancel_after_write = Some(cancel.clone()); }
        let mut stage = HypaV1Retrieval::new(target(&b), cache, service, 2).unwrap();
        assert!(matches!(stage.prepare(vec![text("first", &b)], &b), Ok(Step::NeedsHost(_))));
        assert!(matches!(stage.resume(HostReply::Embeddings(Vec::new()), &b), Ok(Step::NeedsHost(_))));
        let prepared = stage.resume(HostReply::Embeddings(Vec::new()), &b);
        assert_eq!(durable.lock().unwrap().len(), 1);
        if cancel_after_write { assert!(matches!(prepared, Err(StageError::Cancelled))); cancel.store(false, Ordering::Relaxed); }
        else {
            assert!(matches!(prepared, Ok(Step::Done(RetrievalOutput::Prepared))));
            assert!(matches!(stage.query(text("q", &b), &b), Ok(Step::NeedsHost(_))));
            assert!(matches!(stage.resume(HostReply::Failed { message: Text::from_units(&[0xd800], &b).unwrap() }, &b), Err(StageError::Host(message)) if message == [0xd800]));
        }
        assert!(matches!(stage.resume(HostReply::Embeddings(Vec::new()), &b), Err(StageError::Invalid(_))));
        assert!(matches!(stage.prepare(Vec::new(), &b), Err(StageError::Invalid(_))));
        assert_eq!(durable.lock().unwrap().len(), 1);
    }
}
#[test]
fn normalization_budget_fails_before_cache_io_and_large_budget_completes_same_input() {
    for work in [20_000, 2_000_000] {
        let b = budget(4 << 20, work, Arc::new(AtomicBool::new(false)));
        let value = text(&format!("a{}", "\u{301}".repeat(500)), &b);
        let (cache, service, _) = dependencies(vec![batch(&[Some(&[1.0])], &b)], 0);
        let reads = cache.reads.clone();
        let mut stage = HypaV1Retrieval::new(target(&b), cache, service, 1).unwrap();
        let result = stage.prepare(vec![value], &b);
        if work == 20_000 { assert!(matches!(result, Err(StageError::Limit(_)))); assert_eq!(*reads.lock().unwrap(), 0); }
        else { assert!(matches!(result, Ok(Step::Done(RetrievalOutput::Prepared)))); assert_eq!(*reads.lock().unwrap(), 1); }
    }
}

#[test]
fn normalization_scratch_is_charged_before_allocation() {
    for owned in [8192, 1 << 20] {
        let b = budget(owned, 2_000_000, Arc::new(AtomicBool::new(false)));
        let (cache, service, _) = dependencies(vec![batch(&[Some(&[1.0])], &b)], 0);
        let reads = cache.reads.clone();
        let mut stage = HypaV1Retrieval::new(target(&b), cache, service, 1).unwrap();
        let value = text(&"a".repeat(1000), &b);
        let result = stage.prepare(vec![value], &b);
        if owned == 8192 {
            assert!(matches!(result, Err(StageError::Limit(kind)) if kind == "owned-bytes"));
            assert_eq!(*reads.lock().unwrap(), 0);
        } else { assert!(matches!(result, Ok(Step::Done(RetrievalOutput::Prepared)))); }
    }
}

#[test]
fn cancelled_suspension_does_not_resume_the_dependency() {
    let cancel = Arc::new(AtomicBool::new(false));
    let b = budget(4 << 20, 1_000_000, cancel.clone());
    let (cache, service, durable) = dependencies(vec![batch(&[Some(&[1.0])], &b)], 2);
    let resumed = service.resumes.clone();
    let mut stage = HypaV1Retrieval::new(target(&b), cache, service, 1).unwrap();
    assert!(matches!(stage.prepare(vec![text("document", &b)], &b), Ok(Step::NeedsHost(_))));
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(stage.resume(HostReply::Embeddings(Vec::new()), &b), Err(StageError::Cancelled)));
    assert_eq!(*resumed.lock().unwrap(), 0);
    assert!(durable.lock().unwrap().is_empty());
    cancel.store(false, Ordering::Relaxed);
    assert!(matches!(stage.resume(HostReply::Embeddings(Vec::new()), &b), Err(StageError::Invalid(_))));
}

#[test]
fn terminal_cache_error_is_not_an_ordinary_cache_miss() {
    struct ReadFailure { terminal: bool }
    impl ImmediateCache for ReadFailure {
        fn read_batch(&mut self, _: &[CacheKey], _: &Budget) -> Result<OwnedVec<Option<CacheRow>>, CacheError> {
            Err(if self.terminal { CacheError::Terminal(StageError::Limit("synthetic-cache-limit".into())) } else { CacheError::Unavailable })
        }
        fn write_batch(&mut self, _: &EmbeddingTarget, _: &[CacheWrite], _: &Budget) -> Result<(),CacheError> { Ok(()) }
    }
    for terminal in [false, true] {
        let b = ample();
        let (_, service, _) = dependencies(vec![batch(&[Some(&[1.0])], &b)], 0);
        let requests = service.requests.clone();
        let mut stage = HypaV1Retrieval::new(target(&b), ReadFailure { terminal }, service, 1).unwrap();
        let result = stage.prepare(vec![text("document", &b)], &b);
        if terminal {
            assert!(matches!(result, Err(StageError::Limit(kind)) if kind == "synthetic-cache-limit"));
            assert!(requests.lock().unwrap().is_empty());
        } else {
            assert!(matches!(result, Ok(Step::Done(RetrievalOutput::Prepared))));
            assert_eq!(requests.lock().unwrap().len(), 1);
        }
    }
}

#[test]
fn large_short_embedding_result_is_charged_through_suspension_and_cache_serialization() {
    for (owned, cancel_suspended) in [(100_000, false), (256 << 10, false), (256 << 10, true)] {
        let cancel = Arc::new(AtomicBool::new(false));
        let b = budget(owned, 1_000_000, cancel.clone());
        let values = vec![1.0; 4097];
        let (cache, service, durable) = dependencies(vec![batch(&[Some(&values)], &b), batch(&[], &b), batch(&[Some(&[1.0])], &b)], 2);
        let requests = service.requests.clone();
        let resumed = service.resumes.clone();
        let mut stage = HypaV1Retrieval::new(target(&b), cache, service, 1024).unwrap();
        let documents = (0..51).map(|index| text(&format!("doc{index}"), &b)).collect();
        assert!(matches!(stage.prepare(documents, &b), Ok(Step::NeedsHost(_))));
        assert!(matches!(stage.resume(HostReply::Embeddings(Vec::new()), &b), Ok(Step::NeedsHost(_))));
        assert!(matches!(stage.resume(HostReply::Embeddings(Vec::new()), &b), Ok(Step::NeedsHost(_))));
        assert_eq!(requests.lock().unwrap().iter().map(|request| request.2.len()).collect::<Vec<_>>(), [50, 1]);
        let suspended_owned = b.owned_bytes();
        assert!(suspended_owned >= values.len() * size_of::<f64>());
        if cancel_suspended {
            cancel.store(true, Ordering::Relaxed);
            assert!(matches!(stage.resume(HostReply::Embeddings(Vec::new()), &b), Err(StageError::Cancelled)));
            assert_eq!(*resumed.lock().unwrap(), 2);
            assert!(durable.lock().unwrap().is_empty());
            assert!(suspended_owned - b.owned_bytes() >= values.len() * size_of::<f64>());
            cancel.store(false, Ordering::Relaxed);
            assert!(matches!(stage.resume(HostReply::Embeddings(Vec::new()), &b), Err(StageError::Invalid(_))));
        } else {
            assert!(matches!(stage.resume(HostReply::Embeddings(Vec::new()), &b), Ok(Step::NeedsHost(_))));
            let prepared = stage.resume(HostReply::Embeddings(Vec::new()), &b);
            assert_eq!(*resumed.lock().unwrap(), 4);
            if owned == 100_000 {
                assert!(matches!(prepared, Err(StageError::Limit(kind)) if kind == "owned-bytes"));
                assert!(durable.lock().unwrap().is_empty());
                assert!(suspended_owned - b.owned_bytes() >= values.len() * size_of::<f64>());
                assert!(matches!(stage.resume(HostReply::Embeddings(Vec::new()), &b), Err(StageError::Invalid(_))));
            } else {
                assert!(matches!(prepared, Ok(Step::Done(RetrievalOutput::Prepared))));
                assert_eq!(durable.lock().unwrap().iter().map(|(_, bits)| bits.clone()).collect::<Vec<_>>(), [vec![1f32.to_bits(); values.len()]]);
                let step = stage.query(text("q", &b), &b).unwrap();
                let RetrievalOutput::Scored(results) = complete(&mut stage, step, &b) else { panic!() };
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].document.as_ref().unwrap().as_unicode(), Some("doc0"));
                assert_eq!(results[0].score, 1.0);
            }
        }
        drop(stage);
        assert_eq!(b.owned_bytes(), Budget::context_bytes());
    }
}

#[test]
fn long_unequal_query_vector_obeys_per_dimension_work_limit() {
    for work in [6000, 1_000_000] {
        let b = budget(256 << 10, work, Arc::new(AtomicBool::new(false)));
        let values = vec![1.0; 4097];
        let (cache, service, durable) = dependencies(vec![batch(&[Some(&[1.0])], &b), batch(&[Some(&values)], &b)], 0);
        let requests = service.requests.clone();
        let mut stage = HypaV1Retrieval::new(target(&b), cache, service, 1024).unwrap();
        assert!(matches!(stage.prepare(vec![text("doc", &b)], &b), Ok(Step::Done(RetrievalOutput::Prepared))));
        let before_query = b.owned_bytes();
        let result = stage.query(text("q", &b), &b);
        assert_eq!(requests.lock().unwrap().iter().map(|request| request.0).collect::<Vec<_>>(), [EmbeddingPurpose::Document, EmbeddingPurpose::Query]);
        assert_eq!(durable.lock().unwrap().iter().map(|(_, bits)| bits.clone()).collect::<Vec<_>>(), [vec![1f32.to_bits()]]);
        if work == 6000 {
            assert!(matches!(result, Err(StageError::Limit(kind)) if kind == "work"));
            assert!(before_query - b.owned_bytes() >= values.len() * size_of::<f64>());
            assert!(matches!(stage.resume(HostReply::Embeddings(Vec::new()), &b), Err(StageError::Invalid(_))));
        } else {
            let Ok(Step::Done(RetrievalOutput::Scored(results))) = result else { panic!() };
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].document.as_ref().unwrap().as_unicode(), Some("doc"));
            assert!(results[0].score.is_nan());
        }
        drop(stage);
        assert_eq!(b.owned_bytes(), Budget::context_bytes());
    }
}
