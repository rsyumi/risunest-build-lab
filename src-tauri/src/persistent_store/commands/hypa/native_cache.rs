use super::*;
use crate::persistent_store::{PersistentStore, device_store::hypa::MAX_DIMENSIONS};
use risunest_ir::{Budget, Charge, Error, OwnedVec, Text};
use risunest_ir::stage::StageError;
use risunest_memory::hypa_v1::{CacheError, CacheKey, CacheRow, CacheWrite, EmbeddingTarget, ImmediateCache};
use std::{mem::size_of, sync::Arc};

pub(super) enum Failure { Storage, Terminal(StageError) }
impl From<StoreError> for Failure { fn from(_: StoreError) -> Self { Self::Storage } }
impl From<Error> for Failure { fn from(value: Error) -> Self { Self::Terminal(value.into()) } }
impl From<Failure> for CacheError {
    fn from(value: Failure) -> Self {
        match value { Failure::Storage => Self::Unavailable, Failure::Terminal(error) => Self::Terminal(error) }
    }
}
fn owned_limit() -> Failure { Error::Limit("owned-bytes").into() }
fn checked_add(a: usize, b: usize) -> Result<usize, Failure> { a.checked_add(b).ok_or_else(owned_limit) }
fn checked_mul(a: usize, b: usize) -> Result<usize, Failure> { a.checked_mul(b).ok_or_else(owned_limit) }

struct Lease { _charge: Charge, _excess: Option<Charge> }
struct NativeOwned<T> { value: T, lease: Lease }
pub(super) struct NativeBatch<T> { values: NativeOwned<Vec<T>>, leases: OwnedVec<Lease> }
impl<T> NativeBatch<T> {
    fn new(length: usize, budget: &Budget) -> Result<Self, Failure> {
        Ok(Self { values: native_vec(length, budget)?, leases: OwnedVec::new(budget)? })
    }
}
impl<T> NativeOwned<T> {
    fn keep(self, leases: &mut OwnedVec<Lease>) -> Result<T, Failure> {
        leases.push(self.lease)?;
        Ok(self.value)
    }
}
fn native_vec<T>(length: usize, budget: &Budget) -> Result<NativeOwned<Vec<T>>, Failure> {
    let bytes = checked_mul(length, size_of::<T>())?;
    let charge = budget.reserve_owned(bytes)?;
    let mut value = Vec::new();
    value.try_reserve_exact(length).map_err(|_| Failure::from(Error::Allocation))?;
    let actual = checked_mul(value.capacity(), size_of::<T>())?;
    let excess = if actual > bytes { Some(budget.reserve_owned(actual - bytes)?) } else { None };
    Ok(NativeOwned { value, lease: Lease { _charge: charge, _excess: excess } })
}
fn native_string(bytes: &[u8], budget: &Budget) -> Result<NativeOwned<String>, Failure> {
    budget.work(bytes.len())?;
    let mut out = native_vec(bytes.len(), budget)?;
    out.value.extend_from_slice(bytes);
    let value = String::from_utf8(out.value).map_err(|_| Failure::Storage)?;
    Ok(NativeOwned { value, lease: out.lease })
}
fn metadata(text: &Text, budget: &Budget) -> Result<NativeOwned<String>, Failure> {
    let mut bytes = 0;
    for value in char::decode_utf16(text.units()) {
        budget.work(1)?;
        bytes = checked_add(bytes, value.map_err(|_| Failure::Storage)?.len_utf8())?;
    }
    let mut out = native_vec(bytes, budget)?;
    for value in char::decode_utf16(text.units()) {
        budget.work(1)?;
        let mut encoded = [0; 4];
        out.value.extend_from_slice(value.map_err(|_| Failure::Storage)?.encode_utf8(&mut encoded).as_bytes());
    }
    Ok(NativeOwned { value: String::from_utf8(out.value).map_err(|_| Failure::Storage)?, lease: out.lease })
}
fn quoted_length(text: &str, budget: &Budget) -> Result<usize, Failure> {
    let mut size = 2;
    for byte in text.bytes() {
        budget.work(1)?;
        size = checked_add(size, match byte {
            b'"' | b'\\' | 8 | 9 | 10 | 12 | 13 => 2,
            0..=31 => 6,
            _ => 1,
        })?;
    }
    Ok(size)
}
fn decimal_length(mut value: usize) -> usize {
    let mut length = 1;
    while value >= 10 { value /= 10; length += 1; }
    length
}
pub(super) fn write_frame_length(model: &str, endpoint: Option<&str>, entries: &[CacheWrite], budget: &Budget) -> Result<usize, Failure> {
    let mut length = checked_add(HEADER_LENGTH_BYTES, b"{\"entries\":[]}".len())?;
    let model = quoted_length(model, budget)?;
    let endpoint = match endpoint { Some(value) => quoted_length(value, budget)?, None => 4 };
    for (index, entry) in entries.iter().enumerate() {
        budget.work(1)?;
        let dimensions = entry.bits.as_slice().len();
        let bytes = checked_mul(dimensions, VECTOR_ELEMENT_BYTES)?;
        let fields = b"{\"key\":,\"producer\":,\"model\":,\"endpoint\":,\"preprocessVersion\":1,\"dimensions\":,\"byteLength\":,\"metadata\":null}".len();
        for part in [usize::from(index != 0), fields, 66, b"\"hypa-v1-text\"".len(), model, endpoint,
            decimal_length(dimensions), decimal_length(bytes), bytes] {
            length = checked_add(length, part)?;
        }
    }
    Ok(length)
}

// Field order releases the connection before its admission permit.
pub(super) struct Segment {
    pub(super) store: PersistentStore,
    _operation: super::super::RendererOperationGuard,
}
pub(crate) struct NativeCache {
    state: Arc<PersistentStoreState>,
    notify: Arc<dyn Fn() + Send + Sync>,
    frame_limit: usize,
}
impl NativeCache {
    pub(crate) fn new(state: Arc<PersistentStoreState>, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::with_frame_limit(state, notify, MAX_FRAME_BYTES)
    }
    pub(crate) fn with_frame_limit(state: Arc<PersistentStoreState>, notify: Arc<dyn Fn() + Send + Sync>, frame_limit: usize) -> Self {
        Self { state, notify, frame_limit }
    }
    pub(super) fn segment(&self, budget: &Budget) -> Result<Segment, Failure> {
        budget.check()?;
        let operation = self.state.admit_native_job_operation()?;
        budget.check()?;
        let store = self.state.open_admitted_native_job_store(&operation)?;
        budget.check()?;
        Ok(Segment { store, _operation: operation })
    }
    pub(super) fn prepare_read(keys: &[CacheKey], budget: &Budget) -> Result<NativeBatch<String>, Failure> {
        budget.check()?;
        if keys.len() > MAX_BATCH_ENTRIES { return Err(Failure::Storage); }
        let mut names = NativeBatch::new(keys.len(), budget)?;
        for key in keys {
            names.values.value.push(native_string(&key.hex(), budget)?.keep(&mut names.leases)?);
        }
        Ok(names)
    }
    pub(super) fn read_segment(mut segment: Segment, keys: &[CacheKey], names: NativeBatch<String>, budget: &Budget) -> Result<OwnedVec<Option<CacheRow>>, Failure> {
        budget.check()?;
        let mut output = OwnedVec::new(budget)?;
        {
            let store = &mut segment.store;
            store.device_store()?.visit_hypa_embeddings(&names.values.value, |index, row| {
                budget.work(1)?;
                let Some((dimensions, bytes)) = row.filter(|(dimensions, bytes)| *dimensions > 0 && !bytes.is_empty()) else {
                    output.push(None)?;
                    return Ok::<_, Failure>(());
                };
                let dimensions = usize::try_from(dimensions).map_err(|_| owned_limit())?;
                let required = checked_mul(dimensions, VECTOR_ELEMENT_BYTES)?;
                if required > budget.limits().max_owned_bytes.saturating_sub(budget.owned_bytes()) {
                    return Err(owned_limit());
                }
                budget.work(checked_add(dimensions, bytes.len())?)?;
                if bytes.len() > required { return Err(Failure::Storage); }
                let mut bits = OwnedVec::new(budget)?;
                // Finish admitting the entire output before copying any borrowed blob bytes.
                for _ in 0..dimensions { bits.push(0u32)?; }
                for (value, chunk) in bits.as_mut_slice().iter_mut().zip(bytes.chunks(VECTOR_ELEMENT_BYTES)) {
                    budget.check()?;
                    let mut element = [0; VECTOR_ELEMENT_BYTES];
                    element[..chunk.len()].copy_from_slice(chunk);
                    *value = u32::from_le_bytes(element);
                }
                output.push(Some(CacheRow { key: keys[index], dimensions, bits }))?;
                Ok(())
            })
        }?;
        drop(segment);
        budget.check()?;
        Ok(output)
    }
    pub(super) fn prepare_write(&self, target: &EmbeddingTarget, entries: &[CacheWrite], budget: &Budget) -> Result<Option<NativeBatch<HypaEmbeddingWrite>>, Failure> {
        budget.check()?;
        if entries.len() > MAX_BATCH_ENTRIES { return Err(Failure::Storage); }
        for entry in entries {
            budget.work(1)?;
            if entry.key.is_none() || entry.bits.as_slice().is_empty() || entry.bits.as_slice().len() > MAX_DIMENSIONS as usize {
                return Err(Failure::Storage);
            }
        }
        if entries.is_empty() { return Ok(None); }
        let (model, endpoint) = target.identity();
        let model = metadata(model, budget)?;
        let endpoint = metadata(endpoint, budget)?;
        if model.value.is_empty() || model.value.len() > 256 { return Err(Failure::Storage); }
        let endpoint_ref = (!endpoint.value.is_empty()).then_some(endpoint.value.as_str());
        let frame_bytes = write_frame_length(&model.value, endpoint_ref, entries, budget)?;
        validate_frame_length(frame_bytes, self.frame_limit)?;
        let mut writes = NativeBatch::new(entries.len(), budget)?;
        for entry in entries {
            budget.check()?;
            let key = native_string(&entry.key.ok_or(Failure::Storage)?.hex(), budget)?.keep(&mut writes.leases)?;
            let producer = native_string(b"hypa-v1-text", budget)?.keep(&mut writes.leases)?;
            let model = native_string(model.value.as_bytes(), budget)?.keep(&mut writes.leases)?;
            let endpoint = endpoint_ref.map(|value| native_string(value.as_bytes(), budget)?.keep(&mut writes.leases)).transpose()?;
            let dimensions = entry.bits.as_slice().len();
            let mut vector = native_vec(checked_mul(dimensions, VECTOR_ELEMENT_BYTES)?, budget)?;
            for bits in entry.bits.as_slice() {
                budget.work(VECTOR_ELEMENT_BYTES)?;
                vector.value.extend_from_slice(&bits.to_le_bytes());
            }
            writes.values.value.push(HypaEmbeddingWrite { cache_key: key, producer, model, endpoint,
                preprocess_version: 1, dimensions: dimensions as i64, vector: vector.keep(&mut writes.leases)?, metadata: None });
        }
        Ok(Some(writes))
    }
    pub(super) fn commit_write(&self, mut segment: Segment, writes: NativeBatch<HypaEmbeddingWrite>, budget: &Budget) -> Result<(), Failure> {
        budget.check()?;
        segment.store.device_store_mut()?.write_hypa_embeddings(&writes.values.value)?;
        drop(segment);
        // A successful nonempty write has committed. Notify even if cancellation won meanwhile.
        (self.notify)();
        budget.check()?;
        Ok(())
    }
    fn read(&self, keys: &[CacheKey], budget: &Budget) -> Result<OwnedVec<Option<CacheRow>>, Failure> {
        let names = Self::prepare_read(keys, budget)?;
        Self::read_segment(self.segment(budget)?, keys, names, budget)
    }
    fn write(&self, target: &EmbeddingTarget, entries: &[CacheWrite], budget: &Budget) -> Result<(), Failure> {
        if let Some(writes) = self.prepare_write(target, entries, budget)? {
            self.commit_write(self.segment(budget)?, writes, budget)?;
        }
        Ok(())
    }
}
impl ImmediateCache for NativeCache {
    fn read_batch(&mut self, keys: &[CacheKey], budget: &Budget) -> Result<OwnedVec<Option<CacheRow>>, CacheError> {
        let result = self.read(keys, budget);
        budget.check().map_err(|error| CacheError::Terminal(error.into()))?;
        result.map_err(Into::into)
    }
    fn write_batch(&mut self, target: &EmbeddingTarget, entries: &[CacheWrite], budget: &Budget) -> Result<(), CacheError> {
        let result = self.write(target, entries, budget);
        budget.check().map_err(|error| CacheError::Terminal(error.into()))?;
        result.map_err(Into::into)
    }
}
