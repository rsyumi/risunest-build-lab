mod common;
use common::*;
use risunest_sync_server::store::{ChangeCursor, Store};
use risunest_sync_wire::{hash, Domain, RecordVersion};

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
        .execute("UPDATE receipts SET created=0", [])
        .unwrap();
    rusqlite::Connection::open(dir.join("metadata.sqlite"))
        .unwrap()
        .execute("UPDATE object_leases SET expires=0", [])
        .unwrap();
}

#[test]
fn checkpoint_and_each_device_pin_survive_concurrent_commit_and_collection() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let a = device(&store);
    let b = device(&store);
    store.put_object(&a, &hash(b"old"), b"old").unwrap();
    let h = store.head().unwrap();
    let c = changes("key", b"old");
    let intent = stage(&store, &a, &h, 1, &c);
    let h = store.commit(&a, &intent, &h.etag()).unwrap().head;
    let checkpoint = store.create_checkpoint(&b, &LIBRARY).unwrap();
    let pin = store
        .pin_changes(&b, &h.epoch, &0.into(), &LIBRARY)
        .unwrap();
    store.put_object(&a, &hash(b"new"), b"new").unwrap();
    let mut c2 = changes("key", b"new");
    c2.changes[0].before = c.changes[0].after.clone();
    let intent = stage(&store, &a, &h, 2, &c2);
    let h = store.commit(&a, &intent, &h.etag()).unwrap().head;
    store.acknowledge(&a, &h.epoch, &acks(&h.seq)).unwrap();
    store.acknowledge(&b, &h.epoch, &acks(&h.seq)).unwrap();
    expire_leases(dir.path());
    assert_eq!(store.maintain().unwrap().min_retained_seq.as_str(), "0");
    let page = store
        .pinned_changes(&b, &pin.pin_id, &ChangeCursor::after_commit(0.into()), 128)
        .unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.through.seq.as_str(), "1");
    assert!(store.release_pin(&a, &pin.pin_id).is_err());
    store.release_pin(&b, &pin.pin_id).unwrap();
    assert_eq!(store.maintain().unwrap().min_retained_seq, h.seq);
    let page = store
        .checkpoint_page(&b, &checkpoint.checkpoint_id, None, 1)
        .unwrap();
    assert_eq!(page.records[0].version, c.changes[0].after);
    assert_eq!(store.get_object(&hash(b"old")).unwrap(), b"old");
    store
        .release_checkpoint(&b, &checkpoint.checkpoint_id)
        .unwrap();
    assert_eq!(store.maintain().unwrap().objects_removed, 1);
    assert!(store.object_size(&hash(b"old")).unwrap().is_none());
    assert_eq!(inline_bodies(dir.path()), 1);
    assert_eq!(store.get_object(&hash(b"new")).unwrap(), b"new");
    assert_eq!(
        store
            .changes(
                &h.epoch,
                &ChangeCursor::after_commit(0.into()),
                &h.seq,
                &LIBRARY,
                10
            )
            .err()
            .unwrap()
            .code,
        "checkpoint-required"
    );
    assert_eq!(
        store
            .commit(&a, &intent, &intent.expected_head.etag())
            .unwrap_err()
            .code,
        "operation-history-expired"
    );
}

#[test]
fn offline_ack_keeps_tombstones_history_and_staged_objects_and_epoch_rotates() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let a = device(&store);
    let b = device(&store);
    store.put_object(&a, &hash(b"x"), b"x").unwrap();
    let h = store.head().unwrap();
    let mut c = changes("k", b"x");
    let intent = stage(&store, &a, &h, 1, &c);
    let h = store.commit(&a, &intent, &h.etag()).unwrap().head;
    c.changes[0].before = c.changes[0].after.clone();
    c.changes[0].after = RecordVersion::Tombstone {
        deletion_id: "deleted".into(),
    };
    let intent = stage(&store, &a, &h, 2, &c);
    let h = store.commit(&a, &intent, &h.etag()).unwrap().head;
    store.acknowledge(&a, &h.epoch, &acks(&h.seq)).unwrap();
    store.put_object(&a, &hash(b"pending"), b"pending").unwrap();
    let pending = stage(&store, &a, &h, 3, &changes("pending", b"pending"));
    expire_leases(dir.path());
    assert_eq!(store.maintain().unwrap().min_retained_seq.as_str(), "0");
    assert_eq!(store.get_object(&hash(b"x")).unwrap(), b"x");
    store.revoke_device(&b.id).unwrap();
    store.maintain().unwrap();
    assert_eq!(
        store.record(Domain::Library, "k").unwrap(),
        c.changes[0].after
    );
    assert_eq!(store.get_object(&hash(b"pending")).unwrap(), b"pending");
    // The maintenance floor is advisory, not a new content revision.
    assert_eq!(
        store.commit(&a, &pending, &h.etag()).unwrap().status,
        risunest_sync_wire::TerminalStatus::Committed
    );
    store.rotate_restored_epoch().unwrap();
    let restored = store.head().unwrap();
    assert_ne!(restored.epoch, h.epoch);
    assert_eq!(restored.seq.as_str(), "0");
    assert!(store.record(Domain::Library, "pending").unwrap() != RecordVersion::Absent);
    assert_eq!(
        store
            .pin_changes(&a, &h.epoch, &0.into(), &LIBRARY)
            .err()
            .unwrap()
            .code,
        "epoch-changed"
    );
}

#[test]
fn maintenance_folds_the_write_ahead_log_back_and_reports_a_blocked_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let a = device(&store);
    let head = store.head().unwrap();
    store.put_object(&a, &hash(b"old"), b"old").unwrap();
    let intent = stage(&store, &a, &head, 1, &changes("key", b"old"));
    store.commit(&a, &intent, &head.etag()).unwrap();
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
