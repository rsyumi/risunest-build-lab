//! Process-owned lease receipts. The lifetime lock precedes the startup snapshot.
use super::contract::{ErrorKind, ProviderError, Result};
use risunest_external_storage_format::control::LeaseDocument;
use fs2::FileExt;
use rusqlite::{params, Connection};
use std::{collections::BTreeSet, fs::{File, OpenOptions}, path::Path, sync::Mutex};

fn failed(error: impl std::fmt::Display) -> ProviderError { ProviderError::new(ErrorKind::Transient).caused(&error) }
fn corrupt() -> ProviderError { ProviderError::new(ErrorKind::Corrupt) }
type Identity = (String, String, String);
pub(crate) struct LocalLeaseLedger {
    db: Mutex<Connection>,
    abandoned: BTreeSet<Identity>,
    _lock: File,
}
impl LocalLeaseLedger {
    pub(crate) fn open(cache: &Path) -> Result<Self> {
        let directory = cache.join("external-storage").join("lease-ownership");
        std::fs::create_dir_all(&directory).map_err(failed)?;
        if !super::leftovers::managed_directory(cache, &directory).map_err(failed)? { return Err(corrupt()); }
        let lock_path = directory.join("owner.lock");
        if lock_path.exists() { crate::trust_boundary::open_regular_source(&lock_path).map_err(failed)?; }
        let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path).map_err(failed)?;
        lock.try_lock_exclusive().map_err(|_| ProviderError::new(ErrorKind::RepositoryBusy))?;
        let path = directory.join("leases.sqlite");
        if path.exists() { crate::trust_boundary::open_regular_source(&path).map_err(failed)?; }
        let db = crate::sqlite_open::open(path).map_err(failed)?;
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS leases(repository TEXT NOT NULL, connection TEXT NOT NULL,
            object TEXT NOT NULL, document BLOB NOT NULL, PRIMARY KEY(repository,connection,object));").map_err(failed)?;
        let abandoned = {
            let mut query = db.prepare("SELECT repository,connection,object FROM leases").map_err(failed)?;
            let rows = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).map_err(failed)?;
            rows.collect::<std::result::Result<BTreeSet<_>, _>>().map_err(failed)?
        };
        Ok(Self { db: Mutex::new(db), abandoned, _lock: lock })
    }
    pub(crate) fn record(&self, repository: &str, connection: &str, object: &str, document: &LeaseDocument) -> Result<()> {
        let bytes = document.encode(16 * 1024).map_err(failed)?;
        self.db.lock().map_err(failed)?.execute("INSERT INTO leases VALUES(?1,?2,?3,?4)",params![repository,connection,object,bytes]).map_err(failed)?;
        Ok(())
    }
    pub(crate) fn abandoned(&self, repository: &str, connection: &str, object: &str, document: &LeaseDocument) -> Result<bool> {
        if !self.abandoned.contains(&(repository.into(),connection.into(),object.into())) { return Ok(false); }
        use rusqlite::OptionalExtension;
        let bytes: Option<Vec<u8>> = self.db.lock().map_err(failed)?.query_row(
            "SELECT document FROM leases WHERE repository=?1 AND connection=?2 AND object=?3",
            params![repository,connection,object], |row| row.get(0)).optional().map_err(failed)?;
        Ok(bytes.is_some_and(|bytes| LeaseDocument::decode(&bytes,16 * 1024).ok().as_ref() == Some(document)))
    }
    pub(crate) fn retire_absent(&self, repository: &str, connection: &str, present: &BTreeSet<String>) -> Result<()> {
        // Forgetting an absent old receipt is conservative: a late remote object
        // remains foreign, never permission to bypass its protection.
        let mut db = self.db.lock().map_err(failed)?;
        let tx = db.transaction().map_err(failed)?;
        for (owned_repository, owned_connection, object) in &self.abandoned {
            if owned_repository == repository && owned_connection == connection && !present.contains(object) {
                tx.execute("DELETE FROM leases WHERE repository=?1 AND connection=?2 AND object=?3", params![repository,connection,object]).map_err(failed)?;
            }
        }
        tx.commit().map_err(failed)
    }
    pub(crate) fn released(&self, repository: &str, connection: &str, object: &str) -> Result<()> {
        self.db.lock().map_err(failed)?.execute("DELETE FROM leases WHERE repository=?1 AND connection=?2 AND object=?3", params![repository,connection,object]).map_err(failed)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn document() -> LeaseDocument {
        LeaseDocument::new("writer".into(),"operation".into(),risunest_external_storage_format::control::LeaseKind::Work,0,1_700_000_000_000).unwrap()
    }
    #[test]
    fn absent_startup_receipts_retire_without_touching_new_live_records() {
        let root=tempfile::tempdir().unwrap();
        let old=LocalLeaseLedger::open(root.path()).unwrap();
        let document=document();
        old.record("repository","connection","abandoned",&document).unwrap();
        drop(old);
        let current=LocalLeaseLedger::open(root.path()).unwrap();
        current.record("repository","connection","live",&document).unwrap();
        current.retire_absent("repository","connection",&BTreeSet::new()).unwrap();
        assert!(!current.abandoned("repository","connection","abandoned",&document).unwrap());
        drop(current);
        let next=LocalLeaseLedger::open(root.path()).unwrap();
        assert!(next.abandoned("repository","connection","live",&document).unwrap());
    }

    #[test]
    fn live_lock_prevents_second_owner_and_only_reopen_marks_abandonment() {
        let root=tempfile::tempdir().unwrap();
        let ledger=LocalLeaseLedger::open(root.path()).unwrap();
        let document=document();
        ledger.record("repository","connection","object",&document).unwrap();
        assert!(!ledger.abandoned("repository","connection","object",&document).unwrap());
        assert!(matches!(LocalLeaseLedger::open(root.path()),Err(error) if error.kind==ErrorKind::RepositoryBusy));
        drop(ledger);
        let ledger=LocalLeaseLedger::open(root.path()).unwrap();
        assert!(ledger.abandoned("repository","connection","object",&document).unwrap());
        assert!(!ledger.abandoned("other","connection","object",&document).unwrap());
        assert!(!ledger.abandoned("repository","other","object",&document).unwrap());
        assert!(!ledger.abandoned("repository","connection","unrecorded",&document).unwrap());
        let mut changed=document.clone(); changed.seq+=1;
        assert!(!ledger.abandoned("repository","connection","object",&changed).unwrap());
        ledger.released("repository","connection","object").unwrap();
        assert!(!ledger.abandoned("repository","connection","object",&document).unwrap());
    }
}
