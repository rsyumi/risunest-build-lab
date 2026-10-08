use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How often the counters reach the renderer. A publication places tens of
/// thousands of sources, and every report rewrites the job row.
const REPORT_INTERVAL: Duration = Duration::from_millis(250);

/// What one phase of a job has already done, against what it found to do. A
/// large library is read, compressed and packed for a long time before the
/// first object is sealed, and a receive has no transfer journal at all, so
/// without this a job reports nothing for most of its own work.
///
/// A phase plans what it found to do before it does any of it, and the planned
/// totals never shrink. A publication's preparation and a restore's download
/// plan every domain at once, so their totals stay where the first reading put
/// them.
pub(crate) struct PhaseProgress {
    completed_items: AtomicU64,
    completed_bytes: AtomicU64,
    planned_items: AtomicU64,
    planned_bytes: AtomicU64,
    reported: Mutex<Instant>,
    sink: Box<dyn Fn(PhaseCounters) + Send + Sync>,
}

/// One reading of the counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PhaseCounters {
    pub items: u64,
    pub total_items: u64,
    pub bytes: u64,
    pub total_bytes: u64,
}

impl PhaseProgress {
    pub(crate) fn new(sink: impl Fn(PhaseCounters) + Send + Sync + 'static) -> Arc<Self> {
        Self::resumed(PhaseCounters::default(), sink)
    }

    /// Continues the reading an earlier phase of the same job left, for a
    /// phase whose work that reading already planned.
    pub(crate) fn resumed(
        reading: PhaseCounters,
        sink: impl Fn(PhaseCounters) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            completed_items: AtomicU64::new(reading.items),
            completed_bytes: AtomicU64::new(reading.bytes),
            planned_items: AtomicU64::new(reading.total_items),
            planned_bytes: AtomicU64::new(reading.total_bytes),
            // Far enough back that the first reading reports.
            reported: Mutex::new(Instant::now() - REPORT_INTERVAL),
            sink: Box::new(sink),
        })
    }

    /// Nothing is reported. For the paths that run without a job to tell.
    pub(crate) fn silent() -> Arc<Self> {
        Self::new(|_| {})
    }

    /// What is to be done, added to the totals before any of it is.
    pub(crate) fn plan(&self, items: u64, bytes: u64) {
        self.planned_items.fetch_add(items, Ordering::Relaxed);
        self.planned_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.report();
    }

    /// Plans what is still to be done together with what a walk already did
    /// while finding it, read off the silent counter the walk used. The walk
    /// counts as done, and the totals are reported once, already complete.
    pub(crate) fn plan_after(&self, walked: PhaseCounters, items: u64, bytes: u64) {
        self.planned_items
            .fetch_add(walked.total_items.saturating_add(items), Ordering::Relaxed);
        self.planned_bytes
            .fetch_add(walked.total_bytes.saturating_add(bytes), Ordering::Relaxed);
        self.completed_items.fetch_add(walked.items, Ordering::Relaxed);
        self.completed_bytes.fetch_add(walked.bytes, Ordering::Relaxed);
        self.report();
    }

    /// One item is done, whether it was worked through here or answered for by
    /// something the device already held.
    pub(crate) fn completed(&self, bytes: u64) {
        self.completed_many(1, bytes);
    }

    /// Several items done at once, such as a whole domain the repository
    /// already holds.
    pub(crate) fn completed_many(&self, items: u64, bytes: u64) {
        self.completed_items.fetch_add(items, Ordering::Relaxed);
        self.completed_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.report();
    }

    /// An item found and done in one step, such as a catalog node read as the
    /// walk reaches it.
    pub(crate) fn found_completed(&self, bytes: u64) {
        self.planned_items.fetch_add(1, Ordering::Relaxed);
        self.planned_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.completed(bytes);
    }

    pub(crate) fn read(&self) -> PhaseCounters {
        PhaseCounters {
            items: self.completed_items.load(Ordering::Relaxed),
            total_items: self.planned_items.load(Ordering::Relaxed),
            bytes: self.completed_bytes.load(Ordering::Relaxed),
            total_bytes: self.planned_bytes.load(Ordering::Relaxed),
        }
    }

    /// The last reading of a phase, written whether or not the interval has
    /// passed. A phase that ends between reports would otherwise leave the job
    /// showing where it was partway through.
    pub(crate) fn flush(&self) {
        if self.nothing_planned() {
            return;
        }
        if let Ok(mut reported) = self.reported.lock() {
            *reported = Instant::now();
        }
        (self.sink)(self.read());
    }

    /// A phase with nothing to do says nothing, rather than replacing what the
    /// phase before it reported with a pair of zeroes.
    fn nothing_planned(&self) -> bool {
        self.planned_items.load(Ordering::Relaxed) == 0
            && self.planned_bytes.load(Ordering::Relaxed) == 0
    }

    fn report(&self) {
        if self.nothing_planned() {
            return;
        }
        let Ok(mut reported) = self.reported.lock() else {
            return;
        };
        let now = Instant::now();
        if now.duration_since(*reported) < REPORT_INTERVAL {
            return;
        }
        *reported = now;
        drop(reported);
        (self.sink)(self.read());
    }
}
