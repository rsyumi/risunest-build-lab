//! Per-device plugin values. The renderer holds a cache, so this module answers
//! a whole keyspace at once on start and single keys afterwards. Every write
//! commits before the caller hears about it.
use super::{begin_mutation, finish_mutation, invalid, issue_write_clock, DeviceStore, Section};
use crate::persistent_store::StoreResult;
use rusqlite::{params, Connection, MAIN_DB, OptionalExtension};
use serde::{Deserialize, Serialize};

pub(crate) mod text_value;
pub(crate) mod sessions;

/// The renderer loads a whole keyspace only while it stays under this. Past it
/// the keyspace is read one key at a time instead.
pub(crate) const HYDRATION_LIMIT_BYTES: i64 = 4 * 1024 * 1024;
const MAX_KEY_BYTES: usize = 512;
const MAX_OWNER_BYTES: usize = 512;
const MAX_BATCH_ENTRIES: usize = 1_024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginDeviceEntry {
    pub(crate) space: String,
    pub(crate) key: String,
    pub(crate) value: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginDeviceHydration {
    /// False when the keyspace is past the limit, so `entries` is empty and the
    /// caller reads single keys instead.
    pub(crate) complete: bool,
    pub(crate) byte_size: i64,
    pub(crate) entries: Vec<PluginDeviceEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginDeviceListItem {
    pub(crate) owner: String,
    pub(crate) space: String,
    pub(crate) key: String,
    pub(crate) byte_size: i64,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum PluginDeviceRead {
    Value(String),
    #[serde(rename_all = "camelCase")]
    Ranges { byte_size: u64, version: String },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum PluginDeviceRange {
    Changed,
    #[serde(rename_all = "camelCase")]
    Value { value: String, next_offset: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum PluginDeviceMutation {
    #[serde(rename_all = "camelCase")]
    Set {
        space: String,
        key: String,
        value: String,
    },
    #[serde(rename_all = "camelCase")]
    Delete { space: String, key: String },
    #[serde(rename_all = "camelCase")]
    Clear { space: String },
    #[serde(rename_all = "camelCase")]
    Staged { session_id: String },
}

fn validate_owner(owner: &str) -> StoreResult<()> {
    if owner.is_empty() || owner.len() > MAX_OWNER_BYTES {
        return Err(invalid("plugin device owner is invalid"));
    }
    Ok(())
}

fn validate_space(space: &str) -> StoreResult<()> {
    if space != "string" && space != "json" {
        return Err(invalid("plugin device space is invalid"));
    }
    Ok(())
}

fn validate_key(key: &str) -> StoreResult<()> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES {
        return Err(invalid("plugin device key is invalid"));
    }
    Ok(())
}

impl DeviceStore {
    /// Reports the whole keyspace when it fits, and only its size when it does
    /// not. Tombstoned keys are absent from both answers.
    pub(crate) fn hydrate_plugin_device_storage(
        &self,
        owner: &str,
    ) -> StoreResult<PluginDeviceHydration> {
        validate_owner(owner)?;
        let byte_size: i64 = self.connection.query_row(
            "SELECT coalesce(sum(byte_size),0) FROM plugin_device_storage
                WHERE owner=?1 AND tombstone=0",
            [owner],
            |row| row.get(0),
        )?;
        if byte_size > HYDRATION_LIMIT_BYTES {
            return Ok(PluginDeviceHydration {
                complete: false,
                byte_size,
                entries: Vec::new(),
            });
        }
        let mut statement = self.connection.prepare(
            "SELECT space,key,value FROM plugin_device_storage
                WHERE owner=?1 AND tombstone=0 AND value IS NOT NULL
                ORDER BY space,key",
        )?;
        let entries = statement
            .query_map([owner], |row| {
                Ok(PluginDeviceEntry {
                    space: row.get(0)?,
                    key: row.get(1)?,
                    value: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PluginDeviceHydration {
            complete: true,
            byte_size,
            entries,
        })
    }

    #[cfg(test)]
    pub(crate) fn read_plugin_device_value(
        &self,
        owner: &str,
        space: &str,
        key: &str,
    ) -> StoreResult<Option<String>> {
        validate_owner(owner)?;
        validate_space(space)?;
        validate_key(key)?;
        Ok(self
            .connection
            .query_row(
                "SELECT value FROM plugin_device_storage
                    WHERE owner=?1 AND space=?2 AND key=?3 AND tombstone=0",
                params![owner, space, key],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    fn plugin_value_identity(&self, owner: &str, space: &str, key: &str) -> StoreResult<Option<(i64, u64, String)>> {
        validate_owner(owner)?; validate_space(space)?; validate_key(key)?;
        let row: Option<(i64, i64, String, String)> = self.connection.query_row(
            "SELECT rowid,byte_size,write_clock,writer_id FROM plugin_device_storage WHERE owner=?1 AND space=?2 AND key=?3 AND tombstone=0",
            params![owner, space, key], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).optional()?;
        row.map(|(rowid, size, clock, writer)| {
            let unit_key = super::super::lww::unit_key(&["plugin-local", owner, space, key])?;
            // Sync projection keeps the legacy clock, so its unit version also identifies the value.
            let unit_version: Option<String> = self.connection.query_row(
                "SELECT version FROM lww_units WHERE key=?1", [unit_key.as_str()], |row| row.get(0)).optional()?;
            Ok((rowid, u64::try_from(size).map_err(|_| invalid("plugin device size is invalid"))?,
                serde_json::to_string(&(&self.plugin_read_epoch, rowid, size, clock, writer, unit_version))?))
        }).transpose()
    }

    pub(crate) fn read_plugin_device_transfer(&self, owner: &str, space: &str, key: &str) -> StoreResult<Option<PluginDeviceRead>> {
        let _snapshot = self.connection.unchecked_transaction()?;
        let Some((rowid, byte_size, version)) = self.plugin_value_identity(owner, space, key)? else { return Ok(None); };
        if byte_size <= text_value::PART_BYTES as u64 {
            let value = text_value::range(&self.connection, rowid, 0, text_value::PART_BYTES)?;
            if value.len() as u64 == byte_size { return Ok(Some(PluginDeviceRead::Value(value))); }
        }
        Ok(Some(PluginDeviceRead::Ranges { byte_size, version }))
    }

    pub(crate) fn read_plugin_device_range(&self, owner: &str, space: &str, key: &str, version: &str, offset: u64) -> StoreResult<PluginDeviceRange> {
        let _snapshot = self.connection.unchecked_transaction()?;
        let Some((rowid, _, current)) = self.plugin_value_identity(owner, space, key)? else { return Ok(PluginDeviceRange::Changed); };
        if current != version { return Ok(PluginDeviceRange::Changed); }
        let value = text_value::range(&self.connection, rowid, offset, text_value::PART_BYTES)?;
        let next_offset = offset + value.len() as u64;
        Ok(PluginDeviceRange::Value { value, next_offset })
    }

    pub(crate) fn list_plugin_device_keys(
        &self,
        owner: &str,
        space: &str,
    ) -> StoreResult<Vec<String>> {
        validate_owner(owner)?;
        validate_space(space)?;
        let mut statement = self.connection.prepare(
            "SELECT key FROM plugin_device_storage
                WHERE owner=?1 AND space=?2 AND tombstone=0 ORDER BY key",
        )?;
        let keys = statement
            .query_map(params![owner, space], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(keys)
    }

    /// Every owner and key this installation holds, without the values. The
    /// plugin data screen shows sizes, so the values never leave the store.
    pub(crate) fn list_plugin_device_storage(&self) -> StoreResult<Vec<PluginDeviceListItem>> {
        let mut statement = self.connection.prepare(
            "SELECT owner,space,key,byte_size FROM plugin_device_storage
                WHERE tombstone=0 ORDER BY owner,space,key",
        )?;
        let items = statement
            .query_map([], |row| {
                Ok(PluginDeviceListItem {
                    owner: row.get(0)?,
                    space: row.get(1)?,
                    key: row.get(2)?,
                    byte_size: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(items)
    }

    /// Applies the batch in one transaction. A removal leaves a tombstone with
    /// its own write clock so a later publication can carry the deletion.
    pub(crate) fn write_plugin_device_values(
        &mut self,
        owner: &str,
        mutations: &[PluginDeviceMutation],
    ) -> StoreResult<()> {
        self.write_plugin_device_values_with_limits(owner, mutations, text_value::PART_BYTES as u64, text_value::REQUEST_BYTES)
    }

    pub(crate) fn write_plugin_device_values_with_limits(
        &mut self, owner: &str, mutations: &[PluginDeviceMutation], part_limit: u64, request_limit: u64,
    ) -> StoreResult<()> {
        validate_owner(owner)?;
        if mutations.len() > MAX_BATCH_ENTRIES {
            return Err(invalid("plugin device write batch is too large"));
        }
        struct RequestSize { size: u64, limit: u64 }
        impl std::io::Write for RequestSize {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.size += bytes.len() as u64;
                if self.size > self.limit {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "plugin device write batch is too large"));
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        #[derive(Serialize)]
        struct Request<'a> { owner: &'a str, mutations: &'a [PluginDeviceMutation] }
        serde_json::to_writer(RequestSize { size: 0, limit: request_limit }, &Request { owner, mutations })
            .map_err(|_| invalid("plugin device write batch is too large"))?;
        for mutation in mutations {
            match mutation {
                PluginDeviceMutation::Set { space, key, value } => {
                    validate_space(space)?;
                    validate_key(key)?;
                    if text_value::encoded_length(value) > part_limit {
                        return Err(invalid("plugin device value is too large"));
                    }
                }
                PluginDeviceMutation::Delete { space, key } => {
                    validate_space(space)?;
                    validate_key(key)?;
                }
                PluginDeviceMutation::Clear { space } => validate_space(space)?,
                PluginDeviceMutation::Staged { .. } => {},
            }
        }
        if mutations.is_empty() {
            return Ok(());
        }
        let mut staged = mutations.iter().map(|mutation| match mutation {
            PluginDeviceMutation::Staged { session_id } => self.plugin_writes.take(owner, session_id).map(Some),
            _ => Ok(None),
        }).collect::<StoreResult<Vec<_>>>()?;
        let transaction = self.transaction()?;
        let writer_id: String = transaction.query_row(
            "SELECT writer_id FROM device_meta WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        begin_mutation(&transaction)?;
        for (mutation, staged) in mutations.iter().zip(&mut staged) {
            match mutation {
                PluginDeviceMutation::Set { space, key, value } => {
                    let clock = issue_write_clock(&transaction, Section::LocalPlugins)?;
                    transaction.execute(
                        "INSERT INTO plugin_device_storage
                            (owner,space,key,value,byte_size,tombstone,write_clock,writer_id,
                             published_clock,first_published_generation,first_published_at_ms)
                            VALUES (?1,?2,?3,?4,?5,0,?6,?7,NULL,NULL,NULL)
                            ON CONFLICT(owner,space,key) DO UPDATE SET
                                value=excluded.value,
                                byte_size=excluded.byte_size,
                                tombstone=0,
                                write_clock=excluded.write_clock,
                                writer_id=excluded.writer_id,
                                published_clock=NULL,
                                first_published_generation=NULL,
                                first_published_at_ms=NULL",
                        params![
                            owner,
                            space,
                            key,
                            value,
                            value.len() as i64,
                            clock.as_str(),
                            writer_id
                        ],
                    )?;
                }
                PluginDeviceMutation::Delete { space, key } => {
                    let clock = issue_write_clock(&transaction, Section::LocalPlugins)?;
                    transaction.execute(
                        "UPDATE plugin_device_storage
                            SET value=NULL,byte_size=0,tombstone=1,write_clock=?4,writer_id=?5,
                                published_clock=NULL,first_published_generation=NULL,
                                first_published_at_ms=NULL
                            WHERE owner=?1 AND space=?2 AND key=?3 AND tombstone=0",
                        params![owner, space, key, clock.as_str(), writer_id],
                    )?;
                }
                PluginDeviceMutation::Clear { space } => {
                    let keys: Vec<String> = {
                        let mut statement = transaction.prepare(
                            "SELECT key FROM plugin_device_storage
                                WHERE owner=?1 AND space=?2 AND tombstone=0 ORDER BY key",
                        )?;
                        let keys = statement
                            .query_map(params![owner, space], |row| row.get(0))?
                            .collect::<Result<Vec<_>, _>>()?;
                        keys
                    };
                    for key in keys {
                        let clock = issue_write_clock(&transaction, Section::LocalPlugins)?;
                        transaction.execute(
                            "UPDATE plugin_device_storage
                                SET value=NULL,byte_size=0,tombstone=1,write_clock=?4,writer_id=?5,
                                    published_clock=NULL,first_published_generation=NULL,
                                    first_published_at_ms=NULL
                                WHERE owner=?1 AND space=?2 AND key=?3",
                            params![owner, space, key, clock.as_str(), writer_id],
                        )?;
                    }
                }
                PluginDeviceMutation::Staged { .. } => {
                    let mut staged = staged.take().ok_or_else(|| invalid("plugin device write session is unavailable"))?;
                    let clock = issue_write_clock(&transaction, Section::LocalPlugins)?;
                    write_reserved(&transaction, owner, &staged.space, &staged.key, staged.byte_length, clock.as_str(), &writer_id, |blob| {
                        if std::io::copy(&mut staged.file, blob)? != staged.byte_length {
                            return Err(invalid("plugin device write session is incomplete"));
                        }
                        Ok(())
                    })?;
                }
            }
        }
        finish_mutation(&transaction)?;
        transaction.commit()?;
        Ok(())
    }
}

pub(crate) fn write_reserved(
    db: &Connection, owner: &str, space: &str, key: &str, byte_length: u64,
    clock: &str, writer_id: &str, fill: impl FnOnce(&mut rusqlite::blob::Blob<'_>) -> StoreResult<()>,
) -> StoreResult<()> {
    if byte_length > text_value::MAX_VALUE_BYTES { return Err(invalid("plugin device value is too large")); }
    db.execute("INSERT INTO plugin_device_storage
        (owner,space,key,value,byte_size,tombstone,write_clock,writer_id,published_clock,first_published_generation,first_published_at_ms)
        VALUES(?1,?2,?3,CAST(zeroblob(?4) AS TEXT),?4,0,?5,?6,NULL,NULL,NULL)
        ON CONFLICT(owner,space,key) DO UPDATE SET value=excluded.value,byte_size=excluded.byte_size,tombstone=0,
            write_clock=excluded.write_clock,writer_id=excluded.writer_id,published_clock=NULL,
            first_published_generation=NULL,first_published_at_ms=NULL",
        params![owner, space, key, byte_length as i64, clock, writer_id])?;
    let rowid = db.query_row("SELECT rowid FROM plugin_device_storage WHERE owner=?1 AND space=?2 AND key=?3",
        params![owner, space, key], |row| row.get(0))?;
    let mut blob = db.blob_open(MAIN_DB, "plugin_device_storage", "value", rowid, false)?;
    fill(&mut blob)?;
    blob.close()?;
    Ok(())
}
