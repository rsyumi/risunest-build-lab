//! Restores a local snapshot as a native file job. Staging reports progress and stops on cancel;
//! activation waits for the renderer's confirmation like every other library restore.

use super::{error::store_error, JobControl, JobPhase, JobProgress, JobResultSummary, NativeJobError, SourceFingerprintKind};
use crate::local_backup::CancellationProbe;
use crate::persistent_store::{PersistentStore, SnapshotRestoreStep, StoreError};
use std::sync::{atomic::AtomicBool, Arc, Mutex};

struct Probe<'a> {
    job: &'a JobControl,
    progress: Mutex<(JobProgress, Option<String>)>,
}

impl<'a> Probe<'a> {
    fn new(job: &'a JobControl) -> Self {
        Self { job, progress: Mutex::new((JobProgress::default(), None)) }
    }

    fn track_bytes(&self, total: u64) -> Result<(), String> {
        let mut state = self.progress.lock().map_err(|_| "snapshot restore progress is unavailable".to_owned())?;
        state.0.total_bytes = Some(total);
        self.job.set_progress(state.0)
    }

    fn result(&self) -> Result<(), String> {
        match self.progress.lock().map_err(|_| "snapshot restore progress is unavailable".to_owned())?.1.as_ref() {
            Some(message) => Err(message.clone()),
            None => Ok(()),
        }
    }
}

impl CancellationProbe for Probe<'_> {
    fn is_cancelled(&self) -> bool {
        self.job.is_cancel_requested()
    }

    fn cancellation_flag(&self) -> Option<Arc<AtomicBool>> {
        Some(self.job.cancellation_flag())
    }

    fn backup_bytes_processed(&self, bytes: u64) {
        let Ok(mut state) = self.progress.lock() else { return };
        let Some(total) = state.0.total_bytes else { return };
        state.0.completed_bytes = state.0.completed_bytes.saturating_add(bytes).min(total);
        if let Err(error) = self.job.set_progress(state.0) {
            state.1.get_or_insert(error);
        }
    }
}

fn cancelled() -> NativeJobError {
    NativeJobError::new("cancelled", "Snapshot restore was cancelled")
}

fn job_failure(job: &JobControl, error: String) -> NativeJobError {
    if job.is_cancel_requested() { cancelled() } else { NativeJobError::new("store-error", error) }
}

fn store_failure(job: &JobControl, error: StoreError) -> NativeJobError {
    if job.is_cancel_requested() { cancelled() } else { store_error(error) }
}

pub(super) fn restore_native_snapshot(
    snapshot_id: &str,
    expected_revision: i64,
    mut store: PersistentStore,
    job: &JobControl,
) -> Result<JobResultSummary, NativeJobError> {
    job.start(JobPhase::ReadingSource).map_err(|error| job_failure(job, error))?;
    let probe = Probe::new(job);
    let mut source = None;
    let stage = store
        .snapshot_restore_stage_observed(snapshot_id, &job.id(), &probe, &mut |step| {
            let reported = match step {
                SnapshotRestoreStep::Reading { bytes, sha256 } => {
                    source = Some((bytes, sha256));
                    probe.track_bytes(bytes)
                }
                SnapshotRestoreStep::Staging => job.set_phase(JobPhase::StagingDatabase),
            };
            reported.map_err(|message| StoreError::Store { message })
        })
        .map_err(|error| store_failure(job, error))?;
    let mut committed = false;
    let outcome = (|| {
        probe.result().map_err(|error| job_failure(job, error))?;
        let (source_bytes, source_sha256) =
            source.take().ok_or_else(|| NativeJobError::new("store-error", "Snapshot source identity is unavailable"))?;
        job.status.lock().map_err(|_| NativeJobError::new("store-error", "Snapshot restore status is unavailable"))?
            .snapshot_staging_id = Some(stage.staging_id.clone());
        let counts = store.portable_staged_counts(&stage.staging_id).map_err(|error| store_failure(job, error))?;
        let revision = job
            .wait_for_restore_finalization()
            .map_err(|error| job_failure(job, error))?
            .unwrap_or(expected_revision);
        if job.is_cancel_requested() {
            return Err(cancelled());
        }
        let authority = store.lww_binding_authority().map_err(|error| store_failure(job, error))?;
        let activated = store
            .snapshot_restore_activate(&stage.staging_id, revision, authority.clone())
            .map_err(store_error)?;
        committed = true;
        {
            let mut status = job.status.lock().map_err(|_| NativeJobError::new("store-error", "Snapshot restore status is unavailable"))?;
            status.activation_revision = Some(activated.revision);
            status.activation_authority = Some(authority.0.to_string());
        }
        Ok(JobResultSummary {
            export_exclusions: None,
            revision: activated.revision,
            source_bytes,
            source_sha256,
            source_fingerprint_kind: SourceFingerprintKind::WholeFileSha256,
            character_count: counts.0,
            preset_count: counts.1,
            warning_codes: Vec::new(),
            handoff_path: None,
            publication: None,
        })
    })();
    match outcome {
        Err(mut failure) if !committed => {
            // The bodies job reads the stage after activation, so only an unactivated stage is removed.
            if let Err(cleanup) = store.snapshot_restore_abort(&stage.staging_id) {
                failure = NativeJobError::new(&failure.code, format!("{}; staging abort failed: {cleanup}", failure.message));
            }
            Err(failure)
        }
        outcome => outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{FinalizeOutcome, JobKind, JobRegistry, JobStatus};
    use super::*;
    use crate::server_sync::lww_tests::local;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    fn job(revision: i64) -> Arc<JobControl> {
        JobRegistry::default().create_with_context(JobKind::RestoreNativeSnapshot, Some(revision), Vec::new()).unwrap()
    }

    fn wait_for(job: &JobControl, ready: impl Fn(&JobStatus) -> bool) -> JobStatus {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let status = job.status();
            if ready(&status) || status.state.is_terminal() || Instant::now() > deadline {
                return status;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn a_snapshot_restore_waits_for_confirmation_and_reports_the_receipt_its_bodies_need() {
        let (_root, mut store) = local();
        let snapshot = store.snapshot_create("synthetic-restore-job").unwrap();
        let revision = store.revision().unwrap();
        let job = job(revision);
        let (runner, worker, id) = (Arc::clone(&job), store.open_native_job_store().unwrap(), snapshot.id.clone());
        let running = std::thread::spawn(move || restore_native_snapshot(&id, revision, worker, &runner));
        let waiting = wait_for(&job, |status| status.phase == JobPhase::AwaitingActivation);
        assert_eq!(waiting.phase, JobPhase::AwaitingActivation);
        let total = waiting.progress.total_bytes.unwrap();
        assert!(total > 0);
        assert_eq!(waiting.progress.completed_bytes, total);
        let stage = waiting.snapshot_staging_id.clone().unwrap();
        assert_eq!(store.revision().unwrap(), revision);
        assert_eq!(job.request_finalize(Some(revision)).unwrap(), FinalizeOutcome::Requested);
        let summary = running.join().unwrap().unwrap();
        let status = job.status();
        let authority = store.lww_binding_authority().unwrap().0.to_string();
        assert_eq!(status.activation_revision, Some(summary.revision));
        assert_eq!(status.activation_authority.as_deref(), Some(authority.as_str()));
        assert_eq!((summary.source_bytes, summary.source_sha256.len()), (total, 64));
        assert_eq!(summary.source_fingerprint_kind, SourceFingerprintKind::WholeFileSha256);
        assert_eq!(store.revision().unwrap(), summary.revision);
        store.snapshot_restore_body_plan(&stage, summary.revision, &authority).unwrap();
        assert_eq!(store.snapshot_restore_leftovers().unwrap(), (vec![format!("{stage}:committed")], vec![format!("restore-source-{stage}.sqlite")]));
    }

    #[test]
    fn cancelling_a_snapshot_restore_at_its_confirmation_leaves_no_stage() {
        let (_root, mut store) = local();
        let snapshot = store.snapshot_create("synthetic-restore-job").unwrap();
        let revision = store.revision().unwrap();
        let job = job(revision);
        let (runner, worker, id) = (Arc::clone(&job), store.open_native_job_store().unwrap(), snapshot.id.clone());
        let running = std::thread::spawn(move || restore_native_snapshot(&id, revision, worker, &runner));
        assert_eq!(wait_for(&job, |status| status.phase == JobPhase::AwaitingActivation).phase, JobPhase::AwaitingActivation);
        job.request_cancel().unwrap();
        assert_eq!(running.join().unwrap().unwrap_err().code, "cancelled");
        assert_eq!(store.revision().unwrap(), revision);
        assert_eq!(store.snapshot_restore_leftovers().unwrap(), (Vec::new(), Vec::new()));
    }

    struct CancelAt {
        reading: bool,
        cancelled: Arc<AtomicBool>,
    }
    impl CancellationProbe for CancelAt {
        fn is_cancelled(&self) -> bool {
            self.cancelled.load(Ordering::Acquire)
        }
        fn cancellation_flag(&self) -> Option<Arc<AtomicBool>> {
            Some(Arc::clone(&self.cancelled))
        }
        fn backup_bytes_processed(&self, _bytes: u64) {
            if self.reading {
                self.cancelled.store(true, Ordering::Release);
            }
        }
    }

    #[test]
    fn a_staging_cancelled_while_reading_or_staging_leaves_no_stage_or_restore_file() {
        for reading in [true, false] {
            let (_root, mut store) = local();
            let snapshot = store.snapshot_create("synthetic-restore-job").unwrap();
            let revision = store.revision().unwrap();
            let probe = CancelAt { reading, cancelled: Arc::new(AtomicBool::new(false)) };
            let mut steps = Vec::new();
            let staged = store.snapshot_restore_stage_observed(&snapshot.id, "synthetic-cancelled-restore", &probe, &mut |step| {
                steps.push(matches!(step, SnapshotRestoreStep::Staging));
                if !reading && matches!(step, SnapshotRestoreStep::Staging) {
                    probe.cancelled.store(true, Ordering::Release);
                }
                Ok(())
            });
            assert!(staged.is_err(), "reading={reading}");
            assert_eq!(steps, if reading { vec![false] } else { vec![false, true] });
            assert_eq!(store.revision().unwrap(), revision);
            assert_eq!(store.snapshot_restore_leftovers().unwrap(), (Vec::new(), Vec::new()), "reading={reading}");
        }
    }
}
