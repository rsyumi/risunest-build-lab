use super::PersistentStore;
use crate::server_sync::lww_tests::{local, put_asset};
use serde_json::Value;

fn library_collections() -> usize {
    super::super::ASSET_GC_LIBRARY_ROOT_COLLECTIONS.with(|count| count.get())
}

fn status(store: &PersistentStore) -> Value {
    serde_json::to_value(store.asset_residency_status().unwrap()).unwrap()
}

fn local_bytes(status: &Value) -> u64 {
    status["localBytes"].as_u64().unwrap()
}

/// Stores `bytes` as a body no record references yet.
fn unreferenced_body(store: &PersistentStore, bytes: &[u8]) -> String {
    crate::asset_repository::PayloadCas::new(store.repository_root()).unwrap().prepare_bytes(bytes).unwrap().content_hash
}

/// References `hash` from a plugin storage row, the way a plugin value naming an object does.
fn reference(connection: &rusqlite::Connection, storage_key: &str, hash: &str) {
    connection
        .execute(
            "INSERT INTO plugin_storage (generation, owner, storage_key, byte_size, ordinal, value)
             VALUES ('status-cache', 'synthetic', ?1, 0, 0, ?2)",
            [storage_key, &serde_json::json!({ "object": hash }).to_string()],
        )
        .unwrap();
}

/// The store and every job store opened from it read the library once for repeated
/// statuses, and read it again after a commit from any connection.
#[test]
fn residency_statuses_reuse_the_library_roots_until_a_commit_reaches_the_library() {
    let (_root, mut store) = local();
    let first = b"synthetic status body one".to_vec();
    let second = b"synthetic status body two, a little longer".to_vec();
    let third = b"synthetic status body three, longer than the others".to_vec();
    put_asset(&mut store, "assets/status-cache-one.png", &first);
    super::super::ASSET_GC_LIBRARY_ROOT_COLLECTIONS.with(|count| count.set(0));

    let cold = status(&store);
    assert_eq!(local_bytes(&cold), first.len() as u64);
    assert_eq!(status(&store), cold);
    let job = store.open_native_job_store().unwrap();
    assert_eq!(status(&job), cold);
    assert_eq!(job.asset_residency_connection_objects("unknown-connection").unwrap(), 0);
    assert_eq!(library_collections(), 1);

    put_asset(&mut store, "assets/status-cache-two.png", &second);
    let both = status(&job);
    assert_eq!(local_bytes(&both), (first.len() + second.len()) as u64);
    assert_eq!(status(&store), both);
    assert_eq!(library_collections(), 2);

    let hash = unreferenced_body(&store, &third);
    reference(&job.connection, "status-cache-three", &hash);
    assert_eq!(local_bytes(&status(&job)), (first.len() + second.len() + third.len()) as u64);
    assert_eq!(library_collections(), 3);

    let unrelated = rusqlite::Connection::open(&store.database_path).unwrap();
    unrelated.execute("DELETE FROM plugin_storage WHERE storage_key = 'status-cache-three'", []).unwrap();
    assert_eq!(status(&store), both);
    assert_eq!(library_collections(), 4);
    assert_eq!(status(&job), both);
    assert_eq!(library_collections(), 4);
}

/// A status taken inside an open transaction counts what that transaction wrote, and the
/// library roots read before it still serve the next status after a rollback.
#[test]
fn a_residency_status_inside_a_transaction_reads_its_uncommitted_writes() {
    let (_root, mut store) = local();
    let first = b"synthetic transaction body one".to_vec();
    let second = b"synthetic transaction body two, a little longer".to_vec();
    put_asset(&mut store, "assets/status-transaction-one.png", &first);
    let hash = unreferenced_body(&store, &second);
    super::super::ASSET_GC_LIBRARY_ROOT_COLLECTIONS.with(|count| count.set(0));

    let before = status(&store);
    assert_eq!(local_bytes(&before), first.len() as u64);
    store.connection.execute_batch("BEGIN").unwrap();
    reference(&store.connection, "status-transaction-two", &hash);
    assert_eq!(local_bytes(&status(&store)), (first.len() + second.len()) as u64);
    assert_eq!(library_collections(), 2);
    store.connection.execute_batch("ROLLBACK").unwrap();

    assert_eq!(status(&store), before);
    assert_eq!(library_collections(), 2);
}

/// The connection that watches the library for commits never creates one.
#[test]
fn the_status_cache_watches_nothing_when_the_library_is_absent() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("absent.sqlite");
    assert!(super::super::StatusLibraryRoots::default().take(&path).is_none());
    assert!(!path.exists());
}
