mod common;
use common::*;
use risunest_sync_server::store::Store;
use risunest_sync_wire::hash;

#[test]
#[ignore]
fn child_exit_without_destructors() {
    let path = std::env::var_os("RISUNEST_SYNTHETIC_CRASH_ROOT").unwrap();
    let store = Store::open(std::path::Path::new(&path)).unwrap();
    let actor = device(&store);
    store
        .push(
            &actor,
            &request(
                &store,
                WRITER_A,
                "abrupt",
                vec![inline("key", WRITER_A, 1, "synthetic")],
            ),
        )
        .unwrap();
    std::process::exit(0);
}
#[test]
fn abrupt_process_exit_recovers_atomic_lww_state_journal_and_receipt() {
    let root = tempfile::tempdir().unwrap();
    drop(Store::init(root.path()).unwrap());
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_exit_without_destructors", "--ignored"])
        .env("RISUNEST_SYNTHETIC_CRASH_ROOT", root.path())
        .output()
        .unwrap();
    assert!(child.status.success());
    let store = Store::open(root.path()).unwrap();
    assert_eq!(store.head().unwrap().seq.as_str(), "1");
    let db = rusqlite::Connection::open(root.path().join("metadata.sqlite")).unwrap();
    for table in [
        "units",
        "journal",
        "operations",
        "writers",
        "writer_versions",
    ] {
        assert_eq!(
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}
#[test]
fn corrupt_or_unregistered_object_is_never_served() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let a = device(&store);
    // Above the inline threshold, so this one is served from a file.
    let body = vec![b'o'; 128 * 1024];
    let digest = hash(&body);
    store.put_object(&a, &digest, &body).unwrap();
    std::fs::write(
        dir.path().join("objects").join(&digest[..2]).join(&digest),
        b"broken",
    )
    .unwrap();
    assert_eq!(
        store.get_object(&digest).unwrap_err().code,
        "corrupt-object"
    );
    let inline = hash(b"object");
    store.put_object(&a, &inline, b"object").unwrap();
    rusqlite::Connection::open(dir.path().join("metadata.sqlite"))
        .unwrap()
        .execute(
            "UPDATE small_objects SET body=?1 WHERE hash=?2",
            (b"broken".as_slice(), &inline),
        )
        .unwrap();
    assert_eq!(
        store.get_object(&inline).unwrap_err().code,
        "corrupt-object"
    );
    let unregistered = hash(b"orphan");
    let path = dir.path().join("objects").join(&unregistered[..2]);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join(&unregistered), b"orphan").unwrap();
    assert_eq!(
        store.get_object(&unregistered).unwrap_err().code,
        "object-not-found"
    );
}

#[test]
fn failed_staging_write_never_registers_an_object_or_changes_the_head() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let a = device(&store);
    let head = store.head().unwrap();
    let staging = dir.path().join("staging");
    std::fs::remove_dir(&staging).unwrap();
    std::fs::write(&staging, b"synthetic unavailable storage").unwrap();
    // The staging directory only carries bodies that become files.
    let body = vec![b'x'; 128 * 1024];
    assert!(store.put_object(&a, &hash(&body), &body).is_err());
    assert!(store.object_size(&hash(&body)).unwrap().is_none());
    assert_eq!(store.head().unwrap(), head);
}
