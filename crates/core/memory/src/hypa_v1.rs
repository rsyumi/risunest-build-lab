//! Hypa-v1 preparation and retrieval over immediate cache IO and resumable native embedding services.
mod key;
use crate::hanurai::{Retrieval, RetrievalOutput, ScoredDocument};
use risunest_ir::stage::{HostReply, StageError, Step};
use risunest_ir::{Budget, Charge, OwnedVec, Text};
use std::collections::HashMap;
use std::mem::size_of;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey(pub [u8; 32]);
impl CacheKey {
    pub fn hex(&self) -> [u8; 64] {
        let mut out = [0; 64];
        for (index, byte) in self.0.iter().enumerate() {
            out[index * 2] = b"0123456789abcdef"[(byte >> 4) as usize];
            out[index * 2 + 1] = b"0123456789abcdef"[(byte & 15) as usize];
        }
        out
    }
}

pub struct EmbeddingTarget {
    model: Text,
    custom_endpoint: Text,
    custom_model: Text,
    identity_model: Text,
    identity_endpoint: Text,
}
impl EmbeddingTarget {
    pub fn capture(model: &Text, configured: Option<&Text>, endpoint: &Text, custom_model: &Text, budget: &Budget) -> Result<Self, StageError> {
        let model = if model.equals_ascii(b"auto") {
            match configured { Some(value) if value.len_units() > 0 => value.copy(budget)?, _ => Text::from_str("MiniLM", budget)? }
        } else { model.copy(budget)? };
        let custom_endpoint = endpoint.trim(budget)?;
        let custom_model = custom_model.trim(budget)?;
        let (identity_model, identity_endpoint) = if model.equals_ascii(b"custom") {
            let mut name = Text::from_str("custom", budget)?;
            if custom_model.len_units() > 0 { name.append(&Text::from_str(":", budget)?)?; name.append(&custom_model)?; }
            (name, custom_endpoint.copy(budget)?)
        } else { (model.copy(budget)?, Text::from_str("", budget)?) };
        Ok(Self { model, custom_endpoint, custom_model, identity_model, identity_endpoint })
    }
    fn copy(&self, budget: &Budget) -> Result<Self, StageError> {
        Ok(Self { model: self.model.copy(budget)?, custom_endpoint: self.custom_endpoint.copy(budget)?,
            custom_model: self.custom_model.copy(budget)?, identity_model: self.identity_model.copy(budget)?,
            identity_endpoint: self.identity_endpoint.copy(budget)? })
    }
    pub fn cache_key(&self, document: &Text, budget: &Budget) -> Result<CacheKey, StageError> { key::key(document, self, budget) }
    pub fn model(&self) -> &Text { &self.model }
    pub fn custom_endpoint(&self) -> &Text { &self.custom_endpoint }
    pub fn custom_model(&self) -> &Text { &self.custom_model }
    pub fn identity(&self) -> (&Text, &Text) { (&self.identity_model, &self.identity_endpoint) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbeddingPurpose { Document, Query }
pub enum EmbeddingInput { Scalar(Text), Batch(OwnedVec<Text>) }
pub struct EmbeddingRequest { pub target: EmbeddingTarget, pub purpose: EmbeddingPurpose, pub input: EmbeddingInput }
/// f32 bits preserve cache serialization, including payload bits. Arithmetic widens to f64.
pub enum EmbeddingVector { F64(OwnedVec<f64>), F32(OwnedVec<u32>) }
impl EmbeddingVector {
    pub fn len(&self) -> usize { match self { Self::F64(values) => values.as_slice().len(), Self::F32(bits) => bits.as_slice().len() } }
    pub fn is_empty(&self) -> bool { self.len() == 0 }
    pub fn value(&self, index: usize) -> Option<f64> {
        match self { Self::F64(values) => values.as_slice().get(index).copied(),
            Self::F32(bits) => bits.as_slice().get(index).map(|bits| f32::from_bits(*bits) as f64) }
    }
    fn cache_bits(&self, budget: &Budget) -> Result<OwnedVec<u32>, StageError> {
        let mut out = OwnedVec::new(budget)?;
        for index in 0..self.len() {
            budget.work(1)?;
            out.push(match self { Self::F32(bits) => bits.as_slice()[index], Self::F64(values) => (values.as_slice()[index] as f32).to_bits() })?;
        }
        Ok(out)
    }
}
/// None is a missing/null vector slot, distinct from a present empty vector.
pub type EmbeddingBatch = OwnedVec<Option<EmbeddingVector>>;
pub trait EmbeddingService {
    fn embed(&mut self, request: EmbeddingRequest, budget: &Budget) -> Result<Step<EmbeddingBatch>, StageError>;
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<EmbeddingBatch>, StageError>;
}

pub struct CacheRow { pub key: CacheKey, pub dimensions: usize, pub bits: OwnedVec<u32> }
pub struct CacheWrite { pub key: Option<CacheKey>, pub bits: OwnedVec<u32> }
pub enum CacheError { Unavailable, Terminal(StageError) }
/// Each call completes one ordered, atomic batch. Ordinary write errors preserve earlier completed batches.
pub trait ImmediateCache {
    fn read_batch(&mut self, keys: &[CacheKey], budget: &Budget) -> Result<OwnedVec<Option<CacheRow>>, CacheError>;
    fn write_batch(&mut self, target: &EmbeddingTarget, entries: &[CacheWrite], budget: &Budget) -> Result<(), CacheError>;
}

struct KeyIndex { map: HashMap<CacheKey, usize>, charge: Option<Charge> }
impl KeyIndex {
    fn new() -> Self { Self { map: HashMap::new(), charge: None } }
    fn insert(&mut self, key: CacheKey, index: usize, budget: &Budget) -> Result<(), StageError> {
        budget.work(1)?;
        if self.map.len() == self.map.capacity() {
            let bytes = |capacity: usize| capacity.checked_add(1).and_then(|v| v.checked_mul(2))
                .and_then(|v| v.checked_mul(size_of::<(CacheKey, usize)>() + 1)).and_then(|v| v.checked_add(16)).ok_or_else(owned_limit);
            let capacity = self.map.capacity().checked_mul(2).and_then(|v| v.checked_add(1)).ok_or_else(owned_limit)?.max(3);
            let requested = bytes(capacity)?;
            let mut charge = budget.reserve_owned(requested)?;
            self.map.try_reserve(1).map_err(|_| StageError::Limit("allocation".into()))?;
            let actual = bytes(self.map.capacity())?;
            if actual > requested { charge = budget.reserve_owned(actual)?; }
            self.charge = Some(charge);
        }
        self.map.insert(key, index);
        Ok(())
    }
    fn get(&self, key: &CacheKey, budget: &Budget) -> Result<Option<usize>, StageError> {
        budget.work(1)?;
        Ok(self.map.get(key).copied())
    }
}
fn owned_limit() -> StageError { StageError::Limit("owned-bytes".into()) }
fn invalid() -> StageError { StageError::Invalid("hypa-v1-state".into()) }
struct Entry { document: Option<Text>, vector: EmbeddingVector }
struct Preparation {
    documents: OwnedVec<Text>, keys: OwnedVec<CacheKey>, rows: OwnedVec<Option<CacheRow>>, index: KeyIndex,
    pending: OwnedVec<usize>, vectors: EmbeddingBatch, next: usize, stale_checked: bool,
    fresh_width: usize,
}
enum State { New, Preparing(Preparation), Ready(OwnedVec<Entry>), Querying(OwnedVec<Entry>), Failed }
pub struct HypaV1Retrieval<C, E> {
    target: EmbeddingTarget, cache: C, embeddings: E, cache_batch: usize, state: State,
}
impl<C: ImmediateCache, E: EmbeddingService> HypaV1Retrieval<C, E> {
    pub fn new(target: EmbeddingTarget, cache: C, embeddings: E, cache_batch_limit: usize) -> Result<Self, StageError> {
        if cache_batch_limit == 0 { return Err(StageError::Invalid("hypa-cache-batch-limit".into())); }
        Ok(Self { target, cache, embeddings, cache_batch: cache_batch_limit.min(1024), state: State::New })
    }
    fn initial(&mut self, documents: Vec<Text>, budget: &Budget) -> Result<Preparation, StageError> {
        let _input = budget.reserve_owned(documents.capacity().checked_mul(size_of::<Text>()).ok_or_else(owned_limit)?)?;
        let mut state = Preparation { documents: OwnedVec::new(budget)?, keys: OwnedVec::new(budget)?,
            rows: OwnedVec::new(budget)?, index: KeyIndex::new(), pending: OwnedVec::new(budget)?,
            vectors: OwnedVec::new(budget)?, next: 0, stale_checked: false, fresh_width: 0 };
        for document in documents {
            state.keys.push(self.target.cache_key(&document, budget)?)?;
            state.documents.push(document)?;
        }
        for keys in state.keys.as_slice().chunks(self.cache_batch) {
            budget.check()?;
            match self.cache.read_batch(keys, budget) {
                Ok(mut rows) => for row in rows.as_mut_slice() {
                    budget.work(1)?;
                    if let Some(row) = row.take() {
                        if let Some(index) = state.index.get(&row.key, budget)? { state.rows.as_mut_slice()[index] = Some(row); }
                        else {
                            state.index.insert(row.key, state.rows.as_slice().len(), budget)?;
                            state.rows.push(Some(row))?;
                        }
                    }
                },
                Err(CacheError::Unavailable) => { state.rows = OwnedVec::new(budget)?; state.index = KeyIndex::new(); break; }
                Err(CacheError::Terminal(error)) => return Err(error),
            }
            budget.check()?;
        }
        let mut dimensions = 0;
        for row in state.rows.as_slice().iter().flatten() {
            budget.work(1)?;
            if dimensions == 0 { dimensions = row.dimensions; }
            else if row.dimensions != dimensions { state.rows = OwnedVec::new(budget)?; state.index = KeyIndex::new(); break; }
        }
        for (index, key) in state.keys.as_slice().iter().enumerate() {
            if state.index.get(key, budget)?.is_none() { state.pending.push(index)?; }
        }
        Ok(state)
    }
    fn append(state: &mut Preparation, mut output: EmbeddingBatch, budget: &Budget) -> Result<(), StageError> {
        for slot in output.as_mut_slice() { budget.work(1)?; state.vectors.push(slot.take())?; }
        Ok(())
    }
    fn drive(&mut self, mut state: Preparation, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        loop {
            budget.check()?;
            if state.next < state.pending.as_slice().len() {
                let end = state.next.saturating_add(50).min(state.pending.as_slice().len());
                let mut texts = OwnedVec::new(budget)?;
                for &index in &state.pending.as_slice()[state.next..end] { texts.push(state.documents.as_slice()[index].copy(budget)?)?; }
                state.next = end;
                let request = EmbeddingRequest { target: self.target.copy(budget)?, purpose: EmbeddingPurpose::Document, input: EmbeddingInput::Batch(texts) };
                let step = self.embeddings.embed(request, budget)?;
                budget.check()?;
                match step {
                    Step::NeedsHost(request) => { self.state = State::Preparing(state); return Ok(Step::NeedsHost(request)); }
                    Step::Done(output) => { budget.check()?; Self::append(&mut state, output, budget)?; continue; }
                }
            }
            if !state.stale_checked {
                state.stale_checked = true;
                state.fresh_width = state.vectors.as_slice().first().and_then(Option::as_ref).map_or(0, EmbeddingVector::len);
                if state.fresh_width > 0 {
                    for (index, key) in state.keys.as_slice().iter().enumerate() {
                        if let Some(row) = state.index.get(key, budget)? {
                            if state.rows.as_slice()[row].as_ref().expect("indexed row").dimensions != state.fresh_width { state.pending.push(index)?; }
                        }
                    }
                    if state.next < state.pending.as_slice().len() { continue; }
                }
            }
            return self.finish(state, budget);
        }
    }
    fn finish(&mut self, mut state: Preparation, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        let mut entries = OwnedVec::new(budget)?;
        let mut writes = OwnedVec::new(budget)?;
        // Construct every cache entry before any write; a late absent vector must leave zero writes.
        for (index, vector) in state.vectors.as_mut_slice().iter_mut().enumerate() {
            budget.work(1)?;
            let vector = vector.take().ok_or_else(|| StageError::Invalid("hypa-embedding-vector".into()))?;
            let source = state.pending.as_slice().get(index).copied();
            let key = source.map(|source| state.keys.as_slice()[source]);
            let document = source.map(|source| state.documents.as_slice()[source].copy(budget)).transpose()?;
            writes.push(CacheWrite { key, bits: vector.cache_bits(budget)? })?;
            entries.push(Entry { document, vector })?;
        }
        for batch in writes.as_slice().chunks(self.cache_batch) {
            budget.check()?;
            match self.cache.write_batch(&self.target, batch, budget) {
                Ok(()) => {}, Err(CacheError::Unavailable) => break, Err(CacheError::Terminal(error)) => return Err(error),
            }
            budget.check()?;
        }
        for (index, key) in state.keys.as_slice().iter().enumerate() {
            if let Some(row) = state.index.get(key, budget)? {
                let row = state.rows.as_slice()[row].as_ref().expect("indexed row");
                if state.fresh_width > 0 && row.dimensions != state.fresh_width { continue; }
                let mut bits = OwnedVec::new(budget)?;
                bits.extend(row.bits.as_slice())?;
                entries.push(Entry { document: Some(state.documents.as_slice()[index].copy(budget)?), vector: EmbeddingVector::F32(bits) })?;
            }
        }
        self.state = State::Ready(entries);
        Ok(Step::Done(RetrievalOutput::Prepared))
    }
    fn scored(&mut self, entries: OwnedVec<Entry>, output: EmbeddingBatch, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        budget.check()?;
        let query = output.as_slice().first().and_then(Option::as_ref);
        let mut scores = OwnedVec::new(budget)?;
        let mut order = OwnedVec::new(budget)?;
        for (index, entry) in entries.as_slice().iter().enumerate() {
            let query = query.ok_or_else(|| StageError::Invalid("hypa-query-vector".into()))?;
            let mut score = 0.0;
            for at in 0..query.len() {
                budget.work(1)?;
                let product = query.value(at).expect("query index") * entry.vector.value(at).unwrap_or(f64::NAN);
                score += product;
            }
            scores.push(score)?;
            order.push(index)?;
        }
        rank(order.as_mut_slice(), &|index| scores.as_slice()[index], budget)?;
        let _result_charge = budget.reserve_owned(order.as_slice().len().checked_mul(size_of::<ScoredDocument>()).ok_or_else(owned_limit)?)?;
        let mut result = Vec::new();
        result.try_reserve_exact(order.as_slice().len()).map_err(|_| StageError::Limit("allocation".into()))?;
        for &index in order.as_slice() {
            budget.work(1)?;
            let document = entries.as_slice()[index].document.as_ref().map(|text| text.copy(budget)).transpose()?;
            result.push(ScoredDocument { document, score: scores.as_slice()[index] });
        }
        self.state = State::Ready(entries);
        Ok(Step::Done(RetrievalOutput::Scored(result)))
    }
}

impl<C: ImmediateCache, E: EmbeddingService> Retrieval for HypaV1Retrieval<C, E> {
    fn prepare(&mut self, documents: Vec<Text>, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        if !matches!(std::mem::replace(&mut self.state, State::Failed), State::New) { return Err(invalid()); }
        let state = self.initial(documents, budget)?;
        self.drive(state, budget)
    }
    fn query(&mut self, query: Text, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        let State::Ready(entries) = std::mem::replace(&mut self.state, State::Failed) else { return Err(invalid()); };
        budget.check()?;
        let request = EmbeddingRequest { target: self.target.copy(budget)?, purpose: EmbeddingPurpose::Query, input: EmbeddingInput::Scalar(query) };
        let step = self.embeddings.embed(request, budget)?;
        budget.check()?;
        match step {
            Step::NeedsHost(request) => { self.state = State::Querying(entries); Ok(Step::NeedsHost(request)) }
            Step::Done(output) => self.scored(entries, output, budget),
        }
    }
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError> {
        let state = std::mem::replace(&mut self.state, State::Failed);
        if !matches!(state, State::Preparing(_) | State::Querying(_)) { return Err(invalid()); }
        budget.check()?;
        let step = self.embeddings.resume(reply, budget)?;
        budget.check()?;
        match step {
            Step::NeedsHost(request) => { self.state = state; Ok(Step::NeedsHost(request)) }
            Step::Done(output) => match state {
                State::Preparing(mut state) => { budget.check()?; Self::append(&mut state, output, budget)?; self.drive(state, budget) }
                State::Querying(entries) => self.scored(entries, output, budget),
                _ => unreachable!(),
            },
        }
    }
}

fn rank(order: &mut [usize], key: &dyn Fn(usize) -> f64, budget: &Budget) -> Result<(), StageError> {
    let length = order.len();
    if length < 2 { budget.check()?; return Ok(()); }
    let levels = length.ilog2() as usize + 1;
    // Cover merge copies and the retained run stack as well as transient old/new allocations.
    budget.work(length.checked_mul(levels).and_then(|v| v.checked_mul(64)).ok_or_else(|| StageError::Limit("work".into()))?)?;
    let _scratch = budget.reserve_owned(length.checked_add(256).and_then(|v| v.checked_mul(size_of::<usize>() * 4)).ok_or_else(owned_limit)?)?;
    let failure = std::cell::RefCell::new(None);
    risunest_ir::ordering::sort_descending(order, &|index| {
        if failure.borrow().is_none() {
            if let Err(error) = budget.work(1) { *failure.borrow_mut() = Some(error); }
        }
        // Finish the bounded internal sort without further comparisons after cancellation/exhaustion.
        if failure.borrow().is_some() { f64::NAN } else { key(index) }
    }, budget)?;
    if let Some(error) = failure.into_inner() { return Err(error.into()); }
    budget.check()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::Limits;
    use std::cell::Cell;
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

    fn budget(work: usize, cancel: Arc<AtomicBool>) -> Budget {
        Budget::new(Limits { max_owned_bytes: 1 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
            max_values: 10000, max_depth: 128, max_work: work }, cancel).unwrap()
    }
    fn merge_scores(length: usize) -> Vec<f64> {
        (0..length).map(|index| if index < length / 2 { (length / 2 - index) as f64 }
            else { (length + length / 2 - index) as f64 }).collect()
    }
    fn healthy_reads(scores: &[f64]) -> usize {
        let b = budget(10_000_000, Arc::new(AtomicBool::new(false)));
        let reads = Cell::new(0);
        let mut order: Vec<_> = (0..scores.len()).collect();
        rank(&mut order, &|index| { reads.set(reads.get() + 1); scores[index] }, &b).unwrap();
        assert!(order.windows(2).all(|pair| scores[pair[0]] >= scores[pair[1]]));
        assert_eq!(b.owned_bytes(), Budget::context_bytes());
        reads.get()
    }

    #[test]
    fn sorting_work_exhaustion_inside_comparison_stops_key_reads_and_releases_scratch() {
        for length in [64usize, 129, 1025] {
            let scores = merge_scores(length);
            let total = healthy_reads(&scores);
            let preflight = length * (length.ilog2() as usize + 1) * 64 + length * usize::BITS as usize;
            for allowed in [8, total / 2, total - 8] {
                let b = budget(preflight + allowed, Arc::new(AtomicBool::new(false)));
                let reads = Cell::new(0usize);
                let mut order: Vec<_> = (0..length).collect();
                let result = rank(&mut order, &|index| { reads.set(reads.get() + 1); scores[index] }, &b);
                assert!(matches!(result, Err(StageError::Limit(kind)) if kind == "work"));
                assert_eq!(reads.get(), allowed, "stop inside the actual comparator, including late merge/gallop work");
                assert_eq!(b.owned_bytes(), Budget::context_bytes());
            }
        }
    }

    #[test]
    fn sorting_cancellation_inside_comparison_stops_key_reads_and_releases_scratch() {
        for length in [64usize, 129, 1025] {
            let scores = merge_scores(length);
            let total = healthy_reads(&scores);
            for stop_at in [8, total / 2, total - 8] {
                let cancel = Arc::new(AtomicBool::new(false));
                let b = budget(10_000_000, cancel.clone());
                let reads = Cell::new(0usize);
                let mut order: Vec<_> = (0..length).collect();
                let result = rank(&mut order, &|index| {
                    let count = reads.get() + 1;
                    reads.set(count);
                    if count == stop_at { cancel.store(true, Ordering::Relaxed); }
                    scores[index]
                }, &b);
                assert!(matches!(result, Err(StageError::Cancelled)));
                assert_eq!(reads.get(), stop_at, "a final-only cancellation check would continue reading keys");
                assert_eq!(b.owned_bytes(), Budget::context_bytes());
            }
        }
    }
}
