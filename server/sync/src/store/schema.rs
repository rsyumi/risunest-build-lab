pub const VERSION: i64 = 1;

pub const SCHEMA: &str = r#"
CREATE TABLE library (singleton INTEGER PRIMARY KEY CHECK(singleton=1), head TEXT NOT NULL);
CREATE TABLE media_secret(singleton INTEGER PRIMARY KEY CHECK(singleton=1),key BLOB NOT NULL CHECK(length(key)=32));
CREATE TABLE devices (
 id TEXT PRIMARY KEY, verifier TEXT NOT NULL UNIQUE, revoked INTEGER NOT NULL DEFAULT 0,
 ack TEXT NOT NULL DEFAULT '0',
 name TEXT NOT NULL DEFAULT '', registration_request TEXT UNIQUE, last_ack INTEGER
);
CREATE TABLE objects (hash TEXT PRIMARY KEY, size INTEGER NOT NULL CHECK(size>=0), storage TEXT NOT NULL CHECK(storage IN ('file','inline')));
CREATE TABLE object_leases(device TEXT NOT NULL REFERENCES devices(id),hash TEXT NOT NULL REFERENCES objects(hash),expires INTEGER NOT NULL,PRIMARY KEY(device,hash));
CREATE TABLE object_custody(device TEXT NOT NULL REFERENCES devices(id),hash TEXT NOT NULL REFERENCES objects(hash),retention_id TEXT NOT NULL,PRIMARY KEY(device,hash));
CREATE TABLE object_trash(hash TEXT PRIMARY KEY);
CREATE TABLE uploads (id TEXT PRIMARY KEY,device TEXT NOT NULL REFERENCES devices(id),hash TEXT NOT NULL,size INTEGER NOT NULL,expires INTEGER NOT NULL,state TEXT NOT NULL DEFAULT 'open');
CREATE INDEX uploads_device ON uploads(device);
CREATE TABLE upload_jobs(upload TEXT PRIMARY KEY REFERENCES uploads(id) ON DELETE CASCADE,retry_after INTEGER NOT NULL DEFAULT 0,terminal INTEGER NOT NULL DEFAULT 0,error TEXT,attempts INTEGER NOT NULL DEFAULT 0);
CREATE TABLE upload_deltas(upload TEXT PRIMARY KEY REFERENCES uploads(id) ON DELETE CASCADE,body BLOB NOT NULL);
CREATE TABLE upload_delta_bases(upload TEXT NOT NULL REFERENCES uploads(id) ON DELETE CASCADE,hash TEXT NOT NULL,PRIMARY KEY(upload,hash));
CREATE TABLE download_deltas(id TEXT PRIMARY KEY,device TEXT NOT NULL REFERENCES devices(id),request TEXT NOT NULL,expires INTEGER NOT NULL,state TEXT NOT NULL DEFAULT 'queued',body BLOB,error TEXT);
CREATE TABLE download_delta_bases(job TEXT NOT NULL REFERENCES download_deltas(id) ON DELETE CASCADE,hash TEXT NOT NULL,PRIMARY KEY(job,hash));
CREATE TABLE upload_chunks (upload TEXT NOT NULL REFERENCES uploads(id) ON DELETE CASCADE,ordinal INTEGER NOT NULL,hash TEXT NOT NULL,size INTEGER NOT NULL,PRIMARY KEY(upload,ordinal));
CREATE TABLE staging_trash(upload TEXT NOT NULL,ordinal INTEGER NOT NULL,PRIMARY KEY(upload,ordinal));
CREATE TABLE reference_nodes (hash TEXT PRIMARY KEY REFERENCES objects(hash), kind TEXT NOT NULL, item_count INTEGER NOT NULL, tree_depth INTEGER NOT NULL, first_value TEXT NOT NULL, last_value TEXT NOT NULL);
CREATE TABLE reference_children (root TEXT NOT NULL REFERENCES reference_nodes(hash), child TEXT NOT NULL REFERENCES reference_nodes(hash), PRIMARY KEY(root,child));
CREATE TABLE reference_objects (root TEXT NOT NULL REFERENCES reference_nodes(hash), object TEXT NOT NULL REFERENCES objects(hash), PRIMARY KEY(root,object));
CREATE TABLE reference_relations (root TEXT NOT NULL REFERENCES reference_nodes(hash), target TEXT NOT NULL, PRIMARY KEY(root,target));
CREATE TABLE descriptors (hash TEXT PRIMARY KEY REFERENCES objects(hash), object TEXT NOT NULL REFERENCES objects(hash), body TEXT NOT NULL);
CREATE TABLE writers(writer TEXT PRIMARY KEY,device TEXT NOT NULL);
CREATE TABLE device_writer_claims(device TEXT PRIMARY KEY,authorization TEXT NOT NULL UNIQUE,writer TEXT NOT NULL UNIQUE REFERENCES writers(writer),digest TEXT NOT NULL,body TEXT NOT NULL);
CREATE TABLE writer_versions(writer TEXT NOT NULL,key TEXT NOT NULL,physical TEXT NOT NULL,logical TEXT NOT NULL,identity TEXT NOT NULL,created INTEGER NOT NULL DEFAULT(unixepoch()),PRIMARY KEY(writer,key,physical,logical));
CREATE TABLE units(key TEXT PRIMARY KEY,body TEXT NOT NULL);
CREATE TABLE retired(key TEXT PRIMARY KEY,body TEXT NOT NULL);
CREATE TABLE unit_parents(parent TEXT NOT NULL,child TEXT NOT NULL,PRIMARY KEY(parent,child));
CREATE INDEX unit_parents_child ON unit_parents(child);
CREATE TABLE journal(seq TEXT PRIMARY KEY,key TEXT NOT NULL UNIQUE,body TEXT NOT NULL,created INTEGER NOT NULL DEFAULT(unixepoch()));
CREATE INDEX journal_cursor ON journal(length(seq),seq);
CREATE TABLE operations(device TEXT NOT NULL REFERENCES devices(id),operation TEXT NOT NULL,digest TEXT NOT NULL,body TEXT NOT NULL,error_status INTEGER,error_key TEXT,created INTEGER NOT NULL DEFAULT(unixepoch()),PRIMARY KEY(device,operation));
CREATE TABLE state_pins(id TEXT PRIMARY KEY,device TEXT NOT NULL REFERENCES devices(id),start_seq TEXT NOT NULL,expires INTEGER NOT NULL);
CREATE TABLE state_pin_units(pin TEXT NOT NULL REFERENCES state_pins(id) ON DELETE CASCADE,key TEXT NOT NULL,body TEXT NOT NULL,PRIMARY KEY(pin,key));
PRAGMA user_version=1;
"#;

pub fn verify(db: &rusqlite::Connection) -> crate::Result<()> {
    fn structure(
        db: &rusqlite::Connection,
    ) -> rusqlite::Result<Vec<(String, String, String, String)>> {
        let mut query = db.prepare("SELECT type,name,tbl_name,COALESCE(sql,'') FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name")?;
        let rows = query.query_map([], |row| {
            let sql: String = row.get(3)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                sql.split_whitespace().collect::<Vec<_>>().join(" "),
            ))
        })?;
        rows.collect()
    }
    let reference = rusqlite::Connection::open_in_memory()?;
    reference.execute_batch(SCHEMA)?;
    risunest_small_object_store::initialize(&reference)
        .map_err(|_| crate::Error::new("metadata-storage", 503))?;
    if structure(db)? != structure(&reference)? {
        return Err(crate::Error::new("incompatible-store", 409));
    }
    Ok(())
}
