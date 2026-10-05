//! Bulk intent inputs kept as device rows. The intent body carries one digest
//! per row set, so the body stays small and a replay proves the rows it reads.

use super::{error, Change, StoreResult};
use risunest_sync_wire::unit::{UnitKey, UnitValue};
use rusqlite::{params, Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const BATCH_ROWS: usize = 1024;

/// The count and digest of one row set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct IntentRows {
    pub rows: u64,
    pub digest: String,
}

/// One row as it is stored. `stamp` is set for received changes only.
pub(super) struct RowText {
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

pub(super) fn replacement_row(key: &UnitKey, value: &UnitValue, source_override: bool) -> StoreResult<RowText> {
    Ok(RowText { key: key.as_str().to_owned(), stamp: None, value: serde_json::to_string(value)?, source_override })
}

pub(super) fn target_row(change: &Change) -> StoreResult<RowText> {
    Ok(RowText {
        key: change.key.as_str().to_owned(),
        stamp: Some(serde_json::to_string(&change.stamp)?),
        value: serde_json::to_string(&change.value)?,
        source_override: false,
    })
}

/// The digest of rows that are not stored, such as the source units a
/// replacement only has to recognize when it is retried.
pub(super) fn digest(kind: &str, rows: impl IntoIterator<Item = StoreResult<RowText>>) -> StoreResult<IntentRows> {
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
pub(super) fn write(
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

pub(super) fn read_replacement(
    device: &Connection,
    request_id: &str,
    expected: &IntentRows,
) -> StoreResult<(Vec<(UnitKey, UnitValue)>, BTreeSet<UnitKey>)> {
    let mut changes = Vec::new();
    let mut overrides = BTreeSet::new();
    read(device, request_id, "changes", expected, |key, stamp, value, source_override| {
        if stamp.is_some() {
            return Err(error("request-id-integrity"));
        }
        let key: UnitKey = super::wire(key.to_owned().try_into())?;
        if source_override {
            overrides.insert(key.clone());
        }
        changes.push((key, serde_json::from_str(value)?));
        Ok(())
    })?;
    Ok((changes, overrides))
}

pub(super) fn read_target(device: &Connection, request_id: &str, expected: &IntentRows) -> StoreResult<Vec<Change>> {
    let mut changes = Vec::new();
    read(device, request_id, "target", expected, |key, stamp, value, source_override| {
        let Some(stamp) = stamp.filter(|_| !source_override) else {
            return Err(error("request-id-integrity"));
        };
        changes.push(Change {
            key: super::wire(key.to_owned().try_into())?,
            stamp: serde_json::from_str(stamp)?,
            value: serde_json::from_str(value)?,
        });
        Ok(())
    })?;
    Ok(changes)
}

#[cfg(test)]
pub(super) fn source_digest(units: &std::collections::BTreeMap<UnitKey, UnitValue>) -> StoreResult<IntentRows> {
    digest("source", units.iter().map(|(key, value)| replacement_row(key, value, false)))
}

/// Drops the rows of every intent that is not waiting for a replay: those of
/// completed intents and those an attempt left before it issued its intent.
pub(super) fn delete_settled(device: &Connection) -> StoreResult<()> {
    device.execute(
        "DELETE FROM lww_intent_rows WHERE request_id NOT IN (SELECT request_id FROM lww_intents WHERE complete=0)",
        [],
    )?;
    Ok(())
}
