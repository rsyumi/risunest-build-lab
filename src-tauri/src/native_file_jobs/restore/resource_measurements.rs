#[test]
#[ignore = "explicit synthetic encrypted database, attachment and record resource measurements"]
fn native_backup_resource_measurement() {
    use std::sync::atomic::{AtomicBool, AtomicU64};
    let manifest_path = PathBuf::from(std::env::var_os("RISUNEST_RESTORE_RESOURCE_MANIFEST").expect("synthetic fixture manifest"));
    let report_path = PathBuf::from(std::env::var_os("RISUNEST_RESTORE_RESOURCE_REPORT").expect("measurement report path"));
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["schema"], "risunest.synthetic-restore-resources/v1");
    assert_eq!(manifest["synthetic"], true);
    assert_eq!(manifest["source"], "source.bin");
    let source = manifest_path.parent().unwrap().join("source.bin");
    let mut source_file = File::open(&source).unwrap();
    let mut source_hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = source_file.read(&mut buffer).unwrap();
        if count == 0 { break; }
        source_hash.update(&buffer[..count]);
    }
    assert_eq!(hex::encode(source_hash.finalize()), manifest["sourceSha256"]);
    assert_eq!(fs::metadata(&source).unwrap().len(), manifest["sourceBytes"].as_u64().unwrap());
    let (directory, mut sink) = fixture();
    let job = JobRegistry::default().create(JobKind::RestoreLegacyLocalBackup).unwrap();
    fn disk_bytes(path: &Path) -> u64 {
        fs::read_dir(path).into_iter().flatten().filter_map(Result::ok).map(|entry| {
            let Ok(metadata) = entry.metadata() else { return 0; };
            if metadata.is_dir() { disk_bytes(&entry.path()) } else { metadata.len() }
        }).sum()
    }
    let disk_before = disk_bytes(directory.path());
    let disk_peak = AtomicU64::new(disk_before);
    let staging_peak = AtomicU64::new(0);
    let database_peak = AtomicU64::new(0);
    let sampling = AtomicBool::new(true);
    struct StopSampling<'a>(&'a AtomicBool);
    impl Drop for StopSampling<'_> {
        fn drop(&mut self) { self.0.store(false, Ordering::Release); }
    }
    let started = Instant::now();
    let compiled_sources = [
        ("src-tauri/src/local_backup.rs", include_bytes!("../../local_backup.rs").as_slice()),
        ("src-tauri/src/native_file_jobs/legacy_backup.rs", include_bytes!("../legacy_backup.rs").as_slice()),
        ("src-tauri/src/native_file_jobs/legacy_backup/account_encryption.rs", include_bytes!("../legacy_backup/account_encryption.rs").as_slice()),
        ("src-tauri/src/native_file_jobs/restore.rs", include_bytes!("../restore.rs").as_slice()),
        ("src-tauri/src/native_file_jobs/restore/historical.rs", include_bytes!("historical.rs").as_slice()),
        ("src-tauri/src/native_file_jobs/restore/resource_measurements.rs", include_bytes!("resource_measurements.rs").as_slice()),
        ("src-tauri/src/asset_repository/import_payloads.rs", include_bytes!("../../asset_repository/import_payloads.rs").as_slice()),
        ("src-tauri/src/asset_repository/payload_cas.rs", include_bytes!("../../asset_repository/payload_cas.rs").as_slice()),
        ("src-tauri/src/test_memory.rs", include_bytes!("../../test_memory.rs").as_slice()),
    ].map(|(path, bytes)| (path.to_owned(), Value::String(hex::encode(Sha256::digest(bytes)))))
        .into_iter().collect::<serde_json::Map<String, Value>>();
    let measurement = thread::scope(|scope| {
        scope.spawn(|| {
            while sampling.load(Ordering::Acquire) {
                disk_peak.fetch_max(disk_bytes(directory.path()), Ordering::AcqRel);
                staging_peak.fetch_max(disk_bytes(&directory.path().join("native-file-jobs")) + disk_bytes(&directory.path().join("assets/staging")), Ordering::AcqRel);
                database_peak.fetch_max(disk_bytes(&directory.path().join("persistent")), Ordering::AcqRel);
                thread::sleep(Duration::from_millis(25));
            }
        });
        let _stop = StopSampling(&sampling);
        let value = crate::test_memory::measure_working_set(|| {
            restore_container_payloads_with_key(&source, directory.path(), &mut sink, &job,
                RestoreLimits::default(), manifest["key"].as_str())
        });
        disk_peak.fetch_max(disk_bytes(directory.path()), Ordering::AcqRel);
        sampling.store(false, Ordering::Release);
        value
    });
    let restore_elapsed = started.elapsed().as_millis();
    let (result, payloads) = measurement.value.unwrap();
    fs::remove_dir_all(directory.path().join("native-file-jobs").join("jobs").join(job.id())).unwrap();
    assert_eq!(result.revision, 2);
    let expected = manifest["expected"].as_array().unwrap();
    assert_eq!(result.character_count, expected.len() as u64);
    let store = sink.store.lock().unwrap();
    assert_eq!(store.read_root(None).unwrap().value["username"], "Synthetic memory fixture");
    for character in expected {
        let count = character["messages"].as_u64().unwrap();
        let mut projection = Sha256::new();
        projection.update(b"[");
        let mut seen = 0;
        while seen < count {
            let query = serde_json::from_value(json!({"characterId":character["characterId"],"conversationId":character["conversationId"],"startIndex":seen,"limit":128})).unwrap();
            let window = store.read_conversation_window(&query, None).unwrap().unwrap().value;
            assert_eq!(window.total_messages, count as i64);
            assert!(!window.messages.is_empty());
            for message in window.messages {
                if seen != 0 { projection.update(b","); }
                projection.update(serde_json::to_vec(&json!([message["role"],message["chatId"],message["data"]])).unwrap());
                seen += 1;
            }
        }
        projection.update(b"]");
        assert_eq!(seen, count);
        assert_eq!(hex::encode(projection.finalize()), character["projectionSha256"]);
    }
    if let Some(attachment) = manifest.get("attachment") {
        let aliases = payloads["aliases"].as_array().unwrap();
        assert_eq!(aliases.len(), 1);
        let alias = &aliases[0];
        assert_eq!(alias["objectHash"], attachment["sha256"]);
        assert_eq!(alias["size"], attachment["bytes"]);
        let cas = crate::asset_repository::PayloadCas::new(directory.path()).unwrap();
        let hash = attachment["sha256"].as_str().unwrap();
        let mut body = cas.open_object_tracked(hash).unwrap().unwrap();
        let mut digest = Sha256::new();
        loop {
            let count = body.read(&mut buffer).unwrap();
            if count == 0 { break; }
            digest.update(&buffer[..count]);
        }
        assert_eq!(hex::encode(digest.finalize()), hash);
    }
    fs::write(report_path, serde_json::to_vec_pretty(&json!({
        "schema":"risunest.synthetic-restore-resources-result/v1", "synthetic":true,
        "compiledSourceHashes":compiled_sources,
        "axis":manifest["axis"],"size":manifest["size"],"sourceBytes":manifest["sourceBytes"],
        "databaseBytes":manifest["databaseBytes"],"sourceSha256":manifest["sourceSha256"],
        "baselineWorkingSetBytes":measurement.baseline_working_set_bytes,
        "peakWorkingSetBytes":measurement.peak_working_set_bytes,
        "retainedWorkingSetBytes":measurement.retained_working_set_bytes,
        "diskBaselineBytes":disk_before,"sampledPeakOwnedDiskBytes":disk_peak.load(Ordering::Acquire),
        "sampledPeakStagingBytes":staging_peak.load(Ordering::Acquire),
        "sampledPeakDatabaseBytes":database_peak.load(Ordering::Acquire),
        "diskAfterBytes":disk_bytes(directory.path()),"elapsedMs":restore_elapsed,
        "characterCount":result.character_count,"verified":true,
    })).unwrap()).unwrap();
}
