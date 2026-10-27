use super::*;
use std::io::Read;

const DIRECTORY: &str = "portable-export-receipts";
const MAX_RECEIPT_BYTES: u64 = 32 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    version: u8,
    job_id: String,
    handoff_name: String,
    revision: i64,
    source_bytes: u64,
    source_sha256: String,
    character_count: u64,
    preset_count: u64,
    warning_codes: Vec<String>,
}

fn directory(root: &Path, create: bool) -> Result<PathBuf, String> {
    let path = root.join(DIRECTORY);
    if create { fs::create_dir_all(&path).map_err(|error| error.to_string())?; }
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() && !crate::trust_boundary::is_link_like(&metadata) => Ok(path),
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => Ok(path),
        _ => Err("Portable export receipt directory is unavailable".into()),
    }
}

fn handoff(root: &Path, name: &str) -> Result<PathBuf, NativeJobError> {
    if !handoff_name(Path::new(name), "risunest-backup-", ".risunest")
        || Path::new(name).file_name().and_then(|name| name.to_str()) != Some(name) {
        return Err(NativeJobError::new("invalid-receipt", "Portable export receipt has an invalid handoff name"));
    }
    let parent = root.join("handoffs");
    let metadata = fs::symlink_metadata(&parent).map_err(|error| NativeJobError::new("export-receipt-unavailable", error.to_string()))?;
    if !metadata.is_dir() || crate::trust_boundary::is_link_like(&metadata) {
        return Err(NativeJobError::new("export-receipt-unavailable", "Portable export handoff directory is unavailable"));
    }
    let path = parent.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !crate::trust_boundary::is_link_like(&metadata) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(NativeJobError::new("export-receipt-unavailable", error.to_string())),
        _ => return Err(NativeJobError::new("invalid-receipt", "Portable export handoff is not an owned regular file")),
    }
    Ok(path)
}

impl Receipt {
    fn result(&self, root: &Path) -> Result<JobResultSummary, NativeJobError> {
        if self.version != 1 || Uuid::parse_str(&self.job_id).is_err() {
            return Err(NativeJobError::new("invalid-receipt", "Portable export receipt identity is invalid"));
        }
        let result = JobResultSummary {
            revision: self.revision, source_bytes: self.source_bytes,
            source_sha256: self.source_sha256.clone(), source_fingerprint_kind: SourceFingerprintKind::WholeFileSha256,
            character_count: self.character_count, preset_count: self.preset_count,
            warning_codes: self.warning_codes.clone(), handoff_path: Some(handoff(root, &self.handoff_name)?.to_string_lossy().into_owned()),
            export_exclusions: None, publication: None,
        };
        validate_result(&result).map_err(|error| NativeJobError::new("invalid-receipt", error))?;
        Ok(result)
    }
}

pub(super) fn persist(root: &Path, id: &str, result: &JobResultSummary) -> Result<(), String> {
    if Uuid::parse_str(id).is_err() || result.source_fingerprint_kind != SourceFingerprintKind::WholeFileSha256
        || result.export_exclusions.is_some() || result.publication.is_some() {
        return Err("Portable export result cannot be retained".into());
    }
    validate_result(result)?;
    let source = Path::new(result.handoff_path.as_deref().ok_or("Portable export handoff is missing")?);
    let name = source.file_name().and_then(|name| name.to_str()).ok_or("Portable export handoff name is invalid")?;
    let owned = handoff(root, name).map_err(|error| error.message)?;
    if source.parent().ok_or("Portable export handoff parent is missing")?.canonicalize().map_err(|error|error.to_string())?
        != root.join("handoffs").canonicalize().map_err(|error|error.to_string())?
        || fs::metadata(&owned).map_err(|error|error.to_string())?.len() != result.source_bytes {
        return Err("Portable export handoff does not match its result".into());
    }
    let receipt = Receipt {
        version: 1, job_id: id.into(), handoff_name: name.into(), revision: result.revision,
        source_bytes: result.source_bytes, source_sha256: result.source_sha256.clone(),
        character_count: result.character_count, preset_count: result.preset_count, warning_codes: result.warning_codes.clone(),
    };
    let bytes = serde_json::to_vec(&receipt).map_err(|error|error.to_string())?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES { return Err("Portable export receipt is too large".into()); }
    let parent = directory(root, true)?;
    let mut temporary = tempfile::NamedTempFile::new_in(&parent).map_err(|error|error.to_string())?;
    temporary.write_all(&bytes).map_err(|error|error.to_string())?;
    temporary.as_file().sync_all().map_err(|error|error.to_string())?;
    temporary.persist(parent.join(format!("{id}.json"))).map_err(|error|error.to_string())?;
    // Rename is the commit point. Directory sync cannot turn a visible receipt
    // back into a failed export after a process restart.
    let _ = crate::trust_boundary::sync_directory(&parent);
    Ok(())
}

fn read(root: &Path, path: &Path) -> Result<Receipt, NativeJobError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| NativeJobError::new("export-receipt-unavailable", error.to_string()))?;
    if !metadata.is_file() || crate::trust_boundary::is_link_like(&metadata) || metadata.len() > MAX_RECEIPT_BYTES {
        return Err(NativeJobError::new("invalid-receipt", "Portable export receipt is not a bounded regular file"));
    }
    let mut bytes = Vec::new();
    File::open(path).map_err(|error| NativeJobError::new("export-receipt-unavailable", error.to_string()))?.take(MAX_RECEIPT_BYTES + 1).read_to_end(&mut bytes).map_err(|error| NativeJobError::new("export-receipt-unavailable", error.to_string()))?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES { return Err(NativeJobError::new("invalid-receipt", "Portable export receipt is too large")); }
    let receipt: Receipt = serde_json::from_slice(&bytes).map_err(|error| NativeJobError::new("invalid-receipt", error.to_string()))?;
    if path.file_stem().and_then(|name|name.to_str()) != Some(receipt.job_id.as_str()) { return Err(NativeJobError::new("invalid-receipt", "Portable export receipt filename differs from its job")); }
    receipt.result(root)?;
    Ok(receipt)
}

fn paths(root: &Path) -> Result<Vec<PathBuf>, String> {
    let parent = directory(root, false)?;
    if !parent.exists() { return Ok(Vec::new()); }
    fs::read_dir(parent).map_err(|error|error.to_string())?.filter_map(|entry| match entry {
        Ok(entry) if entry.path().extension().is_some_and(|extension| extension == "json") => Some(Ok(entry.path())),
        Ok(_) => None,
        Err(error) => Some(Err(error.to_string())),
    }).collect()
}

pub(super) fn retained_handoffs(root: &Path) -> HashSet<PathBuf> {
    paths(root).unwrap_or_default().into_iter().filter_map(|path| read(root, &path).ok())
        .filter_map(|receipt| handoff(root, &receipt.handoff_name).ok()?.canonicalize().ok()).collect()
}

pub(super) fn restore(root: &Path, registry: &JobRegistry) -> Result<(), String> {
    let mut errors = Vec::new();
    for path in paths(root)? {
        let receipt = match read(root, &path) {
            Ok(receipt) => receipt,
            Err(error) => {
                if error.code == "invalid-receipt" {
                    if let Err(quarantine_error) = fs::rename(&path, path.with_extension("invalid")) {
                        errors.push(quarantine_error.to_string());
                    } else if let Some(parent) = path.parent() {
                        let _ = crate::trust_boundary::sync_directory(parent);
                    }
                }
                errors.push(error.message);
                continue;
            }
        };
        let restored = (|| {
            if registry.lookup(&receipt.job_id)?.is_some() { return Ok(()); }
            let result = receipt.result(root).map_err(|error| error.message)?;
            let job = registry.create_internal(JobKind::ExportPortableBackup, Some(result.revision), vec![], false)?;
            let temporary_id = job.id();
            job.status.lock().map_err(|error|error.to_string())?.job_id = receipt.job_id.clone();
            {
                let mut jobs = registry.jobs.lock().map_err(|error|error.to_string())?;
                jobs.remove(&temporary_id);
                jobs.insert(receipt.job_id, job.clone());
            }
            job.start(JobPhase::WritingExport)?;
            job.publish_export_capture(result.revision)?;
            job.finish_success(result)
        })();
        if let Err(error) = restored { errors.push(error); }
    }
    if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
}

pub(super) fn forget(root: &Path, id: &str) -> Result<(), String> {
    if Uuid::parse_str(id).is_err() { return Ok(()); }
    let parent = directory(root, false)?;
    match fs::remove_file(parent.join(format!("{id}.json"))) {
        Ok(()) => { let _ = crate::trust_boundary::sync_directory(&parent); Ok(()) }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, NativeFileJobState, Arc<JobControl>, JobResultSummary) {
        let directory = tempfile::tempdir().unwrap();
        let state = NativeFileJobState::initialize(directory.path().to_path_buf());
        let job = state.registry.create_with_context(JobKind::ExportPortableBackup, Some(7), vec!["context-warning".into()]).unwrap();
        job.start(JobPhase::WritingExport).unwrap();
        let source = directory.path().join("handoffs").join(format!("risunest-backup-{}.risunest", Uuid::new_v4()));
        fs::write(&source, b"synthetic export").unwrap();
        let result = JobResultSummary {
            revision: 7, source_bytes: 16, source_sha256: "ab".repeat(32), source_fingerprint_kind: SourceFingerprintKind::WholeFileSha256,
            character_count: 3, preset_count: 2, warning_codes: vec!["source-preserved-repair-required".into()],
            handoff_path: Some(source.to_string_lossy().into_owned()), export_exclusions: None, publication: None,
        };
        (directory, state, job, result)
    }

    #[test]
    fn completed_managed_export_restores_exact_list_and_direct_status_after_process_loss() {
        let (directory, state, job, result) = fixture();
        let id = job.id();
        finish_worker_outcome(&job, JobKind::ExportPortableBackup, Ok(result), vec!["synthetic cleanup failure".into()], Some(directory.path()));
        let expected = state.status(&id).unwrap();
        assert_eq!(expected.state, JobState::Succeeded);
        assert_eq!(expected.result.as_ref().unwrap().warning_codes, vec!["source-preserved-repair-required", "cleanup-failed", "context-warning"]);
        drop(state); drop(job);
        let restarted = NativeFileJobState::initialize(directory.path().to_path_buf());
        let direct = restarted.status(&id).unwrap();
        assert_eq!(direct.kind, JobKind::ExportPortableBackup);
        assert_eq!(direct.state, JobState::Succeeded);
        assert_eq!(direct.result, expected.result);
        assert_eq!(restarted.list().unwrap().iter().map(|job|job.job_id.as_str()).collect::<Vec<_>>(), vec![id.as_str()]);
        assert_eq!(direct.export_capture_revision, Some(7));
    }

    #[test]
    fn receipt_write_failure_never_exposes_a_copy_ready_success() {
        let (directory, state, job, result) = fixture();
        fs::write(directory.path().join(DIRECTORY), b"blocked").unwrap();
        finish_worker_outcome(&job, JobKind::ExportPortableBackup, Ok(result), vec![], Some(directory.path()));
        let status = state.status(&job.id()).unwrap();
        assert_eq!(status.state, JobState::Failed);
        assert_eq!(status.error.unwrap().code, "export-receipt-write-failed");
        assert!(status.result.is_none());
        let unrelated = state.registry.create(JobKind::ExportDataset).unwrap();
        unrelated.start(JobPhase::WritingExport).unwrap();
        unrelated.finish_failure("synthetic-failure", "Synthetic terminal job").unwrap();
        assert!(state.forget(&unrelated.id()).unwrap());
        assert!(!state.forget(&Uuid::new_v4().to_string()).unwrap());
        assert!(state.forget(&job.id()).unwrap());
        assert!(NativeFileJobState::initialize(directory.path().to_path_buf()).list().unwrap().is_empty());
    }

    #[test]
    fn invalid_export_result_is_not_reported_as_a_receipt_write_failure() {
        let (directory, state, job, mut result) = fixture();
        result.source_sha256 = "invalid".into();
        finish_worker_outcome(&job, JobKind::ExportPortableBackup, Ok(result), vec![], Some(directory.path()));
        let status = state.status(&job.id()).unwrap();
        assert_eq!(status.state, JobState::Failed);
        assert_eq!(status.error.unwrap().code, "store-error");
        assert!(status.result.is_none());
        assert!(!directory.path().join(DIRECTORY).exists());
    }

    #[test]
    fn invalid_receipts_are_quarantined_without_repeating_startup_warnings() {
        for corruption in ["json", "identity", "handoff", "digest", "oversize"] {
            let (directory, state, job, result) = fixture();
            finish_worker_outcome(&job, JobKind::ExportPortableBackup, Ok(result), vec![], Some(directory.path()));
            let valid = directory.path().join(DIRECTORY).join(format!("{}.json", job.id()));
            if corruption == "json" {
                let handoffs = directory.path().join("handoffs");
                let held = directory.path().join("held-handoffs");
                fs::rename(&handoffs, &held).unwrap();
                fs::write(&handoffs, b"temporarily unavailable").unwrap();
                assert!(restore(directory.path(), &JobRegistry::default()).is_err());
                assert!(valid.exists());
                assert!(!valid.with_extension("invalid").exists());
                fs::remove_file(&handoffs).unwrap();
                fs::rename(held, handoffs).unwrap();
            }
            let invalid_id = Uuid::new_v4().to_string();
            let invalid = valid.with_file_name(format!("{invalid_id}.json"));
            let mut value: Value = serde_json::from_slice(&fs::read(&valid).unwrap()).unwrap();
            value["jobId"] = Value::String(invalid_id.clone());
            match corruption {
                "identity" => value["version"] = Value::from(2),
                "handoff" => value["handoffName"] = Value::String("../outside.risunest".into()),
                "digest" => value["sourceSha256"] = Value::String("invalid".into()),
                _ => {}
            }
            let bytes = match corruption {
                "json" => b"{broken".to_vec(),
                "oversize" => vec![b' '; MAX_RECEIPT_BYTES as usize + 1],
                _ => serde_json::to_vec(&value).unwrap(),
            };
            fs::write(&invalid, &bytes).unwrap();
            drop(state);
            let restarted = NativeFileJobState::initialize(directory.path().to_path_buf());
            assert_eq!(restarted.list().unwrap().len(), 1, "{corruption}");
            assert_eq!(restarted.status(&job.id()).unwrap().state, JobState::Succeeded);
            assert!(!invalid.exists(), "{corruption}");
            assert_eq!(fs::read(invalid.with_extension("invalid")).unwrap(), bytes);
            drop(restarted);
            let next = NativeFileJobState::initialize(directory.path().to_path_buf());
            assert!(next.startup_warnings.is_empty(), "{corruption}");
            assert_eq!(next.list().unwrap().len(), 1);
        }
    }

    #[test]
    fn cancellation_before_completion_does_not_leave_durable_success() {
        let (directory, state, job, result) = fixture();
        assert_eq!(job.request_cancel().unwrap(), CancelOutcome::Requested);
        finish_worker_outcome(&job, JobKind::ExportPortableBackup, Ok(result), vec![], Some(directory.path()));
        assert_eq!(state.status(&job.id()).unwrap().state, JobState::Cancelled);
        assert!(NativeFileJobState::initialize(directory.path().to_path_buf()).list().unwrap().is_empty());
    }

    #[test]
    fn missing_handoff_after_publication_still_allows_idempotent_result_cleanup() {
        let (directory, state, job, result) = fixture();
        finish_worker_outcome(&job, JobKind::ExportPortableBackup, Ok(result.clone()), vec![], Some(directory.path()));
        fs::remove_file(result.handoff_path.unwrap()).unwrap();
        drop(state);
        let restarted = NativeFileJobState::initialize(directory.path().to_path_buf());
        assert_eq!(restarted.status(&job.id()).unwrap().state, JobState::Succeeded);
        assert!(restarted.forget(&job.id()).unwrap());
        assert!(!restarted.forget(&job.id()).unwrap());
        assert!(NativeFileJobState::initialize(directory.path().to_path_buf()).list().unwrap().is_empty());
    }

    #[test]
    fn failed_receipt_removal_keeps_job_available_for_retry() {
        let (directory, state, job, result) = fixture();
        finish_worker_outcome(&job, JobKind::ExportPortableBackup, Ok(result), vec![], Some(directory.path()));
        let path = directory.path().join(DIRECTORY).join(format!("{}.json", job.id()));
        let saved = path.with_extension("saved");
        fs::rename(&path, &saved).unwrap(); fs::create_dir(&path).unwrap();
        assert_eq!(state.forget(&job.id()).unwrap_err().code, "cleanup-failed");
        assert_eq!(state.status(&job.id()).unwrap().state, JobState::Succeeded);
        fs::remove_dir(&path).unwrap(); fs::rename(saved, path).unwrap();
        assert!(state.forget(&job.id()).unwrap());
        assert!(NativeFileJobState::initialize(directory.path().to_path_buf()).list().unwrap().is_empty());
    }

    #[test]
    fn owned_receipt_retains_handoff_until_forget_but_corrupt_record_does_not() {
        let (directory, state, job, result) = fixture();
        let source = PathBuf::from(result.handoff_path.clone().unwrap());
        finish_worker_outcome(&job, JobKind::ExportPortableBackup, Ok(result), vec![], Some(directory.path()));
        cleanup_stale_handoffs_at(&directory.path().join("handoffs"), SystemTime::now() + Duration::from_secs(2), Duration::ZERO).unwrap();
        assert!(source.exists());
        let receipt = directory.path().join(DIRECTORY).join(format!("{}.json", job.id()));
        let mut value: Value = serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
        value["handoffName"] = Value::String("../outside.risunest".into());
        fs::write(&receipt, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(NativeFileJobState::initialize(directory.path().to_path_buf()).list().unwrap().is_empty());
        cleanup_stale_handoffs_at(&directory.path().join("handoffs"), SystemTime::now() + Duration::from_secs(2), Duration::ZERO).unwrap();
        assert!(!source.exists());
        drop(state);
    }

    #[test]
    fn desktop_destination_does_not_create_a_mobile_handoff_receipt() {
        let (directory, state, job, mut result) = fixture();
        result.handoff_path = None;
        finish_worker_outcome(&job, JobKind::ExportPortableBackup, Ok(result), vec![], Some(directory.path()));
        assert_eq!(state.status(&job.id()).unwrap().state, JobState::Succeeded);
        assert!(!directory.path().join(DIRECTORY).exists());
    }
}
