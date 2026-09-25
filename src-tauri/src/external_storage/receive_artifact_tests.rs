//! A job's downloaded bodies leave the disk once nothing that owns the job can
//! read them again, and not before.
use super::{
    activate_prepared_receive, completed_receive_result,
    receive_tests::{bind, fixture, local_edit, prepare},
};
use crate::external_storage::{
    content_store::{ContentStore, ObjectSource},
    job_store::{DurableJob, JobCommandState, JobStore, ReceiveArtifacts},
    receive_artifacts::{self, Owners, SettlementPass},
    runtime::job_directory,
    snapshot_restore::PreparedRemoteSnapshot,
};
use crate::persistent_store::{
    external_conflicts::{
        self, ExternalConflictRecord, PreservedHeadObservation, PreservedRemoteState,
    },
    PersistentStore,
};
use risunest_external_storage_format::{
    content_identity::hash,
    snapshot::{envelope_length, ObjectRole, PublicObjectHeader, StoredObject, WireLocator},
};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

type Admission = Arc<crate::native_file_jobs::admission::Admission>;

fn receive_directory(root: &Path, job: &str) -> PathBuf {
    job_directory(root, "connection", job).join("receive")
}

/// Writes a job's receive the way a download leaves it: the record bodies the
/// preparation reads, a pack, and a content store with its SQLite files.
fn receive_input(
    root: &Path,
    job: &DurableJob,
    template: &PreparedRemoteSnapshot,
    snapshot: &str,
) -> PreparedRemoteSnapshot {
    let staging = receive_directory(root, &job.id);
    fs::create_dir_all(&staging).unwrap();
    let mut input = template.clone();
    for record in &mut input.records {
        let ObjectSource::File(source) = &record.source else {
            panic!("the template stages its records as files");
        };
        let path = staging.join(source.file_name().unwrap());
        fs::copy(source, &path).unwrap();
        record.source = ObjectSource::File(path);
    }
    fs::write(staging.join("pack-synthetic"), vec![7u8; 4096]).unwrap();
    let body = format!("synthetic body of {}", job.id).into_bytes();
    let mut content = ContentStore::open(&staging.join("external-storage")).unwrap();
    content.put(&hex::encode(hash(&body)), &body).unwrap();
    content.commit().unwrap();
    drop(content);
    input.snapshot_id = snapshot.into();
    input.staging_root = staging;
    input
}

fn held(root: &Path, jobs: &JobStore, job: &DurableJob) {
    jobs.put(job).unwrap();
    receive_artifacts::hold(root, &job.id).unwrap();
}

fn settle(jobs: &JobStore, id: &str, state: &str) {
    let mut job = jobs.read(id).unwrap();
    job.summary["state"] = json!(state);
    jobs.put(&job).unwrap();
}

fn marked(root: &Path, id: &str) -> bool {
    JobStore::open(root).unwrap().read(id).unwrap().receive_artifacts
        == Some(ReceiveArtifacts::Held)
}

fn set_phase(root: &Path, id: &str, phase: &str) {
    rusqlite::Connection::open(root.join("persistent").join(crate::persistent_store::DATABASE_FILE))
        .unwrap()
        .execute("UPDATE external_storage_jobs SET phase=?2 WHERE id=?1", [id, phase])
        .unwrap();
}

fn stored(object_id: &str, role: ObjectRole) -> StoredObject {
    let header = PublicObjectHeader::new("repository".into(), object_id.into(), role, 1).unwrap();
    StoredObject {
        ciphertext_length: envelope_length(&header).unwrap(),
        header,
        locator: WireLocator {
            connection_identity: "account/root".into(),
            collection: None,
            object: object_id.into(),
        },
        ciphertext_sha256: [2; 32],
        plaintext_length: 1,
        plaintext_sha256: [1; 32],
    }
}

fn preserve_conflict(store: &PersistentStore, job: &DurableJob) {
    let record = ExternalConflictRecord {
        id: job.id.clone(),
        created_at_ms: 1,
        connection_id: "connection".into(),
        repository_id: "repository".into(),
        local: crate::external_storage::capture::DurableCaptureReference {
            capture_id: format!("capture-{}", job.id),
            identity: job.admission_identity.clone(),
            catalog_path: format!("captures/{}/capture.sqlite", job.id),
            catalog_hash: "01".repeat(32),
        },
        remote: PreservedRemoteState {
            snapshot: stored("snapshot-remote", ObjectRole::SyncState),
            logical_revision: 8,
            commit_id: "remote-commit".into(),
            head: PreservedHeadObservation {
                commit_id: "remote-commit".into(),
                authenticated_body_hash: "02".repeat(32),
            },
        },
        remote_point: None,
        resolved: false,
    };
    external_conflicts::preserve_local_conflict(store.device_store().unwrap().connection(), &record)
        .unwrap();
}

/// One pass as a settling worker runs it: moved aside under a claim on the
/// connection, then deleted.
fn pass(
    root: &Path,
    store: &PersistentStore,
    state: &JobCommandState,
    admission: &Admission,
    claimed: &DurableJob,
) -> Vec<String> {
    let (_, claim) = state.claim(claimed).unwrap();
    let detached = receive_artifacts::detach_connection(
        &Owners { root, state, store, admission, repository_id: "repository" },
        &claim,
        "connection",
    )
    .unwrap();
    drop(claim);
    receive_artifacts::remove_detached(root, &detached);
    detached
}

#[cfg(unix)]
fn link_directory(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("create synthetic directory link");
}

#[cfg(windows)]
fn link_directory(target: &Path, link: &Path) {
    let output = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .expect("create synthetic directory junction");
    assert!(output.status.success(), "create junction: {output:?}");
}

fn remove_link(link: &Path) {
    #[cfg(windows)]
    fs::remove_dir(link).unwrap();
    #[cfg(unix)]
    fs::remove_file(link).unwrap();
}

#[test]
fn repeated_receives_reclaim_their_bodies_and_keep_the_library_and_other_owners() {
    let (directory, mut store, first, template) = fixture();
    let root = directory.path();
    let jobs = JobStore::open(root).unwrap();
    let state = JobCommandState::default();
    let admission = Admission::default();
    let shared = root.join("external-storage").join("objects").join("shared-sentinel");
    fs::create_dir_all(shared.parent().unwrap()).unwrap();
    fs::write(&shared, b"shared capture content").unwrap();

    held(root, &jobs, &first);
    let prepared = prepare(&mut store, &first, receive_input(root, &first, &template, "snapshot"));
    activate_prepared_receive(&mut store, &prepared, &first.admission_identity).unwrap();
    settle(&jobs, &first.id, "succeeded");

    // A preserved conflict whose job reads cancelled, and a publication whose
    // outcome is unknown while its job reads failed. Neither label releases them.
    let conflict = bind(&mut store, "conflict-snapshot");
    held(root, &jobs, &conflict);
    receive_input(root, &conflict, &template, "conflict-snapshot");
    store.external_cancel_prepared(&conflict.id).unwrap();
    preserve_conflict(&store, &conflict);
    settle(&jobs, &conflict.id, "cancelled");
    let unknown = bind(&mut store, "unknown-snapshot");
    held(root, &jobs, &unknown);
    receive_input(root, &unknown, &template, "unknown-snapshot");
    set_phase(root, &unknown.id, "publicationUnknown");
    settle(&jobs, &unknown.id, "failed");

    let mut completed = vec![(first.clone(), "snapshot".to_owned(), 0i64)];
    for cycle in 0..3 {
        let revision = store.revision().unwrap();
        local_edit(&mut store, revision);
        let snapshot = format!("snapshot-{cycle}");
        let job = bind(&mut store, &snapshot);
        held(root, &jobs, &job);
        let input = receive_input(root, &job, &template, &snapshot);
        let prepared = prepare(&mut store, &job, input);
        let expected = store.revision().unwrap();
        assert_eq!(activate_prepared_receive(&mut store, &prepared, &job.admission_identity).unwrap(),
            expected + 1);
        settle(&jobs, &job.id, "succeeded");
        completed.push((job.clone(), snapshot.clone(), expected));

        let library = store.materialize(None).unwrap();
        let identity = store.external_identity().unwrap();
        let base = serde_json::to_value(store.external_base("connection").unwrap()).unwrap();
        let mut reclaimed = pass(root, &store, &state, &admission, &job);
        reclaimed.sort();
        let mut expected_reclaimed = vec![job.id.clone()];
        if cycle == 0 {
            expected_reclaimed.push(first.id.clone());
        }
        expected_reclaimed.sort();
        assert_eq!(reclaimed, expected_reclaimed);

        assert_eq!(store.materialize(None).unwrap(), library);
        assert_eq!(store.external_identity().unwrap(), identity);
        assert_eq!(serde_json::to_value(store.external_base("connection").unwrap()).unwrap(), base);
        assert_eq!(base["snapshot_id"], snapshot.as_str());
        for (done, snapshot, expected) in &completed {
            assert!(!receive_directory(root, &done.id).exists());
            assert!(!marked(root, &done.id));
            let result = completed_receive_result(&store, done, *expected).unwrap().unwrap();
            assert_eq!(result["snapshotId"], snapshot.as_str());
            assert_eq!(result["receivedRevision"], (expected + 1).to_string());
        }
        for owner in [&conflict, &unknown] {
            assert!(receive_directory(root, &owner.id).join("pack-synthetic").is_file());
            assert!(receive_directory(root, &owner.id)
                .join("external-storage")
                .join("content.sqlite")
                .is_file());
            assert!(marked(root, &owner.id));
        }
        assert_eq!(fs::read(&shared).unwrap(), b"shared capture content");
    }
    // A job that kept nothing else in its directory loses the directory too.
    assert!(!job_directory(root, "connection", &completed[1].0.id).exists());

    // A confirmed remote point makes the conflict exportable; it still owns them.
    external_conflicts::confirm_external_conflict_point(
        store.device_store().unwrap().connection(),
        &conflict.id,
        &stored(&format!("backup-point-{}", conflict.id), ObjectRole::BackupPoint),
    )
    .unwrap();
    assert!(pass(root, &store, &state, &admission, &first).is_empty());
    assert!(receive_directory(root, &conflict.id).join("pack-synthetic").is_file());
    // Once the conflict is settled its bodies go too; the unknown outcome stays.
    external_conflicts::mark_external_conflict_resolved(
        store.device_store().unwrap().connection(),
        &conflict.id,
    )
    .unwrap();
    assert_eq!(pass(root, &store, &state, &admission, &first), vec![conflict.id.clone()]);
    assert!(!receive_directory(root, &conflict.id).exists());
    assert!(receive_directory(root, &unknown.id).is_dir());
}

#[test]
fn interrupted_deletions_still_use_a_bounded_cleanup_page() {
    let (directory, mut store, claim_job, _template) = fixture();
    store.external_cancel_prepared(&claim_job.id).unwrap();
    let root = directory.path();
    let jobs = JobStore::open(root).unwrap();
    let state = JobCommandState::default();
    let admission = Admission::default();
    let mut queued = Vec::new();
    for index in 0..12 {
        let job = bind(&mut store, &format!("interrupted-{index}"));
        held(root, &jobs, &job);
        store.external_cancel_prepared(&job.id).unwrap();
        settle(&jobs, &job.id, "cancelled");
        let moved = job_directory(root, "connection", &job.id).join(".reclaim-receive");
        fs::create_dir_all(&moved).unwrap();
        fs::write(moved.join("body"), b"synthetic interrupted deletion").unwrap();
        queued.push(job.id);
    }
    let first = pass(root, &store, &state, &admission, &claim_job);
    assert_eq!(first.len(), 8, "already detached jobs must count toward the pass limit");
    let restarted = JobCommandState::default();
    let second = pass(root, &store, &restarted, &admission, &claim_job);
    assert_eq!(second.len(), 4);
    for id in queued {
        assert!(!marked(root, &id));
        assert!(!job_directory(root, "connection", &id).exists());
    }
}

#[test]
fn a_restarted_application_reclaims_every_settled_job_without_a_later_settlement() {
    let (directory, mut store, open, _template) = fixture();
    let root = directory.path();
    let jobs = JobStore::open(root).unwrap();
    let state = JobCommandState::default();
    let admission = Admission::default();
    // An open job may still read its bodies, and its claim is not one to borrow.
    assert!(!open.terminal());
    store.external_cancel_prepared(&open.id).unwrap();
    held(root, &jobs, &open);
    fs::create_dir_all(receive_directory(root, &open.id)).unwrap();
    assert!(receive_artifacts::settled_holder(root, "connection").unwrap().is_none());
    // Settled for now only so the later jobs can be recorded beside it.
    let mut paused = jobs.read(&open.id).unwrap();
    paused.summary["state"] = json!("cancelled");
    jobs.put(&paused).unwrap();
    let unknown = bind(&mut store, "unknown-snapshot");
    held(root, &jobs, &unknown);
    fs::create_dir_all(receive_directory(root, &unknown.id)).unwrap();
    set_phase(root, &unknown.id, "publicationUnknown");
    settle(&jobs, &unknown.id, "failed");
    let mut settled = Vec::new();
    for index in 0..12 {
        let job = bind(&mut store, &format!("settled-{index}"));
        held(root, &jobs, &job);
        store.external_cancel_prepared(&job.id).unwrap();
        settle(&jobs, &job.id, "cancelled");
        fs::create_dir_all(receive_directory(root, &job.id)).unwrap();
        fs::write(receive_directory(root, &job.id).join("body"), b"synthetic body").unwrap();
        settled.push(job.id);
    }
    paused.summary["state"] = open.summary["state"].clone();
    jobs.put(&paused).unwrap();
    // An earlier pass stopped partway through the connection's jobs.
    let (cursor, _) = jobs
        .list_holding_receive_artifacts("connection", 0, 5)
        .unwrap()
        .pop()
        .unwrap();
    jobs.set_receive_cleanup_after("connection", cursor).unwrap();

    let holder = receive_artifacts::settled_holder(root, "connection").unwrap().unwrap();
    assert_eq!(holder.id, unknown.id);
    let (_, claim) = state.claim(&holder).unwrap();
    assert!(state.claim(&open).is_err());
    let mut detached = receive_artifacts::detach_every_page(
        &Owners { root, state: &state, store: &store, admission: &admission, repository_id: "repository" },
        &claim,
        "connection",
    )
    .unwrap();
    drop(claim);
    receive_artifacts::remove_detached(root, &detached);

    detached.sort();
    settled.sort();
    assert_eq!(detached, settled);
    for id in &settled {
        assert!(!marked(root, id));
        assert!(!job_directory(root, "connection", id).exists());
    }
    for owner in [&open.id, &unknown.id] {
        assert!(marked(root, owner));
        assert!(receive_directory(root, owner).is_dir());
    }
    assert_eq!(jobs.receive_cleanup_after("connection").unwrap(), 0);
}

#[test]
fn retained_jobs_count_toward_the_page_without_starving_later_or_released_owners() {
    let (directory, mut store, claim_job, _template) = fixture();
    store.external_cancel_prepared(&claim_job.id).unwrap();
    let root = directory.path();
    let jobs = JobStore::open(root).unwrap();
    let state = JobCommandState::default();
    let admission = Admission::default();
    let mut retained = Vec::new();
    for index in 0..8 {
        let job = bind(&mut store, &format!("retained-{index}"));
        held(root, &jobs, &job);
        store.external_cancel_prepared(&job.id).unwrap();
        preserve_conflict(&store, &job);
        settle(&jobs, &job.id, "cancelled");
        retained.push(job);
    }
    let later = bind(&mut store, "later-completion");
    held(root, &jobs, &later);
    store.external_cancel_prepared(&later.id).unwrap();
    settle(&jobs, &later.id, "cancelled");
    let receive = receive_directory(root, &later.id);
    fs::create_dir_all(&receive).unwrap();
    fs::write(receive.join("body"), b"synthetic completed body").unwrap();

    assert!(pass(root, &store, &state, &admission, &claim_job).is_empty());
    assert!(receive.is_dir());
    // Reopening the application must not start on the same eight owners again.
    let restarted = JobCommandState::default();
    assert_eq!(pass(root, &store, &restarted, &admission, &claim_job), vec![later.id.clone()]);
    for job in &retained {
        assert!(marked(root, &job.id));
    }
    external_conflicts::confirm_external_conflict_point(
        store.device_store().unwrap().connection(), &retained[0].id,
        &stored(&format!("backup-point-{}", retained[0].id), ObjectRole::BackupPoint),
    ).unwrap();
    external_conflicts::mark_external_conflict_resolved(
        store.device_store().unwrap().connection(), &retained[0].id,
    ).unwrap();
    assert_eq!(pass(root, &store, &restarted, &admission, &claim_job), vec![retained[0].id.clone()]);
    assert!(!marked(root, &retained[0].id));
}

#[test]
fn only_the_authoritative_row_releases_bodies_whatever_the_summary_says() {
    let (directory, mut store, job, template) = fixture();
    let root = directory.path();
    let jobs = JobStore::open(root).unwrap();
    let state = JobCommandState::default();
    let admission = Admission::default();
    held(root, &jobs, &job);
    let prepared = prepare(&mut store, &job, receive_input(root, &job, &template, "snapshot"));
    // A summary that looks finished while the intent is still ready.
    settle(&jobs, &job.id, "succeeded");
    assert!(pass(root, &store, &state, &admission, &job).is_empty());
    assert!(receive_directory(root, &job.id).is_dir());

    activate_prepared_receive(&mut store, &prepared, &job.admission_identity).unwrap();
    // The auxiliary summary is left queued, as after a failed cache write.
    settle(&jobs, &job.id, "queued");
    assert_eq!(pass(root, &store, &state, &admission, &job), vec![job.id.clone()]);
    assert!(!receive_directory(root, &job.id).exists());
    assert!(!marked(root, &job.id));
    let stale = jobs.read(&job.id).unwrap();
    assert_eq!(stale.summary["state"], "queued");
    for _ in 0..2 {
        assert_eq!(completed_receive_result(&store, &stale, 0).unwrap().unwrap()["receivedRevision"], "1");
    }
    assert_eq!(store.revision().unwrap(), 1);
}

#[test]
fn cancellation_releases_bodies_only_after_the_intent_and_handle_are_settled() {
    let (directory, mut store, job, template) = fixture();
    let root = directory.path();
    let jobs = JobStore::open(root).unwrap();
    let state = JobCommandState::default();
    let admission = Admission::default();
    held(root, &jobs, &job);
    let prepared = prepare(&mut store, &job, receive_input(root, &job, &template, "snapshot"));
    let staging = prepared.apply.staging_id().map(str::to_owned);
    state.prepared_receives.lock().unwrap().insert(job.id.clone(), prepared);
    // Paused with a live apply handle.
    settle(&jobs, &job.id, "waiting");
    assert!(pass(root, &store, &state, &admission, &job).is_empty());
    // A cancelled label while the handle and the ready intent remain.
    settle(&jobs, &job.id, "cancelled");
    assert!(pass(root, &store, &state, &admission, &job).is_empty());
    // The handle is released, but the intent has not been cancelled yet.
    state.prepared_receives.lock().unwrap().remove(&job.id);
    if let Some(staging) = staging {
        store.replace_abort(&staging).unwrap();
    }
    assert!(pass(root, &store, &state, &admission, &job).is_empty());
    // A cancelled intent under a job that may still run again.
    store.external_cancel_prepared(&job.id).unwrap();
    settle(&jobs, &job.id, "waiting");
    assert!(pass(root, &store, &state, &admission, &job).is_empty());
    assert!(receive_directory(root, &job.id).join("pack-synthetic").is_file());
    settle(&jobs, &job.id, "cancelled");
    assert_eq!(pass(root, &store, &state, &admission, &job), vec![job.id.clone()]);
    assert!(!receive_directory(root, &job.id).exists());
    assert_eq!(store.revision().unwrap(), 0);

    // A cancellation that arrives after the receive committed changes nothing
    // about the commit and still releases the bodies.
    let later = bind(&mut store, "later-snapshot");
    held(root, &jobs, &later);
    let prepared = prepare(&mut store, &later, receive_input(root, &later, &template, "later-snapshot"));
    activate_prepared_receive(&mut store, &prepared, &later.admission_identity).unwrap();
    assert!(store.external_cancel_prepared(&later.id).is_err());
    settle(&jobs, &later.id, "cancelled");
    assert_eq!(pass(root, &store, &state, &admission, &later), vec![later.id.clone()]);
    assert_eq!(completed_receive_result(&store, &later, 0).unwrap().unwrap()["receivedRevision"], "1");
    assert_eq!(store.revision().unwrap(), 1);
}

#[test]
fn only_the_claim_on_the_connection_admits_a_pass() {
    let (directory, mut store, job, template) = fixture();
    let root = directory.path();
    let jobs = JobStore::open(root).unwrap();
    let state = JobCommandState::default();
    let admission = Admission::default();
    held(root, &jobs, &job);
    let prepared = prepare(&mut store, &job, receive_input(root, &job, &template, "snapshot"));
    activate_prepared_receive(&mut store, &prepared, &job.admission_identity).unwrap();
    settle(&jobs, &job.id, "succeeded");
    let owners = Owners { root, state: &state, store: &store, admission: &admission, repository_id: "repository" };
    let other_state = JobCommandState::default();
    let (_, foreign) = other_state.claim(&job).unwrap();
    assert!(receive_artifacts::detach_connection(&owners, &foreign, "connection").is_err());
    let mut elsewhere = job.clone();
    elsewhere.id = "another-job".into();
    elsewhere.request.connection_id = "another-connection".into();
    let (_, unrelated) = state.claim(&elsewhere).unwrap();
    assert!(receive_artifacts::detach_connection(&owners, &unrelated, "connection").is_err());
    drop(unrelated);
    // A library operation holding exclusive admission postpones the pass.
    let exclusive = admission.file(true).unwrap();
    let (_, claim) = state.claim(&job).unwrap();
    assert!(receive_artifacts::detach_connection(&owners, &claim, "connection").unwrap().is_empty());
    drop(claim);
    drop(exclusive);
    assert!(receive_directory(root, &job.id).is_dir());
    assert!(marked(root, &job.id));
    // A receive for another repository behind the same connection name.
    let (_, claim) = state.claim(&job).unwrap();
    let moved = Owners { repository_id: "another-repository", ..owners };
    assert!(receive_artifacts::detach_connection(&moved, &claim, "connection").unwrap().is_empty());
    drop(claim);
    assert_eq!(pass(root, &store, &state, &admission, &job), vec![job.id.clone()]);
}

#[cfg(windows)]
#[test]
fn an_open_file_postpones_reclamation_without_touching_the_committed_receive() {
    let (directory, mut store, job, template) = fixture();
    let root = directory.path();
    let jobs = JobStore::open(root).unwrap();
    let state = JobCommandState::default();
    let admission = Admission::default();
    held(root, &jobs, &job);
    let prepared = prepare(&mut store, &job, receive_input(root, &job, &template, "snapshot"));
    activate_prepared_receive(&mut store, &prepared, &job.admission_identity).unwrap();
    settle(&jobs, &job.id, "succeeded");
    let before = jobs.read(&job.id).unwrap().summary;
    let database = receive_directory(root, &job.id).join("external-storage").join("content.sqlite");
    let reader = rusqlite::Connection::open(&database).unwrap();
    reader.query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| row.get::<_, i64>(0)).unwrap();
    assert!(pass(root, &store, &state, &admission, &job).is_empty());
    assert!(database.is_file());
    assert!(marked(root, &job.id));
    assert_eq!(jobs.read(&job.id).unwrap().summary, before);
    assert_eq!(completed_receive_result(&store, &job, 0).unwrap().unwrap()["receivedRevision"], "1");
    assert_eq!(store.revision().unwrap(), 1);
    drop(reader);
    assert_eq!(pass(root, &store, &state, &admission, &job), vec![job.id.clone()]);
    assert!(!receive_directory(root, &job.id).exists());
    assert_eq!(store.revision().unwrap(), 1);
}

#[test]
fn an_interrupted_pass_finishes_and_bodies_already_gone_are_fine() {
    let (directory, mut store, job, template) = fixture();
    let root = directory.path();
    let jobs = JobStore::open(root).unwrap();
    let state = JobCommandState::default();
    let admission = Admission::default();
    held(root, &jobs, &job);
    let prepared = prepare(&mut store, &job, receive_input(root, &job, &template, "snapshot"));
    activate_prepared_receive(&mut store, &prepared, &job.admission_identity).unwrap();
    settle(&jobs, &job.id, "succeeded");
    let job_root = job_directory(root, "connection", &job.id);
    // Stopped after moving the bodies aside, before deleting any of them.
    let (_, claim) = state.claim(&job).unwrap();
    let detached = receive_artifacts::detach_connection(
        &Owners { root, state: &state, store: &store, admission: &admission, repository_id: "repository" },
        &claim,
        "connection",
    )
    .unwrap();
    drop(claim);
    assert_eq!(detached, vec![job.id.clone()]);
    let moved = job_root.join(".reclaim-receive");
    assert!(moved.is_dir());
    assert!(!job_root.join("receive").exists());
    // Stopped again partway through the deletion.
    fs::remove_file(moved.join("pack-synthetic")).unwrap();
    assert!(marked(root, &job.id));
    assert_eq!(pass(root, &store, &state, &admission, &job), vec![job.id.clone()]);
    assert!(!job_root.exists());
    assert!(!marked(root, &job.id));

    // A hold whose bodies never reached the disk, or were already removed.
    receive_artifacts::hold(root, &job.id).unwrap();
    assert_eq!(pass(root, &store, &state, &admission, &job), vec![job.id.clone()]);
    assert!(!marked(root, &job.id));
    assert_eq!(completed_receive_result(&store, &job, 0).unwrap().unwrap()["receivedRevision"], "1");
}

#[test]
fn a_link_in_a_receive_is_never_followed() {
    let (directory, mut store, job, template) = fixture();
    let root = directory.path();
    let jobs = JobStore::open(root).unwrap();
    let state = JobCommandState::default();
    let admission = Admission::default();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("keep"), b"outside the job").unwrap();
    held(root, &jobs, &job);
    let prepared = prepare(&mut store, &job, receive_input(root, &job, &template, "snapshot"));
    activate_prepared_receive(&mut store, &prepared, &job.admission_identity).unwrap();
    settle(&jobs, &job.id, "succeeded");
    let receive = receive_directory(root, &job.id);

    // The receive directory itself redirected elsewhere stays where it is.
    let kept = receive.with_file_name("receive-kept");
    fs::rename(&receive, &kept).unwrap();
    link_directory(outside.path(), &receive);
    assert!(pass(root, &store, &state, &admission, &job).is_empty());
    assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"outside the job");
    assert!(marked(root, &job.id));
    remove_link(&receive);
    fs::rename(&kept, &receive).unwrap();

    // A link inside moves aside with the tree, and its removal stops there.
    link_directory(outside.path(), &receive.join("external-storage").join("linked"));
    let moved = receive.with_file_name(".reclaim-receive");
    for _ in 0..2 {
        assert_eq!(pass(root, &store, &state, &admission, &job), vec![job.id.clone()]);
        assert!(!receive.exists());
        assert!(moved.join("external-storage").join("linked").exists());
        assert!(marked(root, &job.id));
        assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"outside the job");
    }
    remove_link(&moved.join("external-storage").join("linked"));

    assert_eq!(pass(root, &store, &state, &admission, &job), vec![job.id.clone()]);
    assert!(!job_directory(root, "connection", &job.id).exists());
    assert!(!marked(root, &job.id));
    assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"outside the job");
}

#[test]
fn releasing_a_hold_keeps_a_summary_settled_meanwhile() {
    let (directory, mut store, job, template) = fixture();
    let root = directory.path();
    let jobs = JobStore::open(root).unwrap();
    let state = JobCommandState::default();
    let admission = Admission::default();
    held(root, &jobs, &job);
    let prepared = prepare(&mut store, &job, receive_input(root, &job, &template, "snapshot"));
    activate_prepared_receive(&mut store, &prepared, &job.admission_identity).unwrap();
    settle(&jobs, &job.id, "succeeded");
    // The copy a removal starts from, then a reconciliation settling the row.
    let (_, claim) = state.claim(&job).unwrap();
    let detached = receive_artifacts::detach_connection(
        &Owners { root, state: &state, store: &store, admission: &admission, repository_id: "repository" },
        &claim,
        "connection",
    )
    .unwrap();
    drop(claim);
    let mut reconciled = jobs.read(&job.id).unwrap();
    reconciled.summary["result"] = json!({"snapshotId":"snapshot","receivedRevision":"1"});
    reconciled.summary["updatedAtMs"] = json!("99");
    jobs.put(&reconciled).unwrap();
    receive_artifacts::remove_detached(root, &detached);

    let released = jobs.read(&job.id).unwrap();
    assert_eq!(released.receive_artifacts, None);
    let mut expected = serde_json::to_value(&reconciled).unwrap();
    expected["receiveArtifacts"] = serde_json::Value::Null;
    assert_eq!(serde_json::to_value(&released).unwrap(), expected);
    // Nothing is rewritten once the hold is gone.
    let mut later = released.clone();
    later.summary["updatedAtMs"] = json!("100");
    jobs.put(&later).unwrap();
    jobs.release_receive_artifacts(&job.id).unwrap();
    assert_eq!(jobs.read(&job.id).unwrap().summary, later.summary);
}

#[test]
fn only_an_outcome_that_releases_nothing_of_its_own_passes_before_it_is_published() {
    let (_directory, _store, mut job, _template) = fixture();
    for (state, phase, expected) in [
        ("waiting", "remote-apply", SettlementPass::Skip),
        ("succeeded", "complete", SettlementPass::AfterOutcome),
        ("failed", "paused", SettlementPass::AfterOutcome),
        ("cancelled", "cancelled", SettlementPass::AfterOutcome),
        ("waiting", "paused", SettlementPass::BeforeOutcome),
        ("waiting", "conflict-preservation-paused", SettlementPass::BeforeOutcome),
        ("conflict", "conflict-choice", SettlementPass::BeforeOutcome),
        ("uncertain", "publication-unknown", SettlementPass::BeforeOutcome),
    ] {
        job.summary["state"] = json!(state);
        job.summary["phase"] = json!(phase);
        assert_eq!(receive_artifacts::settlement_pass(&job), expected, "{state}/{phase}");
    }
}
