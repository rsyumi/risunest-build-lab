//! Only validated records enter this private, disk-backed preparation.
//! SQLite bounds its page cache; activation materializes one record at a time.
use super::*;

fn staging_directory(root: &std::path::Path) -> StoreResult<std::path::PathBuf> {
    let server = root.join("server-sync");
    let staging = server.join("staging");
    for directory in [&server, &staging] {
        match std::fs::symlink_metadata(directory) {
            Ok(metadata) if !metadata.is_dir() || crate::trust_boundary::is_link_like(&metadata) => {
                return Err(StoreError::Validation { message: "Unsafe receive staging directory".into() });
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(directory)?,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(staging)
}

pub(crate) fn sweep_staging(root: &std::path::Path) -> StoreResult<()> {
    for entry in std::fs::read_dir(staging_directory(root)?)? {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with("receive-staging-") { continue; }
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.is_file() && !crate::trust_boundary::is_link_like(&metadata) {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

pub(crate) struct ValidatedRecords {
    db: rusqlite::Connection,
    _file: tempfile::NamedTempFile,
    len: usize,
    plugins_changed: bool,
}
impl ValidatedRecords {
    pub fn new(root: &std::path::Path) -> StoreResult<Self> {
        let directory = staging_directory(root)?;
        let file = tempfile::Builder::new().prefix("receive-staging-").tempfile_in(directory)?;
        let db = rusqlite::Connection::open(file.path())?;
        db.execute_batch(
            "PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-1024; PRAGMA mmap_size=0;
            CREATE TABLE records(key TEXT PRIMARY KEY,priority INTEGER NOT NULL,body TEXT NOT NULL,digest TEXT NOT NULL);
            CREATE INDEX records_order ON records(priority,key);",
        )?;
        Ok(Self {
            db,
            _file: file,
            len: 0,
            plugins_changed: false,
        })
    }
    pub fn push(&mut self, record: ValidatedRecord) -> StoreResult<()> {
        let priority = match (&record.record.payload, &record.locator) {
            (None, _) => 0,
            (Some(_), LogicalRecordLocator::Root) => 1,
            (Some(_), LogicalRecordLocator::Character { .. }) => 2,
            _ => 3,
        };
        let body = serde_json::to_string(&record.record)?;
        self.db.execute(
            "INSERT INTO records VALUES(?1,?2,?3,?4)",
            params![
                record.record.key,
                priority,
                body,
                risunest_sync_wire::hash(body.as_bytes())
            ],
        )?;
        self.len += 1;
        self.plugins_changed |= matches!(record.locator,
            LogicalRecordLocator::Root | LogicalRecordLocator::Plugin { .. });
        Ok(())
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn affects_plugins(&self) -> bool {
        self.plugins_changed
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn clear(&mut self) -> StoreResult<()> {
        self.db.execute("DELETE FROM records", [])?;
        self.len = 0;
        self.plugins_changed = false;
        Ok(())
    }
    pub fn deletes(&self, key: &str) -> StoreResult<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM records WHERE key=?1 AND priority=0)",
            [key],
            |r| r.get(0),
        )?)
    }
    pub fn visit(
        &self,
        only_deletions: bool,
        mut operation: impl FnMut(ValidatedRecord) -> StoreResult<()>,
    ) -> StoreResult<()> {
        let mut statement = self.db.prepare(if only_deletions {
            "SELECT key,body,digest FROM records WHERE priority=0 ORDER BY key"
        } else {
            "SELECT key,body,digest FROM records ORDER BY priority,key"
        })?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let body: String = row.get(1)?;
            if risunest_sync_wire::hash(body.as_bytes()) != row.get::<_, String>(2)? {
                return invalid("Server preparation content changed");
            }
            let record: RemoteRecord = serde_json::from_str(&body)?;
            if record.key != key {
                return invalid("Server preparation key changed");
            }
            let locator =
                decode_logical_record_key(&record.key).map_err(|_| StoreError::Validation {
                    message: "Invalid server preparation key".into(),
                })?;
            operation(ValidatedRecord { record, locator })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn server_sync_staging_sweep_after_reopen_removes_only_abandoned_receive_files() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::persistent_store::PersistentStore::open(root.path()).unwrap();
        let revision = store.revision().unwrap();
        let staged = ValidatedRecords::new(root.path()).unwrap();
        let path = staged._file.path().to_owned();
        assert_eq!(path.parent().unwrap(), root.path().join("server-sync/staging"));
        let ValidatedRecords { db, _file: file, .. } = staged;
        drop(db);
        let (file, kept_path) = file.keep().unwrap();
        drop(file);
        assert_eq!(kept_path, path);
        let unrelated = path.parent().unwrap().join("other-job.sqlite");
        std::fs::write(&unrelated, b"synthetic unrelated job").unwrap();
        let directory = path.parent().unwrap().join("receive-staging-directory");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("keep"), b"synthetic nested file").unwrap();
        drop(store);

        let reopened = crate::persistent_store::PersistentStore::open(root.path()).unwrap();
        assert!(path.is_file());
        sweep_staging(reopened.repository_root()).unwrap();
        sweep_staging(reopened.repository_root()).unwrap();
        assert!(!path.exists());
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"synthetic unrelated job");
        assert_eq!(std::fs::read(directory.join("keep")).unwrap(), b"synthetic nested file");
        assert_eq!(reopened.revision().unwrap(), revision);
        let next = ValidatedRecords::new(reopened.repository_root()).unwrap();
        let next_path = next._file.path().to_owned();
        assert!(next_path.is_file());
        drop(next);
        assert!(!next_path.exists());
    }

    #[test]
    fn server_sync_staging_pages_payloads_and_rejects_corruption_before_delivery() {
        let root = tempfile::tempdir().unwrap();
        let cas = PayloadCas::new(root.path()).unwrap();
        let mut staged = ValidatedRecords::new(root.path()).unwrap();
        for i in 0..256u64 {
            let locator = LogicalRecordLocator::Plugin {
                owner: "synthetic-plugin".to_owned(),
                storage_key: format!("synthetic-{i:04}"),
            };
            let record = RemoteRecord {
                key: encode_logical_record_key(&locator).unwrap(),
                version: RecordVersion::Live {
                    object_hash: "a".repeat(64),
                    descriptor_hash: None,
                },
                payload: Some(ServerPayload {
                    record: LogicalRecordEnvelope::Plugin {
                        owner: "synthetic-plugin".to_owned(),
                        ordinal: i,
                        value: Value::String("x".repeat(16 * 1024)),
                    },
                    messages: None,
                    derived_objects: Default::default(),
                }),
                local_hash: None,
            };
            staged.push(validate_remote(record, &cas).unwrap()).unwrap();
        }
        assert_eq!(staged.len(), 256);
        assert!(staged.affects_plugins());
        assert!(staged._file.as_file().metadata().unwrap().len() > 4 * 1024 * 1024);
        let mut delivered_ordinals = BTreeSet::new();
        staged
            .visit(false, |item| {
                let Some(ServerPayload {
                    record: LogicalRecordEnvelope::Plugin { ordinal, value, .. },
                    ..
                }) = item.record.payload
                else {
                    panic!("wrong staged family")
                };
                let LogicalRecordLocator::Plugin { storage_key, .. } = item.locator else {
                    panic!("wrong locator")
                };
                assert_eq!(storage_key, format!("synthetic-{ordinal:04}"));
                assert_eq!(value.as_str().unwrap().len(), 16 * 1024);
                assert!(delivered_ordinals.insert(ordinal));
                Ok(())
            })
            .unwrap();
        assert_eq!(delivered_ordinals, (0..256).collect());
        staged
            .db
            .execute(
                "UPDATE records SET body=replace(body,'xxxxxxxx','yyyyyyyy')",
                [],
            )
            .unwrap();
        let mut delivered = 0;
        assert!(staged
            .visit(false, |_| {
                delivered += 1;
                Ok(())
            })
            .is_err());
        assert_eq!(delivered, 0);
        let path = staged._file.path().to_owned();
        drop(staged);
        assert!(!path.exists());
    }
}
