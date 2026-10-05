use super::*;

#[test]
fn plugin_gc_revision_tracks_generic_own_and_second_connection_writes() {
    let directory = tempfile::tempdir().unwrap();
    let store = PersistentStore::open(directory.path()).unwrap();
    let revision = || store.connection.query_row("SELECT revision FROM plugin_gc_revision WHERE singleton = 1", [], |row| row.get::<_, i64>(0)).unwrap();
    let initial = revision();
    store.connection.execute("INSERT INTO plugin_storage (generation,owner,storage_key,byte_size,ordinal,value) VALUES ('synthetic','synthetic','key',2,0,'{}')", []).unwrap();
    assert_eq!(revision(), initial + 1);
    store.connection.execute("UPDATE plugin_storage SET value = '[]' WHERE generation = 'synthetic'", []).unwrap();
    assert_eq!(revision(), initial + 2);
    let second = rusqlite::Connection::open(database_path(directory.path())).unwrap();
    second.execute("DELETE FROM plugin_storage WHERE generation = 'synthetic'", []).unwrap();
    assert_eq!(revision(), initial + 3);
    drop(second);
    drop(store);
    let reopened = PersistentStore::open(directory.path()).unwrap();
    assert_eq!(reopened.connection.query_row("SELECT revision FROM plugin_gc_revision WHERE singleton = 1", [], |row| row.get::<_, i64>(0)).unwrap(), initial + 3);
}

fn database_path(directory: &Path) -> PathBuf {
    directory.join("persistent").join("persistent.sqlite")
}

fn assert_payload_alias_schema(connection: &rusqlite::Connection) {
    assert_eq!(
        table_columns(connection, "asset_aliases"),
        vec![
            ("generation".to_owned(), "TEXT".to_owned(), true, None, 1),
            ("logical_key".to_owned(), "TEXT".to_owned(), true, None, 3),
            ("object_hash".to_owned(), "TEXT".to_owned(), false, None, 0),
            ("kind".to_owned(), "TEXT".to_owned(), true, None, 2),
            ("size".to_owned(), "INTEGER".to_owned(), true, None, 0),
            ("mime".to_owned(), "TEXT".to_owned(), true, None, 0),
            ("name".to_owned(), "TEXT".to_owned(), true, None, 0),
            ("ext".to_owned(), "TEXT".to_owned(), true, None, 0),
            ("inlay_type".to_owned(), "TEXT".to_owned(), false, None, 0),
            ("width".to_owned(), "INTEGER".to_owned(), false, None, 0),
            ("height".to_owned(), "INTEGER".to_owned(), false, None, 0),
            (
                "metadata".to_owned(),
                "TEXT".to_owned(),
                true,
                Some("'{}'".to_owned()),
                0,
            ),
        ]
    );
    assert_eq!(
        table_columns(connection, "asset_owner_heads"),
        vec![
            ("generation".to_owned(), "TEXT".to_owned(), true, None, 1),
            ("owner_kind".to_owned(), "TEXT".to_owned(), true, None, 2),
            ("owner_locator".to_owned(), "TEXT".to_owned(), true, None, 3),
            ("present".to_owned(), "INTEGER".to_owned(), true, None, 0),
            (
                "manifest_hash".to_owned(),
                "TEXT".to_owned(),
                false,
                None,
                0
            ),
            (
                "entry_count".to_owned(),
                "INTEGER".to_owned(),
                true,
                None,
                0
            ),
        ]
    );
    assert_eq!(
        table_columns(connection, "asset_alias_replacement_candidates"),
        vec![
            ("generation".to_owned(), "TEXT".to_owned(), true, None, 1),
            ("kind".to_owned(), "TEXT".to_owned(), true, None, 2),
            ("logical_key".to_owned(), "TEXT".to_owned(), true, None, 3),
            ("object_hash".to_owned(), "TEXT".to_owned(), true, None, 4),
            ("byte_size".to_owned(), "INTEGER".to_owned(), true, None, 0),
        ]
    );
}

fn assert_asset_object_schema(connection: &rusqlite::Connection) {
    assert_eq!(
        table_columns(connection, "asset_objects"),
        vec![
            ("object_hash".to_owned(), "TEXT".to_owned(), false, None, 1),
            ("byte_size".to_owned(), "INTEGER".to_owned(), true, None, 0),
            (
                "created_at_ms".to_owned(),
                "INTEGER".to_owned(),
                true,
                None,
                0
            ),
        ]
    );
    assert_eq!(
        table_columns(connection, "asset_object_deletions"),
        vec![
            ("object_hash".to_owned(), "TEXT".to_owned(), false, None, 1),
            ("byte_size".to_owned(), "INTEGER".to_owned(), true, None, 0),
            ("physical_key".to_owned(), "TEXT".to_owned(), true, None, 0),
            ("state".to_owned(), "TEXT".to_owned(), true, None, 0),
            (
                "created_at_ms".to_owned(),
                "INTEGER".to_owned(),
                true,
                None,
                0
            ),
        ]
    );
    assert_eq!(
        table_columns(connection, "asset_gc_maintenance_state"),
        vec![
            ("singleton".to_owned(), "INTEGER".to_owned(), false, None, 1),
            (
                "catalog_cursor".to_owned(),
                "TEXT".to_owned(),
                false,
                None,
                0
            ),
        ]
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM asset_gc_maintenance_state WHERE singleton = 1
                 AND catalog_cursor IS NULL",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count asset GC maintenance rows"),
        1
    );
}

fn assert_no_peer_schema(connection: &rusqlite::Connection) {
    let count: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name LIKE 'logical_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "fresh and reopened stores must not contain peer tables or indexes"
    );
}

#[test]
fn empty_database_creates_the_whole_current_schema() {
    let directory = tempfile::tempdir().expect("create fresh current directory");
    let store = PersistentStore::open(directory.path()).expect("open fresh current store");

    assert_eq!(
        store
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("read fresh schema version"),
        1
    );
    assert_payload_alias_schema(&store.connection);
    assert_asset_object_schema(&store.connection);
    assert_no_peer_schema(&store.connection);
}

#[test]
fn existing_current_database_revalidates_on_reopen() {
    let directory = tempfile::tempdir().expect("create current reopen directory");
    let store = PersistentStore::open(directory.path()).expect("create current store");
    drop(store);

    let store = PersistentStore::open(directory.path()).expect("reopen current store");
    assert_eq!(
        store
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("read reopened schema version"),
        1
    );
    assert_payload_alias_schema(&store.connection);
    assert_asset_object_schema(&store.connection);
    assert_no_peer_schema(&store.connection);
    drop(store);

    let connection =
        rusqlite::Connection::open(database_path(directory.path())).expect("open current database");
    connection
        .execute_batch("DELETE FROM asset_gc_maintenance_state;")
        .expect("clear asset GC maintenance state");
    drop(connection);

    let Err(error) = PersistentStore::open(directory.path()) else {
        panic!("reopen must revalidate the current schema");
    };
    assert_eq!(
        error,
        StoreError::Validation {
            message: "asset GC maintenance state row is invalid".to_owned(),
        }
    );
}

#[test]
fn a_broken_device_store_is_reported_without_blocking_the_library() {
    let directory = tempfile::tempdir().expect("create device failure directory");
    let mut store = PersistentStore::open(directory.path()).expect("create current store");
    assert!(store.device_store().is_ok());
    assert!(store
        .open_native_job_store()
        .expect("open native job store")
        .device_store()
        .is_ok());
    let stage = store.replace_begin().unwrap().staging_id;
    store.replace_put_root(&stage, &json!({"username":"protected stage"})).unwrap();
    drop(store);

    let device_path = directory
        .path()
        .join("persistent")
        .join(super::super::device_store::DEVICE_DATABASE_FILE);
    let connection = rusqlite::Connection::open(&device_path).expect("open device database");
    connection
        .execute_batch("PRAGMA user_version = 17;")
        .expect("stamp unknown device schema version");
    drop(connection);

    let mut store =
        PersistentStore::open(directory.path()).expect("library opens without the device store");
    assert_eq!(store.revision().expect("read library revision"), 0);
    let Err(error) = store.device_store() else {
        panic!("a damaged device store must be reported");
    };
    assert_eq!(
        error,
        StoreError::SchemaMismatch {
            message: "device store is unavailable: unsupported device schema version 17".to_owned(),
        }
    );
    assert_eq!(store.connection.query_row(
        "SELECT count(*) FROM root WHERE generation=?1", [&stage], |row| row.get::<_, i64>(0),
    ).unwrap(), 1);
    assert!(store.commit(&WorkingSetCommit {
        root: Some(json!({"username":"must not commit"})),
        ..empty_working_set_commit(0)
    }).is_err());
    assert_eq!(store.revision().unwrap(), 0);
}

#[test]
fn a_store_written_by_another_build_reports_the_schema_mismatch_code() {
    let code = |error: &StoreError| serde_json::to_value(error).unwrap()["code"].clone();
    for change in [
        "PRAGMA user_version = 17;",
        "ALTER TABLE generations ADD COLUMN synthetic TEXT;",
        "ALTER TABLE lww_retired ADD COLUMN synthetic TEXT;",
        "ALTER TABLE server_sync_state ADD COLUMN synthetic TEXT;",
        "ALTER TABLE content_change_floor ADD COLUMN synthetic TEXT;",
        "ALTER TABLE library_sync_selection ADD COLUMN synthetic TEXT;",
    ] {
        let directory = tempfile::tempdir().expect("create mismatch directory");
        drop(PersistentStore::open(directory.path()).expect("create current store"));
        let connection = rusqlite::Connection::open(database_path(directory.path())).expect("open current database");
        connection.execute_batch(change).expect("write another build's schema");
        drop(connection);
        let Err(error) = PersistentStore::open(directory.path()) else {
            panic!("{change} must be rejected");
        };
        assert_eq!(code(&error), "schema-mismatch", "{change}");
    }

    let directory = tempfile::tempdir().expect("create device mismatch directory");
    drop(PersistentStore::open(directory.path()).expect("create current store"));
    let device_path = directory.path().join("persistent").join(super::super::device_store::DEVICE_DATABASE_FILE);
    let connection = rusqlite::Connection::open(&device_path).expect("open device database");
    connection.execute_batch("PRAGMA user_version = 17;").expect("stamp another build's device schema");
    drop(connection);
    let store = PersistentStore::open(directory.path()).expect("library opens without the device store");
    let Err(error) = store.device_store() else {
        panic!("another build's device store must be reported");
    };
    assert_eq!(code(&error), "schema-mismatch");
}

#[test]
fn unknown_schema_version_is_rejected() {
    for version in [2_i64, 3, 4, 5, 17] {
        let directory = tempfile::tempdir().expect("create unknown version directory");
        let store = PersistentStore::open(directory.path()).expect("create current store");
        drop(store);

        let connection = rusqlite::Connection::open(database_path(directory.path()))
            .expect("open current database");
        connection
            .execute_batch(&format!("PRAGMA user_version = {version};"))
            .expect("stamp unknown schema version");
        drop(connection);

        let Err(error) = PersistentStore::open(directory.path()) else {
            panic!("unknown schema version {version} must be rejected");
        };
        assert_eq!(
            error,
            StoreError::SchemaMismatch {
                message: format!("unsupported persistent schema version {version}"),
            }
        );
    }
}
