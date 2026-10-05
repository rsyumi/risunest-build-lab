//! Source units of a replacement, staged as rows of its staging generation so a
//! commit reads them by key instead of holding them all in memory. Layer 0 keeps
//! the units as they were staged; layer 1 keeps them after an identity remap.
//! A committed replacement releases its rows, and an abort or the purge of a
//! retired library deletes any that remain.

use super::{error, intent_rows, projection, wire};
use crate::persistent_store::{PersistentStore, StoreResult};
use risunest_sync_wire::unit::{UnitKey, UnitValue};
use rusqlite::{params, Connection, ErrorCode, Transaction, TransactionBehavior};
use std::collections::VecDeque;

const PAGE_ROWS: i64 = 256;
pub(in crate::persistent_store) const STAGED: i64 = 0;
pub(in crate::persistent_store) const REMAPPED: i64 = 1;

/// One layer of the source units staged for `generation`.
#[derive(Clone, Copy)]
pub(in crate::persistent_store) struct SourceLayer<'a> {
    pub generation: &'a str,
    pub layer: i64,
}

impl<'a> SourceLayer<'a> {
    pub(in crate::persistent_store) fn staged(generation: &'a str) -> Self {
        Self { generation, layer: STAGED }
    }
}

/// The units of one layer in key order, read a page at a time. No statement
/// stays open between pages, so a caller may write the table while it reads.
pub(crate) struct Units<'a> {
    db: &'a Connection,
    generation: String,
    layer: i64,
    after: String,
    page: VecDeque<(String, String)>,
    done: bool,
}

impl Iterator for Units<'_> {
    type Item = StoreResult<(UnitKey, UnitValue)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.page.is_empty() && !self.done {
            if let Err(failure) = self.fill() {
                self.done = true;
                return Some(Err(failure));
            }
        }
        let (key, value) = self.page.pop_front()?;
        Some(parse(key, &value))
    }
}

impl Units<'_> {
    fn fill(&mut self) -> StoreResult<()> {
        let mut statement = self.db.prepare_cached(
            "SELECT key,value FROM replacement_source_units WHERE generation=?1 AND layer=?2 AND key>?3 ORDER BY key LIMIT ?4",
        )?;
        let rows = statement
            .query_map(params![self.generation, self.layer, self.after, PAGE_ROWS], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<VecDeque<(String, String)>, _>>()?;
        self.done = (rows.len() as i64) < PAGE_ROWS;
        if let Some((last, _)) = rows.back() {
            self.after = last.clone();
        }
        self.page = rows;
        Ok(())
    }
}

fn parse(key: String, value: &str) -> StoreResult<(UnitKey, UnitValue)> {
    Ok((wire(key.try_into())?, serde_json::from_str(value)?))
}

pub(in crate::persistent_store) fn units<'a>(db: &'a Connection, source: SourceLayer<'_>) -> Units<'a> {
    Units {
        db,
        generation: source.generation.to_owned(),
        layer: source.layer,
        after: String::new(),
        page: VecDeque::new(),
        done: false,
    }
}

pub(in crate::persistent_store) fn get(db: &Connection, source: SourceLayer<'_>, key: &UnitKey) -> StoreResult<Option<UnitValue>> {
    let mut statement = db.prepare_cached(
        "SELECT value FROM replacement_source_units WHERE generation=?1 AND layer=?2 AND key=?3",
    )?;
    let mut rows = statement.query(params![source.generation, source.layer, key.as_str()])?;
    match rows.next()? {
        Some(row) => Ok(Some(serde_json::from_str(&row.get::<_, String>(0)?)?)),
        None => Ok(None),
    }
}

pub(in crate::persistent_store) fn insert(db: &Connection, generation: &str, layer: i64, key: &UnitKey, value: &UnitValue) -> StoreResult<()> {
    let mut statement = db.prepare_cached(
        "INSERT INTO replacement_source_units(generation,layer,key,value) VALUES(?1,?2,?3,?4)",
    )?;
    match statement.execute(params![generation, layer, key.as_str(), serde_json::to_string(value)?]) {
        Err(rusqlite::Error::SqliteFailure(failure, _)) if failure.code == ErrorCode::ConstraintViolation => {
            Err(error("replacement-source-duplicate-unit"))
        }
        result => result.map(|_| ()).map_err(Into::into),
    }
}

/// Deletes the rows of `layer`, or of both layers, one bounded statement at a time.
pub(in crate::persistent_store) fn clear(db: &Connection, generation: &str, layer: Option<i64>) -> StoreResult<()> {
    let mut select = db.prepare_cached(
        "SELECT rowid FROM replacement_source_units WHERE generation=?1 AND (?2 IS NULL OR layer=?2) LIMIT ?3",
    )?;
    let mut delete = db.prepare_cached("DELETE FROM replacement_source_units WHERE rowid=?1")?;
    loop {
        let rows = select
            .query_map(params![generation, layer, PAGE_ROWS], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for row in &rows {
            delete.execute([row])?;
        }
        if (rows.len() as i64) < PAGE_ROWS {
            return Ok(());
        }
    }
}

/// The digest of the staged units, equal to the digest of the same units held in a map.
pub(super) fn digest(db: &Connection, generation: &str) -> StoreResult<intent_rows::IntentRows> {
    intent_rows::digest(
        "source",
        units(db, SourceLayer::staged(generation))
            .map(|unit| unit.and_then(|(key, value)| intent_rows::replacement_row(&key, &value, false))),
    )
}

pub(super) fn contains_device_unit(db: &Connection, generation: &str) -> StoreResult<bool> {
    for unit in units(db, SourceLayer::staged(generation)) {
        if projection::is_device(&unit?.0) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Stages the source units of one replacement. Units staged earlier for the
/// same generation are replaced, and nothing is kept unless `finish` commits.
pub(crate) struct ReplacementSourceWriter<'a> {
    tx: Transaction<'a>,
    generation: String,
}

impl ReplacementSourceWriter<'_> {
    pub(crate) fn put(&self, key: &UnitKey, value: &UnitValue) -> StoreResult<()> {
        insert(&self.tx, &self.generation, STAGED, key, value)
    }

    pub(crate) fn finish(self) -> StoreResult<()> {
        Ok(self.tx.commit()?)
    }
}

impl PersistentStore {
    pub(crate) fn replacement_source_writer(&mut self, generation: &str) -> StoreResult<ReplacementSourceWriter<'_>> {
        let tx = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        clear(&tx, generation, None)?;
        Ok(ReplacementSourceWriter { tx, generation: generation.to_owned() })
    }

    /// Deletes the source units staged for `generation` once its replacement
    /// committed. A commit that is retried stages them again first.
    pub(crate) fn release_replacement_source(&mut self, generation: &str) -> StoreResult<()> {
        let tx = self.connection.transaction()?;
        clear(&tx, generation, None)?;
        Ok(tx.commit()?)
    }

    /// The source units staged for `generation`, in key order.
    pub(crate) fn replacement_source_units(&self, generation: &str) -> Units<'_> {
        units(&self.connection, SourceLayer::staged(generation))
    }

    #[cfg(test)]
    pub(crate) fn stage_replacement_source(
        &mut self,
        generation: &str,
        source: &std::collections::BTreeMap<UnitKey, UnitValue>,
    ) -> StoreResult<()> {
        let writer = self.replacement_source_writer(generation)?;
        for (key, value) in source {
            writer.put(key, value)?;
        }
        writer.finish()
    }

    #[cfg(test)]
    pub(crate) fn replacement_source_row_total(&self) -> StoreResult<i64> {
        Ok(self.connection.query_row("SELECT COUNT(*) FROM replacement_source_units", [], |row| row.get(0))?)
    }

    #[cfg(test)]
    pub(crate) fn replacement_source_rows(&self, generation: &str) -> StoreResult<i64> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM replacement_source_units WHERE generation=?1",
            [generation],
            |row| row.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_sync_wire::unit::UnitKey;
    use std::collections::BTreeMap;

    fn sample() -> BTreeMap<UnitKey, UnitValue> {
        let mut units = BTreeMap::new();
        for index in 0..(PAGE_ROWS * 2 + 7) {
            let id = format!("synthetic-{index:04}");
            units.insert(
                UnitKey::new(&["character", &id, "detail"]).unwrap(),
                UnitValue::inline(format!("{{\"name\":\"{id}\"}}").as_bytes()).unwrap(),
            );
            units.insert(UnitKey::new(&["exists", "character", &id]).unwrap(), UnitValue::Deleted);
        }
        units.insert(UnitKey::new(&["order", "characters"]).unwrap(), UnitValue::inline(b"[]").unwrap());
        units
    }

    #[test]
    fn staged_units_read_back_in_map_order_with_the_map_digest() {
        let (_root, mut store) = crate::server_sync::lww_tests::local();
        let units = sample();
        store.stage_replacement_source("staging-synthetic", &units).unwrap();
        let read = store.replacement_source_units("staging-synthetic").collect::<StoreResult<Vec<_>>>().unwrap();
        assert_eq!(read, units.clone().into_iter().collect::<Vec<_>>());
        assert_eq!(
            digest(&store.connection, "staging-synthetic").unwrap(),
            intent_rows::source_digest(&units).unwrap(),
        );
        let probe = UnitKey::new(&["character", "synthetic-0300", "detail"]).unwrap();
        assert_eq!(get(&store.connection, SourceLayer::staged("staging-synthetic"), &probe).unwrap().as_ref(), units.get(&probe));
        assert_eq!(get(&store.connection, SourceLayer { generation: "staging-synthetic", layer: REMAPPED }, &probe).unwrap(), None);
    }

    #[test]
    fn staging_again_replaces_the_rows_and_a_duplicate_key_is_refused() {
        let (_root, mut store) = crate::server_sync::lww_tests::local();
        store.stage_replacement_source("staging-synthetic", &sample()).unwrap();
        let one = BTreeMap::from([(UnitKey::new(&["order", "characters"]).unwrap(), UnitValue::Deleted)]);
        store.stage_replacement_source("staging-synthetic", &one).unwrap();
        assert_eq!(store.replacement_source_rows("staging-synthetic").unwrap(), 1);
        let writer = store.replacement_source_writer("staging-synthetic").unwrap();
        let key = UnitKey::new(&["order", "characters"]).unwrap();
        writer.put(&key, &UnitValue::Deleted).unwrap();
        assert!(writer.put(&key, &UnitValue::Deleted).is_err());
        drop(writer);
        assert_eq!(store.replacement_source_rows("staging-synthetic").unwrap(), 1);
    }
}
