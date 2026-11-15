pub(super) use super::super::memory_http::Operation;
pub(super) use super::super::memory_http::{limit, arc_bytes, payload, header_payload, header_map_bytes, Buffer, Flight};
use risunest_ir::Charge;
use risunest_providers::embeddings::Failure;
use std::sync::Arc;

pub(super) struct SharedFailure {
    diagnostic: std::sync::Mutex<Option<Failure>>,
    _charge: Charge,
    _operation: Operation,
}

impl SharedFailure {
    pub fn new(operation: &Operation) -> Result<Arc<Self>, Failure> {
        let charge = operation.budget().reserve_owned(arc_bytes::<Self>()?)?;
        Ok(Arc::new(Self { diagnostic: std::sync::Mutex::new(None), _charge: charge, _operation: operation.clone() }))
    }
    pub fn set(&self, failure: Failure) { *self.diagnostic.lock().unwrap() = Some(failure); }
    pub fn take(&self) -> Option<Failure> { self.diagnostic.lock().unwrap().take() }
}

pub(super) struct Observed<T> {
    pub value: T,
    pub diagnostic: Option<Failure>,
    _charge: Option<Charge>,
    _operation: Operation,
}

impl<T> Observed<T> {
    pub fn new(value: T, diagnostic: Option<Failure>, operation: &Operation) -> Self {
        Self { value, diagnostic, _charge: None, _operation: operation.clone() }
    }
}

impl Observed<Result<super::RetrievalResult, super::StageError>> {
    pub fn failed(mut error: super::StageError, diagnostic: Option<Failure>, operation: &Operation) -> Self {
        let charge = match &error {
            super::StageError::Host(units) => units.capacity().checked_mul(std::mem::size_of::<u16>())
                .ok_or(Failure::Limit("native-http-result-bytes")).and_then(|bytes| operation.budget().reserve_owned(bytes).map_err(Into::into)),
            _ => operation.budget().reserve_owned(0).map_err(Into::into),
        };
        let charge = match charge { Ok(charge) => Some(charge), Err(failure) => { error = super::product_retrieval::terminal_failure(failure); None } };
        Self { value: Err(error), diagnostic, _charge: charge, _operation: operation.clone() }
    }
}
