use super::*;
use crate::server_sync::backups::Side;
use risunest_sync_wire::{CommitIntent, RecordChange, RecordVersion, RemoteHead};

fn commit_records(
    server: &Store,
    device: &risunest_sync_server::store::Device,
    sequence: u64,
    changes: Vec<RecordChange>,
) -> RemoteHead {
    let expected = server.head().unwrap();
    let staged = server.stage_changes(device, &ChangeSet {
        changes, read_fences: vec![], scope_fences: vec![],
    }).unwrap();
    let receipt = server.commit(device, &CommitIntent {
        device_operation_seq: sequence.into(),
        expected_head: expected.clone(),
        changes_digest: staged.changes_digest,
        staged_changes_id: staged.staged_changes_id,
    }, &expected.etag()).unwrap();
    assert_eq!(receipt.head.seq, expected.seq.next().unwrap());
    receipt.head
}

#[test]
fn checkpoint_pages_keep_the_original_head_when_a_later_commit_changes_unread_records() {
    let (fixture, trace) = measured_fixture();
    let (_root, mut local) = prepared();
    fixture.bind(&mut local);
    let config = local.server_config().unwrap().unwrap();
    let device = fixture.server.authenticate(&config.library_id, &config.token).unwrap();
    let mut keys = (0..references::PAGE * 2 + 3).map(|number|
        crate::logical_records::encode_logical_record_key(&crate::logical_records::LogicalRecordLocator::Asset {
            logical_key: format!("assets/checkpoint-{number:04}.png"),
        }).unwrap()).collect::<Vec<_>>();
    keys.sort();
    let before = RecordVersion::Tombstone { deletion_id: "original-deletion".into() };
    let expected = commit_records(&fixture.server, &device, 1, keys.iter().map(|key| RecordChange {
        domain: Domain::Library, key: key.clone(), before: RecordVersion::Absent, after: before.clone(),
    }).collect());
    let client = ServerClient::new(config).unwrap();
    trace.armed.store(true, Ordering::SeqCst);
    let read = references::RemoteRead::begin(&client, &expected).unwrap();
    let mut visited = Vec::new();
    read.visit_pages(|page, total| {
        assert_eq!(total, keys.len() as u64);
        if visited.is_empty() {
            commit_records(&fixture.server, &device, 2, vec![RecordChange {
                domain: Domain::Library, key: keys.last().unwrap().clone(), before: before.clone(),
                after: RecordVersion::Tombstone { deletion_id: "later-deletion".into() },
            }, RecordChange {
                domain: Domain::Library,
                key: crate::logical_records::encode_logical_record_key(&crate::logical_records::LogicalRecordLocator::Asset {
                    logical_key: "assets/created-after-checkpoint.png".into(),
                }).unwrap(),
                before: RecordVersion::Absent, after: before.clone(),
            }]);
        }
        assert!(!page.is_empty() && page.len() <= references::PAGE);
        for record in page {
            assert_eq!(record.version, before, "a checkpoint must not follow the live head between pages");
            visited.push(record.key);
        }
        Ok(())
    }).unwrap();
    assert_eq!(visited, keys);
    assert_ne!(fixture.server.head().unwrap(), expected);
    let requests = trace.requests.lock().unwrap();
    let pages = requests.iter().filter(|request| request.method == "GET").collect::<Vec<_>>();
    assert_eq!(pages.len(), 3);
    assert!(pages.iter().all(|request| request.path.starts_with("/checkpoints/")
        && request.query.as_ref().unwrap().split('&').any(|part| part == "limit=256")));
    assert_eq!(requests.iter().filter(|request| request.method == "POST" && request.path == "/checkpoints").count(), 1);
    drop(requests);
    trace.armed.store(false, Ordering::SeqCst);
    read.release().unwrap();
}

#[test]
fn checkpoint_totals_reject_missing_or_inconsistent_counts_before_visiting_records() {
    let (fixture, trace) = measured_fixture();
    let (_root, mut local) = prepared();
    fixture.bind(&mut local);
    let config = local.server_config().unwrap().unwrap();
    let device = fixture.server.authenticate(&config.library_id, &config.token).unwrap();
    let key = crate::logical_records::encode_logical_record_key(&crate::logical_records::LogicalRecordLocator::Asset {
        logical_key: "assets/counted.png".into(),
    }).unwrap();
    let head = commit_records(&fixture.server, &device, 1, vec![RecordChange {
        domain: Domain::Library, key, before: RecordVersion::Absent,
        after: RecordVersion::Tombstone { deletion_id: "counted-deletion".into() },
    }]);
    let client = ServerClient::new(config).unwrap();
    let read = references::RemoteRead::begin(&client, &head).unwrap();
    trace.armed.store(true, Ordering::SeqCst);
    for total in [Value::Null, json!("0"), json!("2"), json!("18446744073709551616")] {
        *trace.checkpoint_total_override.lock().unwrap() = Some(total);
        let error = read.visit_pages(|_, _| panic!("an inconsistent count must fail before record processing")).unwrap_err();
        assert_eq!(error.code, "invalid-checkpoint-count");
    }
    *trace.checkpoint_total_override.lock().unwrap() = None;
    let mut visited = 0;
    read.visit_pages(|page, total| {
        assert_eq!(total, 1);
        visited += page.len();
        Ok(())
    }).unwrap();
    assert_eq!(visited, 1);
    trace.armed.store(false, Ordering::SeqCst);
    read.release().unwrap();
}

#[test]
fn partial_retention_and_failed_confirmation_keep_roots_and_resume_the_same_capture() {
    let (fixture, trace) = measured_fixture();
    let (root, mut local) = prepared();
    fixture.bind(&mut local);
    let config = local.server_config().unwrap().unwrap();
    let device = fixture.server.authenticate(&config.library_id, &config.token).unwrap();
    let stored = local.server_stored_config().unwrap().unwrap();
    let head = fixture.server.head().unwrap();
    let revision = local.revision().unwrap();
    let generation = active_generation(&local.connection).unwrap();
    let context = Residency::context_id(&stored, &head.epoch);
    let mut capture = references::Capture::begin(root.path(), revision, &generation, &head).unwrap();
    let metadata = capture.metadata(Side::Local, b"synthetic pinned metadata").unwrap();
    let id = capture.id.clone();
    let mut hashes = Vec::new();
    for number in 0..references::PAGE + 1 {
        let bytes = format!("synthetic remote payload {number}").into_bytes();
        let hash = risunest_sync_wire::hash(&bytes);
        fixture.server.put_object(&device, &hash, &bytes).unwrap();
        capture.object(Side::Remote, &references::Object {
            hash: hash.clone(), byte_size: Some(bytes.len() as u64), metadata: false,
            context_id: Some(context.clone()), local_required: false,
        }).unwrap();
        hashes.push(hash);
    }
    hashes.sort();
    capture.complete_side(Side::Local).unwrap();
    capture.complete_side(Side::Remote).unwrap();
    *trace.root.lock().unwrap() = Some(root.path().to_path_buf());
    trace.armed.store(true, Ordering::SeqCst);
    trace.fail_retention_page.store(2, Ordering::SeqCst);
    let client = ServerClient::new(config).unwrap();
    let mut residency = Residency::open(root.path()).unwrap();
    assert!(capture.retain(&client, &stored, &mut residency).is_err());
    drop(capture);
    drop(residency);

    let assert_incomplete = || {
        assert!(references::inspect(root.path(), &id).is_err());
        assert!(!DurableCasJob::open(root.path(), &id).unwrap().is_released());
        let residency = Residency::open(root.path()).unwrap();
        let confirmed = hashes.iter().filter(|hash| residency.object(hash, Some(&context)).unwrap().is_some()).count();
        assert_eq!(confirmed, references::PAGE);
        let mut roots = std::collections::BTreeSet::new();
        references::visit_roots(root.path(), |object| { roots.insert(object.hash); Ok(()) }).unwrap();
        assert!(roots.contains(&metadata));
        assert!(hashes.iter().all(|hash| roots.contains(hash)));
        assert_eq!(fixture.server.head().unwrap(), head);
    };
    assert_incomplete();

    // Fail the local confirmation only after the server has granted the last page.
    let db = rusqlite::Connection::open(Residency::path(root.path())).unwrap();
    db.execute_batch(&format!("CREATE TRIGGER fail_reference_confirmation BEFORE INSERT ON objects
        WHEN NEW.hash='{}' BEGIN SELECT RAISE(ABORT,'synthetic confirmation failure'); END;",
        hashes.last().unwrap())).unwrap();
    trace.fail_retention_page.store(0, Ordering::SeqCst);
    let resumed = references::Capture::resume(root.path(), &id, revision, &generation, &head).unwrap();
    let mut residency = Residency::open(root.path()).unwrap();
    assert!(resumed.retain(&client, &stored, &mut residency).is_err());
    drop(resumed);
    drop(residency);
    assert_incomplete();
    db.execute_batch("DROP TRIGGER fail_reference_confirmation").unwrap();
    drop(db);

    let resumed = references::Capture::resume(root.path(), &id, revision, &generation, &head).unwrap();
    let mut residency = Residency::open(root.path()).unwrap();
    resumed.retain(&client, &stored, &mut residency).unwrap();
    let receipt = resumed.finish(&mut local, &|| client.ensure_active()).unwrap();
    assert_eq!(receipt.id, id);
    assert_eq!(local.revision().unwrap(), revision);
    assert_eq!(fixture.server.head().unwrap(), head);
    let requests = trace.requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    for request in requests.iter() {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/objects/retention");
        assert!(request.body["objects"].as_array().unwrap().len() <= references::PAGE);
    }
    assert!(hashes.iter().all(|hash| PayloadCas::new(root.path()).unwrap().stat_object(hash).unwrap().is_none()));
    assert!(!root.path().join("server-sync/backups").join(&id).join("local.risunest").exists());
    assert!(!root.path().join("server-sync/backups").join(&id).join("remote.risunest").exists());
}

#[test]
fn remote_policy_receive_requests_custody_once_per_page() {
    let (fixture, trace) = measured_fixture();
    let (_first_root, mut first) = prepared();
    fixture.bind(&mut first);
    assert_eq!(settle(&mut first).phase, "idle");
    let mut hashes = std::collections::BTreeSet::new();
    for number in 0..40 {
        let alias = put(&mut first, &format!("assets/received-{number:02}.png"),
            format!("synthetic received asset body {number:04}").as_bytes());
        hashes.insert(alias.object_hash.unwrap());
    }
    // Two records naming one body must not name it twice in one request.
    let shared = put(&mut first, "assets/received-shared.png", b"synthetic received asset body 0000");
    assert!(hashes.contains(shared.object_hash.as_ref().unwrap()));
    assert_eq!(settle(&mut first).phase, "idle");

    let directory = tempfile::tempdir().unwrap();
    let mut second = PersistentStore::open(directory.path()).unwrap();
    fixture.bind(&mut second);
    second.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    *trace.root.lock().unwrap() = Some(directory.path().to_path_buf());
    trace.armed.store(true, Ordering::SeqCst);
    let counter = Arc::new(crate::persistent_store::server_sync_engine::CycleItemCounter::default());
    let crate::persistent_store::server_sync_engine::Preparation::Ready(mut ready) = second
        .server_prepare_cycle(&CycleOptions { cycle_items: Some(counter.clone()), ..Default::default() })
        .unwrap() else { panic!("an empty replica receives without a conflict") };
    assert_eq!(counter.processed.load(Ordering::Relaxed), counter.expected.load(Ordering::Relaxed));
    second.server_activate_cycle(&mut ready).unwrap();
    assert_eq!(second.server_publish_cycle(&ready).unwrap().phase, "idle");
    trace.armed.store(false, Ordering::SeqCst);

    let requests = trace.requests.lock().unwrap();
    let retention = requests.iter()
        .filter(|request| request.method == "POST" && request.path == "/objects/retention")
        .collect::<Vec<_>>();
    assert_eq!(retention.len(), 1, "custody is requested per page, not per record");
    let requested = retention[0].body["objects"].as_array().unwrap().iter()
        .map(|object| object["hash"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(requested.len(), requested.iter().collect::<std::collections::BTreeSet<_>>().len());
    assert!(hashes.iter().all(|hash| requested.contains(hash)));
    for request in requests.iter().filter(|request| request.path == "/objects/transfer") {
        for target in request.body.as_array().unwrap() {
            assert!(!hashes.contains(target["target"].as_str().unwrap()),
                "a remote replica must not download asset bodies to receive them");
        }
    }
    drop(requests);
    let residency = Residency::open(directory.path()).unwrap();
    let cas = PayloadCas::new(directory.path()).unwrap();
    for hash in &hashes {
        assert!(residency.object(hash, None).unwrap().is_some());
        assert!(cas.stat_object(hash).unwrap().is_none());
    }
    let generation = active_generation(&second.connection).unwrap();
    let aliases: i64 = second.connection.query_row(
        "SELECT count(*) FROM asset_aliases WHERE generation=?1 AND logical_key GLOB 'assets/received-*'",
        [&generation], |r| r.get(0)).unwrap();
    assert_eq!(aliases, 41);
}

#[test]
fn releasing_unused_custody_contacts_no_server_while_the_library_uses_every_object() {
    let (fixture, trace) = measured_fixture();
    let (_first_root, mut first) = prepared();
    fixture.bind(&mut first);
    assert_eq!(settle(&mut first).phase, "idle");
    let mut hashes = std::collections::BTreeSet::new();
    for number in 0..40 {
        let alias = put(&mut first, &format!("assets/kept-{number:02}.png"),
            format!("synthetic kept asset body {number:04}").as_bytes());
        hashes.insert(alias.object_hash.unwrap());
    }
    assert_eq!(settle(&mut first).phase, "idle");

    let directory = tempfile::tempdir().unwrap();
    let mut second = PersistentStore::open(directory.path()).unwrap();
    fixture.bind(&mut second);
    second.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    assert_eq!(settle(&mut second).phase, "idle");
    let residency = Residency::open(directory.path()).unwrap();
    assert!(hashes.iter().all(|hash| residency.object(hash, None).unwrap().is_some()));

    trace.armed.store(true, Ordering::SeqCst);
    second.asset_residency_release_unused(|| Ok(())).unwrap();
    trace.armed.store(false, Ordering::SeqCst);
    assert!(trace.requests.lock().unwrap().is_empty(),
        "custody the library still uses is kept without asking the server");
    assert!(hashes.iter().all(|hash| residency.object(hash, None).unwrap().is_some()));
}
