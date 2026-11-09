use super::super::ollama_decode::{CancelOrigin, Cancellation};
use bytes::Bytes;
use risunest_ir::{Budget, Charge};
use risunest_providers::embeddings::Failure;
use std::{alloc::Layout, mem::{align_of, size_of}, sync::{Arc, atomic::{AtomicBool, AtomicU8, AtomicUsize}}};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

pub(super) fn limit() -> Failure { Failure::Limit("native-http-owned-bytes") }

pub(super) fn arc_bytes<T>() -> Result<usize, Failure> {
    Layout::new::<[usize; 2]>().extend(Layout::new::<T>()).map(|(layout, _)| layout.pad_to_align().size()).map_err(|_| limit())
}

struct OperationState {
    budget: Budget,
    cancel: Cancellation,
    _charge: Charge,
    _permit: OwnedSemaphorePermit,
}

#[derive(Clone)]
pub(super) struct Operation(Arc<OperationState>);

impl Operation {
    pub fn admit(slots: Arc<Semaphore>, budget: Budget, cancel: Cancellation) -> Result<Self, Failure> {
        budget.check()?;
        let permit = slots.try_acquire_owned().map_err(|_| Failure::Limit("native-http-active-operations"))?;
        let controls = arc_bytes::<AtomicBool>()?.checked_add(arc_bytes::<AtomicU8>()?)
            .and_then(|bytes| bytes.checked_add(arc_bytes::<Notify>().ok()?)).ok_or_else(limit)?;
        let charge = budget.reserve_owned(arc_bytes::<OperationState>()?.checked_add(controls).ok_or_else(limit)?)?;
        Ok(Self(Arc::new(OperationState { budget, cancel, _permit: permit, _charge: charge })))
    }
    pub fn budget(&self) -> &Budget { &self.0.budget }
    pub fn cancel(&self) -> &Cancellation { &self.0.cancel }
    pub fn same(&self, other: &Self) -> bool { Arc::ptr_eq(&self.0, &other.0) }
    pub fn check(&self) -> Result<(), Failure> { self.budget().check().map_err(Into::into) }
}

pub(super) struct Flight {
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

pub(super) fn payload(data: Vec<u8>, first: usize, last: usize, operation: &Operation) -> Result<Bytes, Failure> {
    operation.check()?;
    if first > last || last > data.len() { return Err(Failure::Invalid("native-http-payload-range")); }
    let storage = operation.budget().reserve_owned(data.capacity())?;
    // bytes 1.12.1 uses repr(C) Owned<T> { AtomicUsize, T } for from_owner.
    let wrapper_size = Layout::new::<AtomicUsize>().extend(Layout::new::<Payload>())
        .map_err(|_| limit())?.0.pad_to_align().size();
    let wrapper = operation.budget().reserve_owned(wrapper_size)?;
    Ok(Bytes::from_owner(Payload { data, first, last, _storage: storage, _wrapper: wrapper, _operation: operation.clone() }))
}

pub(super) fn header_payload(value: String, operation: &Operation) -> Result<Bytes, Failure> {
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
    payload(value.into_bytes(), first, last, operation)
}

pub(super) fn header_map_bytes(entries: usize) -> Result<usize, Failure> {
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

pub(super) struct Buffer {
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
    _operation: Operation,
}

impl<T> Observed<T> {
    pub fn new(value: T, diagnostic: Option<Failure>, operation: &Operation) -> Self {
        Self { value, diagnostic, _operation: operation.clone() }
    }
}
