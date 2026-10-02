mod common;
use risunest_sync_server::store::Store;
use risunest_sync_wire::hash;
#[test]
fn cold_backup_restores_exact_objects_under_a_new_epoch_and_rejects_corruption() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let backup = directory.path().join("backup");
    let restored = directory.path().join("restored");
    let store = Store::init(&source).unwrap();
    let credential = store.add_device().unwrap();
    let device = store
        .authenticate(&credential.library_id, &credential.token)
        .unwrap();
    let bytes = b"synthetic exact bytes\0\xff";
    let digest = hash(bytes);
    store.put_object(&device, &digest, bytes).unwrap();
    // Large enough to be a file, so one backup carries both an inline body and
    // a published one.
    let published = vec![b'p'; 128 * 1024];
    let published_digest = hash(&published);
    store
        .put_object(&device, &published_digest, &published)
        .unwrap();
    let request = common::request(
        &store,
        common::WRITER_A,
        "backup-write",
        vec![common::inline("key", common::WRITER_A, 1, "synthetic")],
    );
    store.push(&device, &request).unwrap();
    let original_head = store.head().unwrap();
    let manifest = store.backup(&backup).unwrap();
    assert_eq!(manifest.head, original_head);
    assert!(store.backup(&backup).is_err());
    let restored = Store::restore_backup(&backup, &restored).unwrap();
    assert_eq!(restored.get_object(&digest).unwrap(), bytes);
    assert_eq!(restored.get_object(&published_digest).unwrap(), published);
    assert!(restored
        .authenticate(&credential.library_id, &credential.token)
        .is_err());
    let replacement = common::device(&restored);
    let new_body = vec![b'n'; 128 * 1024];
    restored
        .put_object(&replacement, &hash(&new_body), &new_body)
        .unwrap();
    assert_eq!(restored.get_object(&hash(&new_body)).unwrap(), new_body);
    // The incremental setting travels with the metadata copy, so collection on
    // a restored store can still return the pages it frees.
    assert_eq!(
        rusqlite::Connection::open(directory.path().join("restored").join("metadata.sqlite"))
            .unwrap()
            .query_row("PRAGMA auto_vacuum", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    // The inline body travelled inside the metadata copy rather than as a file
    // the backup had to write out and read back.
    assert!(!backup
        .join("objects")
        .join(&digest[..2])
        .join(&digest)
        .exists());
    assert_eq!(restored.head().unwrap().seq, original_head.seq);
    assert_ne!(restored.head().unwrap().epoch, original_head.epoch);
    assert_eq!(
        restored.head().unwrap().library_id,
        original_head.library_id
    );
    assert!(restored.operation(&device, &request.operation_id).is_err());
    let db = rusqlite::Connection::open(restored.data_path().join("metadata.sqlite")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM operations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    std::fs::write(
        backup
            .join("objects")
            .join(&published_digest[..2])
            .join(&published_digest),
        b"corrupt",
    )
    .unwrap();
    assert!(Store::restore_backup(&backup, &directory.path().join("bad-restore")).is_err());
    // An altered inline body changes the metadata copy the manifest hashes, so
    // it is refused by the same check that covers every other row.
    let second = directory.path().join("second");
    store.backup(&second).unwrap();
    rusqlite::Connection::open(second.join("metadata.sqlite"))
        .unwrap()
        .execute(
            "UPDATE small_objects SET body=?1 WHERE hash=?2",
            (b"corrupt".as_slice(), &digest),
        )
        .unwrap();
    assert!(Store::restore_backup(&second, &directory.path().join("bad-inline-restore")).is_err());
}

#[test]
fn backup_rejects_equal_length_inline_corruption_before_completion_marker() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(&dir.path().join("store")).unwrap();
    let device = common::device(&store);
    store
        .put_object(&device, &hash(b"healthy"), b"healthy")
        .unwrap();
    rusqlite::Connection::open(store.data_path().join("metadata.sqlite"))
        .unwrap()
        .execute("UPDATE small_objects SET body=?1", [b"corrupt".as_slice()])
        .unwrap();
    let backup = dir.path().join("backup");
    assert_eq!(
        store.backup(&backup).err().unwrap().code,
        "corrupt-backup-object"
    );
    assert!(!backup.join("backup.json").exists());
}

#[test]
fn failed_restore_after_staging_does_not_publish_a_destination() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(&dir.path().join("store")).unwrap();
    let backup = dir.path().join("backup");
    let mut manifest = store.backup(&backup).unwrap();
    manifest.objects = 1.into();
    std::fs::write(
        backup.join("backup.json"),
        risunest_sync_wire::canonical::encode(&manifest).unwrap(),
    )
    .unwrap();
    let destination = dir.path().join("restored");
    assert!(Store::restore_backup(&backup, &destination).is_err());
    assert!(!destination.exists());
    manifest.objects = 0.into();
    std::fs::write(
        backup.join("backup.json"),
        risunest_sync_wire::canonical::encode(&manifest).unwrap(),
    )
    .unwrap();
    assert!(Store::restore_backup(&backup, &destination).is_ok());
}

#[test]
fn restore_failure_after_metadata_copy_never_exposes_the_old_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(&dir.path().join("store")).unwrap();
    let backup = dir.path().join("backup");
    let mut manifest = store.backup(&backup).unwrap();
    let metadata = backup.join("metadata.sqlite");
    let db = rusqlite::Connection::open(&metadata).unwrap();
    db.execute_batch(
        "CREATE TABLE synthetic_incompatible(value TEXT); PRAGMA wal_checkpoint(TRUNCATE);",
    )
    .unwrap();
    drop(db);
    manifest.metadata_hash = hash(&std::fs::read(&metadata).unwrap());
    std::fs::write(
        backup.join("backup.json"),
        risunest_sync_wire::canonical::encode(&manifest).unwrap(),
    )
    .unwrap();
    let destination = dir.path().join("restored");
    assert_eq!(
        Store::restore_backup(&backup, &destination)
            .err()
            .unwrap()
            .code,
        "incompatible-store"
    );
    assert!(!destination.exists());
    assert!(Store::open(&destination).is_err());
    let db = rusqlite::Connection::open(&metadata).unwrap();
    db.execute_batch("DROP TABLE synthetic_incompatible; PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    drop(db);
    manifest.metadata_hash = hash(&std::fs::read(&metadata).unwrap());
    std::fs::write(
        backup.join("backup.json"),
        risunest_sync_wire::canonical::encode(&manifest).unwrap(),
    )
    .unwrap();
    let restored = Store::restore_backup(&backup, &destination).unwrap();
    assert_ne!(restored.head().unwrap().epoch, manifest.head.epoch);
}
