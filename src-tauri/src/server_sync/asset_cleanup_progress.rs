use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Stage { Preparing, Checking, Uploading, Deleting, Finalizing }

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    pub sequence: u64,
    pub stage: Stage,
    pub completed_items: usize,
    pub total_items: Option<usize>,
    pub evicted_items: usize,
    pub evicted_bytes: u64,
}

pub(crate) struct Progress {
    snapshot: Snapshot,
    reported: Instant,
    sink: Box<dyn Fn(Snapshot) + Send + Sync>,
}

impl Progress {
    pub(crate) fn new(sink: impl Fn(Snapshot) + Send + Sync + 'static) -> Self {
        Self {
            snapshot: Snapshot { sequence: 0, stage: Stage::Preparing, completed_items: 0, total_items: None, evicted_items: 0, evicted_bytes: 0 },
            reported: Instant::now(), sink: Box::new(sink),
        }
    }
    pub(crate) fn stage(&mut self, stage: Stage, completed: usize, total: Option<usize>) {
        self.snapshot.stage = stage;
        self.snapshot.completed_items = completed;
        self.snapshot.total_items = total;
        self.flush();
    }
    pub(crate) fn completed(&mut self, removed: Option<u64>) {
        self.snapshot.completed_items += 1;
        if let Some(bytes) = removed {
            self.snapshot.evicted_items += 1;
            self.snapshot.evicted_bytes += bytes;
        }
        if self.reported.elapsed() >= Duration::from_millis(250) { self.flush(); }
    }
    pub(crate) fn flush(&mut self) {
        self.snapshot.sequence += 1;
        (self.sink)(self.snapshot.clone());
        self.reported = Instant::now();
    }
}

impl Drop for Progress {
    fn drop(&mut self) { self.flush(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stages_and_final_partial_counts_flush_while_per_file_updates_are_bounded() {
        let readings = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = readings.clone();
        let mut progress = Progress::new(move |value| observed.lock().unwrap().push(value));
        progress.stage(Stage::Checking, 0, Some(1000));
        progress.reported = Instant::now() + Duration::from_secs(60);
        for _ in 0..1000 { progress.completed(None); }
        assert_eq!(readings.lock().unwrap().len(), 1);
        progress.flush();
        assert_eq!(readings.lock().unwrap().last().unwrap().completed_items, 1000);
        progress.stage(Stage::Deleting, 0, Some(1000));
        progress.completed(Some(4096));
        drop(progress);
        let readings = readings.lock().unwrap();
        let last = readings.last().unwrap();
        assert_eq!((last.completed_items, last.evicted_items, last.evicted_bytes), (1, 1, 4096));
        assert!(readings.windows(2).all(|pair| pair[0].sequence < pair[1].sequence));
    }
}
