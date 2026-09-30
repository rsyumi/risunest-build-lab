mod common;
use risunest_sync_server::store::{Store, ObjectIdentity};
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
    assert_eq!(Store::init(unknown.path()).err().unwrap().code, "already-initialized");
    assert_eq!(std::fs::read(unknown.path().join("metadata.sqlite")).unwrap(), b"unknown bytes");
}

#[test]
fn incompatible_schema_is_rejected_before_recovery_writes() {
    let dir = tempfile::tempdir().unwrap();
    drop(Store::init(dir.path()).unwrap());
    let path = dir.path().join("metadata.sqlite");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("ALTER TABLE objects DROP COLUMN storage; PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    drop(db);
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(Store::open(dir.path()).err().unwrap().code, "incompatible-store");
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
    store.retain_objects(&device, &store.head().unwrap().epoch, &[ObjectIdentity { hash: digest.clone(), size: None }]).unwrap();
    let file = dir.path().join("objects").join(&digest[..2]).join(&digest);
    for truncated in [false, true] {
        if truncated { std::fs::write(&file, b"short").unwrap(); } else { std::fs::remove_file(&file).unwrap(); }
        assert_eq!(store.object_size(&digest).unwrap(), Some(body.len() as u64));
        assert!(!store.object_presence(&digest).unwrap());
        assert_eq!(store.managed_devices().unwrap()[0].retained, 1);
        store.put_object(&device, &digest, &body).unwrap();
        assert!(store.object_presence(&digest).unwrap());
        assert_eq!(store.get_object(&digest).unwrap(), body);
        store.retain_objects(&device, &store.head().unwrap().epoch, &[ObjectIdentity { hash: digest.clone(), size: None }]).unwrap();
    }
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
        store.retain_objects(&owner, &store.head().unwrap().epoch, &[ObjectIdentity { hash: digest.clone(), size: None }]).unwrap();
        if body == b"shared" { store.retain_objects(&other, &store.head().unwrap().epoch, &[ObjectIdentity { hash: digest, size: None }]).unwrap(); }
    }
    assert_eq!(store.forget_revoked_device(&owner.id).unwrap_err().code, "revoked-device-required");
    store.revoke_device(&owner.id).unwrap();
    store.maintain().unwrap();
    assert!(store.object_presence(&hash(b"exclusive")).unwrap());
    store.forget_revoked_device(&owner.id).unwrap();
    store.maintain().unwrap();
    assert!(!store.object_presence(&hash(b"exclusive")).unwrap());
    assert!(store.object_presence(&hash(b"shared")).unwrap());
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
        if index < 1024 { std::fs::create_dir(&path).unwrap(); } else { std::fs::write(&path, b"garbage").unwrap(); }
        db.execute("INSERT INTO object_trash VALUES(?1)", [&digest]).unwrap();
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
            let result = std::process::Command::new("cmd.exe").args(["/c", "mklink", "/J"])
                .arg(link.to_string_lossy().replace('/', "\\"))
                .arg(target.to_string_lossy().replace('/', "\\")).output().unwrap();
            assert!(result.status.success(), "synthetic junction creation failed: {}", String::from_utf8_lossy(&result.stderr));
        }
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("actual")).unwrap();
    link(&dir.path().join("actual"), &dir.path().join("linked"));
    let root = dir.path().join("linked/store");
    let store = Store::init(&root).unwrap();
    assert_eq!(store.data_path(), std::fs::canonicalize(dir.path().join("actual")).unwrap().join("store"));
    drop(store);
    std::fs::remove_dir(root.join("objects")).unwrap();
    link(&dir.path().join("actual"), &root.join("objects"));
    assert_eq!(Store::open(&root).err().unwrap().code, "unsafe-storage-path");
}

