use super::{
    restore, ImportCounts, JobControl, JobDetail, JobPhase, JobProgress, JobResultSummary,
    JobStage, NativeJobError, OpenedJobSource, StageUnit,
};
use crate::asset_repository::job_pins::{
    CasJobKind, CasObjectRole, CasReleaseOutcome, DurableCasJob,
};
use crate::asset_repository::{owner_manifest_codec, PayloadCas};
use crate::local_backup::{
    parse_legacy_local_backup_v1_observed, write_legacy_local_backup_v1, CancellationProbe,
    LegacyBackupWriteEntry, LegacyBackupWriteSource, LocalBackupError, LocalBackupErrorCode,
    LocalBackupParseObserver, PayloadTarget, StagedLocalBackupEntry,
    StrictLocalBackupDatabaseRestore,
};
use crate::persistent_store::export::{self, destination};
use crate::persistent_store::{
    AssetAlias, AssetOwnerHead, AssetOwnerLocator, PersistentStore,
    RevisionResult, StagingResult, StoreResult,
};
use crate::server_sync::residency::RemotePayloadAccess;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

mod cold_expansion;
mod compatible_assets;
mod compatible_export;
mod compatible_projection;
mod pocket_risu;
pub(crate) use compatible_export::export_compatible_local_backup;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum CompatibilityTarget {
    #[serde(rename = "risuai")]
    RisuAi,
    #[serde(rename = "pocketrisu")]
    PocketRisu,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompatibilityReportItem {
    pub(crate) code: String,
    pub(crate) items: String,
    pub(crate) bytes: String,
    pub(crate) affected_conversations: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompatibilityReport {
    pub(crate) target: CompatibilityTarget,
    pub(crate) preserved: Vec<CompatibilityReportItem>,
    pub(crate) converted: Vec<CompatibilityReportItem>,
    pub(crate) excluded: Vec<CompatibilityReportItem>,
}
#[cfg(test)]
mod pocket_risu_tests;

const DATABASE_ENTRY: &str = "database.risudat";
const ENCRYPTION_ENTRY: &str = "encryption.risudat";
const MAX_METADATA_BYTES: u32 = 1024 * 1024;
const MAX_OWNER_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
const ARCHIVE_FILE: &str = "archive.bin.part";

struct JobCancellation<'a>(&'a JobControl);

impl CancellationProbe for JobCancellation<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancel_requested()
    }
}

const PROGRESS_REPORT_INTERVAL_BYTES: u64 = 1024 * 1024;

/// Observes attachments being copied into the CAS during a legacy restore.
pub(crate) trait LegacyPrepareObserver {
    fn begin(&self, _total: u64) {}
    fn item_started(&self, _logical_name: &str) {}
    fn item_bytes(&self, _bytes: u64) {}
    fn item_done(&self) {}
}

#[cfg(test)]
pub(crate) struct NoopPrepareObserver;

#[cfg(test)]
impl LegacyPrepareObserver for NoopPrepareObserver {}

#[derive(Default)]
struct LegacyProgressState {
    read_bytes: u64,
    prepared_bytes: u64,
    last_reported_bytes: u64,
    counts: ImportCounts,
    stage: Option<JobStage>,
    stage_completed: u64,
    stage_total: Option<u64>,
    stage_unit: Option<StageUnit>,
    current_item: Option<String>,
}

/// Progress reporter for a legacy backup restore. It is the cancellation
/// probe, the archive parse observer, and the attachment observer at once, and
/// it charges every stage against one fixed budget of twice the archive size:
/// each byte is read from the archive once and then either copied into the
/// CAS or parsed as the database. Reports never fail the restore; a rejected
/// report only costs the dialog one update.
struct LegacyRestoreProgress<'a> {
    job: &'a JobControl,
    archive_bytes: u64,
    state: RefCell<LegacyProgressState>,
}

impl<'a> LegacyRestoreProgress<'a> {
    fn new(job: &'a JobControl, archive_bytes: u64) -> Self {
        Self {
            job,
            archive_bytes,
            state: RefCell::new(LegacyProgressState::default()),
        }
    }

    fn budget(&self) -> u64 {
        self.archive_bytes.saturating_mul(2).max(1)
    }

    fn completed_bytes(&self, state: &LegacyProgressState) -> u64 {
        state
            .read_bytes
            .saturating_add(state.prepared_bytes)
            .min(self.budget())
    }

    fn report(&self) {
        let mut state = self.state.borrow_mut();
        let completed = self.completed_bytes(&state);
        state.last_reported_bytes = completed;
        let _ = self.job.set_progress(JobProgress {
            completed_bytes: completed,
            total_bytes: Some(self.budget()),
            completed_items: state.counts.entries_read,
            total_items: state.counts.entries_total,
        });
        if let (Some(stage), Some(unit)) = (state.stage, state.stage_unit) {
            let mut detail = JobDetail::new(
                stage,
                unit,
                state.stage_completed,
                state.stage_total,
                state.counts.clone(),
            );
            if let Some(item) = &state.current_item {
                detail = detail.with_current_item(item.clone());
            }
            let _ = self.job.set_detail(detail);
        }
    }

    fn report_if_due(&self) {
        let due = {
            let state = self.state.borrow();
            self.completed_bytes(&state)
                .saturating_sub(state.last_reported_bytes)
                >= PROGRESS_REPORT_INTERVAL_BYTES
        };
        if due {
            self.report();
        }
    }

    fn classify(counts: &mut ImportCounts, logical_name: &str) {
        if matches!(logical_name, DATABASE_ENTRY | ENCRYPTION_ENTRY) {
            return;
        }
        match pocket_risu::classify(logical_name) {
            Ok(Some(pocket_risu::Entry::Payload { .. })) => counts.pocket_media += 1,
            Ok(Some(
                pocket_risu::Entry::Metadata { .. } | pocket_risu::Entry::Provenance { .. },
            )) => counts.pocket_metadata += 1,
            Ok(Some(pocket_risu::Entry::Cache)) => counts.skipped += 1,
            Ok(None) => {
                if cold_key(logical_name).is_some() {
                    counts.cold_storage += 1
                } else if inlay_key_hex(logical_name).is_some() {
                    counts.inlays += 1
                } else {
                    counts.assets += 1
                }
            }
            // Invalid names fail the preflight that follows; they are not counted.
            Err(_) => {}
        }
    }

    /// Scale for the database read: it continues the same byte budget and
    /// keeps the archive's entry counter instead of counting RisuSave blocks.
    fn restore_scale(&self) -> restore::RestoreProgressScale {
        let state = self.state.borrow();
        restore::RestoreProgressScale {
            spool_directory: None,
            reject_cold_references: false,
            base_bytes: state.read_bytes.saturating_add(state.prepared_bytes),
            total_bytes: Some(self.budget()),
            fixed_items: Some((state.counts.entries_read, state.counts.entries_total)),
            counts: state.counts.clone(),
        }
    }
}

impl CancellationProbe for LegacyRestoreProgress<'_> {
    fn is_cancelled(&self) -> bool {
        self.job.is_cancel_requested()
    }
}

impl LocalBackupParseObserver for LegacyRestoreProgress<'_> {
    fn entry_started(&self, logical_name: &str, _byte_length: u64, index: usize) {
        {
            let mut state = self.state.borrow_mut();
            state.counts.entries_read = index as u64 + 1;
            Self::classify(&mut state.counts, logical_name);
            state.stage = Some(JobStage::ReadingArchive);
            state.stage_unit = Some(StageUnit::Bytes);
            state.stage_completed = state.read_bytes;
            state.stage_total = Some(self.archive_bytes);
            state.current_item = Some(logical_name.to_owned());
        }
        self.report();
    }

    fn bytes_read(&self, total_read: u64) {
        {
            let mut state = self.state.borrow_mut();
            state.read_bytes = total_read.min(self.archive_bytes);
            state.stage_completed = state.read_bytes;
        }
        self.report_if_due();
    }

    fn entries_complete(&self, count: usize) {
        {
            let mut state = self.state.borrow_mut();
            state.counts.entries_total = Some(count as u64);
            state.stage_completed = state.read_bytes;
            state.current_item = None;
        }
        self.report();
    }
}

impl LegacyPrepareObserver for LegacyRestoreProgress<'_> {
    fn begin(&self, total: u64) {
        {
            let mut state = self.state.borrow_mut();
            state.stage = Some(JobStage::PreparingAttachments);
            state.stage_unit = Some(StageUnit::Items);
            state.stage_completed = 0;
            state.stage_total = Some(total);
            state.current_item = None;
        }
        self.report();
    }

    fn item_started(&self, logical_name: &str) {
        self.state.borrow_mut().current_item = Some(logical_name.to_owned());
        self.report();
    }

    fn item_bytes(&self, bytes: u64) {
        {
            let mut state = self.state.borrow_mut();
            state.prepared_bytes = state
                .prepared_bytes
                .saturating_add(bytes)
                .min(self.archive_bytes);
        }
        self.report_if_due();
    }

    fn item_done(&self) {
        {
            let mut state = self.state.borrow_mut();
            state.counts.attachments_prepared += 1;
            state.stage_completed += 1;
            state.current_item = None;
        }
        self.report();
    }
}

pub(crate) fn restore_legacy_local_backup(
    mut source: OpenedJobSource,
    expected_revision: i64,
    owned_directory: &std::path::Path,
    repository_root: &std::path::Path,
    app: AppHandle,
    job: &JobControl,
) -> Result<JobResultSummary, NativeJobError> {
    job.start(JobPhase::ReadingSource)
        .map_err(job_state_error)?;
    let cas = PayloadCas::new(repository_root).map_err(io_job_error)?;
    let progress = LegacyRestoreProgress::new(job, source.total_bytes);
    let mut callback = LegacyDatabaseRestore {
        app,
        cas: &cas,
        repository_root,
        expected_revision,
        job,
        progress: &progress,
        result: None,
    };
    let parsed = parse_legacy_local_backup_v1_observed(
        &mut source.file,
        owned_directory,
        PayloadTarget::JobStaging,
        &mut callback,
        &progress,
        &progress,
    );
    match parsed {
        Ok(report) => {
            let mut result = callback.result.take().ok_or_else(|| {
                NativeJobError::new("store-error", "legacy backup database restore did not run")
            })??;
            result.source_bytes = report.source_bytes;
            result.source_sha256 = report.source_sha256;
            Ok(result)
        }
        Err(error) => match callback.result.take() {
            Some(Err(native)) => Err(native),
            _ => Err(local_backup_error(error)),
        },
    }
}

struct LegacyDatabaseRestore<'a> {
    app: AppHandle,
    cas: &'a PayloadCas,
    repository_root: &'a std::path::Path,
    expected_revision: i64,
    job: &'a JobControl,
    progress: &'a LegacyRestoreProgress<'a>,
    result: Option<Result<JobResultSummary, NativeJobError>>,
}

impl StrictLocalBackupDatabaseRestore for LegacyDatabaseRestore<'_> {
    fn restore_database(
        &mut self,
        database: &StagedLocalBackupEntry,
        entries: &[StagedLocalBackupEntry],
    ) -> Result<(), LocalBackupError> {
        preflight_legacy_restore_entries(entries)?;
        let path = database.staged_path.as_ref().ok_or_else(|| {
            LocalBackupError::database_restore("legacy backup database is not staged")
        })?;
        let file = File::open(path).map_err(LocalBackupError::io)?;
        let mut durable = DurableCasJob::begin(
            self.repository_root,
            &self.job.id(),
            CasJobKind::LocalBackupRestore,
            now_millis(),
        )
        .map_err(LocalBackupError::io)?;
        let payloads = match prepare_legacy_restore_payloads_observed(
            entries,
            self.cas,
            &mut durable,
            self.progress,
            self.progress,
        ) {
            Ok(payloads) => payloads,
            Err(error) => {
                let _ = durable.release(CasReleaseOutcome::Aborted);
                return Err(error);
            }
        };
        if let Err(error) = check_cancelled(self.progress) {
            let _ = durable.release(CasReleaseOutcome::Aborted);
            return Err(error);
        }
        let sink = LegacyReplacementSink {
            app: self.app.clone(),
            payloads,
            durable: Mutex::new(durable),
            incomplete: Mutex::new(super::IncompleteRestorePreview::default()),
        };
        let result = restore::restore_started_risu_save(
            OpenedJobSource {
                file,
                total_bytes: database.byte_length,
            },
            self.expected_revision,
            self.job,
            &sink,
            restore::RestoreProgressScale {
                spool_directory: path.parent().map(std::path::Path::to_path_buf),
                ..self.progress.restore_scale()
            },
        );
        match result {
            Ok(summary) => {
                self.result = Some(Ok(summary));
                Ok(())
            }
            Err(error) => {
                self.result = Some(Err(error.clone()));
                Err(LocalBackupError::database_restore(error.message))
            }
        }
    }
}

struct LegacyReplacementSink {
    app: AppHandle,
    payloads: PreparedLegacyRestorePayloads,
    durable: Mutex<DurableCasJob>,
    incomplete: Mutex<super::IncompleteRestorePreview>,
}

impl restore::ReplacementSink for LegacyReplacementSink {
    fn begin(&self) -> StoreResult<StagingResult> {
        crate::persistent_store::commands::with_store_mut(
            self.app.state(),
            PersistentStore::replace_begin,
        )
    }

    fn put_root(&self, staging_id: &str, root: &Value) -> StoreResult<()> {
        crate::persistent_store::commands::with_store_mut(self.app.state(), |store| {
            store.replace_put_root(staging_id, root)
        })
    }

    fn put_presets(&self, staging_id: &str, presets: &[Value]) -> StoreResult<()> {
        crate::persistent_store::commands::with_store_mut(self.app.state(), |store| {
            store.replace_put_presets(staging_id, presets)
        })
    }

    fn add_characters(&self, staging_id: &str, characters: &[Value]) -> StoreResult<()> {
        for character in characters {
            let mut unavailable = Vec::new();
            let expanded = cold_expansion::expand_cold_payloads(std::slice::from_ref(character), &self.payloads.cold_payloads, &mut unavailable)
                .map_err(|message| crate::persistent_store::StoreError::Store { message })?;
            if !unavailable.is_empty() {
                let mut preview = self.incomplete.lock().map_err(|error| crate::persistent_store::StoreError::Store { message: error.to_string() })?;
                preview.unavailable_cold_keys.extend(unavailable);
                if let Some(name) = character.get("name").and_then(Value::as_str) { preview.character_names.push(name.to_owned()); }
            }
            let characters = expanded.as_deref().unwrap_or(std::slice::from_ref(character));
            crate::persistent_store::commands::with_store_mut(self.app.state(), |store| {
                store.replace_add_characters(staging_id, characters)
            })?;
        }
        Ok(())
    }

    fn supports_incremental_characters(&self) -> bool { true }

    fn expands_cold_payloads(&self) -> bool { true }

    fn cold_payload_path(&self, key: &str) -> Option<std::path::PathBuf> {
        self.payloads.cold_payloads.get(key).cloned()
    }

    fn unavailable_cold_payload(&self, key: &str, character_name: &str) -> StoreResult<()> {
        let mut preview = self.incomplete.lock().map_err(|error| crate::persistent_store::StoreError::Store { message: error.to_string() })?;
        preview.unavailable_cold_keys.push(key.to_owned());
        if !character_name.is_empty() { preview.character_names.push(character_name.to_owned()); }
        Ok(())
    }

    fn put_character_detail(&self, staging_id: &str, detail: &Value, conversation_count: i64) -> StoreResult<()> {
        crate::persistent_store::commands::with_store_mut(self.app.state(), |store| {
            store.replace_put_character_detail(staging_id, detail, conversation_count)
        })
    }

    fn put_conversation_row(&self, staging_id: &str, character_id: &str, configured_index: i64, detail: &Value, recent_at: i64, message_count: i64) -> StoreResult<()> {
        crate::persistent_store::commands::with_store_mut(self.app.state(), |store| {
            store.replace_put_conversation_row(staging_id, character_id, configured_index, detail, recent_at, message_count)
        })
    }

    fn add_conversation_messages(&self, staging_id: &str, character_id: &str, conversation_id: &str, start: i64, messages: &[Value]) -> StoreResult<()> {
        crate::persistent_store::commands::with_store_mut(self.app.state(), |store| {
            store.replace_add_conversation_messages(staging_id, character_id, conversation_id, start, messages)
        })
    }

    fn incomplete_restore_preview(&self) -> StoreResult<super::IncompleteRestorePreview> {
        let mut preview = self.incomplete.lock().map_err(|error| crate::persistent_store::StoreError::Store { message: error.to_string() })?.clone();
        preview.unavailable_cold_keys.sort();
        preview.unavailable_cold_keys.dedup();
        preview.character_names.sort();
        preview.character_names.dedup();
        preview.invalid_inlays = self.payloads.invalid_inlays.clone();
        Ok(preview)
    }

    fn staged_plugin_preview(
        &self,
        staging_id: &str,
    ) -> StoreResult<crate::persistent_store::commit::StagedPluginPreview> {
        crate::persistent_store::commands::with_store(self.app.state(), |store| {
            store.staged_plugin_preview(staging_id)
        })
    }

    fn commit(&self, staging_id: &str, expected_revision: i64) -> StoreResult<RevisionResult> {
        crate::persistent_store::commands::with_store_mut(self.app.state(), |store| {
            store.replace_put_asset_aliases(staging_id, &self.payloads.asset_aliases)?;
            self.durable
                .lock()
                .map_err(|error| crate::persistent_store::StoreError::Store {
                    message: format!("legacy backup CAS job mutex is poisoned: {error}"),
                })?
                .seal(store, now_millis())
                .map_err(crate::persistent_store::StoreError::from)
        })?;
        let committed = crate::persistent_store::commands::replace_commit_with_snapshot(
            &self.app,
            staging_id,
            Some(expected_revision),
        );
        if committed.is_ok() {
            let _ = self.release_durable(CasReleaseOutcome::Committed);
        }
        committed
    }

    fn abort(&self, staging_id: &str) -> StoreResult<()> {
        let abort = crate::persistent_store::commands::with_store_mut(self.app.state(), |store| {
            store.replace_abort(staging_id)
        });
        let release = self.release_durable(CasReleaseOutcome::Aborted);
        abort.and(release)
    }
}

impl LegacyReplacementSink {
    fn release_durable(&self, outcome: CasReleaseOutcome) -> StoreResult<()> {
        self.durable
            .lock()
            .map_err(|error| crate::persistent_store::StoreError::Store {
                message: format!("legacy backup CAS job mutex is poisoned: {error}"),
            })?
            .release(outcome)
            .map_err(crate::persistent_store::StoreError::from)
    }
}

impl Drop for LegacyReplacementSink {
    fn drop(&mut self) {
        if let Ok(durable) = self.durable.get_mut() {
            let _ = durable.release(CasReleaseOutcome::Aborted);
        }
    }
}

fn local_backup_error(error: LocalBackupError) -> NativeJobError {
    let code = match error.code {
        LocalBackupErrorCode::CompatibilityImportRequired => "compatibility-import-required",
        LocalBackupErrorCode::UnsupportedEncryption | LocalBackupErrorCode::UnsupportedFormat => {
            "unsupported-format"
        }
        LocalBackupErrorCode::Cancelled => "cancelled",
        LocalBackupErrorCode::Io => "store-error",
        _ => "invalid-source",
    };
    NativeJobError::new(code, error.message)
}

fn io_job_error(error: io::Error) -> NativeJobError {
    NativeJobError::new("store-error", error.to_string())
}

fn store_job_error(error: crate::persistent_store::StoreError) -> NativeJobError {
    let code = match &error {
        crate::persistent_store::StoreError::RevisionConflict { .. } => "revision-conflict",
        crate::persistent_store::StoreError::Validation { .. } => "invalid-source",
        _ => "store-error",
    };
    NativeJobError::new(code, error.to_string())
}

fn job_state_error(error: String) -> NativeJobError {
    NativeJobError::new("store-error", error)
}

fn destination_job_error(
    error: destination::DestinationWriteError,
    phase_error: Option<String>,
) -> NativeJobError {
    if let Some(error) = phase_error {
        return job_state_error(error);
    }
    match error {
        destination::DestinationWriteError::InvalidSource => {
            NativeJobError::new("store-error", "legacy backup staged archive is invalid")
        }
        destination::DestinationWriteError::InvalidDestination => NativeJobError::new(
            "invalid-destination",
            "legacy backup destination is invalid",
        ),
        destination::DestinationWriteError::Cancelled => {
            NativeJobError::new("cancelled", "legacy backup export was cancelled")
        }
        destination::DestinationWriteError::Io { source, .. } => io_job_error(source),
    }
}

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

pub(crate) struct PreparedLegacyRestorePayloads {
    pub(crate) asset_aliases: Vec<AssetAlias>,
    pub(crate) invalid_inlays: Vec<String>,
    /// Upstream cold payload key to the staged file that holds its body.
    pub(crate) cold_payloads: HashMap<String, std::path::PathBuf>,
}

#[cfg(test)]
pub(crate) fn prepare_legacy_restore_payloads(
    entries: &[StagedLocalBackupEntry],
    cas: &PayloadCas,
    durable: &mut DurableCasJob,
    cancellation: &dyn CancellationProbe,
) -> Result<PreparedLegacyRestorePayloads, LocalBackupError> {
    prepare_legacy_restore_payloads_observed(
        entries,
        cas,
        durable,
        cancellation,
        &NoopPrepareObserver,
    )
}

/// Whether an archive entry becomes an asset or cold alias. Metadata and cache
/// entries only feed the payloads they describe.
fn produces_alias(logical_name: &str) -> bool {
    if matches!(logical_name, DATABASE_ENTRY | ENCRYPTION_ENTRY) {
        return false;
    }
    matches!(
        pocket_risu::classify(logical_name),
        Ok(Some(pocket_risu::Entry::Payload { .. })) | Ok(None)
    )
}

pub(crate) fn prepare_legacy_restore_payloads_observed(
    entries: &[StagedLocalBackupEntry],
    cas: &PayloadCas,
    durable: &mut DurableCasJob,
    cancellation: &dyn CancellationProbe,
    observer: &dyn LegacyPrepareObserver,
) -> Result<PreparedLegacyRestorePayloads, LocalBackupError> {
    preflight_legacy_restore_entries(entries)?;
    let pocket_metadata = pocket_risu::index_metadata(entries, cancellation)?;
    let mut asset_aliases = Vec::new();
    let mut invalid_inlays = Vec::new();
    let mut cold_payloads = HashMap::new();
    let mut asset_keys = HashSet::new();
    observer.begin(
        entries
            .iter()
            .filter(|entry| produces_alias(&entry.logical_name))
            .count() as u64,
    );

    for entry in entries {
        check_cancelled(cancellation)?;
        if matches!(
            entry.logical_name.as_str(),
            DATABASE_ENTRY | ENCRYPTION_ENTRY
        ) {
            continue;
        }
        if let Some(pocket_entry) = pocket_risu::classify(&entry.logical_name)? {
            if let pocket_risu::Entry::Payload { id, ext } = pocket_entry {
                if !asset_keys.insert(("inlay".to_owned(), id.to_owned())) {
                    return Err(invalid("legacy backup contains a duplicate Inlay key"));
                }
                observer.item_started(&entry.logical_name);
                asset_aliases.push(pocket_risu::prepare(
                    entry,
                    id,
                    ext,
                    &pocket_metadata,
                    cas,
                    durable,
                    cancellation,
                    observer,
                )?);
                observer.item_done();
            }
            continue;
        }
        observer.item_started(&entry.logical_name);
        if let Some(key) = cold_key(&entry.logical_name) {
            let staged = prepare_cold(entry, cancellation, observer)?;
            if cold_payloads.insert(key, staged).is_some() {
                return Err(invalid(
                    "legacy backup contains a duplicate cold payload key",
                ));
            }
        } else if let Some(encoded_key) = inlay_key_hex(&entry.logical_name) {
            let alias = match prepare_inlay(entry, encoded_key, cas, durable, cancellation, observer) {
                Ok(alias) => alias,
                Err(error) if error.code == LocalBackupErrorCode::DatabaseRestore => {
                    invalid_inlays.push(entry.logical_name.clone());
                    observer.item_done();
                    continue;
                }
                Err(error) => return Err(error),
            };
            if !asset_keys.insert((alias.kind.clone(), alias.key.clone())) {
                return Err(invalid("legacy backup contains a duplicate Inlay key"));
            }
            asset_aliases.push(alias);
        } else {
            let alias = prepare_asset(entry, cas, durable, cancellation, observer)?;
            if !asset_keys.insert((alias.kind.clone(), alias.key.clone())) {
                return Err(invalid("legacy backup contains a duplicate asset key"));
            }
            asset_aliases.push(alias);
        }
        observer.item_done();
    }
    check_cancelled(cancellation)?;
    Ok(PreparedLegacyRestorePayloads {
        invalid_inlays,
        asset_aliases,
        cold_payloads,
    })
}

fn preflight_legacy_restore_entries(
    entries: &[StagedLocalBackupEntry],
) -> Result<(), LocalBackupError> {
    reject_account_encryption(entries)?;
    for entry in entries {
        pocket_risu::classify(&entry.logical_name)?;
    }
    Ok(())
}

fn reject_account_encryption(entries: &[StagedLocalBackupEntry]) -> Result<(), LocalBackupError> {
    let Some(entry) = entries
        .iter()
        .find(|entry| entry.logical_name == ENCRYPTION_ENTRY)
    else {
        return Ok(());
    };
    if entry.byte_length > MAX_METADATA_BYTES as u64 {
        return Err(invalid("legacy backup encryption metadata is too large"));
    }
    let file = open_staged(entry)?;
    let metadata: Value = serde_json::from_reader(BufReader::new(file))
        .map_err(|_| invalid("legacy backup encryption metadata is invalid"))?;
    if metadata.get("type").and_then(Value::as_str) == Some("account") {
        return Err(LocalBackupError::new(
            LocalBackupErrorCode::CompatibilityImportRequired,
            "account-encrypted legacy backup requires the compatibility importer",
        ));
    }
    Ok(())
}

fn prepare_asset(
    entry: &StagedLocalBackupEntry,
    cas: &PayloadCas,
    durable: &mut DurableCasJob,
    cancellation: &dyn CancellationProbe,
    observer: &dyn LegacyPrepareObserver,
) -> Result<AssetAlias, LocalBackupError> {
    let file = open_staged(entry)?;
    let mut source = CancellationReader::observed(file, cancellation, observer);
    let payload = durable
        .prepare_reader(cas, &mut source, CasObjectRole::DirectObject)
        .map_err(|error| cancellation_io(error, cancellation))?;
    let name = entry
        .logical_name
        .rsplit('/')
        .next()
        .unwrap_or(&entry.logical_name)
        .to_owned();
    let ext = name
        .rsplit_once('.')
        .map(|(_, ext)| ext)
        .unwrap_or("")
        .to_owned();
    Ok(AssetAlias {
        key: format!("assets/{}", entry.logical_name),
        object_hash: Some(payload.content_hash),
        kind: "asset".to_owned(),
        size: to_alias_size(payload.byte_size)?,
        mime: String::new(),
        name,
        ext,
        inlay_type: None,
        width: None,
        height: None,
        metadata: Value::Object(Map::new()),
    })
}

fn prepare_inlay(
    entry: &StagedLocalBackupEntry,
    encoded_key: &str,
    cas: &PayloadCas,
    durable: &mut DurableCasJob,
    cancellation: &dyn CancellationProbe,
    observer: &dyn LegacyPrepareObserver,
) -> Result<AssetAlias, LocalBackupError> {
    let expected_key = String::from_utf8(
        hex::decode(encoded_key).map_err(|_| invalid("invalid Inlay entry name"))?,
    )
    .map_err(|_| invalid("invalid Inlay entry key"))?;
    let mut file = open_staged(entry)?;
    if entry.byte_length < 4 { return Err(invalid("legacy backup Inlay metadata length is invalid")); }
    let mut length = [0_u8; 4];
    file.read_exact(&mut length).map_err(LocalBackupError::io)?;
    let header_length = u32::from_le_bytes(length);
    if header_length == 0
        || header_length > MAX_METADATA_BYTES
        || u64::from(header_length) + 4 > entry.byte_length
    {
        return Err(invalid("legacy backup Inlay metadata length is invalid"));
    }
    let mut header = vec![0_u8; header_length as usize];
    file.read_exact(&mut header).map_err(LocalBackupError::io)?;
    let metadata: Value = serde_json::from_slice(&header)
        .map_err(|_| invalid("legacy backup Inlay metadata is invalid"))?;
    let object = metadata
        .as_object()
        .ok_or_else(|| invalid("legacy backup Inlay metadata must be an object"))?;
    let key = string_field(object, "key")?;
    if key != expected_key || key.is_empty() || key.starts_with("assets/") {
        return Err(invalid(
            "legacy backup Inlay key does not match its entry name",
        ));
    }
    if string_field(object, "kind")? != "inlay" {
        return Err(invalid("legacy backup Inlay kind is invalid"));
    }
    let inlay_type = string_field(object, "inlayType")?;
    if !matches!(
        inlay_type.as_str(),
        "image" | "video" | "audio" | "signature"
    ) {
        return Err(invalid("legacy backup Inlay type is invalid"));
    }
    let mut source = CancellationReader::observed(file, cancellation, observer);
    let payload = durable
        .prepare_reader(cas, &mut source, CasObjectRole::DirectObject)
        .map_err(|error| cancellation_io(error, cancellation))?;
    let dimension = |field: &str| -> Result<Option<i64>, LocalBackupError> {
        match object.get(field) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_i64()
                .filter(|value| *value >= 0)
                .map(Some)
                .ok_or_else(|| invalid(format!("legacy backup Inlay {field} is invalid"))),
        }
    };
    let mut retained_metadata = object.clone();
    for key in [
        "key",
        "kind",
        "size",
        "mime",
        "name",
        "ext",
        "inlayType",
        "width",
        "height",
    ] {
        retained_metadata.remove(key);
    }
    Ok(AssetAlias {
        key,
        object_hash: Some(payload.content_hash),
        kind: "inlay".to_owned(),
        size: to_alias_size(payload.byte_size)?,
        mime: string_field(object, "mime")?,
        name: string_field(object, "name")?,
        ext: string_field(object, "ext")?,
        inlay_type: Some(inlay_type),
        width: dimension("width")?,
        height: dimension("height")?,
        metadata: Value::Object(retained_metadata),
    })
}

/// Validates a staged upstream cold payload without materializing its full body.
/// The body is spliced into its referencing record when characters are staged.
fn prepare_cold(
    entry: &StagedLocalBackupEntry,
    cancellation: &dyn CancellationProbe,
    observer: &dyn LegacyPrepareObserver,
) -> Result<std::path::PathBuf, LocalBackupError> {
    prepare_cold_with_limit(entry, cancellation, observer, restore::RestoreLimits::default().max_decoded_block_bytes)
}

fn prepare_cold_with_limit(
    entry: &StagedLocalBackupEntry,
    cancellation: &dyn CancellationProbe,
    observer: &dyn LegacyPrepareObserver,
    max_decoded_bytes: u64,
) -> Result<std::path::PathBuf, LocalBackupError> {
    let file = open_staged(entry)?;
    if entry.byte_length > max_decoded_bytes || file.metadata().map_err(LocalBackupError::io)?.len() > max_decoded_bytes {
        return Err(invalid("decoded block limit exceeded for cold payload"));
    }
    let source = CancellationReader::observed(file, cancellation, observer);
    let record_bytes = Cell::new(Some(0));
    let source = restore::RecordLimitReader { inner: BufReader::new(source), bytes: &record_bytes };
    let mut deserializer = serde_json::Deserializer::from_reader(source);
    if !serde::de::Deserializer::deserialize_any(&mut deserializer, ColdRootVisitor(&record_bytes)).map_err(
        |error| {
            if cancellation.is_cancelled() {
                LocalBackupError::new(
                    LocalBackupErrorCode::Cancelled,
                    "legacy backup job was cancelled",
                )
            } else if error.to_string().contains("restore record limit exceeded") {
                invalid("legacy backup cold payload exceeds the record limit")
            } else {
                invalid("legacy backup cold payload is invalid")
            }
        },
    )? {
        return Err(invalid(
            "legacy backup cold payload has an unsupported root",
        ));
    }
    deserializer
        .end()
        .map_err(|_| invalid("legacy backup cold payload has trailing data"))?;

    check_cancelled(cancellation)?;
    entry
        .staged_path
        .clone()
        .ok_or_else(|| invalid("legacy backup cold payload is not staged"))
}

struct ColdRootVisitor<'a>(&'a Cell<Option<u64>>);

impl<'de> Visitor<'de> for ColdRootVisitor<'_> {
    type Value = bool;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a cold payload array or object")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<bool, A::Error>
    where
        A: SeqAccess<'de>,
    {
        self.0.set(None);
        while let Some(mut value) = sequence.next_element_seed(restore::BoundedValueSeed(self.0))? {
            super::restore::pocket_features::message(&mut value, "cold")
                .map_err(serde::de::Error::custom)?;
        }
        Ok(true)
    }

    fn visit_map<A>(self, mut map: A) -> Result<bool, A::Error>
    where
        A: MapAccess<'de>,
    {
        self.0.set(None);
        let mut compatible = false;
        while let Some(key) = map.next_key_seed(restore::BoundedStringSeed(self.0))? {
            compatible |= matches!(key.as_str(), "character" | "message");
            match key.as_str() {
                "message" => {
                    map.next_value_seed(ColdFeatureSeed("messages", self.0))?;
                }
                "character" => {
                    map.next_value_seed(ColdFeatureSeed("character", self.0))?;
                }
                "savedToggleValues" | "bindedPersona" => {
                    let value = map.next_value_seed(restore::BoundedValueSeed(self.0))?;
                    let mut chat = serde_json::json!({key: value});
                    super::restore::pocket_features::chat(&mut chat, "cold")
                        .map_err(serde::de::Error::custom)?;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(compatible)
    }
}

// Validate feature fields while retaining the bounded streaming cold-payload path.
struct ColdFeatureSeed<'a>(&'static str, &'a Cell<Option<u64>>);
impl<'de> serde::de::DeserializeSeed<'de> for ColdFeatureSeed<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        let record_bytes = self.1;
        record_bytes.set(Some(0));
        let result = deserializer.deserialize_any(self);
        record_bytes.set(None);
        result
    }
}
impl<'de> Visitor<'de> for ColdFeatureSeed<'_> {
    type Value = ();
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a cold feature container")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<(), A::Error> {
        self.1.set(None);
        if self.0 == "messages" {
            while let Some(mut value) = sequence.next_element_seed(restore::BoundedValueSeed(self.1))? {
                super::restore::pocket_features::message(&mut value, "cold")
                    .map_err(serde::de::Error::custom)?;
            }
        } else {
            while sequence
                .next_element_seed(ColdFeatureSeed("chat", self.1))?
                .is_some()
            {}
        }
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        self.1.set(None);
        while let Some(key) = map.next_key_seed(restore::BoundedStringSeed(self.1))? {
            match key.as_str() {
                "chats" if self.0 == "character" => {
                    map.next_value_seed(ColdFeatureSeed("chats", self.1))?;
                }
                "message" if self.0 == "chat" => {
                    map.next_value_seed(ColdFeatureSeed("messages", self.1))?;
                }
                "savedToggleValues" | "bindedPersona" if self.0 == "chat" => {
                    let value = map.next_value_seed(restore::BoundedValueSeed(self.1))?;
                    let mut chat = serde_json::json!({key: value});
                    super::restore::pocket_features::chat(&mut chat, "cold")
                        .map_err(serde::de::Error::custom)?;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
}

struct CancellationReader<'a, R> {
    inner: R,
    cancellation: &'a dyn CancellationProbe,
    observer: Option<&'a dyn LegacyPrepareObserver>,
}

impl<'a, R> CancellationReader<'a, R> {
    fn new(inner: R, cancellation: &'a dyn CancellationProbe) -> Self {
        Self {
            inner,
            cancellation,
            observer: None,
        }
    }

    fn observed(
        inner: R,
        cancellation: &'a dyn CancellationProbe,
        observer: &'a dyn LegacyPrepareObserver,
    ) -> Self {
        Self {
            inner,
            cancellation,
            observer: Some(observer),
        }
    }
}

impl<R: Read> Read for CancellationReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancellation.is_cancelled() {
            return Err(io::Error::other("legacy backup operation was cancelled"));
        }
        let read = self.inner.read(buffer)?;
        if let Some(observer) = self.observer {
            observer.item_bytes(read as u64);
        }
        Ok(read)
    }
}

fn open_staged(entry: &StagedLocalBackupEntry) -> Result<File, LocalBackupError> {
    let path = entry
        .staged_path
        .as_ref()
        .ok_or_else(|| invalid("legacy backup payload is not staged"))?;
    File::open(path).map_err(LocalBackupError::io)
}

fn string_field(object: &Map<String, Value>, field: &str) -> Result<String, LocalBackupError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| invalid(format!("legacy backup Inlay {field} is invalid")))
}

fn inlay_key_hex(name: &str) -> Option<&str> {
    let key = name.strip_prefix("inlay_")?.strip_suffix(".risuinlay")?;
    (!key.is_empty() && key.len() % 2 == 0 && key.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))).then_some(key)
}

fn cold_key(name: &str) -> Option<String> {
    if let Some(rest) = name.strip_prefix("coldstorage/") {
        let key = rest.strip_suffix(".json")?;
        return (!key.is_empty() && !key.contains(['/', '\\'])).then(|| key.to_owned());
    }
    let key = name.strip_prefix("coldstorage_")?.strip_suffix(".json")?;
    uuid::Uuid::parse_str(key)
        .ok()
        .map(|value| value.to_string())
}

fn to_alias_size(size: u64) -> Result<i64, LocalBackupError> {
    i64::try_from(size).map_err(|_| invalid("legacy backup payload is too large"))
}

fn check_cancelled(cancellation: &dyn CancellationProbe) -> Result<(), LocalBackupError> {
    if cancellation.is_cancelled() {
        return Err(LocalBackupError::new(
            LocalBackupErrorCode::Cancelled,
            "legacy backup job was cancelled",
        ));
    }
    Ok(())
}

fn cancellation_io(error: io::Error, cancellation: &dyn CancellationProbe) -> LocalBackupError {
    if cancellation.is_cancelled() {
        LocalBackupError::new(
            LocalBackupErrorCode::Cancelled,
            "legacy backup job was cancelled",
        )
    } else {
        LocalBackupError::io(error)
    }
}

fn invalid(message: impl Into<String>) -> LocalBackupError {
    LocalBackupError::new(LocalBackupErrorCode::DatabaseRestore, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_repository::PayloadCas;
    use crate::import_export_jobs::{parse_json_card, JobStaging};
    use crate::local_backup::{
        parse_legacy_local_backup_v1, NeverCancelled, PayloadTarget, StagedLocalBackupEntry,
        StrictLocalBackupDatabaseRestore,
    };
    use crate::native_file_jobs::content::content_classification_limits;
    use crate::native_file_jobs::{
        character_json_export::export_character_json, JobKind, JobRegistry, JobState,
    };
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::io::Cursor;

    struct PayloadPlanner<'a> {
        cas: &'a PayloadCas,
        durable: DurableCasJob,
        prepared: Option<PreparedLegacyRestorePayloads>,
    }

    impl StrictLocalBackupDatabaseRestore for PayloadPlanner<'_> {
        fn restore_database(
            &mut self,
            _database: &StagedLocalBackupEntry,
            entries: &[StagedLocalBackupEntry],
        ) -> Result<(), crate::local_backup::LocalBackupError> {
            self.prepared = Some(prepare_legacy_restore_payloads(
                entries,
                self.cas,
                &mut self.durable,
                &NeverCancelled,
            )?);
            Ok(())
        }
    }

    fn entry(name: &[u8], data: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(name.len() as u32).to_le_bytes());
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
        bytes
    }

    #[test]
    fn cold_preparation_bounds_values_keys_and_scalar_dispatch_before_reading_the_whole_record() {
        struct ObservedBytes(Cell<u64>);
        impl LegacyPrepareObserver for ObservedBytes {
            fn item_bytes(&self, bytes: u64) { self.0.set(self.0.get() + bytes); }
        }
        for (prefix, suffix) in [
            (br#"[{"role":"user","data":""#.as_slice(), b"\"}]".as_slice()),
            (br#"{"character":{"chats":[{"message":[{"data":""#.as_slice(), b"\"}]}]}}".as_slice()),
            (br#"{"savedToggleValues":{"toggle_large":""#.as_slice(), b"\"},\"message\":[]}".as_slice()),
            (br#"{"character":""#.as_slice(), b"\",\"message\":[]}".as_slice()),
            (br#"{""#.as_slice(), b"\":0,\"message\":[]}".as_slice()),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("cold.json");
            let mut output = File::create(&path).unwrap();
            output.write_all(prefix).unwrap();
            for _ in 0..144 { output.write_all(&[b'x';64*1024]).unwrap(); }
            output.write_all(suffix).unwrap();
            drop(output);
            let entry = StagedLocalBackupEntry { logical_name:"coldstorage/large.json".to_owned(), byte_length:fs::metadata(&path).unwrap().len(), sha256:String::new(), staged_path:Some(path) };
            let observed = ObservedBytes(Cell::new(0));
            let error = prepare_cold(&entry, &NeverCancelled, &observed).unwrap_err();
            assert!(error.message.contains("cold payload exceeds the record limit"), "{error:?}");
            assert!(observed.0.get() <= 8 * 1024 * 1024 + 64 * 1024);
            assert!(observed.0.get() < entry.byte_length);
        }
    }

    #[test]
    fn cold_preparation_checks_decoded_file_size_before_any_parse() {
        struct UnexpectedRead;
        impl LegacyPrepareObserver for UnexpectedRead {
            fn item_bytes(&self, _: u64) { panic!("oversized cold file was read"); }
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cold.json");
        fs::write(&path, br#"{"message":[]}"#).unwrap();
        for (byte_length, limit) in [(0, 8), (1024, 8), (1024, 64)] {
            let entry = StagedLocalBackupEntry { logical_name:"coldstorage/large.json".to_owned(), byte_length, sha256:String::new(), staged_path:Some(path.clone()) };
            let error = prepare_cold_with_limit(&entry, &NeverCancelled, &UnexpectedRead, limit).unwrap_err();
            assert!(error.message.contains("decoded block limit exceeded"));
        }
    }

    #[test]
    fn cancellation_reader_uses_a_non_retryable_error_for_exact_reads() {
        struct AlwaysCancelled;
        impl CancellationProbe for AlwaysCancelled {
            fn is_cancelled(&self) -> bool {
                true
            }
        }

        let mut reader = CancellationReader::new(io::empty(), &AlwaysCancelled);
        let error = reader.read_exact(&mut [0_u8; 1]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(
            cancellation_io(error, &AlwaysCancelled).code,
            LocalBackupErrorCode::Cancelled
        );
    }

    #[derive(Default)]
    struct ArchiveCapture {
        entries: Vec<(String, Vec<u8>)>,
    }

    impl StrictLocalBackupDatabaseRestore for ArchiveCapture {
        fn restore_database(
            &mut self,
            _database: &StagedLocalBackupEntry,
            entries: &[StagedLocalBackupEntry],
        ) -> Result<(), crate::local_backup::LocalBackupError> {
            self.entries = entries
                .iter()
                .map(|entry| {
                    let bytes = fs::read(
                        entry
                            .staged_path
                            .as_ref()
                            .expect("job-staged archive entry has a path"),
                    )
                    .map_err(crate::local_backup::LocalBackupError::io)?;
                    Ok((entry.logical_name.clone(), bytes))
                })
                .collect::<Result<_, crate::local_backup::LocalBackupError>>()?;
            Ok(())
        }
    }

    #[test]
    fn legacy_restore_progress_charges_read_prepare_and_database_against_one_budget() {
        let job = JobRegistry::default()
            .create(JobKind::RestoreLegacyLocalBackup)
            .unwrap();
        job.start(JobPhase::ReadingSource).unwrap();
        let progress = LegacyRestoreProgress::new(&job, 1_000);

        progress.entry_started("assets/portrait.png", 300, 0);
        progress.bytes_read(300);
        progress.entry_started("inlay/picture.webp", 200, 1);
        progress.entry_started("inlay_sidecar/picture", 10, 2);
        progress.entry_started("inlay_meta/picture", 10, 3);
        progress.entry_started("inlay_thumb/picture", 10, 4);
        progress.entry_started("coldstorage/cold-1.json", 40, 5);
        progress.entry_started("database.risudat", 430, 6);
        progress.bytes_read(1_000);
        progress.entries_complete(7);

        let status = job.status();
        assert_eq!(
            status.progress,
            JobProgress {
                completed_bytes: 1_000,
                total_bytes: Some(2_000),
                completed_items: 7,
                total_items: Some(7),
            }
        );
        let detail = status.detail.expect("archive read reports detail");
        assert_eq!(detail.stage, JobStage::ReadingArchive);
        assert_eq!(detail.stage_unit, StageUnit::Bytes);
        assert_eq!(
            (detail.stage_completed, detail.stage_total),
            (1_000, Some(1_000))
        );
        assert!(detail.current_item.is_none());
        assert_eq!(
            detail.counts,
            ImportCounts {
                entries_read: 7,
                entries_total: Some(7),
                assets: 1,
                pocket_media: 1,
                pocket_metadata: 2,
                skipped: 1,
                cold_storage: 1,
                ..ImportCounts::default()
            }
        );

        progress.begin(3);
        progress.item_started("assets/portrait.png");
        let started = job.status().detail.unwrap();
        assert_eq!(started.stage, JobStage::PreparingAttachments);
        assert_eq!(started.current_item.as_deref(), Some("assets/portrait.png"));
        progress.item_bytes(300);
        progress.item_done();

        let status = job.status();
        assert_eq!(status.progress.completed_bytes, 1_300);
        assert_eq!(status.progress.total_bytes, Some(2_000));
        assert_eq!(status.progress.completed_items, 7);
        let detail = status.detail.unwrap();
        assert_eq!(detail.stage, JobStage::PreparingAttachments);
        assert_eq!(detail.stage_unit, StageUnit::Items);
        assert_eq!((detail.stage_completed, detail.stage_total), (1, Some(3)));
        assert_eq!(detail.counts.attachments_prepared, 1);
        assert!(detail.current_item.is_none());

        let scale = progress.restore_scale();
        assert_eq!(scale.base_bytes, 1_300);
        assert_eq!(scale.total_bytes, Some(2_000));
        assert_eq!(scale.fixed_items, Some((7, Some(7))));
        assert_eq!(scale.counts.cold_storage, 1);
    }

    #[test]
    fn legacy_restore_progress_never_exceeds_its_budget_or_reports_backwards() {
        let job = JobRegistry::default()
            .create(JobKind::RestoreLegacyLocalBackup)
            .unwrap();
        job.start(JobPhase::ReadingSource).unwrap();
        let progress = LegacyRestoreProgress::new(&job, 100);
        progress.entry_started("assets/a.png", 100, 0);
        progress.bytes_read(100);
        progress.entries_complete(1);
        progress.begin(1);
        progress.item_started("assets/a.png");
        // A reader that overshoots (for example a re-read) is clamped to the archive size.
        progress.item_bytes(150);
        progress.item_done();
        let status = job.status();
        assert_eq!(status.progress.completed_bytes, 200);
        assert_eq!(status.progress.total_bytes, Some(200));
        assert_eq!(status.state, JobState::Running);
        assert_eq!(progress.restore_scale().base_bytes, 200);
    }



    #[test]
    fn prepares_asset_inlay_and_cold_payloads_without_mutating_live_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let _store = PersistentStore::open(directory.path()).unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let inlay_metadata = serde_json::json!({
            "key": "inlay-id",
            "kind": "inlay",
            "inlayType": "image",
            "mime": "image/webp",
            "name": "image.webp",
            "ext": "webp",
            "size": 4,
            "width": 2,
            "height": 3,
            "pocketRisu": {
                "createdAt": 11,
                "updatedAt": 22,
                "charId": "synthetic-character",
                "chatId": "synthetic-chat"
            },
            "unknownMetadata": {
                "nested": {
                    "retained": true
                }
            }
        });
        let header = serde_json::to_vec(&inlay_metadata).unwrap();
        let mut inlay = Vec::new();
        inlay.extend_from_slice(&(header.len() as u32).to_le_bytes());
        inlay.extend_from_slice(&header);
        inlay.extend_from_slice(b"webp");
        let cold_key = "123e4567-e89b-42d3-a456-426614174000";
        let bytes = [
            entry(b"portrait.png", b"original"),
            entry(b"inlay_696e6c61792d6964.risuinlay", &inlay),
            entry(
                format!("coldstorage_{cold_key}.json").as_bytes(),
                br#"{"message":[{"data":"cold"}]}"#,
            ),
            entry(b"database.risudat", b"RISUSAVE\0"),
        ]
        .concat();
        let mut planner = PayloadPlanner {
            cas: &cas,
            durable: DurableCasJob::begin(
                directory.path(),
                &Uuid::new_v4().to_string(),
                CasJobKind::LocalBackupRestore,
                now_millis(),
            )
            .unwrap(),
            prepared: None,
        };

        parse_legacy_local_backup_v1(
            &mut Cursor::new(bytes),
            directory.path(),
            PayloadTarget::JobStaging,
            &mut planner,
            &NeverCancelled,
        )
        .unwrap();

        let prepared = planner.prepared.take().unwrap();
        assert_eq!(prepared.asset_aliases.len(), 2);
        assert_eq!(prepared.asset_aliases[0].key, "assets/portrait.png");
        assert_eq!(prepared.asset_aliases[1].key, "inlay-id");
        assert_eq!(
            prepared.asset_aliases[1].metadata,
            serde_json::json!({
                "pocketRisu": {
                    "createdAt": 11,
                    "updatedAt": 22,
                    "charId": "synthetic-character",
                    "chatId": "synthetic-chat"
                },
                "unknownMetadata": {
                    "nested": {
                        "retained": true
                    }
                }
            })
        );
        assert!(prepared.cold_payloads[cold_key].is_file());
        for alias in &prepared.asset_aliases {
            assert!(cas
                .object_path(alias.object_hash.as_deref().unwrap())
                .unwrap()
                .is_some());
        }
        assert!(!directory
            .path()
            .join("persistent/persistent.sqlite3")
            .exists());
        assert_eq!(planner.durable.pin_count(), 2);
        planner.durable.release(CasReleaseOutcome::Aborted).unwrap();
    }

    #[test]
    fn legacy_backup_empty_mime_asset_roundtrips_through_native_json_with_exact_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let payload = b"legacy-empty-mime-payload";
        let bytes = [
            entry(b"legacy.bin", payload),
            entry(b"database.risudat", b"RISUSAVE\0"),
        ]
        .concat();
        let mut planner = PayloadPlanner {
            cas: &cas,
            durable: DurableCasJob::begin(
                directory.path(),
                &Uuid::new_v4().to_string(),
                CasJobKind::LocalBackupRestore,
                now_millis(),
            )
            .unwrap(),
            prepared: None,
        };
        parse_legacy_local_backup_v1(
            &mut Cursor::new(bytes),
            directory.path(),
            PayloadTarget::JobStaging,
            &mut planner,
            &NeverCancelled,
        )
        .unwrap();
        let prepared_payloads = planner.prepared.take().unwrap();
        let alias = prepared_payloads.asset_aliases[0].clone();
        assert_eq!(alias.key, "assets/legacy.bin");
        assert!(alias.mime.is_empty());

        let mut store = PersistentStore::open(directory.path()).unwrap();
        let character = serde_json::json!({
            "type": "character",
            "chaId": "legacy-json-character",
            "name": "Legacy JSON",
            "image": "",
            "ccAssets": [{
                "type": "x-risu-asset",
                "uri": alias.key,
                "name": "legacy",
                "ext": "bin"
            }],
            "additionalAssets": [],
            "emotionImages": [],
            "triggerscript": [],
            "customscript": [],
            "chats": []
        });
        let staging = store.replace_begin().unwrap().staging_id;
        store
            .replace_put_root(&staging, &serde_json::json!({}))
            .unwrap();
        store.replace_put_presets(&staging, &[]).unwrap();
        store
            .replace_add_characters(&staging, &[character])
            .unwrap();
        store.replace_put_asset_aliases(&staging, &[alias]).unwrap();
        let revision = store.replace_commit(&staging, Some(0)).unwrap().revision;
        let metadata = serde_json::json!({
            "spec": "chara_card_v3",
            "spec_version": "3.0",
            "data": {
                "name": "Legacy JSON",
                "extensions": {"risuai": {
                    "triggerscript": [],
                    "customScripts": []
                }},
                "assets": [
                    {"type": "x-risu-asset", "uri": "assets/legacy.bin", "name": "legacy", "ext": "bin"},
                    {"type": "icon", "uri": "ccdefault:", "name": "main", "ext": "png"}
                ]
            }
        });
        let prepared = store.prepare_risu_save_export(revision).unwrap();
        let owned = directory.path().join("json-owned");
        let parsed_root = directory.path().join("json-parsed");
        fs::create_dir(&owned).unwrap();
        fs::create_dir(&parsed_root).unwrap();
        let destination = directory.path().join("legacy.json");
        let job = JobRegistry::default()
            .create(JobKind::ExportCharacterCard)
            .unwrap();

        export_character_json(
            prepared,
            "legacy-json-character",
            metadata,
            &owned,
            &directory.path().join("handoffs"),
            Some(&destination),
            &job,
        )
        .unwrap();

        let exported = fs::read(&destination).unwrap();
        assert!(
            String::from_utf8_lossy(&exported).contains("data:application/octet-stream;base64,")
        );
        let staging = JobStaging::open(&parsed_root).unwrap();
        let parsed = parse_json_card(
            &mut exported.as_slice(),
            &staging,
            &content_classification_limits(),
            &|| false,
        )
        .unwrap();
        assert_eq!(parsed.payloads[0].payload.byte_size, payload.len() as u64);
        assert_eq!(
            parsed.payloads[0].payload.sha256,
            hex::encode(Sha256::digest(payload))
        );
        assert_eq!(
            fs::read(parsed_root.join(&parsed.payloads[0].payload.staged_name)).unwrap(),
            payload
        );
        planner.durable.release(CasReleaseOutcome::Aborted).unwrap();
    }

    #[test]
    fn rejects_account_encryption_before_preparing_any_payload_object() {
        let directory = tempfile::tempdir().unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let bytes = [
            entry(b"portrait.png", b"original"),
            entry(b"encryption.risudat", br#"{"type":"account","time":123}"#),
            entry(b"database.risudat", b"encrypted"),
        ]
        .concat();
        let mut planner = PayloadPlanner {
            cas: &cas,
            durable: DurableCasJob::begin(
                directory.path(),
                &Uuid::new_v4().to_string(),
                CasJobKind::LocalBackupRestore,
                now_millis(),
            )
            .unwrap(),
            prepared: None,
        };

        let error = parse_legacy_local_backup_v1(
            &mut Cursor::new(bytes),
            directory.path(),
            PayloadTarget::JobStaging,
            &mut planner,
            &NeverCancelled,
        )
        .unwrap_err();

        assert_eq!(
            error.code,
            crate::local_backup::LocalBackupErrorCode::CompatibilityImportRequired
        );
        assert_eq!(
            fs::read_dir(directory.path().join("assets/objects"))
                .map(|entries| entries.count())
                .unwrap_or(0),
            0,
        );
        assert_eq!(planner.durable.pin_count(), 0);
        planner.durable.release(CasReleaseOutcome::Aborted).unwrap();
    }

    #[test]
    fn imports_pocket_risu_110_inlays_with_sidecars_after_payloads() {
        let directory = tempfile::tempdir().unwrap();
        let _store = PersistentStore::open(directory.path()).unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let bytes = [
            entry(b"portrait.png", b"original"),
            entry(b"inlay/pocket.webp", b"webp"),
            entry(b"inlay/voice.mp3", b"audio"),
            entry(b"inlay_meta/pocket", br#"{"derived":true}"#),
            entry(b"inlay_thumb/pocket", b"thumbnail"),
            entry(
                b"inlay_sidecar/pocket",
                br#"{"ext":"webp","name":"original.webp","type":"image","width":2,"height":3}"#,
            ),
            entry(
                b"inlay_sidecar/voice",
                br#"{"ext":"mp3","name":"voice.mp3","type":"audio"}"#,
            ),
            entry(b"database.risudat", b"RISUSAVE\0"),
        ]
        .concat();
        let mut planner = PayloadPlanner {
            cas: &cas,
            durable: DurableCasJob::begin(
                directory.path(),
                &Uuid::new_v4().to_string(),
                CasJobKind::LocalBackupRestore,
                now_millis(),
            )
            .unwrap(),
            prepared: None,
        };

        parse_legacy_local_backup_v1(
            &mut Cursor::new(bytes),
            directory.path(),
            PayloadTarget::JobStaging,
            &mut planner,
            &NeverCancelled,
        )
        .unwrap();

        let prepared = planner.prepared.take().unwrap();
        assert_eq!(prepared.asset_aliases.len(), 3);
        for (key, bytes, mime, inlay_type) in [
            ("pocket", b"webp".as_slice(), "image/webp", "image"),
            ("voice", b"audio".as_slice(), "audio/mpeg", "audio"),
        ] {
            let alias = prepared
                .asset_aliases
                .iter()
                .find(|alias| alias.key == key)
                .unwrap();
            assert_eq!(alias.kind, "inlay");
            assert_eq!(alias.mime, mime);
            assert_eq!(alias.inlay_type.as_deref(), Some(inlay_type));
            assert_eq!(
                fs::read(
                    cas.object_path(alias.object_hash.as_deref().unwrap())
                        .unwrap()
                        .unwrap()
                )
                .unwrap(),
                bytes
            );
        }
        let image = &prepared.asset_aliases[1];
        assert_eq!(image.name, "original.webp");
        assert_eq!((image.width, image.height), (Some(2), Some(3)));
        assert!(!directory
            .path()
            .join("persistent/persistent.sqlite3")
            .exists());
        assert_eq!(planner.durable.pin_count(), 3);
        planner.durable.release(CasReleaseOutcome::Aborted).unwrap();
    }


}
