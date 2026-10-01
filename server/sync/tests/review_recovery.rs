mod common;
use risunest_sync_server::store::{ObjectIdentity, Store};
use risunest_sync_wire::hash;
use rusqlite::Connection;

#[test]
fn incomplete_initialization_is_retryable_without_replacing_unknown_metadata() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("objects"), b"synthetic obstruction").unwrap();
    assert!(Store::init(dir.path()).is_err());
    assert!(!dir.path().join("metadata.sqlite").exists());
    std::fs::remove_file(dir.path().join("objects")).unwrap();
    drop(Store::init(dir.path()).unwrap());
    assert!(Store::open(dir.path()).is_ok());
    let unknown = tempfile::tempdir().unwrap();
    std::fs::write(unknown.path().join("metadata.sqlite"), b"unknown bytes").unwrap();
    assert_eq!(
        Store::init(unknown.path()).err().unwrap().code,
        "already-initialized"
    );
    assert_eq!(
        std::fs::read(unknown.path().join("metadata.sqlite")).unwrap(),
        b"unknown bytes"
    );
}

#[test]
fn incompatible_schema_is_rejected_before_recovery_writes() {
    let dir = tempfile::tempdir().unwrap();
    drop(Store::init(dir.path()).unwrap());
    let path = dir.path().join("metadata.sqlite");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("ALTER TABLE objects DROP COLUMN storage; PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    drop(db);
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        Store::open(dir.path()).err().unwrap().code,
        "incompatible-store"
    );
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn missing_and_truncated_bodies_request_repair_without_losing_custody() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let device = common::device(&store);
    let body = vec![b'x'; 128 * 1024];
    let digest = hash(&body);
    store.put_object(&device, &digest, &body).unwrap();
    store
        .retain_objects(
            &device,
            &store.head().unwrap().epoch,
            &[ObjectIdentity {
                hash: digest.clone(),
                size: None,
            }],
        )
        .unwrap();
    let file = dir.path().join("objects").join(&digest[..2]).join(&digest);
    for truncated in [false, true] {
        if truncated {
            std::fs::write(&file, b"short").unwrap();
        } else {
            std::fs::remove_file(&file).unwrap();
        }
        assert_eq!(store.object_size(&digest).unwrap(), Some(body.len() as u64));
        assert!(!store.object_presence(&digest).unwrap());
        assert_eq!(store.managed_devices().unwrap()[0].retained, 1);
        store.put_object(&device, &digest, &body).unwrap();
        assert!(store.object_presence(&digest).unwrap());
        assert_eq!(store.get_object(&digest).unwrap(), body);
        store
            .retain_objects(
                &device,
                &store.head().unwrap().epoch,
                &[ObjectIdentity {
                    hash: digest.clone(),
                    size: None,
                }],
            )
            .unwrap();
    }
}

#[test]
fn unused_registration_does_not_pin_history_but_acknowledged_offline_device_does() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let active = common::device(&store);
    let offline = common::device(&store);
    let unused = common::device(&store);
    let head = store.head().unwrap();
    store
        .acknowledge(&offline, &head.epoch, &common::acks(&head.seq))
        .unwrap();
    store.put_object(&active, &hash(b"body"), b"body").unwrap();
    let intent = common::stage(&store, &active, &head, 1, &common::changes("key", b"body"));
    let head = store.commit(&active, &intent, &head.etag()).unwrap().head;
    store
        .acknowledge(&active, &head.epoch, &common::acks(&head.seq))
        .unwrap();
    assert_eq!(store.maintain().unwrap().min_retained_seq.as_str(), "0");
    store.revoke_device(&offline.id).unwrap();
    assert_eq!(store.maintain().unwrap().min_retained_seq, head.seq);
    let error = store
        .pin_changes(&unused, &head.epoch, &0.into(), &common::LIBRARY)
        .err()
        .unwrap();
    assert_eq!((error.code, error.status), ("checkpoint-required", 410));
    let checkpoint = store.create_checkpoint(&unused, &common::LIBRARY).unwrap();
    assert_eq!(checkpoint.head.seq, head.seq);
    let devices = store.managed_devices().unwrap();
    assert!(devices
        .iter()
        .find(|d| d.id == active.id)
        .unwrap()
        .last_ack
        .is_some());
}

#[test]
fn only_explicit_forget_releases_revoked_custody_and_keeps_shared_objects() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let owner = common::device(&store);
    let other = common::device(&store);
    for body in [b"exclusive".as_slice(), b"shared".as_slice()] {
        let digest = hash(body);
        store.put_object(&owner, &digest, body).unwrap();
        store
            .retain_objects(
                &owner,
                &store.head().unwrap().epoch,
                &[ObjectIdentity {
                    hash: digest.clone(),
                    size: None,
                }],
            )
            .unwrap();
        if body == b"shared" {
            store
                .retain_objects(
                    &other,
                    &store.head().unwrap().epoch,
                    &[ObjectIdentity {
                        hash: digest,
                        size: None,
                    }],
                )
                .unwrap();
        }
    }
    assert_eq!(
        store.forget_revoked_device(&owner.id).unwrap_err().code,
        "revoked-device-required"
    );
    store.revoke_device(&owner.id).unwrap();
    store.maintain().unwrap();
    assert!(store.object_presence(&hash(b"exclusive")).unwrap());
    store.forget_revoked_device(&owner.id).unwrap();
    store.maintain().unwrap();
    assert!(!store.object_presence(&hash(b"exclusive")).unwrap());
    assert!(store.object_presence(&hash(b"shared")).unwrap());
}

#[test]
fn persistent_commit_failure_is_visible_backed_off_and_still_reserved() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let device = common::device(&store);
    let db = Connection::open(dir.path().join("metadata.sqlite")).unwrap();
    db.execute("INSERT INTO commit_jobs(operation,device,digest,body,stage) VALUES('synthetic',?1,'synthetic','invalid','synthetic')", [&device.id]).unwrap();
    for attempt in 0..12 {
        db.execute("UPDATE commit_jobs SET retry_after=0", [])
            .unwrap();
        assert_eq!(
            store.run_pending_commit().unwrap_err().code,
            "corrupt-metadata"
        );
        let delay: i64 = db
            .query_row("SELECT retry_after-unixepoch() FROM commit_jobs", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(delay, (1i64 << attempt.min(9)).min(300));
        assert!(!store.run_pending_commit().unwrap());
    }
    let status = store.managed_devices().unwrap();
    assert!(status[0].pending);
    assert_eq!(status[0].pending_error.as_deref(), Some("corrupt-metadata"));
}

#[test]
fn trash_passes_are_bounded_fair_and_drain_without_another_reachability_scan() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let db = Connection::open(dir.path().join("metadata.sqlite")).unwrap();
    let mut paths = Vec::new();
    for index in 0..1025 {
        let digest = hash(format!("synthetic-{index}").as_bytes());
        let path = dir.path().join("objects").join(&digest[..2]).join(&digest);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        if index < 1024 {
            std::fs::create_dir(&path).unwrap();
        } else {
            std::fs::write(&path, b"garbage").unwrap();
        }
        db.execute("INSERT INTO object_trash VALUES(?1)", [&digest])
            .unwrap();
        paths.push(path);
    }
    let first = store.drain_trash().unwrap();
    assert_eq!(first.selected, 1024);
    assert_eq!(first.failed, 1024);
    let next = store.drain_trash().unwrap();
    assert_eq!(next.removed, 1);
    assert!(!paths[1024].exists());
    assert_eq!(next.backlog, 1024);
    assert!(paths[..1024].iter().all(|path| path.is_dir()));
}

#[cfg(any(unix, windows))]
#[test]
fn linked_parent_is_resolved_but_links_inside_the_store_are_refused() {
    fn link(target: &std::path::Path, link: &std::path::Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let result = std::process::Command::new("cmd.exe")
                .creation_flags(0x08000000)
                .args(["/c", "mklink", "/J"])
                .arg(link.to_string_lossy().replace('/', "\\"))
                .arg(target.to_string_lossy().replace('/', "\\"))
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "synthetic junction creation failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("actual")).unwrap();
    link(&dir.path().join("actual"), &dir.path().join("linked"));
    let root = dir.path().join("linked/store");
    let store = Store::init(&root).unwrap();
    assert_eq!(
        store.data_path(),
        std::fs::canonicalize(dir.path().join("actual"))
            .unwrap()
            .join("store")
    );
    drop(store);
    std::fs::remove_dir(root.join("objects")).unwrap();
    link(&dir.path().join("actual"), &root.join("objects"));
    assert_eq!(
        Store::open(&root).err().unwrap().code,
        "unsafe-storage-path"
    );
}

#[test]
fn reserved_commit_completes_exactly_once_when_storage_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let device = common::device(&store);
    store.put_object(&device, &hash(b"body"), b"body").unwrap();
    let head = store.head().unwrap();
    let intent = common::stage(&store, &device, &head, 1, &common::changes("key", b"body"));
    store.submit_commit(&device, &intent, &head.etag()).unwrap();
    let db = Connection::open(dir.path().join("metadata.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER synthetic_write_failure BEFORE INSERT ON records BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
    assert_eq!(
        store.run_pending_commit().unwrap_err().code,
        "metadata-storage"
    );
    let pending = store.managed_devices().unwrap();
    assert!(pending[0].pending);
    assert_eq!(
        pending[0].pending_error.as_deref(),
        Some("metadata-storage")
    );
    let competing = common::stage(
        &store,
        &device,
        &head,
        2,
        &common::changes("other", b"body"),
    );
    assert_eq!(
        store
            .submit_commit(&device, &competing, &head.etag())
            .err()
            .unwrap()
            .code,
        "device-operation-active"
    );
    db.execute_batch("DROP TRIGGER synthetic_write_failure; UPDATE commit_jobs SET retry_after=0;")
        .unwrap();
    assert!(store.run_pending_commit().unwrap());
    assert!(!store.run_pending_commit().unwrap());
    assert_eq!(store.head().unwrap().seq.as_str(), "1");
    let count: i64 = db
        .query_row("SELECT count(*) FROM receipts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}
