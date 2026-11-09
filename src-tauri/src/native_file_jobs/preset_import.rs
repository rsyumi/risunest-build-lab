use super::card_mapping::{MAPPING_LIMITS, MAPPING_STACK_BYTES, MAX_MAPPED_CARD_BYTES};
use super::error::{cancelled, io_error, job_error};
use super::{JobControl, JobKind, JobPhase, JobProgress, JobResultSummary, JobState, NativeFileJobState, NativeJobError, OpenedJobSource, SourceFingerprintKind};
use risunest_formats::{ids::RandomIds, preset::import_preset};
use risunest_ir::{Budget, rng::OsEntropy, stage::StageError};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::{fs::{self, OpenOptions}, io::{Read, Write, Seek, SeekFrom}, path::{Path, PathBuf}};

const RECEIPT: &str = "preset-receipt.json";
const SOURCE: &str = "preset-source";
pub(super) const MAX_SOURCE_CHUNK: usize = 1 << 20;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreparedPreset {
    display_name: String,
    source_bytes: u64,
    native_mapping: PresetMapping,
}

#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
enum PresetMapping {
    Mapped { preset: Box<RawValue> },
    Declined { reason: String, name: String },
}

impl<'de> Deserialize<'de> for PresetMapping {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Fields {
            outcome: String,
            preset: Option<Box<RawValue>>,
            reason: Option<String>,
            name: Option<String>,
        }
        // Read RawValue directly, before an internally tagged enum could buffer it.
        let fields = Fields::deserialize(deserializer)?;
        match fields.outcome.as_str() {
            "mapped" => Ok(Self::Mapped {
                preset: fields.preset.ok_or_else(|| serde::de::Error::missing_field("preset"))?,
            }),
            "declined" => Ok(Self::Declined {
                reason: fields.reason.ok_or_else(|| serde::de::Error::missing_field("reason"))?,
                name: fields.name.ok_or_else(|| serde::de::Error::missing_field("name"))?,
            }),
            value => Err(serde::de::Error::unknown_variant(value, &["mapped", "declined"])),
        }
    }
}

fn decline(error: StageError) -> PresetMapping {
    let (reason, name) = match error {
        StageError::Unsupported(name) => ("unsupported", name.into_owned()),
        StageError::Invalid(name) => ("invalid", name.into_owned()),
        StageError::Limit(name) => ("limit", name.into_owned()),
        StageError::Cancelled => ("cancelled", String::new()),
        StageError::Host(_) => ("host", "preset".into()),
        _ => ("failed", "preset".into()),
    };
    PresetMapping::Declined { reason: reason.into(), name }
}

pub(super) fn prepare(mut source: OpenedJobSource, name: &str, directory: &Path, job: &JobControl) -> Result<JobResultSummary, NativeJobError> {
    if job.is_cancel_requested() { return Err(cancelled("Preset preparation cancelled")); }
    job.start(JobPhase::ReadingSource).map_err(|error| if job.is_cancel_requested() { cancelled("Preset preparation cancelled") } else { job_error(error) })?;
    let mut output = OpenOptions::new().create_new(true).write(true).open(directory.join(SOURCE)).map_err(io_error)?;
    let mut copied = 0u64;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        if job.is_cancel_requested() { return Err(cancelled("Preset preparation cancelled")); }
        let read = source.file.read(&mut buffer).map_err(io_error)?;
        if read == 0 { break; }
        copied = copied.checked_add(read as u64).ok_or_else(|| NativeJobError::new("invalid-source", "Preset source size overflow"))?;
        if copied > source.total_bytes { return Err(NativeJobError::new("invalid-source", "Preset source length changed")); }
        output.write_all(&buffer[..read]).map_err(io_error)?;
        digest.update(&buffer[..read]);
        job.set_progress(JobProgress { completed_bytes: copied, total_bytes: Some(source.total_bytes), completed_items: 0, total_items: Some(1) }).map_err(job_error)?;
    }
    if copied != source.total_bytes { return Err(NativeJobError::new("invalid-source", "Preset source length changed")); }
    output.flush().map_err(io_error)?;
    output.sync_all().map_err(io_error)?;
    drop(output);
    drop(source);
    let mapping = if copied > super::preset_codec::MAX_SOURCE_BYTES as u64 {
        decline(StageError::Limit("presetSourceBytes".into()))
    } else {
        std::thread::scope(|scope| {
            std::thread::Builder::new().name("preset-mapping".into()).stack_size(MAPPING_STACK_BYTES).spawn_scoped(scope, || {
                let run = || -> Result<Box<RawValue>, StageError> {
                    let budget = Budget::new(MAPPING_LIMITS, job.cancellation_flag())?;
                    let bytes = fs::read(directory.join(SOURCE)).map_err(|_| StageError::Host(Vec::new()))?;
                    let decoded = super::preset_codec::decode(&bytes, name, &budget, &|| job.is_cancel_requested())?;
                    let mapped = import_preset(&decoded, &mut RandomIds(OsEntropy), &budget)?;
                    let text = risunest_ir::json::stringify(&mapped, &budget)?.ok_or_else(|| StageError::Invalid("presetJson".into()))?;
                    let text = text.as_unicode().ok_or_else(|| StageError::Unsupported("presetJson".into()))?;
                    if text.len() > MAX_MAPPED_CARD_BYTES { return Err(StageError::Limit("presetReceiptBytes".into())); }
                    RawValue::from_string(text.to_owned()).map_err(|_| StageError::Invalid("presetJson".into()))
                };
                match run() { Ok(preset) => PresetMapping::Mapped { preset }, Err(error) => decline(error) }
            }).map(|worker| worker.join())
        }).ok().and_then(Result::ok).unwrap_or_else(|| PresetMapping::Declined { reason: "failed".into(), name: "presetThread".into() })
    };
    if job.is_cancel_requested() { return Err(cancelled("Preset preparation cancelled")); }
    let receipt = PreparedPreset { display_name: name.to_owned(), source_bytes: copied, native_mapping: mapping };
    let mut output = OpenOptions::new().create_new(true).write(true).open(directory.join(RECEIPT)).map_err(io_error)?;
    serde_json::to_writer(&mut output, &receipt).map_err(|error| NativeJobError::new("store-error", error.to_string()))?;
    output.flush().map_err(io_error)?;
    output.sync_all().map_err(io_error)?;
    // Synchronize completion with cancellation before retaining the original for a possible TS fallback.
    {
        let mut status = job.status.lock().map_err(|_| job_error("native job status mutex poisoned"))?;
        if job.is_cancel_requested() || status.state != JobState::Running { return Err(cancelled("Preset preparation cancelled")); }
        status.phase = JobPhase::FinalizingExport;
    }
    Ok(JobResultSummary { revision: 0, source_bytes: copied, source_sha256: hex::encode(digest.finalize()), source_fingerprint_kind: SourceFingerprintKind::WholeFileSha256,
        character_count: 0, preset_count: 0, warning_codes: vec![], handoff_path: None, publication: None, export_exclusions: None })
}

fn directory(state: &NativeFileJobState, id: &str) -> Result<PathBuf, NativeJobError> {
    let status = state.status_summary(id)?;
    if status.kind != JobKind::PreparePresetImport || status.state != JobState::Succeeded {
        return Err(NativeJobError::new("invalid-state", "Preset preparation is not available"));
    }
    Ok(state.root.join("jobs").join(id))
}

pub(crate) fn receipt(state: &NativeFileJobState, id: &str) -> Result<PreparedPreset, NativeJobError> {
    let _operation = state.admit_cleanup_operation()?;
    let directory = directory(state, id)?;
    let mut opened = super::open_regular_file_no_follow(&directory.join(RECEIPT))?;
    if opened.total_bytes > (MAX_MAPPED_CARD_BYTES + 8192) as u64 { return Err(NativeJobError::new("invalid-state", "Preset receipt is too large")); }
    let mut bytes = Vec::new();
    Read::by_ref(&mut opened.file).take(opened.total_bytes + 1).read_to_end(&mut bytes).map_err(io_error)?;
    if bytes.len() as u64 != opened.total_bytes { return Err(NativeJobError::new("invalid-state", "Preset receipt length changed")); }
    serde_json::from_slice(&bytes).map_err(|error| NativeJobError::new("invalid-state", error.to_string()))
}

pub(crate) fn source_chunk(state: &NativeFileJobState, id: &str, offset: u64, length: usize) -> Result<Vec<u8>, NativeJobError> {
    let _operation = state.admit_cleanup_operation()?;
    let directory = directory(state, id)?;
    if length > MAX_SOURCE_CHUNK { return Err(NativeJobError::new("invalid-input", "Preset source chunk is too large")); }
    let mut opened = super::open_regular_file_no_follow(&directory.join(SOURCE))?;
    let expected = state.status_summary(id)?.result.ok_or_else(|| NativeJobError::new("invalid-state", "Preset result is unavailable"))?.source_bytes;
    if opened.total_bytes != expected || offset.checked_add(length as u64).is_none_or(|end| end > expected) {
        return Err(NativeJobError::new("invalid-input", "Preset source range is invalid"));
    }
    opened.file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
    let mut bytes = vec![0; length];
    opened.file.read_exact(&mut bytes).map_err(io_error)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_file_jobs::{NativeFileJobTask, JobSource};
    use tempfile::TempDir;
    use std::time::{Duration, Instant};

    fn start(state: &NativeFileJobState, source: &Path, name: &str) -> String {
        state.spawn(NativeFileJobTask::PreparePresetImport {
            opened_source: Some(super::super::open_regular_file_no_follow(source).unwrap()),
            source: JobSource::DesktopPath { path: source.to_string_lossy().into_owned() }, display_name: name.into(),
        }, false).unwrap().job_id
    }
    fn settled(state: &NativeFileJobState, id: &str) -> super::super::JobStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = state.status_summary(id).unwrap();
            if status.state.is_terminal() { return status; }
            assert!(Instant::now() < deadline, "preset worker did not settle");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn preset_receipt_maps_once_keeps_original_and_never_embeds_the_body_in_status() {
        let temp = TempDir::new().unwrap();
        let state = NativeFileJobState::initialize(temp.path().join("native-file-jobs"));
        let source = temp.path().join("input.json");
        let original = br#"{"id":"old","name":"Synthetic preset","unknown":{"x":1},"openAIKey":"synthetic"}"#;
        fs::write(&source, original).unwrap();
        let id = start(&state, &source, "input.json");
        assert_eq!(settled(&state, &id).state, JobState::Succeeded);
        let first = receipt(&state, &id).unwrap();
        let second = receipt(&state, &id).unwrap();
        let value = |receipt: PreparedPreset| match receipt.native_mapping {
            PresetMapping::Mapped { preset } => serde_json::from_str::<serde_json::Value>(preset.get()).unwrap(),
            PresetMapping::Declined { reason, name } => panic!("unexpected decline {reason}/{name}"),
        };
        let first = value(first);
        assert_eq!(first, value(second));
        assert_ne!(first["id"], "old");
        assert!(uuid::Uuid::parse_str(first["id"].as_str().unwrap()).is_ok());
        assert_eq!(first["unknown"]["x"], 1);
        assert_eq!(first["openAIKey"], "synthetic");
        fs::write(&source, b"replaced by caller").unwrap();
        assert_eq!(source_chunk(&state, &id, 0, original.len()).unwrap(), original);
        let status = serde_json::to_string(&state.status_summary(&id).unwrap()).unwrap();
        assert!(!status.contains("Synthetic preset"));
        assert!(state.forget(&id).unwrap());
        assert!(!state.root.join("jobs").join(&id).exists());
        assert_eq!(fs::read(&source).unwrap(), b"replaced by caller");
    }

    #[test]
    fn declined_preset_keeps_exact_invalid_bytes_for_the_existing_reader() {
        let temp = TempDir::new().unwrap();
        let state = NativeFileJobState::initialize(temp.path().join("native-file-jobs"));
        let source = temp.path().join("input.risup");
        let bytes = [0, 255, 1, 2, 3];
        fs::write(&source, bytes).unwrap();
        let id = start(&state, &source, "input.risup");
        assert_eq!(settled(&state, &id).state, JobState::Succeeded);
        assert!(matches!(receipt(&state, &id).unwrap().native_mapping, PresetMapping::Declined { .. }));
        assert_eq!(source_chunk(&state, &id, 0, bytes.len()).unwrap(), bytes);
        assert!(source_chunk(&state, &id, bytes.len() as u64, 1).is_err());
        assert!(source_chunk(&state, &id, 0, MAX_SOURCE_CHUNK + 1).is_err());
        state.forget(&id).unwrap();
    }

    #[test]
    fn cancelled_preset_preparation_never_creates_a_receipt() {
        let temp = TempDir::new().unwrap();
        let state = NativeFileJobState::initialize(temp.path().join("native-file-jobs"));
        let job = state.registry.create(JobKind::PreparePresetImport).unwrap();
        let source = temp.path().join("input.json");
        fs::write(&source, b"{}").unwrap();
        job.request_cancel().unwrap();
        let result = prepare(super::super::open_regular_file_no_follow(&source).unwrap(), "input.json", temp.path(), &job);
        assert_eq!(result.unwrap_err().code, "cancelled");
        assert!(!temp.path().join(RECEIPT).exists());
        assert_eq!(fs::read(source).unwrap(), b"{}");
    }

    #[test]
    fn preset_copy_rejects_a_changed_opened_length_before_mapping() {
        let temp = TempDir::new().unwrap();
        let state = NativeFileJobState::initialize(temp.path().join("native-file-jobs"));
        let job = state.registry.create(JobKind::PreparePresetImport).unwrap();
        let source = temp.path().join("input.json");
        fs::write(&source, b"{}").unwrap();
        let mut opened = super::super::open_regular_file_no_follow(&source).unwrap();
        opened.total_bytes += 1;
        assert_eq!(prepare(opened, "input.json", temp.path(), &job).unwrap_err().code, "invalid-source");
        assert!(!temp.path().join(RECEIPT).exists());
    }
    #[test]
    fn restart_discards_uncommitted_preparation_without_replaying_import() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("native-file-jobs");
        let source = temp.path().join("input.json");
        fs::write(&source, br#"{"name":"Uncommitted"}"#).unwrap();
        let id = {
            let state = NativeFileJobState::initialize(root.clone());
            let id = start(&state, &source, "input.json");
            assert_eq!(settled(&state, &id).state, JobState::Succeeded);
            assert!(receipt(&state, &id).is_ok());
            id
        };
        let reopened = NativeFileJobState::initialize(root.clone());
        assert!(reopened.list().unwrap().is_empty());
        assert!(!root.join("jobs").join(&id).exists());
        assert_eq!(fs::read(source).unwrap(), br#"{"name":"Uncommitted"}"#);
    }

    #[test]
    fn android_preset_claim_preserves_exact_bytes_and_rejects_replay_or_length_mismatch() {
        for bad_length in [false, true] {
            let temp = TempDir::new().unwrap();
            let state = NativeFileJobState::initialize(temp.path().join("native-file-jobs"));
            let token = uuid::Uuid::new_v4().to_string();
            let spool = state.root.join("sources").join(&token); fs::create_dir_all(&spool).unwrap();
            let bytes = br#"{"name":"Android synthetic","custom":1}"#;
            fs::write(spool.join("source.risudat"), bytes).unwrap();
            fs::write(spool.join("ownership.json"), serde_json::to_vec(&serde_json::json!({
                "format":"risunest-android-saf-spool", "version":1, "token":token, "createdAtMillis":1,
            })).unwrap()).unwrap();
            fs::write(spool.join("source.json"), serde_json::to_vec(&serde_json::json!({
                "token":token, "state":"ready", "displayName":"input.preset", "bytes":bytes.len() + usize::from(bad_length),
                "totalBytes":bytes.len(), "importDestination":"preset",
            })).unwrap()).unwrap();
            let task = || NativeFileJobTask::PreparePresetImport {
                opened_source: None, source: JobSource::AndroidSpool { token: token.clone() }, display_name: "input.preset".into(),
            };
            let started = state.spawn(task(), false);
            if bad_length {
                assert_eq!(started.unwrap_err().code, "invalid-source");
            } else {
                let id = started.unwrap().job_id;
                assert_eq!(settled(&state, &id).state, JobState::Succeeded);
                assert_eq!(source_chunk(&state, &id, 0, bytes.len()).unwrap(), bytes);
                assert!(matches!(receipt(&state, &id).unwrap().native_mapping, PresetMapping::Mapped { .. }));
                assert!(state.spawn(task(), false).is_err());
                state.forget(&id).unwrap();
            }
            assert!(!spool.exists());
        }
    }

}
