//! Bulk intent inputs kept as device rows. The intent body carries one digest
//! per row set, so the body stays small and a replay proves the rows it reads.

use super::{error, Change, StoreResult};
use risunest_sync_wire::unit::{UnitKey, UnitValue};
use rusqlite::{params, Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;


const BATCH_ROWS: usize = 1024;
const PROJECTION_ORDER: &str = "CASE WHEN json_extract(key,'$[0]')='exists' AND json_extract(key,'$[1]')<>'conversation' THEN 0 WHEN json_extract(key,'$[0]')='exists' THEN 1 WHEN json_extract(key,'$[0]')='archive' THEN 2 ELSE 3 END,key";
pub(in crate::persistent_store) const BINDING_ORDER: &str = "CASE WHEN json_extract(key,'$[0]')='exists' AND json_extract(key,'$[1]')<>'conversation' THEN 0 WHEN json_extract(key,'$[0]')='archive' THEN 2 WHEN json_extract(key,'$[0]')='exists' THEN 3 WHEN json_extract(key,'$[0]') IN ('conversation','messages') THEN 4 ELSE 1 END,key";

/// The count and digest of one row set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::persistent_store) struct IntentRows {
    pub rows: u64,
    pub digest: String,
}

/// One row as it is stored. `stamp` is set for received changes only.
pub(in crate::persistent_store) struct RowText {
    pub key: String,
    pub stamp: Option<String>,
    pub value: String,
    pub source_override: bool,
}

struct RowDigest {
    hasher: Sha256,
    rows: u64,
}

impl RowDigest {
    fn new(kind: &str) -> Self {
        #[cfg(test)]
        crate::persistent_store::hash_work::begin("native_intent_rows");
        let mut digest = Self { hasher: Sha256::new(), rows: 0 };
        digest.update(b"risunest.lww-intent-rows/v1\0");
        digest.frame(kind.as_bytes());
        digest
    }

    fn update(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
        #[cfg(test)]
        crate::persistent_store::hash_work::update("native_intent_rows", bytes.len());
    }

    fn frame(&mut self, bytes: &[u8]) {
        self.update(&(bytes.len() as u64).to_le_bytes());
        self.update(bytes);
    }

    fn push(&mut self, key: &str, stamp: Option<&str>, value: &str, source_override: bool) {
        self.frame(key.as_bytes());
        match stamp {
            Some(stamp) => {
                self.update(&[1]);
                self.frame(stamp.as_bytes());
            }
            None => self.update(&[0]),
        }
        self.frame(value.as_bytes());
        self.update(&[u8::from(source_override)]);
        self.rows += 1;
    }

    fn finish(self) -> IntentRows {
        IntentRows { rows: self.rows, digest: hex::encode(self.hasher.finalize()) }
    }
}

pub(in crate::persistent_store) fn replacement_row(key: &UnitKey, value: &UnitValue, source_override: bool) -> StoreResult<RowText> {
    Ok(RowText { key: key.as_str().to_owned(), stamp: None, value: serde_json::to_string(value)?, source_override })
}

pub(in crate::persistent_store) fn target_row(change: &Change) -> StoreResult<RowText> {
    Ok(RowText {
        key: change.key.as_str().to_owned(),
        stamp: Some(serde_json::to_string(&change.stamp)?),
        value: serde_json::to_string(&change.value)?,
        source_override: false,
    })
}

/// The digest of rows that are not stored, such as the source units a
/// replacement only has to recognize when it is retried.
pub(in crate::persistent_store) fn digest(kind: &str, rows: impl IntoIterator<Item = StoreResult<RowText>>) -> StoreResult<IntentRows> {
    let mut digest = RowDigest::new(kind);
    for row in rows {
        let row = row?;
        digest.push(&row.key, row.stamp.as_deref(), &row.value, row.source_override);
    }
    Ok(digest.finish())
}

/// Stores `rows` for an intent that is not issued yet, in batches of device
/// transactions, and returns their digest. Rows an earlier attempt left
/// before it issued the intent are replaced.
pub(in crate::persistent_store) fn write(
    device: &mut Connection,
    request_id: &str,
    kind: &str,
    rows: impl IntoIterator<Item = StoreResult<RowText>>,
) -> StoreResult<IntentRows> {
    let mut digest = RowDigest::new(kind);
    let mut rows = rows.into_iter().peekable();
    let mut first = true;
    while first || rows.peek().is_some() {
        let tx = device.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if first {
            let issued: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM lww_intents WHERE request_id=?1)",
                [request_id],
                |row| row.get(0),
            )?;
            if issued {
                return Err(error("request-id-integrity"));
            }
            tx.execute("DELETE FROM lww_intent_rows WHERE request_id=?1", [request_id])?;
            first = false;
        }
        {
            let mut insert = tx.prepare_cached("INSERT INTO lww_intent_rows VALUES(?1,?2,?3,?4,?5,?6)")?;
            for row in rows.by_ref().take(BATCH_ROWS) {
                let row = row?;
                digest.push(&row.key, row.stamp.as_deref(), &row.value, row.source_override);
                insert.execute(params![
                    request_id,
                    digest.rows as i64,
                    row.key,
                    row.stamp,
                    row.value,
                    row.source_override
                ])?;
            }
        }
        tx.commit()?;
    }
    Ok(digest.finish())
}

/// Reads an issued intent's rows in order and proves them against `expected`.
/// What `visit` collects is used only when the whole set is proved.
fn read(
    device: &Connection,
    request_id: &str,
    kind: &str,
    expected: &IntentRows,
    mut visit: impl FnMut(&str, Option<&str>, &str, bool) -> StoreResult<()>,
) -> StoreResult<()> {
    let mut digest = RowDigest::new(kind);
    let mut statement = device.prepare(
        "SELECT ordinal,key,stamp,value,source_override FROM lww_intent_rows WHERE request_id=?1 ORDER BY ordinal",
    )?;
    let mut rows = statement.query([request_id])?;
    while let Some(row) = rows.next()? {
        let ordinal: i64 = row.get(0)?;
        let key: String = row.get(1)?;
        let stamp: Option<String> = row.get(2)?;
        let value: String = row.get(3)?;
        let source_override: bool = row.get(4)?;
        if ordinal != digest.rows as i64 + 1 {
            return Err(error("request-id-integrity"));
        }
        digest.push(&key, stamp.as_deref(), &value, source_override);
        visit(&key, stamp.as_deref(), &value, source_override)?;
    }
    if &digest.finish() != expected {
        return Err(error("request-id-integrity"));
    }
    Ok(())
}

/// A scoped disk-backed input. No caller can observe an unverified replay set.
pub(in crate::persistent_store) struct FrozenRows {
    pub(in crate::persistent_store) db: Connection,
    _directory: tempfile::TempDir,
}

impl FrozenRows {
    pub(in crate::persistent_store) fn new(root: &Path) -> StoreResult<Self> {
        let directory = crate::external_storage::leftovers::local_scratch(root, "frozen-rows-")?;
        // Android's SQLITE_TEMP_STORE=3 keeps an empty-filename database in RAM.
        let db = crate::sqlite_open::open(directory.path().join("rows.sqlite"))?;
        // Scratch is disposable; durable intents and activation keep their own sync settings.
        db.execute_batch(&format!("PRAGMA temp_store=FILE; PRAGMA cache_size=-2048; PRAGMA synchronous=OFF;
            CREATE TABLE rows(ordinal INTEGER PRIMARY KEY,key TEXT NOT NULL UNIQUE,stamp TEXT,value TEXT NOT NULL,source_override INTEGER NOT NULL);
            CREATE INDEX rows_projection_order ON rows({PROJECTION_ORDER});
            CREATE INDEX rows_binding_order ON rows({BINDING_ORDER}); BEGIN;"))?;
        Ok(Self { db, _directory: directory })
    }

    pub(in crate::persistent_store) fn insert(&self, row: RowText) -> StoreResult<()> {
        self.db.execute("INSERT INTO rows(key,stamp,value,source_override) VALUES(?1,?2,?3,?4)",
            params![row.key,row.stamp,row.value,row.source_override])?;
        Ok(())
    }

    pub(in crate::persistent_store) fn visit(&self, projection_order: bool, mut visit: impl FnMut(UnitKey, Option<super::Stamp>, UnitValue, bool) -> StoreResult<()>) -> StoreResult<()> {
        let order = if projection_order {
            PROJECTION_ORDER
        } else { "key" };
        let mut statement = self.db.prepare(&format!("SELECT key,stamp,value,source_override FROM rows ORDER BY {order}"))?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let stamp: Option<String> = row.get(1)?;
            let value: String = row.get(2)?;
            visit(super::wire(key.try_into())?, stamp.map(|stamp| serde_json::from_str(&stamp)).transpose()?, serde_json::from_str(&value)?, row.get(3)?)?;
        }
        Ok(())
    }

    pub(in crate::persistent_store) fn texts(&self) -> impl Iterator<Item = StoreResult<RowText>> + '_ {
        let mut after = String::new();
        let mut done = false;
        std::iter::from_fn(move || {
            if done { return None; }
            use rusqlite::OptionalExtension;
            let result = self.db.query_row("SELECT key,stamp,value,source_override FROM rows WHERE key>?1 ORDER BY key LIMIT 1", [&after],
                |row| Ok(RowText { key: row.get(0)?, stamp: row.get(1)?, value: row.get(2)?, source_override: row.get(3)? })).optional();
            match result {
                Ok(Some(row)) => { after = row.key.clone(); Some(Ok(row)) }
                Ok(None) => { done = true; None }
                Err(failure) => { done = true; Some(Err(failure.into())) }
            }
        })
    }

    pub(in crate::persistent_store) fn refresh_orders(&self, tx: &rusqlite::Transaction<'_>, generation: &str) -> StoreResult<()> {
        self.refresh_orders_observed(tx, generation, &mut || Ok(()))
    }

    pub(in crate::persistent_store) fn refresh_orders_observed(&self, tx: &rusqlite::Transaction<'_>, generation: &str, observe: &mut dyn FnMut() -> StoreResult<()>) -> StoreResult<()> {
        self.visit_order_scopes(&mut |key| {
            if let Some(key) = key { super::refresh_orders(tx, generation, &[key])?; }
            observe()
        })
    }

    pub(in crate::persistent_store) fn visit_order_scopes(&self, visit: &mut dyn FnMut(Option<UnitKey>) -> StoreResult<()>) -> StoreResult<()> {
        self.db.execute_batch("CREATE TABLE IF NOT EXISTS scopes(key TEXT PRIMARY KEY); DELETE FROM scopes;")?;
        self.visit(false, |key, _, _, _| {
            let p = key.components();
            let scope = match p[0].as_str() {
                "order" => Some(key),
                "exists" => Some(match p[1].as_str() {
                    "character" => super::unit_key(&["order", "characters"])?,
                    "conversation" => super::unit_key(&["order", "conversations", &p[2]])?,
                    "preset" => super::unit_key(&["order", "presets"])?,
                    "persona" => super::unit_key(&["order", "personas"])?,
                    other => super::unit_key(&["order", other])?,
                }),
                "record" => Some(super::unit_key(&["order", &p[1]])?),
                "plugin" => Some(super::unit_key(&["order", "plugin-storage", &p[1]])?),
                "character" if p.get(2).is_some_and(|field| field == "trashTime") => Some(super::unit_key(&["order", "characters"])?),
                _ => None,
            };
            if let Some(scope) = scope { self.db.execute("INSERT OR IGNORE INTO scopes VALUES(?1)", [scope.as_str()])?; }
            visit(None)
        })?;
        let mut statement = self.db.prepare("SELECT key FROM scopes ORDER BY key")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let key = super::wire(row.get::<_, String>(0)?.try_into())?;
            visit(Some(key))?;
        }
        Ok(())
    }
}

pub(in crate::persistent_store) fn read_replacement(device: &Connection, root: &Path, request_id: &str, expected: &IntentRows) -> StoreResult<FrozenRows> {
    let frozen = FrozenRows::new(root)?;
    read(device, request_id, "changes", expected, |key, stamp, value, source_override| {
        if stamp.is_some() { return Err(error("request-id-integrity")); }
        let _: UnitKey = super::wire(key.to_owned().try_into())?;
        let _: UnitValue = serde_json::from_str(value)?;
        frozen.insert(RowText { key: key.into(), stamp: None, value: value.into(), source_override })
    })?;
    Ok(frozen)
}

pub(in crate::persistent_store) fn read_target(device: &Connection, root: &Path, request_id: &str, expected: &IntentRows) -> StoreResult<FrozenRows> {
    let frozen = FrozenRows::new(root)?;
    read(device, request_id, "target", expected, |key, stamp, value, source_override| {
        let Some(stamp) = stamp.filter(|_| !source_override) else { return Err(error("request-id-integrity")); };
        let _: UnitKey = super::wire(key.to_owned().try_into())?;
        let _: super::Stamp = serde_json::from_str(stamp)?;
        let _: UnitValue = serde_json::from_str(value)?;
        frozen.insert(RowText { key: key.into(), stamp: Some(stamp.into()), value: value.into(), source_override: false })
    })?;
    Ok(frozen)
}

#[cfg(test)]
pub(in crate::persistent_store) fn source_digest(units: &std::collections::BTreeMap<UnitKey, UnitValue>) -> StoreResult<IntentRows> {
    digest("source", units.iter().map(|(key, value)| replacement_row(key, value, false)))
}

/// Drops the rows of every intent that is not waiting for a replay: those of
/// completed intents and those an attempt left before it issued its intent.
pub(in crate::persistent_store) fn delete_settled(device: &Connection) -> StoreResult<()> {
    device.execute(
        "DELETE FROM lww_intent_rows WHERE request_id NOT IN (SELECT request_id FROM lww_intents WHERE complete=0)",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_checks_every_row_before_exposing_the_frozen_input() {
        let root = tempfile::tempdir().unwrap();
        let device_path = root.path().join("device.sqlite");
        let mut db = crate::sqlite_open::open(&device_path).unwrap();
        db.execute_batch(super::super::DEVICE_SCHEMA).unwrap();
        let expected = write(&mut db, "synthetic", "changes", (0..1025).map(|index| {
            replacement_row(&super::super::unit_key(&["root", &format!("synthetic-{index:04}")])?, &super::super::inline(&serde_json::json!(index))?, index % 2 == 0)
        })).unwrap();
        let frozen = read_replacement(&db, root.path(), "synthetic", &expected).unwrap();
        let mut count = 0;
        frozen.visit(false, |key, stamp, value, source_override| {
            assert_eq!(key, super::super::unit_key(&["root", &format!("synthetic-{count:04}")]).unwrap());
            assert!(stamp.is_none());
            assert_eq!(value, super::super::inline(&serde_json::json!(count)).unwrap());
            assert_eq!(source_override, count % 2 == 0);
            count += 1;
            Ok(())
        }).unwrap();
        assert_eq!(count, 1025);
        assert_eq!(digest("changes", frozen.texts()).unwrap(), expected);
        let FrozenRows { db: scratch, _directory: directory } = frozen;
        drop(scratch);
        let orphan = directory.keep();
        drop(db);
        crate::external_storage::leftovers::remove_at_startup(root.path());
        assert!(!orphan.exists());
        let db = crate::sqlite_open::open(&device_path).unwrap();
        let frozen = read_replacement(&db, root.path(), "synthetic", &expected).unwrap();
        assert_eq!(digest("changes", frozen.texts()).unwrap(), expected);
        db.execute("UPDATE lww_intent_rows SET source_override=1-source_override WHERE request_id='synthetic' AND ordinal=1025", []).unwrap();
        assert!(read_replacement(&db, root.path(), "synthetic", &expected).is_err());
        db.execute("DELETE FROM lww_intent_rows WHERE request_id='synthetic' AND ordinal=1025", []).unwrap();
        assert!(read_replacement(&db, root.path(), "synthetic", &expected).is_err());
        let directory = frozen._directory.path().to_owned();
        let scratch_root = directory.parent().unwrap().to_owned();
        assert_eq!(std::fs::read_dir(&scratch_root).unwrap().count(), 1);
        drop(frozen);
        assert!(!directory.exists());
        assert_eq!(std::fs::read_dir(scratch_root).unwrap().count(), 0);
    }

    #[test]
    fn target_replay_preserves_stamps_and_rejects_a_late_invalid_row() {
        let root = tempfile::tempdir().unwrap();
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(super::super::DEVICE_SCHEMA).unwrap();
        let change = Change {
            key: super::super::unit_key(&["root","username"]).unwrap(),
            stamp: super::super::Stamp { physical_ms: 7.into(), logical: 9, writer_id: "00000000-0000-4000-8000-000000000001".into() },
            value: super::super::inline(&serde_json::json!("synthetic")).unwrap(),
        };
        let expected = write(&mut db, "target", "target", [target_row(&change)]).unwrap();
        let frozen = read_target(&db, root.path(), "target", &expected).unwrap();
        frozen.visit(false, |key, stamp, value, source_override| {
            assert_eq!((key,stamp,value,source_override), (change.key.clone(),Some(change.stamp.clone()),change.value.clone(),false));
            Ok(())
        }).unwrap();
        db.execute("UPDATE lww_intent_rows SET stamp=NULL WHERE request_id='target'", []).unwrap();
        assert!(read_target(&db, root.path(), "target", &expected).is_err());
    }

    #[test]
    fn frozen_rows_spill_and_order_large_values_with_a_bounded_cache() {
        if crate::test_process::run_isolated(concat!(module_path!(), "::frozen_rows_spill_and_order_large_values_with_a_bounded_cache")) {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        for count in [16, 64] {
            let frozen = FrozenRows::new(root.path()).unwrap();
            frozen.db.pragma_update(None, "cache_size", -64).unwrap();
            let value = super::super::inline(&serde_json::json!("x".repeat(16 * 1024))).unwrap();
            for index in (0..count).rev() {
                let key = super::super::unit_key(&["root", &format!("synthetic-{index:04}")]).unwrap();
                frozen.insert(replacement_row(&key, &value, false).unwrap()).unwrap();
            }
            let file = frozen._directory.path().join("rows.sqlite");
            assert!(std::fs::metadata(&file).unwrap().len() > 64 * 1024);
            let mut used = 0;
            let mut high = 0;
            let status = unsafe {
                rusqlite::ffi::sqlite3_db_status(frozen.db.handle(), rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED,
                    &mut used, &mut high, 0)
            };
            assert_eq!(status, rusqlite::ffi::SQLITE_OK);
            assert!(used < 256 * 1024, "{count} rows retained {used} cache bytes");
            for projection in [false, true] {
                let mut seen = 0;
                frozen.visit(projection, |key, stamp, actual, source_override| {
                    assert_eq!(key, super::super::unit_key(&["root", &format!("synthetic-{seen:04}")]).unwrap());
                    assert_eq!(actual, value);
                    assert!(stamp.is_none() && !source_override);
                    seen += 1;
                    Ok(())
                }).unwrap();
                assert_eq!(seen, count);
            }
            // A sorter would retain complete values in Android's TEMP_STORE=3.
            for order in [PROJECTION_ORDER, BINDING_ORDER] {
                let mut statement = frozen.db.prepare(&format!("SELECT key,value FROM rows ORDER BY {order}")).unwrap();
                let mut rows = statement.query([]).unwrap();
                let mut seen = 0;
                while let Some(row) = rows.next().unwrap() {
                    assert_eq!(row.get::<_, String>(0).unwrap(),
                        super::super::unit_key(&["root", &format!("synthetic-{seen:04}")]).unwrap().as_str());
                    assert_eq!(serde_json::from_str::<UnitValue>(&row.get::<_,String>(1).unwrap()).unwrap(), value);
                    seen += 1;
                }
                assert_eq!(seen, count);
                drop(rows);
                assert_eq!(statement.get_status(rusqlite::StatementStatus::Sort), 0);
            }
            let directory = frozen._directory.path().to_owned();
            drop(frozen);
            assert!(!directory.exists());
        }
    }

    #[test]
    fn full_scratch_returns_an_error_and_releases_its_files() {
        let root = tempfile::tempdir().unwrap();
        let frozen = FrozenRows::new(root.path()).unwrap();
        let pages: i64 = frozen.db.query_row("PRAGMA page_count", [], |row| row.get(0)).unwrap();
        frozen.db.pragma_update(None, "max_page_count", pages).unwrap();
        let key = super::super::unit_key(&["root", "synthetic"]).unwrap();
        let value = super::super::inline(&serde_json::json!("x".repeat(64 * 1024))).unwrap();
        let failure = frozen.insert(replacement_row(&key, &value, false).unwrap()).unwrap_err();
        assert!(failure.to_string().contains("full"), "{failure}");
        let directory = frozen._directory.path().to_owned();
        drop(frozen);
        assert!(!directory.exists());
    }
}
