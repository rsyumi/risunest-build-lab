//! Counters the settings screen reads to show what server sync is doing.
//!
//! Every counter only grows, so a reader subtracts what it saw when its
//! attempt started. `backlog_left` is the exception: it is what remains of
//! known work now. Lanes that can run at the same time never share counters.
use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering::Relaxed};
use std::sync::{Arc, LazyLock, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Step {
    Idle = 0,
    /// Reading local changes and the objects they reference.
    Preparing = 1,
    /// Asking the server which objects it already holds.
    Checking = 2,
    Uploading = 3,
    /// Waiting for the server to accept a publication or a receipt.
    Confirming = 4,
    /// Reading change and state pages.
    Listing = 5,
    Downloading = 6,
    /// Writing received units into this device's staging area.
    Staging = 7,
}
impl Step {
    fn name(value: u8) -> &'static str {
        match value {
            1 => "preparing",
            2 => "checking",
            3 => "uploading",
            4 => "confirming",
            5 => "listing",
            6 => "downloading",
            7 => "staging",
            _ => "idle",
        }
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AssetScope {
    pub id: u64,
    pub done: u64,
    pub total: Option<u64>,
    pub settled: bool,
}

#[derive(Default)]
pub(crate) struct ProgressLane {
    active: AtomicU32,
    step: AtomicU8,
    /// Units read from the server, before their bodies are fetched.
    listed: AtomicU64,
    items_done: AtomicU64,
    items_total: AtomicU64,
    files_done: AtomicU64,
    files_total: AtomicU64,
    bytes_done: AtomicU64,
    /// Payload bytes planned where sizes are known before the transfer.
    bytes_total: AtomicU64,
    sent: AtomicU64,
    received: AtomicU64,
    /// The server's changes after the receive cursor.
    backlog_done: AtomicU64,
    backlog_left: AtomicU64,
    asset_scope: Mutex<Option<AssetScope>>,
    asset_scope_id: AtomicU64,
}
impl ProgressLane {
    pub(crate) fn step(&self, step: Step) {
        self.step.store(step as u8, Relaxed);
    }
    pub(crate) fn listed(&self, count: usize) {
        self.listed.fetch_add(count as u64, Relaxed);
    }
    pub(crate) fn plan_items(&self, count: usize) {
        self.items_total.fetch_add(count as u64, Relaxed);
    }
    pub(crate) fn items_done(&self, count: usize) {
        self.items_done.fetch_add(count as u64, Relaxed);
    }
    pub(crate) fn plan_files(&self, count: usize, bytes: u64) {
        self.files_total.fetch_add(count as u64, Relaxed);
        self.bytes_total.fetch_add(bytes, Relaxed);
    }
    pub(crate) fn file_done(&self, bytes: u64) {
        self.files_done.fetch_add(1, Relaxed);
        self.bytes_done.fetch_add(bytes, Relaxed);
    }
    pub(crate) fn wire(&self, sent: u64, received: u64) {
        self.sent.fetch_add(sent, Relaxed);
        self.received.fetch_add(received, Relaxed);
    }
    /// Counts `done` units of known work and records what is left of it.
    pub(crate) fn backlog(&self, done: u64, left: u64) {
        self.backlog_done.fetch_add(done, Relaxed);
        self.backlog_left.store(left, Relaxed);
    }
    pub(crate) fn asset_plan(&self, total: Option<usize>) -> u64 {
        let mut scope = self.asset_scope.lock().unwrap_or_else(|error| error.into_inner());
        let id = self.asset_scope_id.fetch_add(1, Relaxed) + 1;
        *scope = Some(AssetScope { id, done: 0, total: total.map(|total| total as u64), settled: false });
        id
    }
    pub(crate) fn asset_done(&self, id: u64) {
        let mut scope = self.asset_scope.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(scope) = scope.as_mut().filter(|scope| scope.id == id) {
            scope.done = (scope.done + 1).min(scope.total.unwrap_or(0));
        }
    }
    pub(crate) fn asset_settled(&self, id: u64) {
        let mut scope = self.asset_scope.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(scope) = scope.as_mut().filter(|scope| scope.id == id) {
            scope.settled = scope.total == Some(scope.done);
        }
    }
    pub(crate) fn snapshot(&self, lane: &'static str) -> LaneSnapshot {
        let active = self.active.load(Relaxed) > 0;
        LaneSnapshot {
            lane,
            active,
            step: if active { Step::name(self.step.load(Relaxed)) } else { "idle" },
            listed: self.listed.load(Relaxed),
            items_done: self.items_done.load(Relaxed),
            items_total: self.items_total.load(Relaxed),
            files_done: self.files_done.load(Relaxed),
            files_total: self.files_total.load(Relaxed),
            bytes_done: self.bytes_done.load(Relaxed),
            bytes_total: self.bytes_total.load(Relaxed),
            sent_bytes: self.sent.load(Relaxed),
            received_bytes: self.received.load(Relaxed),
            backlog_done: self.backlog_done.load(Relaxed),
            backlog_left: self.backlog_left.load(Relaxed),
            asset_scope: *self.asset_scope.lock().unwrap_or_else(|error| error.into_inner()),
        }
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LaneSnapshot {
    pub lane: &'static str,
    pub active: bool,
    pub step: &'static str,
    pub listed: u64,
    pub items_done: u64,
    pub items_total: u64,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub sent_bytes: u64,
    pub received_bytes: u64,
    pub backlog_done: u64,
    pub backlog_left: u64,
    pub asset_scope: Option<AssetScope>,
}

#[derive(Default)]
pub(crate) struct Lanes {
    pub send: Arc<ProgressLane>,
    pub receive: Arc<ProgressLane>,
    pub hydrate: Arc<ProgressLane>,
    /// First binding: inspecting, staging and activating a target.
    pub binding: Arc<ProgressLane>,
    /// Asset storage changes: downloading every file or clearing local copies.
    pub assets: Arc<ProgressLane>,
}
impl Lanes {
    pub(crate) fn snapshot(&self) -> Vec<LaneSnapshot> {
        vec![
            self.send.snapshot("send"),
            self.receive.snapshot("receive"),
            self.hydrate.snapshot("hydrate"),
            self.binding.snapshot("binding"),
            self.assets.snapshot("assets"),
        ]
    }
}
pub(crate) static LANES: LazyLock<Lanes> = LazyLock::new(Lanes::default);

thread_local! {
    static CURRENT: RefCell<Option<Arc<ProgressLane>>> = const { RefCell::new(None) };
}

/// Runs an operation with `lane` active. Server clients created on this thread
/// meanwhile report to it, including the workers that borrow them.
pub(crate) fn within<T>(lane: &Arc<ProgressLane>, operation: impl FnOnce() -> T) -> T {
    struct Scope<'a> {
        lane: &'a ProgressLane,
        previous: Option<Arc<ProgressLane>>,
    }
    impl Drop for Scope<'_> {
        fn drop(&mut self) {
            if self.lane.active.fetch_sub(1, Relaxed) == 1 {
                self.lane.step(Step::Idle);
            }
            let previous = self.previous.take();
            CURRENT.with(|current| *current.borrow_mut() = previous);
        }
    }
    if lane.active.fetch_add(1, Relaxed) == 0 {
        *lane.asset_scope.lock().unwrap_or_else(|error| error.into_inner()) = None;
    }
    let previous = CURRENT.with(|current| current.borrow_mut().replace(lane.clone()));
    let _scope = Scope { lane, previous };
    operation()
}

/// The lane of the operation running on this thread, if any.
pub(crate) fn current() -> Option<Arc<ProgressLane>> {
    CURRENT.with(|current| current.borrow().clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lane_is_active_only_inside_its_scope_and_keeps_its_counts() {
        let lane = Arc::new(ProgressLane::default());
        assert!(current().is_none());
        within(&lane, || {
            let reporting = current().expect("lane");
            reporting.step(Step::Uploading);
            reporting.plan_files(2, 300);
            reporting.file_done(100);
            reporting.wire(120, 40);
            let inside = lane.snapshot("send");
            assert!(inside.active);
            assert_eq!(inside.step, "uploading");
            assert_eq!((inside.files_done, inside.files_total, inside.bytes_done, inside.bytes_total), (1, 2, 100, 300));
        });
        assert!(current().is_none());
        let after = lane.snapshot("send");
        assert!(!after.active);
        assert_eq!(after.step, "idle");
        assert_eq!((after.sent_bytes, after.received_bytes), (120, 40));
    }

    #[test]
    fn known_work_adds_what_was_done_and_keeps_only_what_is_left() {
        let lane = ProgressLane::default();
        lane.backlog(40, 60);
        lane.backlog(60, 5);
        let read = lane.snapshot("receive");
        assert_eq!((read.backlog_done, read.backlog_left), (100, 5));
    }

    #[test]
    fn asset_scopes_are_finite_and_only_settle_after_all_items() {
        let lane = Arc::new(ProgressLane::default());
        within(&lane, || {
            let first = lane.asset_plan(Some(130));
            for _ in 0..64 { lane.asset_done(first); }
            lane.asset_settled(first);
            let scope = lane.snapshot("hydrate").asset_scope.unwrap();
            assert_eq!((scope.done, scope.total, scope.settled), (64, Some(130), false));
            for _ in 64..130 { lane.asset_done(first); }
            assert!(!lane.snapshot("hydrate").asset_scope.unwrap().settled);
            lane.asset_settled(first);
            assert!(lane.snapshot("hydrate").asset_scope.unwrap().settled);
            let next = lane.asset_plan(None);
            lane.asset_done(first);
            assert_eq!(lane.snapshot("hydrate").asset_scope.unwrap().done, 0);
            assert!(next > first);
        });
        within(&lane, || assert!(lane.snapshot("hydrate").asset_scope.is_none()));
    }

    #[test]
    fn a_nested_scope_restores_the_outer_lane() {
        let outer = Arc::new(ProgressLane::default());
        let inner = Arc::new(ProgressLane::default());
        within(&outer, || {
            outer.step(Step::Listing);
            within(&inner, || assert!(Arc::ptr_eq(&current().unwrap(), &inner)));
            assert!(Arc::ptr_eq(&current().unwrap(), &outer));
            assert_eq!(outer.snapshot("receive").step, "listing");
        });
    }

    #[test]
    fn the_same_lane_stays_active_until_its_last_scope_ends() {
        let lane = Arc::new(ProgressLane::default());
        within(&lane, || {
            lane.step(Step::Confirming);
            within(&lane, || ());
            assert!(lane.snapshot("send").active);
            assert_eq!(lane.snapshot("send").step, "confirming");
        });
        assert!(!lane.snapshot("send").active);
    }
}
