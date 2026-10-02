//! Same-thread SHA input accounting for native verification entries only.
//!
//! Reset immediately before the operation and take on that same thread. Worker
//! threads must return their own sample. Incomplete delegated calls invalidate
//! a total, even when the domains still contain useful observed partial work.
//! The byte component of `lww::take_work_metrics` aliases native_unit_envelope.
//! Intent input capture is off by default and only for synthetic
//! diagnostics. Reset enables it; take disables it. Parse after timing ends.
#![cfg(test)]

use risunest_sync_wire::{canonical, descriptor::BuiltReferenceTree, unit::{LwwDecision, UnitValue}, WireError};
use std::{cell::RefCell, collections::BTreeMap};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DomainWork {
    pub calls: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HashWork {
    pub domains: BTreeMap<&'static str, DomainWork>,
    pub incomplete: BTreeMap<&'static str, u64>,
}

thread_local! { static WORK: RefCell<HashWork> = RefCell::new(HashWork::default()); }
thread_local! { static RECEIVE_INTENT_INPUTS: RefCell<Option<Vec<Vec<u8>>>> = const { RefCell::new(None) }; }
thread_local! { static COMMIT_INTENT_INPUTS: RefCell<Option<Vec<Vec<u8>>>> = const { RefCell::new(None) }; }

pub(crate) fn reset_commit_intent_inputs() {
    COMMIT_INTENT_INPUTS.with(|inputs| *inputs.borrow_mut() = Some(Vec::new()));
}

pub(crate) fn take_commit_intent_inputs() -> Vec<Vec<u8>> {
    COMMIT_INTENT_INPUTS.with(|inputs| inputs.borrow_mut().take().unwrap_or_default())
}

pub(super) fn record_commit_intent_input(bytes: &[u8]) {
    COMMIT_INTENT_INPUTS.with(|inputs| {
        if let Some(inputs) = inputs.borrow_mut().as_mut() { inputs.push(bytes.to_vec()); }
    });
}

// Opt-in diagnostic capture. Parse only after taking the operation's timings
// and counters; captured inputs are not additional hash work.
pub(crate) fn reset_receive_intent_inputs() {
    RECEIVE_INTENT_INPUTS.with(|inputs| *inputs.borrow_mut() = Some(Vec::new()));
}

pub(crate) fn take_receive_intent_inputs() -> Vec<Vec<u8>> {
    RECEIVE_INTENT_INPUTS.with(|inputs| inputs.borrow_mut().take().unwrap_or_default())
}

pub(super) fn record_receive_intent_input(bytes: &[u8]) {
    RECEIVE_INTENT_INPUTS.with(|inputs| {
        if let Some(inputs) = inputs.borrow_mut().as_mut() { inputs.push(bytes.to_vec()); }
    });
}

pub(crate) fn reset_hash_work() {
    WORK.with(|work| *work.borrow_mut() = HashWork::default());
}

pub(crate) fn take_hash_work() -> HashWork {
    WORK.with(|work| std::mem::take(&mut *work.borrow_mut()))
}

pub(crate) fn begin(domain: &'static str) {
    WORK.with(|work| work.borrow_mut().domains.entry(domain).or_default().calls += 1);
}

pub(crate) fn update(domain: &'static str, bytes: usize) {
    WORK.with(|work| work.borrow_mut().domains.entry(domain).or_default().bytes += bytes as u64);
}

pub(crate) fn observe(domain: &'static str, bytes: usize) {
    begin(domain);
    update(domain, bytes);
}

pub(crate) fn incomplete(domain: &'static str) {
    WORK.with(|work| *work.borrow_mut().incomplete.entry(domain).or_default() += 1);
}

// RecordDescriptor::bytes validates and canonically encodes, without hashing.
// UnitValue::validate hashes only after both these checks succeed, even when
// the final descriptor hash comparison fails.
pub(crate) fn validation(value: &UnitValue) {
    if let UnitValue::Object { descriptor_hash, descriptor } = value {
        if risunest_sync_wire::validate_hash(descriptor_hash).is_ok() {
            if let Ok(bytes) = descriptor.bytes() {
                observe("native_wire_descriptor_validation", bytes.len());
            }
        }
    }
}

pub(crate) fn identity(value: &UnitValue, result: &Result<String, WireError>) {
    validation(value);
    if result.is_ok() {
        match canonical::encode(value) {
            Ok(bytes) => observe("native_wire_identity", bytes.len()),
            Err(_) => incomplete("native_wire_identity"),
        }
    } else {
        incomplete("native_wire_identity");
    }
}

pub(crate) fn comparison(local: &UnitValue, remote: &UnitValue, result: &Result<LwwDecision, WireError>) {
    // An integrity disagreement happens after both complete identity calls.
    if result.is_ok() || result.as_ref().is_err_and(|error| error.0 == "equal-stamp-integrity") {
        for value in [local, remote] {
            validation(value);
            match canonical::encode(value) {
                Ok(bytes) => observe("native_wire_identity", bytes.len()),
                Err(_) => incomplete("native_wire_identity"),
            }
        }
    } else {
        incomplete("native_wire_identity");
    }
}

pub(crate) fn descriptor_creation(result: &Result<UnitValue, WireError>) {
    if let Ok(UnitValue::Object { descriptor, .. }) = result {
        match descriptor.bytes() {
            Ok(bytes) => observe("native_descriptor_creation", bytes.len()),
            Err(_) => incomplete("native_descriptor_creation"),
        }
    } else if result.is_err() {
        incomplete("native_descriptor_creation");
    }
}

pub(crate) fn reference_creation(result: &Result<BuiltReferenceTree, WireError>) {
    match result {
        Ok((_, objects)) => {
            for (_, bytes) in objects {
                observe("native_reference_create", bytes.len());
            }
        }
        Err(_) => incomplete("native_reference_create"),
    }
}

pub(crate) fn encoded_object<E>(domain: &'static str, result: &Result<risunest_external_storage_format::logical_records::EncodedLogicalObject, E>) {
    match result {
        Ok(object) => observe(domain, object.bytes.len()),
        Err(_) => incomplete(domain),
    }
}

pub(crate) fn decoded<E, T>(domain: &'static str, bytes: &[u8], result: &Result<T, E>) {
    if result.is_ok() {
        // Both decoders re-encode once and require exact canonical equality.
        observe(domain, bytes.len());
    } else {
        incomplete(domain);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thread_local_samples_reset_and_take_independently() {
        reset_hash_work(); observe("outer", 3);
        let inner = std::thread::spawn(|| {
            assert_eq!(take_hash_work(), HashWork::default());
            observe("inner", 7); take_hash_work()
        }).join().unwrap();
        assert_eq!(inner.domains["inner"], DomainWork { calls: 1, bytes: 7 });
        let outer = take_hash_work();
        assert_eq!(outer.domains["outer"], DomainWork { calls: 1, bytes: 3 });
        assert!(!outer.domains.contains_key("inner"));
        assert_eq!(take_hash_work(), HashWork::default());
        observe("discarded", 1); reset_hash_work();
        assert_eq!(take_hash_work(), HashWork::default());
    }

    #[test]
    fn receive_input_capture_is_disabled_until_reset_and_take_disables_it() {
        assert!(take_receive_intent_inputs().is_empty());
        record_receive_intent_input(b"disabled"); assert!(take_receive_intent_inputs().is_empty());
        reset_receive_intent_inputs(); record_receive_intent_input(b"discarded");
        reset_receive_intent_inputs();
        record_receive_intent_input(b"\"synthetic\\nbytes\""); record_receive_intent_input(&[0, 255]);
        assert_eq!(take_receive_intent_inputs(), vec![b"\"synthetic\\nbytes\"".to_vec(), vec![0, 255]]);
        record_receive_intent_input(b"disabled-again"); assert!(take_receive_intent_inputs().is_empty());
    }

    #[test]
    fn receive_input_capture_is_thread_local_and_independent_of_hash_reset() {
        reset_receive_intent_inputs(); record_receive_intent_input(b"owner");
        let worker = std::thread::spawn(|| {
            record_receive_intent_input(b"worker-disabled"); assert!(take_receive_intent_inputs().is_empty());
            reset_receive_intent_inputs(); record_receive_intent_input(b"worker");
            take_receive_intent_inputs()
        }).join().unwrap();
        reset_hash_work(); assert_eq!(take_hash_work(), HashWork::default());
        assert_eq!(worker, vec![b"worker".to_vec()]);
        assert_eq!(take_receive_intent_inputs(), vec![b"owner".to_vec()]);
    }

    #[test]
    fn commit_input_capture_is_default_off_resettable_and_independent_of_receive_capture() {
        assert!(take_commit_intent_inputs().is_empty());
        record_commit_intent_input(b"disabled"); assert!(take_commit_intent_inputs().is_empty());
        reset_commit_intent_inputs(); record_commit_intent_input(b"discarded"); reset_commit_intent_inputs();
        reset_receive_intent_inputs(); record_receive_intent_input(b"receive");
        record_commit_intent_input(b"commit");
        assert_eq!(take_receive_intent_inputs(), vec![b"receive".to_vec()]);
        record_commit_intent_input(b"commit-after-receive-take");
        assert_eq!(take_commit_intent_inputs(), vec![b"commit".to_vec(), b"commit-after-receive-take".to_vec()]);
        record_commit_intent_input(b"disabled-again"); assert!(take_commit_intent_inputs().is_empty());
        reset_receive_intent_inputs(); record_receive_intent_input(b"receive-before-commit-take");
        reset_commit_intent_inputs(); assert!(take_commit_intent_inputs().is_empty());
        record_receive_intent_input(b"receive-after-commit-take");
        assert_eq!(take_receive_intent_inputs(), vec![b"receive-before-commit-take".to_vec(), b"receive-after-commit-take".to_vec()]);
    }

    #[test]
    fn commit_input_capture_is_thread_local_and_independent_of_hash_reset() {
        reset_commit_intent_inputs(); record_commit_intent_input(b"owner");
        let worker = std::thread::spawn(|| {
            record_commit_intent_input(b"worker-disabled"); assert!(take_commit_intent_inputs().is_empty());
            reset_commit_intent_inputs(); record_commit_intent_input(b"worker"); take_commit_intent_inputs()
        }).join().unwrap();
        reset_hash_work(); assert_eq!(take_hash_work(), HashWork::default());
        assert_eq!(worker, vec![b"worker".to_vec()]);
        assert_eq!(take_commit_intent_inputs(), vec![b"owner".to_vec()]);
    }
}
