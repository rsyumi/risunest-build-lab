use super::card_mapping::{MAPPING_LIMITS, MAPPING_STACK_BYTES, MAX_MAPPED_CARD_BYTES};
use super::error::{cancelled, destination_error_with, io_error, job_error, store_error};
use super::{JobControl, JobPhase, JobResultSummary, NativeJobError, SourceFingerprintKind};
use crate::persistent_store::{PreparedRisuSaveExport, export::{self, destination}};
use risunest_ir::{Budget, stage::StageError};
use serde::Deserialize;
use std::{cell::RefCell, fs::{self, OpenOptions}, io::{Read, Write}, path::Path};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PresetFormat { Json, Risup }
impl PresetFormat {
    fn extension(self) -> &'static str { match self { Self::Json => "json", Self::Risup => "risup" } }
    fn filename(self) -> &'static str { match self { Self::Json => "preset.json", Self::Risup => "preset.risup" } }
}

fn declined() -> NativeJobError { NativeJobError::new("native-preset-export-declined", "Native preset export requires compatibility encoding") }

#[allow(clippy::too_many_arguments)]
pub(crate) fn export(prepared: PreparedRisuSaveExport, index: u64, id: &str, format: PresetFormat,
    compatibility_source: Option<&Path>, directory: &Path, handoffs: &Path, destination_path: Option<&Path>, job: &JobControl,
) -> Result<JobResultSummary, NativeJobError> {
    export_with_release(prepared, index, id, format, compatibility_source, directory, handoffs, destination_path, job, PreparedRisuSaveExport::release_reader)
}

#[allow(clippy::too_many_arguments)]
fn export_with_release<F>(mut prepared: PreparedRisuSaveExport, index: u64, id: &str, format: PresetFormat,
    compatibility_source: Option<&Path>, directory: &Path, handoffs: &Path, destination_path: Option<&Path>, job: &JobControl, release: F,
) -> Result<JobResultSummary, NativeJobError>
where F: FnOnce(&mut PreparedRisuSaveExport) -> crate::persistent_store::StoreResult<()> {
    let mut release = Some(release);
    let outcome = (|| {
        if job.is_cancel_requested() { return Err(cancelled("Preset export cancelled")); }
        job.start(JobPhase::WritingExport).map_err(|error| if job.is_cancel_requested() { cancelled("Preset export cancelled") } else { job_error(error) })?;
        let text = {
            let reader = prepared.reader().map_err(store_error)?;
            export::pinned_preset_text(&reader.connection, &reader.target, index, id,
                if compatibility_source.is_some() { 0 } else { MAX_MAPPED_CARD_BYTES }).map_err(store_error)?
        };
        let source = directory.join(format.filename());
        let mut output = OpenOptions::new().create_new(true).write(true).open(&source).map_err(io_error)?;
        if let Some(path) = compatibility_source {
            // The compatibility writer supplies bytes, never a JSON approximation of a declined value.
            if !super::owned_handoff_file(handoffs.parent().ok_or_else(|| NativeJobError::new("invalid-source", "Preset handoff root is unavailable"))?, path, super::download_handoff::PREFIX, super::download_handoff::SUFFIX, "download", "invalid-source")? {
                return Err(NativeJobError::new("invalid-source", "Preset compatibility source is unavailable"));
            }
            let mut input = super::open_regular_file_no_follow(path)?;
            let mut buffer = vec![0; 64 * 1024];
            let mut copied = 0u64;
            loop {
                if job.is_cancel_requested() { return Err(cancelled("Preset export cancelled")); }
                let read = input.file.read(&mut buffer).map_err(io_error)?;
                if read == 0 { break; }
                copied += read as u64;
                if copied > input.total_bytes { return Err(NativeJobError::new("invalid-source", "Preset compatibility source length changed")); }
                output.write_all(&buffer[..read]).map_err(io_error)?;
                job.set_export_bytes(copied, Some(input.total_bytes)).map_err(job_error)?;
            }
            if copied != input.total_bytes { return Err(NativeJobError::new("invalid-source", "Preset compatibility source length changed")); }
        } else {
            let text = text.ok_or_else(declined)?;
            let bytes = std::thread::scope(|scope| {
                std::thread::Builder::new().name("preset-export".into()).stack_size(MAPPING_STACK_BYTES).spawn_scoped(scope, || -> Result<Vec<u8>, StageError> {
                    let budget = Budget::new(MAPPING_LIMITS, job.cancellation_flag())?;
                    let value = risunest_ir::json::parse_json(&text, &budget)?;
                    let exported = risunest_formats::preset::export_preset(&value, &budget)?;
                    super::preset_codec::encode(&exported, format.extension(), &budget, &|| job.is_cancel_requested())
                }).map(|worker| worker.join())
            }).ok().and_then(Result::ok).and_then(Result::ok).ok_or_else(|| {
                if job.is_cancel_requested() { cancelled("Preset export cancelled") } else { declined() }
            })?;
            for chunk in bytes.chunks(64 * 1024) {
                if job.is_cancel_requested() { return Err(cancelled("Preset export cancelled")); }
                output.write_all(chunk).map_err(io_error)?;
            }
            job.set_export_bytes(bytes.len() as u64, Some(bytes.len() as u64)).map_err(job_error)?;
        }
        output.flush().map_err(io_error)?;
        output.sync_all().map_err(io_error)?;
        drop(output);
        let bytes = fs::metadata(&source).map_err(io_error)?.len();
        release.take().ok_or_else(|| NativeJobError::new("store-error", "preset revision release was already used"))?(&mut prepared).map_err(store_error)?;
        if job.is_cancel_requested() { return Err(cancelled("Preset export cancelled")); }
        job.set_phase(JobPhase::PublishingDestination).map_err(job_error)?;
        let (root, path, handoff_path) = match destination_path {
            Some(path) => (path.parent().ok_or_else(|| NativeJobError::new("invalid-destination", "Preset destination directory is unavailable"))?, path.to_owned(), None),
            None => {
                fs::create_dir_all(handoffs).map_err(io_error)?;
                let path = handoffs.join(format!("preset-{}.{}", Uuid::new_v4(), format.extension()));
                (handoffs, path.clone(), Some(path.to_string_lossy().into_owned()))
            }
        };
        let phase_failure = RefCell::new(None);
        let published = destination::write_preset_destination_controlled(directory, &source, root, &path,
            || job.is_cancel_requested() || phase_failure.borrow().is_some(),
            |progress| {
                if let Err(error) = job.set_export_bytes(progress.copied_bytes, Some(bytes)) { *phase_failure.borrow_mut() = Some(error); }
            },
            || {
                if phase_failure.borrow().is_some() { return Err(destination::DestinationWriteError::Cancelled); }
                job.commit_export_publication().map_err(|error| {
                    *phase_failure.borrow_mut() = Some(error);
                    destination::DestinationWriteError::Cancelled
                })
            },
        ).map_err(|error| phase_failure.into_inner().map(job_error).unwrap_or_else(||
            destination_error_with(error, "Preset export source is invalid", "Preset destination is invalid", "Preset export cancelled")))?;
        Ok(JobResultSummary { revision: prepared.revision, source_bytes: published.bytes, source_sha256: published.sha256,
            source_fingerprint_kind: SourceFingerprintKind::WholeFileSha256, character_count: 0, preset_count: 1,
            warning_codes: vec![], handoff_path, publication: None, export_exclusions: None })
    })();
    match release.take() {
        Some(release) => super::error::finish_with_release(outcome, release(&mut prepared), "preset revision release failed"),
        None => outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_file_jobs::{JobKind, JobRegistry, JobState, NativeFileJobState, NativeFileJobTask};
    use crate::persistent_store::PersistentStore;
    use serde_json::json;
    use tempfile::TempDir;
    use std::{sync::Arc, time::{Duration, Instant}};

    fn fixture(preset: serde_json::Value) -> (TempDir, PersistentStore, i64) {
        let temp = TempDir::new().unwrap();
        let mut store = PersistentStore::open(temp.path()).unwrap();
        let stage = store.replace_begin().unwrap().staging_id;
        store.replace_put_root(&stage, &json!({})).unwrap();
        store.replace_put_presets(&stage, &[preset]).unwrap();
        let revision = store.replace_commit(&stage, None).unwrap().revision;
        (temp, store, revision)
    }
    fn job() -> Arc<JobControl> { JobRegistry::default().create(JobKind::ExportPreset).unwrap() }
    fn run(store: &mut PersistentStore, revision: i64, id: &str, format: PresetFormat,
        directory: &Path, handoffs: &Path, destination: Option<&Path>, control: &JobControl,
    ) -> Result<JobResultSummary, NativeJobError> {
        fs::create_dir_all(directory).unwrap();
        export(store.prepare_risu_save_export(revision).unwrap(), 0, id, format, None, directory, handoffs, destination, control)
    }

    #[test]
    fn pinned_preset_export_blanks_secrets_and_preserves_unknown_fields_in_both_formats() {
        let preset = json!({"id":"preset-a", "name":"Synthetic", "openAIKey":"secret", "forceReplaceUrl":"a", "forceReplaceUrl2":"b", "proxyKey":"c", "textgenWebUIStreamURL":"d", "textgenWebUIBlockingURL":"e", "custom":{"x":9}});
        let (temp, mut store, revision) = fixture(preset);
        for format in [PresetFormat::Json, PresetFormat::Risup] {
            let directory = temp.path().join(format.extension());
            let destination = temp.path().join(format!("output.{}", format.extension()));
            fs::write(&destination, b"previous destination").unwrap();
            let control = job();
            let result = run(&mut store, revision, "preset-a", format, &directory, &temp.path().join("handoffs"), Some(&destination), &control).unwrap();
            let bytes = fs::read(&destination).unwrap();
            assert_eq!(result.source_bytes, bytes.len() as u64);
            let budget = Budget::new(MAPPING_LIMITS, control.cancellation_flag()).unwrap();
            let value = super::super::preset_codec::decode(&bytes, &format!("output.{}", format.extension()), &budget, &|| false).unwrap();
            let text = risunest_ir::json::stringify(&value, &budget).unwrap().unwrap();
            let value: serde_json::Value = serde_json::from_str(text.as_unicode().unwrap()).unwrap();
            for key in ["openAIKey", "forceReplaceUrl", "forceReplaceUrl2", "proxyKey", "textgenWebUIStreamURL", "textgenWebUIBlockingURL"] { assert_eq!(value[key], ""); }
            assert_eq!(value["custom"]["x"], 9);
            assert_eq!(value["id"], "preset-a");
        }
    }

    #[test]
    fn wrong_identity_and_cancel_preserve_existing_destination() {
        let (temp, mut store, revision) = fixture(json!({"id":"preset-a", "name":"Synthetic"}));
        let destination = temp.path().join("output.json");
        fs::write(&destination, b"previous").unwrap();
        assert!(run(&mut store, revision, "different", PresetFormat::Json, &temp.path().join("wrong"), &temp.path().join("handoffs"), Some(&destination), &job()).is_err());
        let control = job(); control.request_cancel().unwrap();
        assert_eq!(run(&mut store, revision, "preset-a", PresetFormat::Json, &temp.path().join("cancel"), &temp.path().join("handoffs"), Some(&destination), &control).unwrap_err().code, "cancelled");
        assert_eq!(fs::read(destination).unwrap(), b"previous");
    }

    #[test]
    fn native_decline_precedes_publication_and_raw_compatibility_reuses_managed_publication() {
        let (temp, mut store, revision) = fixture(json!({"id":"preset-a", "name":"Synthetic", "__proto__":{"x":1}}));
        let destination = temp.path().join("output.risup");
        fs::write(&destination, b"previous").unwrap();
        assert_eq!(run(&mut store, revision, "preset-a", PresetFormat::Risup, &temp.path().join("decline"), &temp.path().join("handoffs"), Some(&destination), &job()).unwrap_err().code, "native-preset-export-declined");
        assert_eq!(fs::read(&destination).unwrap(), b"previous");
        let handoffs = temp.path().join("handoffs"); fs::create_dir_all(&handoffs).unwrap();
        let input = handoffs.join(format!("risu-download-{}.bin", Uuid::new_v4()));
        let encoded = b"exact synthetic TS encoded bytes";
        fs::write(&input, encoded).unwrap();
        let directory = temp.path().join("fallback"); fs::create_dir_all(&directory).unwrap();
        let control = job();
        let result = export(store.prepare_risu_save_export(revision).unwrap(), 0, "preset-a", PresetFormat::Risup, Some(&input), &directory, &handoffs, Some(&destination), &control).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), encoded);
        assert_eq!(result.source_bytes, encoded.len() as u64);
        assert!(matches!(control.request_cancel().unwrap(), super::super::CancelOutcome::TooLate));
        assert_eq!(fs::read(&input).unwrap(), encoded);
    }

    #[test]
    #[ignore = "writes synthetic actual-job outputs for the TS cross-reader; requires RUST_CORE_PRESET_CODEC_NATIVE_DIR"]
    fn write_native_job_outputs_for_ts_cross_reader() {
        let output = std::path::PathBuf::from(std::env::var_os("RUST_CORE_PRESET_CODEC_NATIVE_DIR").expect("synthetic output directory"));
        fs::create_dir_all(&output).unwrap();
        let cases: serde_json::Value = serde_json::from_str(include_str!("../../../tests/fixtures/rust-core/formats/preset-codec.cases.json")).unwrap();
        let mut checked = 0;
        for case in cases["cases"].as_array().unwrap() {
            if case["kind"] != "export" { continue; }
            // serde_json cannot hold an isolated surrogate; parse its JSON with the store's actual JSON reader only for supported stored shapes.
            if case["declineBinaryExport"] == true { continue; }
            let id = case["id"].as_str().unwrap();
            let preset_id = case["body"]["id"].as_str().unwrap();
            let (temp, mut store, revision) = fixture(case["body"].clone());
            let mut prepared = store.prepare_risu_save_export(revision).unwrap();
            let reader = prepared.reader().unwrap();
            let stored = export::pinned_preset_text(&reader.connection, &reader.target, 0, preset_id, MAX_MAPPED_CARD_BYTES).unwrap().unwrap();
            fs::write(output.join(format!("job-{id}.source.json")), stored).unwrap();
            prepared.release_reader().unwrap();
            let state = NativeFileJobState::initialize(temp.path().join("native-file-jobs"));
            for format in [PresetFormat::Json, PresetFormat::Risup] {
                let destination = output.join(format!("job-{id}.{}", format.extension()));
                let started = state.spawn(NativeFileJobTask::ExportPreset { destination: Some(destination), preset_index: 0, preset_id: preset_id.into(), format,
                    prepared: store.prepare_risu_save_export(revision).unwrap(), compatibility_source_path: None }, false).unwrap();
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    let status = state.status_summary(&started.job_id).unwrap();
                    if status.state.is_terminal() { assert_eq!(status.state, JobState::Succeeded, "{:?}", status.error); break; }
                    assert!(Instant::now() < deadline); std::thread::sleep(Duration::from_millis(2));
                }
                state.forget(&started.job_id).unwrap();
            }
            checked += 1;
        }
        assert_eq!(checked, 1);
    }
    #[test]
    fn failed_reader_release_prevents_both_publications_without_repeating_release() {
        for mobile in [false, true] {
            let (temp, mut store, revision) = fixture(json!({"id":"preset-a", "name":"Synthetic"}));
            let directory = temp.path().join("output"); fs::create_dir_all(&directory).unwrap();
            let handoffs = temp.path().join("handoffs");
            let destination = temp.path().join("existing.json"); fs::write(&destination, b"previous").unwrap();
            let calls = std::cell::Cell::new(0);
            let result = export_with_release(store.prepare_risu_save_export(revision).unwrap(), 0, "preset-a", PresetFormat::Json,
                None, &directory, &handoffs, if mobile { None } else { Some(destination.as_path()) }, &job(), |prepared| {
                    calls.set(calls.get() + 1);
                    prepared.release_reader()?;
                    Err(crate::persistent_store::StoreError::Store { message: "synthetic release failure".into() })
                });
            assert_eq!(result.unwrap_err().code, "store-error");
            assert_eq!(calls.get(), 1);
            assert_eq!(fs::read(&destination).unwrap(), b"previous");
            assert!(!handoffs.exists());
        }
    }

}
