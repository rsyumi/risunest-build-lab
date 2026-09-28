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
/// Every domain the phase carries plans itself here as it starts, so the
/// planned totals grow as work is discovered and never shrink.
pub(crate) struct PhaseProgress {
    completed_items: AtomicU64,
    completed_bytes: AtomicU64,
    planned_items: AtomicU64,
    planned_bytes: AtomicU64,
    reported: Mutex<Instant>,
    sink: Box<dyn Fn(PhaseCounters) + Send + Sync>,
}

/// One reading of the counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PhaseCounters {
    pub items: u64,
    pub total_items: u64,
    pub bytes: u64,
    pub total_bytes: u64,
}

impl PhaseProgress {
    pub(crate) fn new(sink: impl Fn(PhaseCounters) + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            completed_items: AtomicU64::new(0),
            completed_bytes: AtomicU64::new(0),
            planned_items: AtomicU64::new(0),
            planned_bytes: AtomicU64::new(0),
            // Far enough back that the first reading reports.
            reported: Mutex::new(Instant::now() - REPORT_INTERVAL),
            sink: Box::new(sink),
        })
    }

    /// Nothing is reported. For the paths that run without a job to tell.
    pub(crate) fn silent() -> Arc<Self> {
        Self::new(|_| {})
    }

    /// What one domain has to do, added to the totals before it does any of it.
    pub(crate) fn plan(&self, items: u64, bytes: u64) {
        self.planned_items.fetch_add(items, Ordering::Relaxed);
        self.planned_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.report();
    }

    /// One item is done, whether it was worked through here or answered for by
    /// something the device already held.
    pub(crate) fn completed(&self, bytes: u64) {
        self.completed_items.fetch_add(1, Ordering::Relaxed);
        self.completed_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.report();
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
