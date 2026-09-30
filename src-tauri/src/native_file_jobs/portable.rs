use super::{JobControl, JobPhase, JobResultSummary, NativeJobError, OpenedJobSource};
use crate::device_backup::{
    capture_native_sections, capture_prepared_native_sections, journal_prepared_native_sections,
    prepare_native_sections, resume_journaled_native_restore, DeviceBackupError, DeviceBackupState,
    Spool,
};
use crate::{
    asset_repository::{
        job_pins::{CasJobKind, CasObjectRole, CasReleaseOutcome, DurableCasJob},
        PayloadCas,
    },
    local_backup::CancellationProbe,
    persistent_store::PersistentStore,
    portable_backup::{self, Catalog, VerifiedArchive},
};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
};
use tauri::Manager;

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PortableSelection {
    pub(crate) library: bool,
    pub(crate) device_sections: Vec<String>,
    /// Absent brings the whole library; present brings only the records it names, closed over
    /// what those records refer to.
    #[serde(default)]
    pub(crate) items: Option<portable_backup::ArchiveSelection>,
    #[serde(default)]
    pub(crate) allow_source_preservation: bool,
}
impl Default for PortableSelection {
    fn default() -> Self {
        Self {
            library: true,
            device_sections: vec![],
            items: None,
            allow_source_preservation: false,
        }
    }
}
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RestorePreview {
    pub(crate) library_included: bool,
    pub(crate) repair_required: bool,
    pub(crate) device_sections: Vec<String>,
    /// What is wrong with the archive's library. An archive the gate refuses still reports this,
    /// which is the only way a reader learns what to leave out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) diagnosis: Option<crate::data_health::ScanResult>,
    /// The records the reader can choose between.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) items: Option<portable_backup::ArchiveInventory>,
}
fn restore_preview(
    archive: &VerifiedArchive,
    probe: &dyn CancellationProbe,
) -> Result<RestorePreview, NativeJobError> {
    let mut sections = Vec::new();
    let mut statement = archive
        .db
        .prepare(
            "SELECT section FROM device_sections WHERE included=1 AND complete=1 ORDER BY section",
        )
        .map_err(error)?;
    let mut rows = statement.query([]).map_err(error)?;
    while let Some(row) = rows.next().map_err(error)? {
        if sections.len() == 1024 {
            return Err(error("Archive contains too many device sections"));
        }
        let section: String = row.get(0).map_err(error)?;
        risunest_external_storage_format::section::SectionKind::parse(&section).map_err(|_| {
            NativeJobError::new(
                "invalid-source",
                "Portable backup contains an unsupported device section",
            )
        })?;
        sections.push(section);
    }
    let (diagnosis, items) = match archive.manifest.library_included {
        true => {
            let mut findings = crate::data_health::Findings::new(2000);
            archive.scan_library(&mut findings, probe).map_err(error)?;
            let items =
                portable_backup::archive_inventory(&archive.db, &findings.items).map_err(error)?;
            (
                Some(crate::data_health::ScanResult::new(
                    0,
                    archive_scanned_at(),
                    crate::data_health::ScanDepth::Deep,
                    findings,
                )),
                Some(items),
            )
        }
        false => (None, None),
    };
    Ok(RestorePreview {
        library_included: archive.manifest.library_included,
        repair_required: archive.manifest.repair_required,
        device_sections: sections,
        diagnosis,
        items,
    })
}

fn archive_scanned_at() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

fn append_cleanup_failure(
    primary: &mut NativeJobError,
    operation: &str,
    cleanup: impl std::fmt::Display,
) {
    *primary = NativeJobError::new(
        &primary.code,
        format!("{}; {operation} failed: {cleanup}", primary.message),
    );
}

struct Probe<'a> {
    job: &'a JobControl,
    progress: std::sync::Mutex<(super::JobProgress, bool, Option<String>)>,
}
impl<'a> Probe<'a> {
    fn new(job: &'a JobControl) -> Self { Self { job, progress: std::sync::Mutex::new((super::JobProgress::default(), false, None)) } }
    fn track_bytes(&self, total: u64) -> Result<(), NativeJobError> {
        let mut state = self.progress.lock().map_err(|_| error("portable progress mutex poisoned"))?;
        state.0.total_bytes = Some(total);
        state.1 = true;
        self.job.set_progress(state.0).map_err(error)
    }
    fn result(&self) -> Result<(), NativeJobError> {
        match self.progress.lock().map_err(|_| error("portable progress mutex poisoned"))?.2.as_ref() {
            Some(message) => Err(NativeJobError::new("job-error", message)), None => Ok(())
        }
    }
}
impl CancellationProbe for Probe<'_> {
    fn cancellation_flag(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> { Some(self.job.cancellation_flag()) }
    fn is_cancelled(&self) -> bool { self.job.is_cancel_requested() }
    fn backup_bytes_processed(&self, bytes: u64) {
        let mut state = self.progress.lock().unwrap();
        if state.1 {
            state.0.completed_bytes = state.0.completed_bytes.saturating_add(bytes).min(state.0.total_bytes.unwrap_or(u64::MAX));
            if let Err(error) = self.job.set_progress(state.0) { state.2 = Some(error); }
        }
    }
    fn backup_item_processed(&self) {
        let mut state = self.progress.lock().unwrap();
        state.0.completed_items = state.0.completed_items.saturating_add(1);
        if let Err(error) = self.job.set_progress(state.0) { state.2 = Some(error); }
    }
}
fn storage_full(mut error: &(dyn std::error::Error + 'static)) -> bool {
    loop {
        if error.downcast_ref::<std::io::Error>().is_some_and(|error| error.kind() == std::io::ErrorKind::StorageFull)
            || error.downcast_ref::<serde_json::Error>().is_some_and(|error| error.io_error_kind() == Some(std::io::ErrorKind::StorageFull))
            || error.downcast_ref::<rusqlite::Error>().is_some_and(|error| matches!(error, rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::DiskFull)) {
            return true;
        }
        match error.source() { Some(source) => error = source, None => return false }
    }
}
fn error(error: impl std::fmt::Display + 'static) -> NativeJobError {
    let value = &error as &dyn std::any::Any;
    if value.downcast_ref::<portable_backup::Error>().is_some_and(|error| matches!(error, portable_backup::Error::Cancelled)) {
        return NativeJobError::new("cancelled", "Portable backup was cancelled");
    }
    let full = value.downcast_ref::<std::io::Error>().is_some_and(|error| storage_full(error))
        || value.downcast_ref::<portable_backup::Error>().is_some_and(|error| storage_full(error))
        || value.downcast_ref::<rusqlite::Error>().is_some_and(|error| storage_full(error));
    NativeJobError::new(if full { "local-storage-full" } else { "portable-backup-failed" }, error.to_string())
}
fn device_error(failure: DeviceBackupError) -> NativeJobError {
    if failure.code == "device-cancelled" {
        NativeJobError::new("cancelled", failure.message)
    } else {
        error(failure)
    }
}
fn portable_error(failure: portable_backup::Error) -> NativeJobError {
    match failure {
        portable_backup::Error::Store(failure) => super::error::store_error(failure),
        portable_backup::Error::Cancelled => {
            NativeJobError::new("cancelled", "Portable backup was cancelled")
        }
        failure => error(failure),
    }
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn new_pins(store: &PersistentStore, kind: CasJobKind) -> Result<DurableCasJob, NativeJobError> {
    DurableCasJob::begin(
        store.repository_root(),
        &uuid::Uuid::new_v4().to_string(),
        kind,
        now(),
    )
    .map_err(error)
}

fn new_pins_for_job(
    store: &PersistentStore,
    kind: CasJobKind,
    job_id: &str,
) -> Result<DurableCasJob, NativeJobError> {
    DurableCasJob::begin(store.repository_root(), job_id, kind, now()).map_err(error)
}
pub(super) fn finish_durable_job(
    outcome: Result<JobResultSummary, NativeJobError>,
    durable: &mut DurableCasJob,
) -> Result<JobResultSummary, NativeJobError> {
    let release_outcome = if outcome.is_ok() {
        CasReleaseOutcome::Committed
    } else {
        CasReleaseOutcome::Aborted
    };
    match (outcome, durable.release(release_outcome)) {
        (Ok(mut result), Err(_)) => {
            result.warning_codes.push("cleanup-failed".into());
            Ok(result)
        }
        (Ok(result), Ok(())) => Ok(result),
        (Err(error), Err(cleanup)) => Err(NativeJobError::new(
            "cleanup-failed",
            format!(
                "{}; durable CAS job release failed: {cleanup}",
                error.message
            ),
        )),
        (Err(error), Ok(())) => Err(error),
    }
}

fn finish_pins(
    outcome: Result<JobResultSummary, NativeJobError>,
    pins: &mut DurableCasJob,
) -> Result<JobResultSummary, NativeJobError> {
    if pins.is_sealed() {
        return finish_durable_job(outcome, pins);
    }
    // Read-only exports did not install CAS objects. Their temporary pins are released without
    // claiming an object-store commit or registering previously unowned source objects.
    match (outcome, pins.release(CasReleaseOutcome::Aborted)) {
        (Ok(mut result), Err(_)) => {
            result.warning_codes.push("cleanup-failed".into());
            Ok(result)
        }
        (result, Ok(())) => result,
        (Err(failure), Err(_)) => Err(failure),
    }
}

fn capture(
    store: &mut PersistentStore,
    revision: i64,
    owned: &Path,
    pins: &mut DurableCasJob,
    probe: &dyn CancellationProbe,
    source_build: &str,
    allow_source_preservation: bool,
) -> Result<portable_backup::CapturedLibrary, NativeJobError> {
    match portable_backup::capture_library(store, revision, owned, pins, false, probe, source_build) {
        Err(portable_backup::Error::SourceNeedsPreservation) => {
            if !allow_source_preservation {
                return Err(NativeJobError::new(
                    "source-preservation-confirmation-required",
                    "Source preservation may include account information and requires confirmation",
                ));
            }
            pins.release(CasReleaseOutcome::Aborted).map_err(error)?;
            *pins = new_pins(store, CasJobKind::OfficialPublicationOrExportPreparation)?;
            portable_backup::capture_library(store, revision, owned, pins, true, probe, source_build)
                .map_err(portable_error)
        }
        result => result.map_err(portable_error),
    }
}

fn write_verified(
    catalog: Catalog,
    repair: bool,
    path: &Path,
    owned: &Path,
    probe: &dyn CancellationProbe,
) -> Result<VerifiedArchive, NativeJobError> {
    catalog
        .write_candidate(path, repair, probe)
        .map_err(portable_error)?;
    let verified = VerifiedArchive::open(File::open(path).map_err(error)?, owned, probe)
        .map_err(portable_error)?;
    if !verified.manifest.repair_required && verified.manifest.library_included {
        verified.validate_library(probe).map_err(error)?;
    }
    Ok(verified)
}

fn publish(
    source: &Path,
    destination: &Path,
    owned: &Path,
    job: &JobControl,
    probe: &Probe<'_>,
) -> Result<crate::persistent_store::export::destination::DestinationWriteResult, NativeJobError> {
    let copied = std::cell::Cell::new(0_u64);
    crate::persistent_store::export::destination::write_portable_destination_controlled(
        owned,
        source,
        destination
            .parent()
            .ok_or_else(|| error("destination has no parent"))?,
        destination,
        || job.is_cancel_requested(),
        |progress| {
            let previous = copied.replace(progress.copied_bytes);
            probe.backup_bytes_processed(progress.copied_bytes.saturating_sub(previous));
        },
        || {
            job.commit_export_publication().map_err(|message| {
                if job.is_cancel_requested() {
                    crate::persistent_store::export::destination::DestinationWriteError::Cancelled
                } else {
                    crate::persistent_store::export::destination::DestinationWriteError::Io {
                        operation: "commit portable export publication",
                        source: std::io::Error::other(message),
                    }
                }
            })
        },
    )
    .map_err(|failure| {
        if let crate::persistent_store::export::destination::DestinationWriteError::Io { source, .. } = &failure {
            if storage_full(source) { return NativeJobError::new("local-storage-full", source.to_string()); }
        }
        super::error::destination_error_with(
            failure,
            "Verified portable archive is unavailable",
            "Portable backup destination is invalid",
            "Portable backup publication was cancelled",
        )
    })
}

pub(crate) fn export_portable(
    destination: Option<&Path>,
    revision: i64,
    owned: &Path,
    handoffs: &Path,
    store: PersistentStore,
    job: &JobControl,
    device: Option<(&tauri::AppHandle, &PortableSelection)>,
    source_build: &str,
) -> Result<JobResultSummary, NativeJobError> {
    super::job_transition(job, job.start(JobPhase::WritingExport))?;
    let fallback = PortableSelection::default();
    let selection = device.map(|(_, selection)| selection).unwrap_or(&fallback);
    export_portable_running(
        destination,
        revision,
        owned,
        handoffs,
        store,
        job,
        selection,
        source_build,
    )
}

fn export_portable_running(
    destination: Option<&Path>,
    revision: i64,
    owned: &Path,
    handoffs: &Path,
    mut store: PersistentStore,
    job: &JobControl,
    selection: &PortableSelection,
    source_build: &str,
) -> Result<JobResultSummary, NativeJobError> {
    if !selection.library && selection.device_sections.is_empty() {
        return Err(error("Select at least one backup section"));
    }
    if store.revision().map_err(error)? != revision {
        return Err(NativeJobError::new(
            "revision-conflict",
            "Library changed before portable backup capture",
        ));
    }
    if selection.library {
        require_capacity(owned, store.portable_export_lower_bound().map_err(error)?)?;
    }
    let probe = Probe::new(job);
    let mut pins = new_pins(&store, CasJobKind::OfficialPublicationOrExportPreparation)?;
    let outcome = (|| {
        let captured = if selection.library {
            capture(&mut store, revision, owned, &mut pins, &probe, source_build, selection.allow_source_preservation)?
        } else {
            let catalog =
                Catalog::create(owned, source_build, revision).map_err(error)?;
            catalog
                .db
                .execute(
                    "UPDATE backup_info SET value='false' WHERE key='libraryIncluded'",
                    [],
                )
                .map_err(error)?;
            portable_backup::CapturedLibrary {
                catalog,
                repair_required: false,
            }
        };
        if !selection.device_sections.is_empty() {
            capture_native_sections(
                &mut store,
                &selection.device_sections,
                &captured.catalog,
                &probe,
            )
            .map_err(device_error)?;
        }
        let payload_bytes: i64 = captured.catalog.db.query_row("SELECT coalesce(sum(byte_length),0) FROM objects", [], |row| row.get(0)).map_err(error)?;
        probe.track_bytes((payload_bytes.max(0) as u64).saturating_mul(3))?;
        let path = owned.join("archive.risunest.part");
        let archive = write_verified(
            captured.catalog,
            captured.repair_required,
            &path,
            owned,
            &probe,
        )?;
        let counts = counts(&archive)?;
        let repair = archive.manifest.repair_required;
        drop(archive);
        let (destination, handoff) = match destination {
            Some(path) => (path.to_path_buf(), None),
            None => {
                fs::create_dir_all(handoffs).map_err(error)?;
                let path =
                    handoffs.join(format!("risunest-backup-{}.risunest", uuid::Uuid::new_v4()));
                (path.clone(), Some(path.to_string_lossy().into_owned()))
            }
        };
        super::job_transition(job, job.set_phase(JobPhase::PublishingDestination))?;
        probe.result()?;
        let published = publish(&path, &destination, owned, job, &probe)?;
        probe.result()?;
        Ok(JobResultSummary {
            export_exclusions: None,
            revision,
            source_bytes: published.bytes,
            source_sha256: published.sha256,
            character_count: counts.0,
            preset_count: counts.1,
            warning_codes: if repair {
                vec!["source-preserved-repair-required".into()]
            } else {
                vec![]
            },
            handoff_path: handoff,
            publication: None,
        })
    })();
    finish_pins(outcome, &mut pins)
}

fn begin_native_restore(
    app: &tauri::AppHandle,
    selection: &PortableSelection,
    store: &mut PersistentStore,
    revision: i64,
    stage: Option<&str>,
    job: &JobControl,
    source: &[crate::device_backup::PreparedDeviceSection],
    probe: &dyn CancellationProbe,
    durable_session_started: &mut bool,
) -> Result<String, NativeJobError> {
    let guard = app
        .state::<crate::persistent_store::commands::PersistentStoreState>()
        .acquire_device_maintenance()
        .map_err(error)?;
    let coordinator = app.state::<DeviceBackupState>();
    coordinator.attach_maintenance_guard(guard).map_err(error)?;
    let id = match coordinator.create_native_portable_session(
        &job.id(),
        selection.library,
        &selection.device_sections,
        revision,
        stage.map(str::to_owned),
    ) {
        Ok(id) => id,
        Err(failure) => {
            let _ = coordinator.release_unused_maintenance();
            return Err(error(failure));
        }
    };
    if let Err(failure) = job.set_device_session(&id) {
        let failure = error(failure);
        let _ = coordinator.fail(&id, "archive-restore-setup-failed");
        let _ = coordinator.recovery_complete(&id);
        let _ = coordinator.cleanup(&id);
        return Err(failure);
    }
    *durable_session_started = true;
    let prepared = (|| {
        journal_prepared_native_sections(&coordinator, &id, Spool::Source, source)
            .map_err(error)?;
        coordinator.source_ready(&id).map_err(error)?;
        let rollback = capture_prepared_native_sections(store, &selection.device_sections, probe)
            .map_err(error)?;
        journal_prepared_native_sections(&coordinator, &id, Spool::Rollback, &rollback)
            .map_err(error)?;
        coordinator.prepared(&id).map_err(error)?;
        Ok(())
    })();
    if let Err(failure) = prepared {
        return Err(failure);
    }
    Ok(id)
}

pub(crate) fn restore_portable(
    mut source: OpenedJobSource,
    already_owned: bool,
    mut revision: i64,
    owned: &Path,
    mut store: PersistentStore,
    job: &JobControl,
    device: Option<(&tauri::AppHandle, Option<&PortableSelection>)>,
) -> Result<JobResultSummary, NativeJobError> {
    job.start(JobPhase::ReadingSource).map_err(error)?;
    let probe = Probe::new(job);
    probe.track_bytes(source.total_bytes.saturating_mul(3))?;
    let (input, source_sha256) = if already_owned {
        let hash = portable_backup::copy_hash(
            &mut source.file,
            &mut std::io::sink(),
            source.total_bytes,
            &probe,
        )
        .map_err(error)?;
        if source.file.read(&mut [0]).map_err(error)? != 0 {
            return Err(error("owned input length changed"));
        }
        source.file.seek(SeekFrom::Start(0)).map_err(error)?;
        (source.file, hash)
    } else {
        require_capacity(owned, source.total_bytes)?;
        let identity = crate::asset_repository::exact_file_identity(&source.file).map_err(error)?;
        let path = owned.join("input.risunest");
        let mut output = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .read(true)
            .open(path)
            .map_err(error)?;
        let hash =
            portable_backup::copy_hash(&mut source.file, &mut output, source.total_bytes, &probe)
                .map_err(error)?;
        if source.file.read(&mut [0]).map_err(error)? != 0 {
            return Err(error("source changed while copying"));
        }
        if crate::asset_repository::exact_file_identity(&source.file).map_err(error)? != identity {
            return Err(error("source identity changed while copying"));
        }
        output.sync_all().map_err(error)?;
        output.seek(SeekFrom::Start(0)).map_err(error)?;
        (output, hash)
    };
    let mut input = input;
    if super::raw_recovery::is_raw_recovery_archive(&mut input)? {
        return Err(NativeJobError::new(
            "rescue-format-not-restorable",
            "RisuNest rescue archives cannot be imported or restored",
        ));
    }
    let archive = VerifiedArchive::open(input, owned, &probe).map_err(error)?;
    let fallback = match device {
        Some((_, None)) => job
            .wait_for_portable_selection(restore_preview(&archive, &probe)?)
            .map_err(error)?,
        _ => PortableSelection::default(),
    };
    let selection = device
        .and_then(|(_, selection)| selection)
        .unwrap_or(&fallback);
    let app = device.map(|(app, _)| app);
    if !selection.library && selection.device_sections.is_empty() {
        return Err(error("Select at least one backup section"));
    }
    job.set_replaces_library(selection.library).map_err(error)?;
    if selection.library && selection.items.is_none() {
        archive.validate_library(&probe).map_err(error)?;
    }
    let mut pins = new_pins_for_job(&store, CasJobKind::LocalBackupRestore, &job.id())?;
    let mut journal_owned = false;
    let outcome = (|| {
        job.set_phase(JobPhase::StagingDatabase).map_err(error)?;
        let stage = match (selection.library, selection.items.as_ref()) {
            (false, _) => None,
            (true, None) => Some(
                store
                    .stage_portable_records(&archive.db, &probe)
                    .map_err(error)?,
            ),
            (true, Some(items)) => {
                let closed = portable_backup::close_selection(&archive.db, items).map_err(error)?;
                Some(
                    crate::persistent_store::portable::stage_portable_records_selected(
                        &mut store,
                        &archive.db,
                        &closed,
                        &probe,
                    )
                    .map_err(error)?,
                )
            }
        };
        let prepared_device =
            prepare_native_sections(&archive, &selection.device_sections, &probe).map_err(error)?;
        let mut committed = false;
        let activated = (|| {
            if selection.library {
                let inventory = portable_backup::RestoreInventory::build(&archive, owned, &probe)
                    .map_err(error)?;
                install(&archive, &inventory, &mut store, &mut pins, &probe)?;
                if let Some(report) = inventory
                    .preserve(&archive, store.repository_root(), &probe)
                    .map_err(error)?
                {
                    job.set_preservation_report(report).map_err(error)?;
                }
            }
            let counts = match stage.as_ref() {
                Some(stage) => store.portable_staged_counts(&stage.staging_id).map_err(error)?,
                None => (0, 0),
            };
            if let Some(finalized) = job.wait_for_restore_finalization().map_err(error)? {
                revision = finalized;
            }
            if store.revision().map_err(error)? != revision {
                return Err(NativeJobError::new(
                    "revision-conflict",
                    "Library changed before portable restore activation",
                ));
            }
            if selection.library {
                if probe.is_cancelled() {
                    return Err(NativeJobError::new(
                        "cancelled",
                        "Portable restore cancelled before activation",
                    ));
                }
                let status = store.server_status().map_err(|_| {
                    NativeJobError::new(
                        "server-status-unavailable",
                        "Cannot verify server operation state",
                    )
                })?;
                if status.operation_pending {
                    return Err(NativeJobError::new(
                        "resolve-pending-operation-first",
                        "Resolve the pending server operation before restoring",
                    ));
                }
            }
            probe.result()?;
            let warning_codes = vec![];
            let final_revision = if !prepared_device.is_empty() {
                let app = app.ok_or_else(|| {
                    NativeJobError::new(
                        "device-maintenance-unavailable",
                        "Native device restore requires the application maintenance state",
                    )
                })?;
                let mut durable_session_started = false;
                let session = match begin_native_restore(
                    app,
                    selection,
                    &mut store,
                    revision,
                    stage.as_ref().map(|stage| stage.staging_id.as_str()),
                    job,
                    &prepared_device,
                    &probe,
                    &mut durable_session_started,
                ) {
                    Ok(session) => session,
                    Err(failure) => {
                        if durable_session_started {
                            committed = true;
                            journal_owned = selection.library;
                        }
                        return Err(failure);
                    }
                };
                // From this point the existing durable device journal and the PDS
                // stage own recovery. A failed attempt resumes this accepted restore.
                committed = true;
                journal_owned = selection.library;
                let result = resume_journaled_native_restore(
                    &app.state::<DeviceBackupState>(),
                    &session,
                    &mut store,
                )
                .map_err(error)?;
                result
            } else if let Some(stage) = stage.as_ref() {
                let prepared = store
                    .prepare_replace_commit(&stage.staging_id, Some(revision))
                    .map_err(super::error::store_error)?;
                let result = store.finish_prepared_replace(prepared).map_err(error)?;
                committed = true;
                result.revision
            } else {
                committed = true;
                revision
            };
            Ok(JobResultSummary {
                export_exclusions: None,
                revision: final_revision,
                source_bytes: source.total_bytes,
                source_sha256,
                character_count: if selection.library { counts.0 } else { 0 },
                preset_count: if selection.library { counts.1 } else { 0 },
                warning_codes,
                handoff_path: None,
                publication: None,
            })
        })();
        match activated {
            Err(mut failure) if !committed => {
                if let Some(stage) = stage.as_ref() {
                    if let Err(cleanup) = store.replace_abort(&stage.staging_id) {
                        append_cleanup_failure(&mut failure, "staging abort", cleanup);
                    }
                }
                Err(failure)
            }
            outcome => outcome,
        }
    })();
    if journal_owned && outcome.is_err() {
        outcome
    } else {
        finish_pins(outcome, &mut pins)
    }
}

fn require_capacity(directory: &Path, bytes: u64) -> Result<(), NativeJobError> {
    if let Ok(available) = fs2::available_space(directory) {
        check_capacity(available, bytes)?;
    }
    Ok(())
}

fn check_capacity(available: u64, bytes: u64) -> Result<(), NativeJobError> {
    let required = bytes.checked_add(1024 * 1024).ok_or_else(|| NativeJobError::new("insufficient-storage", "Backup staging size exceeds available storage"))?;
    if available < required {
        return Err(NativeJobError::new("insufficient-storage", "Not enough storage to stage the backup"));
    }
    Ok(())
}

fn install(
    archive: &VerifiedArchive,
    inventory: &portable_backup::RestoreInventory,
    store: &mut PersistentStore,
    pins: &mut DurableCasJob,
    probe: &dyn CancellationProbe,
) -> Result<(), NativeJobError> {
    let cas = PayloadCas::new(store.repository_root()).map_err(error)?;
    let mut statement = inventory
        .db
        .prepare("SELECT hash,owner FROM live_objects ORDER BY hash")
        .map_err(error)?;
    let mut rows = statement.query([]).map_err(error)?;
    while let Some(row) = rows.next().map_err(error)? {
        if probe.is_cancelled() {
            return Err(NativeJobError::new("cancelled", "Portable restore cancelled during CAS staging"));
        }
        let hash: String = row.get(0).map_err(error)?;
        let owner: bool = row.get(1).map_err(error)?;
        let (input, size) = archive.open_object(&hash).map_err(error)?;
        let mut input = CancelledRead { input, probe };
        pins.prepare_reader_expected(
            &cas,
            &mut input,
            &hash,
            size,
            if owner {
                CasObjectRole::OwnerManifest
            } else {
                CasObjectRole::DirectObject
            },
        )
        .map_err(error)?;
    }
    pins.seal(store, now()).map_err(error)?;
    Ok(())
}
struct CancelledRead<'a, R> {
    input: R,
    probe: &'a dyn CancellationProbe,
}
impl<R: Read> Read for CancelledRead<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if self.probe.is_cancelled() {
            return Err(std::io::Error::other("portable restore cancelled"));
        }
        let count = self.input.read(bytes)?;
        self.probe.backup_bytes_processed(count as u64);
        Ok(count)
    }
}

fn counts(archive: &VerifiedArchive) -> Result<(u64, u64), NativeJobError> {
    Ok((archive.db.query_row("SELECT count(*) FROM characters", [], |row| row.get::<_,i64>(0)).map_err(error)? as u64,
        archive.db.query_row("SELECT count(*) FROM bot_presets", [], |row| row.get::<_,i64>(0)).map_err(error)? as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_backup::NeverCancelled;
    use crate::persistent_store::portable::digest_raw_tables;
    use std::io::Write;

    fn raw_recovery_archive(path: &Path) -> u64 {
        let file = File::create(path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file(
                "manifest.json",
                zip::write::FileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        archive
            .write_all(br#"{"format":"risunest-raw-recovery","version":1}"#)
            .unwrap();
        archive.finish().unwrap();
        fs::metadata(path).unwrap().len()
    }

    #[test]
    fn portable_restore_rejects_a_renamed_raw_recovery_archive() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("renamed.risunest");
        let bytes = raw_recovery_archive(&source);
        let target = directory.path().join("target");
        let store = library(&target);
        let owned = directory.path().join("restore-job");
        fs::create_dir(&owned).unwrap();
        let job = super::super::JobRegistry::default()
            .create_internal(
                super::super::JobKind::RestorePortableBackup,
                Some(1),
                vec![],
                false,
            )
            .unwrap();

        let error = restore_portable(
            OpenedJobSource {
                file: File::open(source).unwrap(),
                total_bytes: bytes,
            },
            true,
            1,
            &owned,
            store,
            &job,
            None,
        )
        .unwrap_err();

        assert_eq!(error.code, "rescue-format-not-restorable");
        let store = PersistentStore::open(&target).unwrap();
        assert_eq!(store.revision().unwrap(), 1);
    }

    pub(super) fn library(root: &Path) -> PersistentStore {
        let mut store = PersistentStore::open(root).unwrap();
        let database: serde_json::Value =
            serde_json::from_str(include_str!("../../fixtures/persistent-fixture.json")).unwrap();
        let stage = store.replace_begin().unwrap();
        let mut value = database.clone();
        value.as_object_mut().unwrap().remove("characters");
        value.as_object_mut().unwrap().remove("botPresets");
        store.replace_put_root(&stage.staging_id, &value).unwrap();
        store
            .replace_put_presets(
                &stage.staging_id,
                database["botPresets"].as_array().unwrap(),
            )
            .unwrap();
        store
            .replace_add_characters(
                &stage.staging_id,
                database["characters"].as_array().unwrap(),
            )
            .unwrap();
        store.replace_commit(&stage.staging_id, Some(0)).unwrap();
        store
    }

    #[test]
    fn cancelled_device_capture_returns_the_canonical_worker_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let owned = directory.path().join("job");
        fs::create_dir(&owned).unwrap();
        let store = library(&source);
        let revision = store.revision().unwrap();
        let registry = super::super::JobRegistry::default();
        let job = registry
            .create_internal(
                super::super::JobKind::ExportPortableBackup,
                Some(revision),
                vec![],
                false,
            )
            .unwrap();
        job.start(JobPhase::WritingExport).unwrap();
        assert_eq!(
            job.request_cancel().unwrap(),
            super::super::CancelOutcome::Requested
        );

        let failure = export_portable_running(
            None,
            revision,
            &owned,
            &directory.path().join("handoffs"),
            store,
            &job,
                &PortableSelection {
                library: false,
                device_sections: vec!["hypa".into()],
                items: None,
                allow_source_preservation: false,
            },
            "9.8.7-synthetic",
        )
        .unwrap_err();

        assert_eq!(failure.code, "cancelled");
        assert_eq!(failure.message, "Device catalog verification was cancelled");
        assert_eq!(job.status().state, super::super::JobState::Cancelling);
        super::super::finish_worker_outcome(
            &job,
            super::super::JobKind::ExportPortableBackup,
            Err(failure),
            vec![],
        );
        let status = job.status();
        assert_eq!(status.state, super::super::JobState::Cancelled);
        assert!(status.error.is_none());
        assert!(status.result.is_none());
        assert!(!owned.join("archive.risunest.part").exists());
        assert!(!directory.path().join("handoffs").exists());
    }

    #[test]
    fn non_cancellation_device_errors_keep_the_portable_failure_contract() {
        let failure = device_error(DeviceBackupError {
            code: "device-storage-failed".into(),
            message: "synthetic storage failure".into(),
        });

        assert_eq!(failure.code, "portable-backup-failed");
        assert_eq!(
            failure.message,
            "device-storage-failed: synthetic storage failure"
        );
    }

    fn add_test_aliases(root: &Path, hash: &str, size: usize, count: usize) {
        let mut db = rusqlite::Connection::open(root.join("persistent/persistent.sqlite")).unwrap();
        let generation: String = serde_json::from_str(
            &db.query_row::<String, _, _>(
                "SELECT value FROM meta WHERE key='activeGeneration'",
                [],
                |r| r.get(0),
            )
            .unwrap(),
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        for index in 0..count {
            tx.execute("INSERT INTO asset_aliases(generation,logical_key,object_hash,kind,size,mime,name,ext,inlay_type,width,height,metadata) VALUES(?1,?2,?3,'asset',?4,'application/octet-stream','','',NULL,NULL,NULL,'{}')",rusqlite::params![generation, format!("assets/synthetic-{index}.bin"),hash,size as i64]).unwrap();
        }
        tx.commit().unwrap();
    }

    fn test_archive(root: &Path, with_asset: bool) -> (std::path::PathBuf, u64, Option<String>) {
        let source = root.join("source");
        let jobs = root.join("export-job");
        fs::create_dir(&jobs).unwrap();
        let store = library(&source);
        let hash = with_asset.then(|| {
            let payload = b"synthetic incoming object";
            let prepared = PayloadCas::new(&source)
                .unwrap()
                .prepare_bytes(payload)
                .unwrap();
            add_test_aliases(&source, &prepared.content_hash, payload.len(), 1);
            prepared.content_hash
        });
        let job = super::super::JobRegistry::default()
            .create_internal(
                super::super::JobKind::ExportPortableBackup,
                Some(1),
                vec![],
                false,
            )
            .unwrap();
        let result =
            export_portable(None, 1, &jobs, &root.join("handoffs"), store, &job, None, "9.8.7-synthetic").unwrap();
        (
            result.handoff_path.unwrap().into(),
            result.source_bytes,
            hash,
        )
    }

    #[test]
    fn portable_progress_reports_real_bytes_without_resetting_between_passes() {
        let job = super::super::JobRegistry::default().create(super::super::JobKind::ExportPortableBackup).unwrap();
        job.start(JobPhase::WritingExport).unwrap();
        let probe = Probe::new(&job);
        portable_backup::copy_hash(&mut &b"fixture"[..], &mut std::io::sink(), 7, &probe).unwrap();
        assert_eq!(job.status().progress.completed_items, 1);
        assert_eq!(job.status().progress.total_bytes, None);
        probe.track_bytes(21).unwrap();
        for expected in [7, 14, 21] {
            portable_backup::copy_hash(&mut &b"fixture"[..], &mut std::io::sink(), 7, &probe).unwrap();
            assert_eq!(job.status().progress.completed_bytes, expected);
            assert_eq!(job.status().progress.total_bytes, Some(21));
            probe.result().unwrap();
        }
    }

    #[test]
    fn portable_disk_full_keeps_its_classification_through_wrappers() {
        let disk_full = || std::io::Error::from(std::io::ErrorKind::StorageFull);
        assert_eq!(error(disk_full()).code, "local-storage-full");
        assert_eq!(error(portable_backup::Error::Io(disk_full())).code, "local-storage-full");
        assert_eq!(portable_error(portable_backup::Error::Zip(zip::result::ZipError::Io(disk_full()))).code, "local-storage-full");
    }

    #[test]
    fn portable_export_writes_runtime_build_provenance() {
        let directory = tempfile::tempdir().unwrap();
        let (path, _, _) = test_archive(directory.path(), false);
        let mut zip = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        let format: serde_json::Value = serde_json::from_reader(zip.by_name("format.json").unwrap()).unwrap();
        assert_eq!(format["sourceAppBuild"], "9.8.7-synthetic");
        assert_eq!(format["formatVersion"], 1);
    }

    #[test]
    fn device_only_portable_export_writes_runtime_build_provenance() {
        let directory = tempfile::tempdir().unwrap();
        let store = library(&directory.path().join("source"));
        let owned = directory.path().join("jobs");
        fs::create_dir_all(&owned).unwrap();
        let job = super::super::JobRegistry::default().create(super::super::JobKind::ExportPortableBackup).unwrap();
        job.start(JobPhase::WritingExport).unwrap();
        let selection = PortableSelection { library: false, device_sections: vec!["local-settings".into()], items: None, allow_source_preservation: false };
        let result = export_portable_running(None, 1, &owned, &directory.path().join("handoffs"), store, &job, &selection, "9.8.7-device").unwrap();
        let mut zip = zip::ZipArchive::new(File::open(result.handoff_path.unwrap()).unwrap()).unwrap();
        let format: serde_json::Value = serde_json::from_reader(zip.by_name("format.json").unwrap()).unwrap();
        assert_eq!(format["sourceAppBuild"], "9.8.7-device");
    }

    #[test]
    fn capacity_preflight_requires_copy_plus_margin_without_overflow() {
        assert!(check_capacity(1024 * 1024 + 7, 7).is_ok());
        assert_eq!(check_capacity(1024 * 1024 + 6, 7).unwrap_err().code, "insufficient-storage");
        assert_eq!(check_capacity(u64::MAX, u64::MAX).unwrap_err().code, "insufficient-storage");
    }

    #[test]
    fn restore_does_not_read_or_back_up_one_hundred_thousand_existing_asset_references() {
        let directory = tempfile::tempdir().unwrap();
        let (path, bytes, _) = test_archive(directory.path(), false);
        let target = directory.path().join("target");
        let store = library(&target);
        let payload = b"synthetic old payload stays untouched";
        let prepared = PayloadCas::new(&target)
            .unwrap()
            .prepare_bytes(payload)
            .unwrap();
        add_test_aliases(&target, &prepared.content_hash, payload.len(), 100_000);
        let object = target.join(&prepared.physical_key);
        #[cfg(windows)]
        let locked = {
            use std::os::windows::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&object)
                .unwrap()
        };
        let jobs = directory.path().join("restore-job");
        fs::create_dir(&jobs).unwrap();
        let job = super::super::JobRegistry::default()
            .create_internal(
                super::super::JobKind::RestorePortableBackup,
                Some(1),
                vec![],
                false,
            )
            .unwrap();
        let result = restore_portable(
            OpenedJobSource {
                file: File::open(path).unwrap(),
                total_bytes: bytes,
            },
            true,
            1,
            &jobs,
            store,
            &job,
            None,
        )
        .unwrap();
        assert_eq!(result.revision, 2);
        #[cfg(windows)]
        drop(locked);
        assert_eq!(fs::read(&object).unwrap(), payload);
        assert!(!target.join("persistent/recovery").exists());
        let store = PersistentStore::open(&target).unwrap();
        assert!(store.snapshot_list().unwrap().is_empty());
        let db = rusqlite::Connection::open(target.join("persistent/persistent.sqlite")).unwrap();
        assert_eq!(
            db.query_row::<i64, _, _>("SELECT count(*) FROM asset_aliases", [], |r| r.get(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn failed_restore_preserves_old_database_and_existing_objects_without_recovery_archive() {
        for corrupt_existing in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let (path, bytes, hash) = test_archive(directory.path(), true);
            let hash = hash.unwrap();
            let target = directory.path().join("target");
            let store = library(&target);
            let before = store.read_root(None).unwrap().value;
            let object = target
                .join("assets/objects")
                .join(&hash[..2])
                .join(&hash[2..]);
            fs::create_dir_all(object.parent().unwrap()).unwrap();
            let payload: &[u8] = if corrupt_existing {
                b"synthetic damaged object"
            } else {
                b"synthetic incoming object"
            };
            fs::write(&object, payload).unwrap();
            add_test_aliases(&target, &hash, payload.len(), 1);
            if !corrupt_existing {
                let db = rusqlite::Connection::open(target.join("persistent/persistent.sqlite"))
                    .unwrap();
                db.execute_batch("CREATE TRIGGER reject_replacement BEFORE DELETE ON root WHEN OLD.generation='revision-1' BEGIN SELECT RAISE(ABORT,'synthetic commit failure'); END;").unwrap();
            }
            let jobs = directory.path().join("restore-job");
            fs::create_dir(&jobs).unwrap();
            let job = super::super::JobRegistry::default()
                .create_internal(
                    super::super::JobKind::RestorePortableBackup,
                    Some(1),
                    vec![],
                    false,
                )
                .unwrap();
            assert!(restore_portable(
                OpenedJobSource {
                    file: File::open(path).unwrap(),
                    total_bytes: bytes
                },
                true,
                1,
                &jobs,
                store,
                &job,
                None
            )
            .is_err());
            assert_eq!(fs::read(&object).unwrap(), payload);
            let store = PersistentStore::open(&target).unwrap();
            assert_eq!(store.revision().unwrap(), 1);
            assert_eq!(store.read_root(None).unwrap().value, before);
            assert!(store.snapshot_list().unwrap().is_empty());
            assert!(!target.join("persistent/recovery").exists());
        }
    }
    #[test]
    fn native_portable_stale_revision_aborts_without_changing_library() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let target = directory.path().join("target");
        let jobs = directory.path().join("jobs");
        let restore_jobs = directory.path().join("restore");
        fs::create_dir(&jobs).unwrap();
        fs::create_dir(&restore_jobs).unwrap();
        let store = library(&source);
        let revision = store.revision().unwrap();
        let registry = super::super::JobRegistry::default();
        let export = registry
            .create_internal(
                super::super::JobKind::ExportPortableBackup,
                Some(revision),
                vec![],
                false,
            )
            .unwrap();
        let exported = export_portable(
            None,
            revision,
            &jobs,
            &directory.path().join("handoffs"),
            store,
            &export,
            None, "9.8.7-synthetic")
        .unwrap();
        let target_store = library(&target);
        let revision = target_store.revision().unwrap();
        let before = target_store.read_root(None).unwrap().value;
        let before_roots = rusqlite::Connection::open(target.join("persistent/persistent.sqlite"))
            .unwrap()
            .query_row::<i64, _, _>("SELECT count(*) FROM root", [], |r| r.get(0))
            .unwrap();
        let restore = registry
            .create_internal(
                super::super::JobKind::RestorePortableBackup,
                Some(revision + 1),
                vec![],
                false,
            )
            .unwrap();
        let failure = restore_portable(
            OpenedJobSource {
                file: File::open(exported.handoff_path.unwrap()).unwrap(),
                total_bytes: exported.source_bytes,
            },
            true,
            revision + 1,
            &restore_jobs,
            target_store,
            &restore,
            None,
        )
        .unwrap_err();
        assert_eq!(failure.code, "revision-conflict");
        let db = rusqlite::Connection::open(target.join("persistent/persistent.sqlite")).unwrap();
        assert_eq!(
            db.query_row::<i64, _, _>("SELECT count(*) FROM root", [], |r| r.get(0))
                .unwrap(),
            before_roots
        );
        drop(db);
        // Opening a store seeds its operational revision-0 row independently of restoration.
        let target_store = PersistentStore::open(&target).unwrap();
        assert_eq!(target_store.revision().unwrap(), revision);
        assert_eq!(target_store.read_root(None).unwrap().value, before);
    }
    #[test]
    fn native_portable_bad_json_is_preserved_raw_and_unknown_generation_uses_sqlite_salvage() {
        for unknown in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let source = directory.path().join("source");
            let jobs = directory.path().join("jobs");
            let handoffs = directory.path().join("handoffs");
            fs::create_dir(&jobs).unwrap();
            let store = library(&source);
            let revision = store.revision().unwrap();
            drop(store);
            let db =
                rusqlite::Connection::open(source.join("persistent/persistent.sqlite")).unwrap();
            if unknown {
                db.execute_batch("CREATE TABLE future_records(generation TEXT,value TEXT)").unwrap();
                db.execute("INSERT INTO future_records VALUES('synthetic',?1)",
                    [r#"{"account":{"id":"synthetic-account","token":"synthetic-token"}}"#]).unwrap();
            } else {
                db.execute("UPDATE messages SET value='not JSON'", [])
                    .unwrap();
            }
            drop(db);
            let mut refused_store = PersistentStore::open(&source).unwrap();
            let mut refused_pins = new_pins(&refused_store, CasJobKind::OfficialPublicationOrExportPreparation).unwrap();
            let denied = capture(&mut refused_store, revision, &jobs, &mut refused_pins,
                &NeverCancelled, "9.8.7-synthetic", false);
            assert!(matches!(denied, Err(ref failure) if failure.code == "source-preservation-confirmation-required"));
            refused_pins.release(CasReleaseOutcome::Aborted).unwrap();
            drop(refused_store);
            assert!(!handoffs.exists());
            let registry = super::super::JobRegistry::default();
            let job = registry
                .create_internal(
                    super::super::JobKind::ExportPortableBackup,
                    Some(revision),
                    vec![],
                    false,
                )
                .unwrap();
            job.start(JobPhase::WritingExport).unwrap();
            let exported = export_portable_running(
                None,
                revision,
                &jobs,
                &handoffs,
                PersistentStore::open(&source).unwrap(),
                &job,
                &PortableSelection { allow_source_preservation: true, ..PortableSelection::default() },
                "9.8.7-synthetic")
            .unwrap();
            assert_eq!(
                exported.warning_codes,
                vec!["source-preserved-repair-required"]
            );
            let archive = VerifiedArchive::open(
                File::open(exported.handoff_path.unwrap()).unwrap(),
                &jobs,
                &NeverCancelled,
            )
            .unwrap();
            assert!(archive.manifest.repair_required);
            assert!(archive.validate_library(&NeverCancelled).is_err());
            if archive.manifest.library_included {
                // The gate refuses this archive, so only the collecting scan can say what is wrong.
                let mut findings = crate::data_health::Findings::new(64);
                archive
                    .scan_library(&mut findings, &NeverCancelled)
                    .unwrap();
                assert!(findings
                    .items
                    .iter()
                    .any(|finding| finding.code == crate::data_health::codes::RECORD_INVALID));
            }
            if unknown {
                assert_eq!(
                    archive.manifest.profile,
                    portable_backup::Profile::SourceSqlite
                );
                assert!(archive.db.query_row::<bool,_,_>("SELECT EXISTS(SELECT 1 FROM files WHERE logical_key='source.sqlite' AND state='present')",[],|r|r.get(0)).unwrap());
                let hash: String = archive.db.query_row("SELECT lower(hex(object_hash)) FROM files WHERE logical_key='source.sqlite'", [], |row| row.get(0)).unwrap();
                let preserved_path = jobs.join("preserved-synthetic.sqlite");
                let mut preserved = File::create(&preserved_path).unwrap();
                archive.copy_object(&hash, &mut preserved, &NeverCancelled).unwrap();
                drop(preserved);
                let recovered = rusqlite::Connection::open(&preserved_path).unwrap();
                let account: String = recovered.query_row("SELECT value FROM future_records", [], |row| row.get(0)).unwrap();
                assert!(account == r#"{"account":{"id":"synthetic-account","token":"synthetic-token"}}"#);
            } else {
                assert_eq!(archive.manifest.profile, portable_backup::Profile::Portable);
                assert!(archive
                    .db
                    .query_row::<bool, _, _>(
                        "SELECT EXISTS(SELECT 1 FROM messages WHERE value='not JSON')",
                        [],
                        |r| r.get(0)
                    )
                    .unwrap());
            }
        }
    }
    #[test]
    fn native_portable_restore_preserves_unregistered_cas_files_outside_live_cas() {
        use sha2::{Digest, Sha256};
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let target = directory.path().join("target");
        let jobs = directory.path().join("jobs");
        let restore_jobs = directory.path().join("restore");
        let handoffs = directory.path().join("handoffs");
        fs::create_dir_all(&jobs).unwrap();
        fs::create_dir_all(&restore_jobs).unwrap();
        let store = library(&source);
        let revision = store.revision().unwrap();
        let payload = b"synthetic unclassified original";
        let hash = hex::encode(Sha256::digest(payload));
        let physical_key = crate::asset_repository::object_physical_key(&hash);
        let source_object = source.join(&physical_key);
        fs::create_dir_all(source_object.parent().unwrap()).unwrap();
        fs::write(source_object, payload).unwrap();
        let registry = super::super::JobRegistry::default();
        let export = registry
            .create_internal(
                super::super::JobKind::ExportPortableBackup,
                Some(revision),
                vec![],
                false,
            )
            .unwrap();
        let exported =
            export_portable(None, revision, &jobs, &handoffs, store, &export, None, "9.8.7-synthetic").unwrap();
        let path = std::path::PathBuf::from(exported.handoff_path.unwrap());
        let target_store = library(&target);
        let target_revision = target_store.revision().unwrap();
        let restore = registry
            .create_internal(
                super::super::JobKind::RestorePortableBackup,
                Some(target_revision),
                vec![],
                false,
            )
            .unwrap();
        let result = restore_portable(
            OpenedJobSource {
                file: File::open(&path).unwrap(),
                total_bytes: exported.source_bytes,
            },
            true,
            target_revision,
            &restore_jobs,
            target_store,
            &restore,
            None,
        )
        .unwrap();
        assert_eq!(result.source_sha256, exported.source_sha256);
        let report = restore.status().preservation_report.unwrap();
        assert_eq!(report.files, "1");
        assert_eq!(report.bytes, payload.len().to_string());
        assert!(report.deletable);
        assert_eq!(
            fs::read(Path::new(&report.path).join("objects").join(&hash)).unwrap(),
            payload
        );
        assert!(PayloadCas::new(&target)
            .unwrap()
            .stat_object(&hash)
            .unwrap()
            .is_none());
        assert!(!target.join("persistent/recovery").exists());
        let mut target_store = PersistentStore::open(&target).unwrap();
        let mut pins = new_pins(
            &target_store,
            CasJobKind::OfficialPublicationOrExportPreparation,
        )
        .unwrap();
        let captured = capture(
            &mut target_store,
            result.revision,
            &jobs,
            &mut pins,
            &NeverCancelled,
            "9.8.7-synthetic",
            false,
        )
        .unwrap();
        let (key,stored_hash,metadata):(String,String,String)=captured.catalog.db.query_row("SELECT logical_key,lower(hex(object_hash)),metadata FROM files WHERE kind='preserved'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert!(key.starts_with("source-preservation/"));
        assert!(key.ends_with(&format!("/{physical_key}")));
        assert_eq!(stored_hash, hash);
        assert_eq!(metadata, "{\"storage\":\"cas\"}");
        pins.release(CasReleaseOutcome::Aborted).unwrap();
    }
    #[test]
    fn native_portable_export_restore_preserves_sql_without_recovery_backup() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let target = directory.path().join("target");
        let jobs = directory.path().join("jobs");
        let handoffs = directory.path().join("handoffs");
        fs::create_dir_all(&jobs).unwrap();
        let mut store = library(&source);
        let revision = store.revision().unwrap();
        let mut catalog = Catalog::create(&jobs, "synthetic", revision).unwrap();
        let lease = store.acquire_revision(revision).unwrap().lease;
        store
            .capture_portable_records(&lease, &mut catalog.db, &NeverCancelled)
            .unwrap();
        store.release_revision(&lease).unwrap();
        catalog.validate_library(&NeverCancelled).unwrap();
        let registry = super::super::JobRegistry::default();
        let export = registry
            .create_internal(
                super::super::JobKind::ExportPortableBackup,
                Some(revision),
                vec![],
                false,
            )
            .unwrap();
        let result =
            export_portable(None, revision, &jobs, &handoffs, store, &export, None, "9.8.7-synthetic").unwrap();
        assert!(result.warning_codes.is_empty());
        let path = std::path::PathBuf::from(result.handoff_path.unwrap());
        let before =
            VerifiedArchive::open(File::open(&path).unwrap(), &jobs, &NeverCancelled).unwrap();
        let expected = digest_raw_tables(&before.db, &NeverCancelled).unwrap();
        drop(before);
        let target_store = library(&target);
        let target_revision = target_store.revision().unwrap();
        let restore_jobs = directory.path().join("restore-job");
        fs::create_dir(&restore_jobs).unwrap();
        let restore = registry
            .create_internal(
                super::super::JobKind::RestorePortableBackup,
                Some(target_revision),
                vec![],
                false,
            )
            .unwrap();
        let bytes = fs::metadata(&path).unwrap().len();
        let restored = restore_portable(
            OpenedJobSource {
                file: File::open(path).unwrap(),
                total_bytes: bytes,
            },
            true,
            target_revision,
            &restore_jobs,
            target_store,
            &restore,
            None,
        )
        .unwrap();
        assert_eq!(restored.revision, target_revision + 1);
        assert!(!target.join("persistent/recovery").exists());
        assert!(PersistentStore::open(&target)
            .unwrap()
            .snapshot_list()
            .unwrap()
            .is_empty());
        let mut store = PersistentStore::open(&target).unwrap();
        let mut pins =
            new_pins(&store, CasJobKind::OfficialPublicationOrExportPreparation).unwrap();
        let captured = capture(
            &mut store,
            restored.revision,
            &jobs,
            &mut pins,
            &NeverCancelled,
            "9.8.7-synthetic",
            false,
        )
        .unwrap();
        assert_eq!(
            expected,
            digest_raw_tables(&captured.catalog.db, &NeverCancelled).unwrap()
        );
        pins.release(CasReleaseOutcome::Aborted).unwrap();
    }
}

#[cfg(test)]
#[path = "portable_scale_tests.rs"]
mod scale_tests;
