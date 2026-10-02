use std::{
    cell::RefCell,
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct HashDomain {
    pub calls: u64,
    pub bytes: u64,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct HashMetrics {
    pub domains: BTreeMap<String, HashDomain>,
    pub incomplete: bool,
}
pub(crate) type Scope = Option<Arc<Mutex<HashMetrics>>>;
thread_local! { static CURRENT: RefCell<Scope> = const { RefCell::new(None) }; }

pub(crate) fn reset_hash_metrics() {
    CURRENT.with(|slot| *slot.borrow_mut() = Some(Arc::new(Mutex::new(HashMetrics::default()))));
}
pub(crate) fn take_hash_metrics() -> HashMetrics {
    CURRENT
        .with(|slot| slot.borrow_mut().take())
        .map(|scope| scope.lock().unwrap().clone())
        .unwrap_or_else(|| HashMetrics {
            incomplete: true,
            ..Default::default()
        })
}
pub(crate) fn capture() -> Scope {
    CURRENT.with(|slot| slot.borrow().clone())
}
pub(crate) struct Guard(Scope);
impl Drop for Guard {
    fn drop(&mut self) {
        CURRENT.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}
pub(crate) fn enter(scope: Scope) -> Guard {
    Guard(CURRENT.with(|slot| slot.replace(scope)))
}
pub(crate) fn record(domain: &str, bytes: usize) {
    add(domain, 1, bytes);
}
pub(crate) fn begin_stream(domain: &str) {
    add(domain, 1, 0);
}
pub(crate) fn stream_bytes(domain: &str, bytes: usize) {
    add(domain, 0, bytes);
}
fn add(domain: &str, calls: u64, bytes: usize) {
    CURRENT.with(|slot| {
        if let Some(scope) = slot.borrow().as_ref() {
            let mut counters = scope.lock().unwrap();
            let value = counters.domains.entry(domain.into()).or_default();
            value.calls += calls;
            value.bytes += bytes as u64;
        }
    });
}
pub(crate) fn validation(value: &risunest_sync_wire::unit::UnitValue) {
    if let risunest_sync_wire::unit::UnitValue::Object { descriptor, .. } = value {
        encoded("c_wire_descriptor_validation", descriptor);
    }
}
pub(crate) fn identity(value: &risunest_sync_wire::unit::UnitValue) {
    validation(value);
    encoded("c_wire_identity", value);
}
pub(crate) fn operation(request: &risunest_sync_wire::lww::PushRequest) {
    encoded("c_operation_identity", request);
}
pub(crate) fn incomplete() {
    CURRENT.with(|slot| {
        if let Some(scope) = slot.borrow().as_ref() {
            scope.lock().unwrap().incomplete = true;
        }
    });
}
fn encoded(domain: &str, value: &impl serde::Serialize) {
    match risunest_sync_wire::canonical::encode(value) {
        Ok(bytes) => record(domain, bytes.len()),
        Err(_) => CURRENT.with(|slot| {
            if let Some(scope) = slot.borrow().as_ref() {
                scope.lock().unwrap().incomplete = true;
            }
        }),
    }
}

#[test]
fn scoped_hash_counters_join_real_worker_updates_and_exclude_other_threads() {
    reset_hash_metrics();
    record("direct", 17);
    let scope = capture();
    std::thread::spawn(move || {
        let _scope = enter(scope);
        begin_stream("stream");
        stream_bytes("stream", 23);
        stream_bytes("stream", 5);
    })
    .join()
    .unwrap();
    std::thread::spawn(|| record("unscoped", 99))
        .join()
        .unwrap();
    let result = take_hash_metrics();
    assert_eq!(
        result.domains["direct"],
        HashDomain {
            calls: 1,
            bytes: 17
        }
    );
    assert_eq!(
        result.domains["stream"],
        HashDomain {
            calls: 1,
            bytes: 28
        }
    );
    assert!(!result.domains.contains_key("unscoped"));
    assert!(!result.incomplete);
}
