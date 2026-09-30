use super::*;
use crate::asset_repository::job_pins::{CasJobKind, CasObjectRole, DurableCasJob};
use crate::server_sync::backups::{references::Capture, Side};

fn inventory(store: &PersistentStore, guarded: bool) -> (std::collections::BTreeSet<String>, std::collections::BTreeSet<String>, bool) {
    let _guard = guarded.then(|| crate::asset_repository::coordinator::lock_repository_mutation().unwrap());
    store.residency_inventory_classification_test(guarded).unwrap()
}

fn external_conflict(store: &PersistentStore, hash: &str, size: u64) {
    use crate::external_storage::capture::CaptureCatalog;
    use crate::persistent_store::{content_capture::ContentCaptureSink,
        external_conflicts::{preserve_local_conflict, ExternalConflictRecord, PreservedHeadObservation, PreservedRemoteState},
        sync_selection::CaptureIdentity};
    use risunest_external_storage_format::snapshot::{envelope_length, ObjectRole, PublicObjectHeader, StoredObject, WireLocator};
    let external = store.repository_root().join("external-storage");
    let mut catalog = CaptureCatalog::create(&external.join("captures/synthetic-classification"), &external, None).unwrap();
    catalog.begin(&CaptureIdentity { store_id: "store".into(), library_epoch: "library".into(),
        generation: "generation".into(), selection_epoch: "selection".into(), revision: 1 }, None).unwrap();
    catalog.record("synthetic-record", b"synthetic capture body").unwrap();
    catalog.reference("synthetic-record", hash, size).unwrap();
    catalog.finish().unwrap();
    let local = catalog.durable_reference("synthetic-classification", store.repository_root()).unwrap();
    let header = PublicObjectHeader::new("repository".into(), "snapshot-remote".into(), ObjectRole::SyncState, 1).unwrap();
    preserve_local_conflict(store.device_store().unwrap().connection(), &ExternalConflictRecord {
        id: "synthetic-classification-conflict".into(), created_at_ms: 1, connection_id: "connection".into(),
        repository_id: "repository".into(), local,
        remote: PreservedRemoteState {
            snapshot: StoredObject { ciphertext_length: envelope_length(&header).unwrap(), header,
                locator: WireLocator { connection_identity: "synthetic/root".into(), collection: None, object: "snapshot-remote".into() },
                ciphertext_sha256: [2; 32], plaintext_length: 1, plaintext_sha256: [1; 32] },
            logical_revision: 8, commit_id: "remote-commit".into(),
            head: PreservedHeadObservation { commit_id: "remote-commit".into(), authenticated_body_hash: "02".repeat(32) },
        }, remote_point: None, resolved: false,
    }).unwrap();
}

#[test]
fn real_store_classification_preserves_exact_live_plugin_backup_conflict_and_job_roots() {
    for completed in [false, true] {
        let (_directory, mut store) = prepared();
        let root = store.repository_root().to_owned();
        let cas = PayloadCas::new(&root).unwrap();
        let live = put(&mut store, "assets/classification-live.png", b"synthetic live").object_hash.unwrap();
        let plugin = cas.prepare_bytes(b"synthetic plugin reference").unwrap().content_hash;
        store.commit(&WorkingSetCommit {
            plugin_storage: Some(vec![PluginStorageMutation::Set { owner: "synthetic-classification".into(), key: "payload".into(),
                value: json!(crate::asset_repository::object_physical_key(&plugin)) }]),
            ..empty_working_set_commit(store.revision().unwrap())
        }).unwrap();
        store.snapshot_create("synthetic-classification").unwrap();
        let mut job = DurableCasJob::begin(&root, "synthetic-classification-job", CasJobKind::OfficialPublicationOrExportPreparation, 1).unwrap();
        let pinned = job.prepare_bytes(&cas, b"synthetic job-only payload", CasObjectRole::DirectObject).unwrap().content_hash;
        job.seal(&mut store, 1).unwrap();
        let conflict = cas.prepare_bytes(b"synthetic external conflict payload").unwrap();
        external_conflict(&store, &conflict.content_hash, conflict.byte_size);
        let baseline = inventory(&store, false);
        assert_eq!(baseline, inventory(&store, true));
        assert!(baseline.2, "opaque plugin storage must retain its release blocker");
        for hash in [&live, &plugin, &pinned, &conflict.content_hash] {
            assert!(baseline.0.contains(hash), "missing actual producer {hash}");
        }
        for hash in [&pinned, &conflict.content_hash] {
            assert!(baseline.1.contains(hash), "missing local requirement {hash}");
        }
        let remote_body = b"synthetic remote-only backup payload";
        let remote = risunest_sync_wire::hash(remote_body);
        crate::server_sync::residency::test_remote::hold(&root, &[(&remote, remote_body.len() as u64)]);
        let context = Residency::open(&root).unwrap().object(&remote, None).unwrap().unwrap().context;
        let head = risunest_sync_wire::RemoteHead::genesis("library".into(), "epoch".into()).unwrap();
        let generation = active_generation(&store.connection).unwrap();
        let mut capture = Capture::begin(&root, store.revision().unwrap(), &generation, &head).unwrap();
        let metadata = capture.metadata(Side::Local, b"synthetic backup metadata").unwrap();
        let required = cas.prepare_bytes(b"synthetic local-required backup payload").unwrap();
        capture.local_payload(&required.content_hash, required.byte_size, &|| Ok(())).unwrap();
        capture.object(Side::Remote, &crate::server_sync::backups::references::Object {
            hash: remote.clone(), byte_size: Some(remote_body.len() as u64), metadata: false,
            context_id: Some(context), local_required: false,
        }).unwrap();
        if completed {
            capture.complete_side(Side::Local).unwrap();
            capture.complete_side(Side::Remote).unwrap();
            capture.finish(&mut store, &|| Ok(())).unwrap();
        } else {
            drop(capture);
        }
        for guarded in [false, true] {
            let all_backup = std::collections::BTreeSet::from([metadata.clone(), required.content_hash.clone(), remote.clone()]);
            let local_backup = std::collections::BTreeSet::from([metadata.clone(), required.content_hash.clone()]);
            {
                let _guard = guarded.then(|| crate::asset_repository::coordinator::lock_repository_mutation().unwrap());
                assert_eq!(store.residency_backup_classification_test(guarded).unwrap(), (all_backup.clone(), local_backup.clone()));
            }
            if completed {
                let mut expected = baseline.clone();
                expected.0.extend(all_backup);
                expected.1.extend(local_backup);
                assert_eq!(inventory(&store, guarded), expected);
            } else {
                let _guard = guarded.then(|| crate::asset_repository::coordinator::lock_repository_mutation().unwrap());
                let error = store.residency_inventory_classification_test(guarded).err().unwrap();
                assert_eq!((error.code.as_str(), error.status, error.retryable), ("asset-jobs-unresolved", 409, false));
            }
        }
    }
}

#[test]
fn real_store_inventory_errors_remain_exact_for_corrupt_backup_and_job_roots() {
    for damaged in ["backup", "job"] {
        let (_directory, store) = prepared();
        let root = store.repository_root().to_owned();
        if damaged == "backup" {
            let head = risunest_sync_wire::RemoteHead::genesis("library".into(), "epoch".into()).unwrap();
            let capture = Capture::begin(&root, store.revision().unwrap(), &active_generation(&store.connection).unwrap(), &head).unwrap();
            let id = capture.id.clone();
            drop(capture);
            rusqlite::Connection::open(root.join("server-sync/backups").join(id).join("index.sqlite")).unwrap()
                .execute_batch("DROP TABLE objects").unwrap();
        } else {
            let job = DurableCasJob::begin(&root, "synthetic-broken-job", CasJobKind::OfficialPublicationOrExportPreparation, 1).unwrap();
            drop(job);
            std::fs::write(root.join("assets/job-pins/job-synthetic-broken-job.journal"), b"synthetic corruption").unwrap();
        }
        for guarded in [false, true] {
            let _guard = guarded.then(|| crate::asset_repository::coordinator::lock_repository_mutation().unwrap());
            let error = store.residency_inventory_classification_test(guarded).err().unwrap();
            let expected = if damaged == "backup" { "local-validation" } else { "asset-jobs-unresolved" };
            assert_eq!((error.code.as_str(), error.status, error.retryable), (expected, 409, false));
        }
    }
}
