use super::{StoreError, StoreResult};
use crate::asset_repository::migration_gc::AssetGcCandidate;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Duration};

pub(crate) const ASSET_OBJECT_CATALOG_MAX_PAGE: i64 = 4_096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AssetObjectRegistration {
    pub(crate) object_hash: String,
    pub(crate) byte_size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AssetObjectCatalogPage {
    pub(crate) items: Vec<AssetGcCandidate>,
    pub(crate) next_cursor: Option<String>,
}

pub(crate) struct AssetObjectCatalog<'a> {
    connection: &'a mut Connection,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CatalogCursor {
    version: u32,
    created_at_ms: i64,
    object_hash: String,
}

impl<'a> AssetObjectCatalog<'a> {
    pub(super) fn new(connection: &'a mut Connection) -> Self {
        Self { connection }
    }

    pub(crate) fn register(
        &mut self,
        objects: &[AssetObjectRegistration],
        created_at_ms: i64,
    ) -> StoreResult<()> {
        let transaction = begin_registration(&mut *self.connection, objects, created_at_ms)?;
        for object in objects {
            register_object(&transaction, object, created_at_ms)?;
        }
        transaction.commit()?;
        Ok(())
    }
}

/// Registers objects found outside a store session on a connection of its
/// own, which a caller keeps for every batch it registers.
pub(crate) struct AssetObjectRegistrar {
    connection: Connection,
}

impl AssetObjectRegistrar {
    pub(crate) fn open(repository_root: &Path) -> StoreResult<Self> {
        let database_path = repository_root.join("persistent").join(super::DATABASE_FILE);
        let connection =
            Connection::open_with_flags(database_path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(Self { connection })
    }

    /// Registers objects in one transaction per `ASSET_OBJECT_CATALOG_MAX_PAGE`
    /// objects. A row already present is kept and must have the same size.
    pub(crate) fn register(
        &mut self,
        objects: &[AssetObjectRegistration],
        created_at_ms: i64,
    ) -> StoreResult<()> {
        for page in objects.chunks(ASSET_OBJECT_CATALOG_MAX_PAGE as usize) {
            let transaction = begin_registration(&mut self.connection, page, created_at_ms)?;
            for object in page {
                register_object(&transaction, object, created_at_ms)?;
            }
            transaction.commit()?;
        }
        Ok(())
    }

    /// Registers the objects whose row is missing, of another size or marked
    /// for deletion, and writes nothing when every row is already in place.
    pub(crate) fn register_missing(
        &mut self,
        objects: &[AssetObjectRegistration],
        created_at_ms: i64,
    ) -> StoreResult<()> {
        let mut missing = Vec::new();
        {
            let mut placed = self.connection.prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM asset_objects WHERE object_hash=?1 AND byte_size=?2)
                    AND NOT EXISTS(SELECT 1 FROM asset_object_deletions WHERE object_hash=?1)",
            )?;
            for object in objects {
                let in_place = match i64::try_from(object.byte_size) {
                    Ok(size) => placed.query_row(params![object.object_hash, size], |row| row.get(0))?,
                    Err(_) => false,
                };
                if !in_place {
                    missing.push(object.clone());
                }
            }
        }
        self.register(&missing, created_at_ms)
    }
}

fn begin_registration<'a>(
    connection: &'a mut Connection,
    objects: &[AssetObjectRegistration],
    created_at_ms: i64,
) -> StoreResult<Transaction<'a>> {
    if created_at_ms < 0 {
        return validation("asset object creation time must be nonnegative");
    }
    if objects.len() > ASSET_OBJECT_CATALOG_MAX_PAGE as usize {
        return validation("asset object registration batch exceeds the bounded limit");
    }
    Ok(connection.transaction_with_behavior(TransactionBehavior::Immediate)?)
}

fn register_object(
    transaction: &Transaction<'_>,
    object: &AssetObjectRegistration,
    created_at_ms: i64,
) -> StoreResult<()> {
    validate_hash(&object.object_hash)?;
    let byte_size = i64::try_from(object.byte_size).map_err(|_| StoreError::Validation {
        message: "asset object size exceeds the SQLite integer limit".to_owned(),
    })?;
    let tombstone: Option<(i64, String)> = transaction
        .query_row(
            "SELECT byte_size, physical_key FROM asset_object_deletions
             WHERE object_hash = ?1",
            [&object.object_hash],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((deleted_size, physical_key)) = tombstone {
        let expected_key = crate::asset_repository::object_physical_key(&object.object_hash);
        if deleted_size != byte_size || physical_key != expected_key {
            return validation(
                "asset object deletion tombstone conflicts with the recreated object",
            );
        }
        transaction.execute(
            "INSERT INTO asset_objects (object_hash, byte_size, created_at_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(object_hash) DO UPDATE SET
                byte_size = excluded.byte_size,
                created_at_ms = excluded.created_at_ms",
            params![object.object_hash, byte_size, created_at_ms],
        )?;
        transaction.execute(
            "DELETE FROM asset_object_deletions WHERE object_hash = ?1",
            [&object.object_hash],
        )?;
    } else {
        transaction.execute(
            "INSERT INTO asset_objects (object_hash, byte_size, created_at_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(object_hash) DO NOTHING",
            params![object.object_hash, byte_size, created_at_ms],
        )?;
    }
    let stored_size: i64 = transaction.query_row(
        "SELECT byte_size FROM asset_objects WHERE object_hash = ?1",
        [&object.object_hash],
        |row| row.get(0),
    )?;
    if stored_size != byte_size {
        return validation("asset object catalog size conflicts with the immutable object");
    }
    Ok(())
}

pub(super) fn query(
    connection: &Connection,
    limit: i64,
    cursor: Option<&str>,
) -> StoreResult<AssetObjectCatalogPage> {
    if !(1..=ASSET_OBJECT_CATALOG_MAX_PAGE).contains(&limit) {
        return validation("asset object catalog limit is outside the bounded range");
    }
    let cursor = cursor.map(decode_cursor).transpose()?;
    let query_limit = limit.checked_add(1).ok_or_else(|| StoreError::Validation {
        message: "asset object catalog limit overflow".to_owned(),
    })?;
    let mut items = Vec::with_capacity(query_limit as usize);
    match cursor {
        None => {
            let mut statement = connection.prepare(
                "SELECT object_hash, byte_size, created_at_ms
                 FROM asset_objects
                 ORDER BY created_at_ms ASC, object_hash ASC
                 LIMIT ?1",
            )?;
            let rows = statement.query_map([query_limit], read_candidate)?;
            for row in rows {
                items.push(row?);
            }
        }
        Some(cursor) => {
            let mut statement = connection.prepare(
                "SELECT object_hash, byte_size, created_at_ms
                 FROM asset_objects
                 WHERE (created_at_ms, object_hash) > (?1, ?2)
                 ORDER BY created_at_ms ASC, object_hash ASC
                 LIMIT ?3",
            )?;
            let rows = statement.query_map(
                params![cursor.created_at_ms, cursor.object_hash, query_limit],
                read_candidate,
            )?;
            for row in rows {
                items.push(row?);
            }
        }
    }
    let has_more = items.len() > limit as usize;
    if has_more {
        items.pop();
    }
    let next_cursor = has_more
        .then(|| items.last())
        .flatten()
        .map(encode_cursor)
        .transpose()?;
    Ok(AssetObjectCatalogPage { items, next_cursor })
}

fn read_candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<AssetGcCandidate> {
    let byte_size: i64 = row.get(1)?;
    Ok(AssetGcCandidate {
        object_hash: row.get(0)?,
        byte_size: u64::try_from(byte_size)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, byte_size))?,
        created_at_ms: row.get(2)?,
    })
}

fn encode_cursor(item: &AssetGcCandidate) -> StoreResult<String> {
    let bytes = serde_json::to_vec(&CatalogCursor {
        version: 1,
        created_at_ms: item.created_at_ms,
        object_hash: item.object_hash.clone(),
    })?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(value: &str) -> StoreResult<CatalogCursor> {
    if value.is_empty() || value.len() > 512 {
        return validation("asset object catalog cursor is invalid");
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| StoreError::Validation {
            message: "asset object catalog cursor is invalid".to_owned(),
        })?;
    let cursor: CatalogCursor =
        serde_json::from_slice(&bytes).map_err(|_| StoreError::Validation {
            message: "asset object catalog cursor is invalid".to_owned(),
        })?;
    if cursor.version != 1 || cursor.created_at_ms < 0 {
        return validation("asset object catalog cursor is invalid");
    }
    validate_hash(&cursor.object_hash)?;
    Ok(cursor)
}

pub(super) fn validate_cursor(value: &str) -> StoreResult<()> {
    decode_cursor(value).map(|_| ())
}

pub(super) fn cursor_has_successor(connection: &Connection, value: &str) -> StoreResult<bool> {
    let cursor = decode_cursor(value)?;
    Ok(connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM asset_objects
            WHERE (created_at_ms, object_hash) > (?1, ?2)
        )",
        params![cursor.created_at_ms, cursor.object_hash],
        |row| row.get(0),
    )?)
}

fn validate_hash(hash: &str) -> StoreResult<()> {
    if super::is_lowercase_sha256_hex(hash) {
        return Ok(());
    }
    validation("asset object hash must be a lowercase SHA-256 hash")
}

fn validation<T>(message: &str) -> StoreResult<T> {
    Err(StoreError::Validation {
        message: message.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registrar_registers_past_one_page_and_keeps_rows_already_present() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = crate::persistent_store::PersistentStore::open(directory.path()).unwrap();
        let existing = AssetObjectRegistration {
            object_hash: "a".repeat(64),
            byte_size: 5,
        };
        store
            .asset_object_catalog()
            .register(std::slice::from_ref(&existing), 1)
            .unwrap();
        let mut objects = (0..ASSET_OBJECT_CATALOG_MAX_PAGE as usize + 1)
            .map(|index| AssetObjectRegistration {
                object_hash: format!("{index:064x}"),
                byte_size: index as u64,
            })
            .collect::<Vec<_>>();
        objects.push(existing.clone());
        let mut registrar = AssetObjectRegistrar::open(directory.path()).unwrap();
        registrar.register(&objects, 2).unwrap();
        let connection = Connection::open(
            directory
                .path()
                .join("persistent")
                .join(super::super::DATABASE_FILE),
        )
        .unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM asset_objects", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count as usize, objects.len());
        let stored = |connection: &Connection| -> (i64, i64) {
            connection
                .query_row(
                    "SELECT byte_size, created_at_ms FROM asset_objects WHERE object_hash = ?1",
                    [&existing.object_hash],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap()
        };
        assert_eq!(stored(&connection), (5, 1), "a row already present is kept");
        let conflicting = AssetObjectRegistration {
            object_hash: existing.object_hash.clone(),
            byte_size: 9,
        };
        assert!(registrar.register(&[conflicting], 3).is_err());
        assert_eq!(stored(&connection), (5, 1));
        assert!(registrar.register(&objects, -1).is_err());
    }

    #[test]
    fn registering_only_missing_rows_writes_nothing_when_every_row_is_in_place() {
        let directory = tempfile::tempdir().unwrap();
        let _store = crate::persistent_store::PersistentStore::open(directory.path()).unwrap();
        let present = AssetObjectRegistration { object_hash: "a".repeat(64), byte_size: 5 };
        let mut registrar = AssetObjectRegistrar::open(directory.path()).unwrap();
        registrar.register(std::slice::from_ref(&present), 1).unwrap();
        let writer = Connection::open(directory.path().join("persistent").join(super::super::DATABASE_FILE)).unwrap();
        // Another writer holds the database, so any write here would wait and fail.
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        registrar.register_missing(std::slice::from_ref(&present), 2).unwrap();
        writer.execute_batch("ROLLBACK").unwrap();
        let absent = AssetObjectRegistration { object_hash: "b".repeat(64), byte_size: 7 };
        let conflicting = AssetObjectRegistration { object_hash: present.object_hash.clone(), byte_size: 9 };
        assert!(registrar.register_missing(&[conflicting], 3).is_err(), "a row of another size is not in place");
        registrar.register_missing(&[present.clone(), absent.clone()], 4).unwrap();
        let size = |hash: &str| -> Option<i64> {
            writer.query_row("SELECT byte_size FROM asset_objects WHERE object_hash = ?1", [hash], |row| row.get(0)).optional().unwrap()
        };
        assert_eq!((size(&present.object_hash), size(&absent.object_hash)), (Some(5), Some(7)));
    }
}
