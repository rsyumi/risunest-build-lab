use super::*;
use super::native_cache::{self, NativeCache as ProductCache, write_frame_length};
use crate::persistent_store::{PersistentStore, device_store::hypa::MAX_DIMENSIONS};
use risunest_ir::{Budget, OwnedVec, Text};
use risunest_ir::stage::StageError;
use risunest_memory::hypa_v1::{CacheError, CacheKey, CacheRow, CacheWrite, EmbeddingTarget, ImmediateCache};
use std::sync::Arc;

pub(crate) mod allocation_observer;
mod contracts;
mod driver;

type StoreHook = Arc<dyn Fn(&mut PersistentStore) + Send + Sync>;
struct NativeCache {
    state: Arc<PersistentStoreState>,
    notify: Arc<dyn Fn() + Send + Sync>,
    frame_limit: usize,
    opened: Option<StoreHook>,
    committed: Option<Arc<dyn Fn() + Send + Sync>>,
}
pub(crate) fn composition_cache(
    state: Arc<PersistentStoreState>,
    notify: Arc<dyn Fn() + Send + Sync>,
    opened: Option<Arc<dyn Fn(&mut PersistentStore) + Send + Sync>>,
    committed: Option<Arc<dyn Fn() + Send + Sync>>,
) -> impl ImmediateCache + Send + 'static {
    let mut cache = NativeCache::new(state, notify);
    cache.opened = opened;
    cache.committed = committed;
    cache
}

pub(crate) fn active_cache_segments(state: &PersistentStoreState) -> StoreResult<usize> {
    let gate = state.renderer_gate.state.try_lock().map_err(|_| super::super::renderer_gate_error())?;
    Ok(gate.operations)
}

impl NativeCache {
    fn new(state: Arc<PersistentStoreState>, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self { state, notify, frame_limit: MAX_FRAME_BYTES, opened: None, committed: None }
    }
    fn product(&self) -> ProductCache {
        let notify = self.notify.clone();
        let committed = self.committed.clone();
        ProductCache::with_frame_limit(self.state.clone(), Arc::new(move || {
            if let Some(committed) = &committed { committed(); }
            notify();
        }), self.frame_limit)
    }
    fn segment(&self, product: &ProductCache, budget: &Budget) -> Result<native_cache::Segment, native_cache::Failure> {
        let mut segment = product.segment(budget)?;
        if let Some(opened) = &self.opened { opened(&mut segment.store); }
        Ok(segment)
    }
}
impl ImmediateCache for NativeCache {
    fn read_batch(&mut self, keys: &[CacheKey], budget: &Budget) -> Result<OwnedVec<Option<CacheRow>>, CacheError> {
        let mut product = self.product();
        if self.opened.is_none() { return product.read_batch(keys, budget); }
        let result = (|| {
            let names = ProductCache::prepare_read(keys, budget)?;
            ProductCache::read_segment(self.segment(&product, budget)?, keys, names, budget)
        })();
        budget.check().map_err(|error| CacheError::Terminal(error.into()))?;
        result.map_err(Into::into)
    }
    fn write_batch(&mut self, target: &EmbeddingTarget, entries: &[CacheWrite], budget: &Budget) -> Result<(), CacheError> {
        let mut product = self.product();
        if self.opened.is_none() { return product.write_batch(target, entries, budget); }
        let result: Result<(), native_cache::Failure> = (|| {
            if let Some(writes) = product.prepare_write(target, entries, budget)? {
                product.commit_write(self.segment(&product, budget)?, writes, budget)?;
            }
            Ok(())
        })();
        budget.check().map_err(|error| CacheError::Terminal(error.into()))?;
        result.map_err(Into::into)
    }
}
