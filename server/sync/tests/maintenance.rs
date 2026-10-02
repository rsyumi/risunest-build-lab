mod common;
use common::*;
use risunest_sync_server::store::Store;
use risunest_sync_wire::hash;

/// How many bodies the metadata database still holds, for the collection
/// assertions that would otherwise only see metadata disappear.
fn inline_bodies(dir: &std::path::Path) -> i64 {
    rusqlite::Connection::open(dir.join("metadata.sqlite"))
        .unwrap()
        .query_row("SELECT count(*) FROM small_objects", [], |r| r.get(0))
        .unwrap()
}

fn expire_leases(dir: &std::path::Path) {
    rusqlite::Connection::open(dir.join("metadata.sqlite"))
        .unwrap()
        .execute("UPDATE object_leases SET expires=0", [])
        .unwrap();
}

#[test]
fn a_revoked_device_leaves_the_device_list_once_nothing_it_holds_remains() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let kept = device(&store);
    let idle = device(&store);
    let operation = request(
        &store,
        WRITER_A,
        "accepted",
        vec![inline("done", WRITER_A, 1, "done")],
    );
    store.push(&idle, &operation).unwrap();
    store.revoke_device(&idle.id).unwrap();
    store.maintain().unwrap();
    assert_eq!(
        store
            .managed_devices()
            .unwrap()
            .into_iter()
            .map(|d| d.id)
            .collect::<Vec<_>>()
            .as_slice(),
        std::slice::from_ref(&kept.id)
    );
    assert_eq!(
        store.push(&idle, &operation).unwrap_err().code,
        "unauthorized"
    );
    let db = rusqlite::Connection::open(dir.path().join("metadata.sqlite")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM operations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM writers", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM writer_versions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let fresh = device(&store);
    assert_eq!(
        store.push(&fresh, &operation).unwrap_err().code,
        "writer-collision"
    );
    assert_eq!(store.head().unwrap().seq.as_str(), "1");
}

#[test]
fn maintenance_folds_the_write_ahead_log_back_and_reports_a_blocked_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let a = device(&store);
    store.put_object(&a, &hash(b"old"), b"old").unwrap();
    store
        .push(
            &a,
            &request(
                &store,
                WRITER_A,
                "first",
                vec![inline("key", WRITER_A, 1, "old")],
            ),
        )
        .unwrap();
    let wal = dir.path().join("metadata.sqlite-wal");
    assert!(std::fs::metadata(&wal).unwrap().len() > 0);
    let result = store.maintain().unwrap();
    assert_eq!(result.wal_checkpoint.busy, 0);
    assert_eq!(
        result.wal_checkpoint.checkpointed_frames,
        result.wal_checkpoint.log_frames
    );
    assert!(!result.wal_checkpoint.incomplete());
    assert_eq!(std::fs::metadata(&wal).unwrap().len(), 0);
    // A reader holding the database keeps frames in the log, which the result
    // reports instead of leaving the growth unexplained.
    store.put_object(&a, &hash(b"new"), b"new").unwrap();
    let reader = rusqlite::Connection::open(dir.path().join("metadata.sqlite")).unwrap();
    reader
        .execute_batch("BEGIN; SELECT count(*) FROM objects;")
        .unwrap();
    let blocked = store.checkpoint_wal().unwrap();
    assert!(blocked.incomplete());
    reader.execute_batch("COMMIT").unwrap();
    assert!(!store.checkpoint_wal().unwrap().incomplete());
}

#[test]
fn collection_removes_a_published_file_and_an_inline_body_together() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let a = device(&store);
    let inline = b"synthetic inline body".to_vec();
    let published = vec![b'p'; 128 * 1024];
    for bytes in [&inline, &published] {
        store.put_object(&a, &hash(bytes), bytes).unwrap();
    }
    let digest = hash(&published);
    let path = dir.path().join("objects").join(&digest[..2]).join(&digest);
    assert!(path.exists());
    assert_eq!(inline_bodies(dir.path()), 1);
    expire_leases(dir.path());
    assert_eq!(store.maintain().unwrap().objects_removed, 2);
    for bytes in [&inline, &published] {
        assert!(store.object_size(&hash(bytes)).unwrap().is_none());
    }
    assert!(!path.exists());
    assert_eq!(inline_bodies(dir.path()), 0);
}

#[test]
fn collection_removes_a_stray_file_under_an_inline_identity() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let a = device(&store);
    let bytes = b"synthetic inline body".to_vec();
    let digest = hash(&bytes);
    store.put_object(&a, &digest, &bytes).unwrap();
    // Model a store where a file was published under an identity the metadata
    // files inline, which is what collection has to survive.
    let path = dir.path().join("objects").join(&digest[..2]).join(&digest);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    expire_leases(dir.path());
    assert_eq!(store.maintain().unwrap().objects_removed, 1);
    assert!(!path.exists());
    assert_eq!(inline_bodies(dir.path()), 0);
}
