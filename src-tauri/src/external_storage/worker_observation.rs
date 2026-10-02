#![cfg(test)]

use crate::{asset_repository::body_io, persistent_store::hash_work::{self, HashWork}};
use std::{cell::RefCell, sync::{Arc, Mutex}};

#[derive(Clone, Debug)]
pub(crate) struct WorkerReceipt {
    pub thread: String,
    pub hashes: HashWork,
    pub completed: bool,
}

#[derive(Default)]
struct State {
    pending: usize,
    receipts: Vec<WorkerReceipt>,
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Mutex<State>>>> = const { RefCell::new(None) };
}

pub(crate) fn begin() {
    CURRENT.with(|current| {
        assert!(current.borrow().is_none(), "external worker observation is already active");
        *current.borrow_mut() = Some(Arc::new(Mutex::new(State::default())));
    });
}

pub(crate) fn take() -> Vec<WorkerReceipt> {
    let state = CURRENT.with(|current| current.borrow().clone())
        .expect("external worker observation is not active");
    let pending = state.lock().unwrap().pending;
    assert_eq!(pending, 0, "external workers have not settled");
    CURRENT.with(|current| current.borrow_mut().take());
    let mut state = state.lock().unwrap();
    std::mem::take(&mut state.receipts)
}

struct ReceiptGuard {
    state: Option<Arc<Mutex<State>>>,
    previous: Option<Arc<Mutex<State>>>,
    completed: bool,
}

impl Drop for ReceiptGuard {
    fn drop(&mut self) {
        let hashes = hash_work::take_hash_work();
        CURRENT.with(|current| *current.borrow_mut() = self.previous.take());
        if let Some(state) = &self.state {
            let mut state = state.lock().unwrap();
            state.pending -= 1;
            state.receipts.push(WorkerReceipt {
                thread: format!("{:?}", std::thread::current().id()),
                hashes,
                completed: self.completed && !std::thread::panicking(),
            });
        }
    }
}

pub(crate) fn spawn_blocking<F, T>(operation: F) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let body_scope = body_io::capture_body_io_scope();
    let state = CURRENT.with(|current| current.borrow().clone());
    if let Some(state) = &state { state.lock().unwrap().pending += 1; }
    tokio::task::spawn_blocking(move || {
        body_io::with_body_io_scope(body_scope, || {
            hash_work::reset_hash_work();
            let previous = CURRENT.with(|current| current.replace(state.clone()));
            let mut receipt = ReceiptGuard { state, previous, completed: false };
            let result = operation();
            receipt.completed = true;
            result
        })
    })
}

#[test]
fn actual_panicked_worker_never_returns_successful_coverage() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    body_io::reset_body_io();
    begin();
    assert!(runtime.block_on(async {spawn_blocking(|| panic!("synthetic worker failure")).await}).is_err());
    let receipts = take();
    assert_eq!(receipts.len(), 1);
    assert!(!receipts[0].completed);
    assert!(!body_io::take_body_io().complete());
}

#[test]
fn actual_worker_cas_read_and_hash_return_positive_joined_receipts() {
    use std::io::Read;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().to_path_buf();
    body_io::reset_body_io();
    body_io::register_object_purpose(&risunest_sync_wire::hash(b"synthetic observed external worker body"),body_io::BodyPurpose::Asset);
    begin();
    runtime.block_on(async {
        spawn_blocking(move || {
            let bytes = b"synthetic observed external worker body";
            let cas = crate::asset_repository::PayloadCas::new(&path).unwrap();
            let prepared = cas.prepare_bytes(bytes).unwrap();
            let mut file = cas.open_object_tracked(&prepared.content_hash).unwrap().unwrap();
            let mut read = Vec::new();
            file.read_to_end(&mut read).unwrap();
            hash_work::observe("external_worker_positive", read.len());
            assert_eq!(risunest_sync_wire::hash(&read), prepared.content_hash);
        }).await.unwrap();
    });
    let receipts = take();
    assert_eq!(receipts.len(), 1);
    assert!(receipts[0].completed);
    assert!(!receipts[0].thread.is_empty());
    assert_eq!(receipts[0].hashes.domains["external_worker_positive"].calls, 1);
    assert_eq!(receipts[0].hashes.domains["external_worker_positive"].bytes, b"synthetic observed external worker body".len() as u64);
    let bodies = body_io::take_body_io();
    assert!(bodies.complete());
    assert_eq!(bodies.worker_scopes_started, 1);
    assert_eq!(bodies.worker_scopes_settled, 1);
    assert!(bodies.asset_work().read_bytes > 0);
}
