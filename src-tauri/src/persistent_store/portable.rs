//! Raw generation projection for portable archives. Never execute source DDL.
use super::{PersistentStore, StoreError, StoreResult};
use crate::local_backup::CancellationProbe;
use rusqlite::{types::ValueRef, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

pub(crate) struct PortableTable {
    pub(crate) name: &'static str,
    /// SQL declaration types describe the live schema. Archive columns intentionally have no
    /// affinity: a damaged source's actual storage class must survive capture unchanged.
    pub(crate) columns: &'static [(&'static str, &'static str)],
    pub(crate) order: &'static str,
}

pub(crate) const TABLES: &[PortableTable] = &[
    PortableTable {
        name: "root",
        columns: &[("value", "TEXT")],
        order: "value",
    },
    PortableTable {
        name: "bot_presets",
        columns: &[
            ("preset_id", "TEXT"),
            ("configured_index", "INTEGER"),
            ("name", "TEXT"),
            ("image", "TEXT"),
            ("value", "TEXT"),
        ],
        order: "preset_id",
    },
    PortableTable {
        name: "characters",
        columns: &[
            ("character_id", "TEXT"),
            ("configured_index", "INTEGER"),
            ("recent_at", "INTEGER"),
            ("trashed", "INTEGER"),
            ("name", "TEXT"),
            ("image", "TEXT"),
            ("conversation_count", "INTEGER"),
            ("type", "TEXT"),
            ("creator_notes", "TEXT"),
            ("trash_time", "INTEGER"),
            ("detail", "TEXT"),
            ("archived_object", "TEXT"),
        ],
        order: "character_id",
    },
    PortableTable {
        name: "conversations",
        columns: &[
            ("character_id", "TEXT"),
            ("conversation_id", "TEXT"),
            ("configured_index", "INTEGER"),
            ("recent_at", "INTEGER"),
            ("name", "TEXT"),
            ("message_count", "INTEGER"),
            ("detail", "TEXT"),
        ],
        order: "character_id, conversation_id",
    },
    PortableTable {
        name: "messages",
        columns: &[
            ("character_id", "TEXT"),
            ("conversation_id", "TEXT"),
            ("message_index", "INTEGER"),
            ("message_id", "TEXT"),
            ("value", "TEXT"),
        ],
        order: "character_id, conversation_id, message_index",
    },
    PortableTable {
        name: "plugin_storage",
        columns: &[
            ("owner", "TEXT"),
            ("storage_key", "TEXT"),
            ("byte_size", "INTEGER"),
            ("ordinal", "INTEGER"),
            ("value", "TEXT"),
            ("claimed_from", "TEXT"),
            ("import_batch_id", "TEXT"),
            ("assigned_at", "INTEGER"),
        ],
        order: "owner, storage_key",
    },
    PortableTable {
        name: "asset_aliases",
        columns: &[
            ("logical_key", "TEXT"),
            ("object_hash", "TEXT"),
            ("kind", "TEXT"),
            ("size", "INTEGER"),
            ("mime", "TEXT"),
            ("name", "TEXT"),
            ("ext", "TEXT"),
            ("inlay_type", "TEXT"),
            ("width", "INTEGER"),
            ("height", "INTEGER"),
            ("metadata", "TEXT"),
        ],
        order: "kind, logical_key",
    },
    PortableTable {
        name: "asset_owner_heads",
        columns: &[
            ("owner_kind", "TEXT"),
            ("owner_locator", "TEXT"),
            ("present", "INTEGER"),
            ("manifest_hash", "TEXT"),
            ("entry_count", "INTEGER"),
        ],
        order: "owner_kind, owner_locator",
    },
];

impl PortableTable {
    pub(crate) fn column_list(&self) -> String {
        self.columns
            .iter()
            .map(|(name, _)| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub(crate) fn create_sql(&self) -> String {
        format!("CREATE TABLE {} ({})", self.name, self.column_list())
    }

    pub(crate) fn index_sql(&self) -> Option<(String, String)> {
        if self.columns.len() == 1 {
            return None;
        }
        let name = format!("portable_{}_order", self.name);
        Some((
            name.clone(),
            format!("CREATE INDEX {name} ON {} ({})", self.name, self.order),
        ))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RawTableDigest {
    pub(crate) table: &'static str,
    pub(crate) rows: u64,
    pub(crate) sha256: [u8; 32],
}

pub(crate) fn create_raw_tables(destination: &Connection) -> StoreResult<()> {
    for table in TABLES {
        destination.execute_batch(&table.create_sql())?;
        if let Some((_, sql)) = table.index_sql() {
            destination.execute_batch(&sql)?;
        }
    }
    Ok(())
}

/// Presents one generation under the raw table names, without copying a row. SQLite resolves an
/// unqualified name in the temp schema first, so this needs a connection of its own.
pub(crate) fn install_generation_views(
    destination: &Connection,
    generation: &str,
) -> StoreResult<()> {
    let quoted = generation.replace('\'', "''");
    for table in TABLES {
        destination.execute_batch(&format!(
            "CREATE TEMP VIEW {name} AS SELECT {columns} FROM main.{name} WHERE generation='{quoted}'",
            name = table.name,
            columns = table.column_list(),
        ))?;
    }
    Ok(())
}

fn cancelled(probe: &dyn CancellationProbe) -> StoreResult<()> {
    if probe.is_cancelled() {
        return Err(StoreError::Validation {
            message: "portable capture cancelled".into(),
        });
    }
    Ok(())
}

/// Copy one stable read transaction into an already-created, owned archive catalog. Values are
/// bound straight from SQLite, so invalid JSON, SQL NULL and even invalid TEXT bytes survive.
/// The transaction rolls back all tables on cancellation/failure. Memory is one SQLite row.
fn copy_generation(
    source: &Connection,
    generation: &str,
    destination: &mut Connection,
    probe: &dyn CancellationProbe,
) -> StoreResult<Vec<RawTableDigest>> {
    validate_live_columns(source)?;
    let transaction = destination.transaction()?;
    let mut result = Vec::with_capacity(TABLES.len());
    for table in TABLES {
        cancelled(probe)?;
        let columns = table.column_list();
        let mut select = source.prepare(&format!(
            "SELECT {columns} FROM {} WHERE generation=?1 ORDER BY {}",
            table.name, table.order
        ))?;
        let placeholders = (0..table.columns.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let mut insert = transaction.prepare(&format!(
            "INSERT INTO {} ({columns}) VALUES ({placeholders})",
            table.name
        ))?;
        let mut rows = select.query([generation])?;
        let mut digest = table_hasher(table);
        let mut count = 0_u64;
        while let Some(row) = rows.next()? {
            cancelled(probe)?;
            digest.update([0xf0]);
            let sanitized_root = if table.name == "root" {
                match row.get_ref(0)? {
                    ValueRef::Text(bytes) => serde_json::from_slice::<serde_json::Value>(bytes).ok().and_then(|mut value| {
                        value.as_object_mut()?.remove("account")?;
                        serde_json::to_string(&value).ok()
                    }),
                    _ => None,
                }
            } else { None };
            for index in 0..table.columns.len() {
                let value = match sanitized_root.as_ref() {
                    Some(root) if index == 0 => ValueRef::Text(root.as_bytes()),
                    _ => row.get_ref(index)?,
                };
                hash_value(&mut digest, value);
                insert
                    .raw_bind_parameter(index + 1, rusqlite::types::ToSqlOutput::Borrowed(value))?;
            }
            insert.raw_execute()?;
            count = count
                .checked_add(1)
                .ok_or_else(|| invalid("portable row count overflow"))?;
        }
        digest.update(count.to_le_bytes());
        result.push(RawTableDigest {
            table: table.name,
            rows: count,
            sha256: digest.finalize().into(),
        });
    }
    cancelled(probe)?;
    transaction.commit()?;
    Ok(result)
}

impl PersistentStore {
    pub(crate) fn portable_staged_counts(&self, staging: &str) -> StoreResult<(u64, u64)> {
        Ok((
            self.connection.query_row("SELECT count(*) FROM characters WHERE generation=?1", [staging], |row| row.get::<_, i64>(0))? as u64,
            self.connection.query_row("SELECT count(*) FROM bot_presets WHERE generation=?1", [staging], |row| row.get::<_, i64>(0))? as u64,
        ))
    }
    pub(crate) fn portable_source_generation(&self, lease: &str) -> StoreResult<String> {
        let (_, target) = self.read_view(Some(lease))?;
        Ok(target.generation.clone())
    }
    pub(crate) fn portable_export_lower_bound(&self) -> StoreResult<u64> {
        let objects: i64 = self.connection.query_row("SELECT coalesce(sum(byte_size),0) FROM asset_objects", [], |row| row.get(0))?;
        let pages: i64 = self.connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let page_size: i64 = self.connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        u64::try_from(objects).ok().and_then(|objects| {
            u64::try_from(pages).ok()?.checked_mul(u64::try_from(page_size).ok()?)?.checked_add(objects)
        }).ok_or_else(|| invalid("portable export size overflow"))
    }

    pub(crate) fn capture_portable_units(&self, lease: &str, destination: &Connection, probe: &dyn CancellationProbe) -> StoreResult<super::external_capture::BackupDependencyInventory> {
        use super::external_capture::BackupBodyRole;
        let units = self.lww_backup_unit_values(lease)?;
        destination.execute_batch("CREATE TEMP TABLE backup_payload_spool(hash TEXT PRIMARY KEY,body BLOB NOT NULL)")?;
        let inventory = self.lww_backup_dependency_inventory(lease, &units, probe, true, &mut |hash,body,role| {
            cancelled(probe)?;
            match role {
                BackupBodyRole::Control=>{destination.execute("INSERT INTO backup_controls VALUES(?1,?2)",rusqlite::params![hash,body])?;},
                BackupBodyRole::Payload=>{destination.execute("INSERT INTO backup_payload_spool VALUES(?1,?2)",rusqlite::params![hash,body])?;},
            }
            Ok(())
        })?;
        for (key, value) in units {
            cancelled(probe)?;
            destination.execute("INSERT INTO backup_units VALUES(?1,?2)",rusqlite::params![String::from(key),serde_json::to_string(&value)?])?;
        }
        for (hash, size) in &inventory.payloads {
            let size=size.ok_or_else(||invalid("original payload size is unavailable"))?;
            destination.execute("INSERT INTO backup_payloads VALUES(?1,?2,?3)",rusqlite::params![hash,i64::try_from(size).map_err(|_|invalid("original payload size exceeds SQLite limit"))?,inventory.record_payloads.contains(hash)])?;
        }
        Ok(inventory)
    }

    pub(crate) fn stage_portable_units(&self, source: &Connection, probe: &dyn CancellationProbe) -> StoreResult<std::collections::BTreeMap<risunest_sync_wire::unit::UnitKey,risunest_sync_wire::unit::UnitValue>> {
        let mut units = std::collections::BTreeMap::new();
        let mut statement = source.prepare("SELECT key,value FROM backup_units ORDER BY key")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            cancelled(probe)?;
            let key = row.get::<_,String>(0)?.try_into().map_err(|_|invalid("invalid original unit key"))?;
            let value: risunest_sync_wire::unit::UnitValue = serde_json::from_str(&row.get::<_,String>(1)?)?;
            value.validate().map_err(|e| invalid(&e.to_string()))?;
            units.insert(key,value);
        }
        let temporary=tempfile::tempdir_in(&self.snapshots_dir)?;
        let certified=Connection::open(temporary.path().join("certified.sqlite"))?;
        certified.execute_batch("PRAGMA cache_size=-16384; PRAGMA temp_store=FILE; CREATE TABLE controls(hash TEXT PRIMARY KEY,body BLOB NOT NULL)")?;
        let inventory = super::external_capture::original_unit_dependency_inventory(&units,
            &|hash| Ok(source.query_row("SELECT body FROM backup_controls WHERE hash=?1 AND length(body)<=?2",rusqlite::params![hash,risunest_sync_wire::MAX_METADATA_BYTES as i64],|row|row.get(0)).optional()?),
            &|hash| {
                let size:Option<i64>=source.query_row("SELECT byte_length FROM backup_payloads WHERE hash=?1",[hash],|row|row.get(0)).optional()?;
                size.map(|size|u64::try_from(size).map_err(|_|invalid("invalid original payload size"))).transpose()
            },probe,true,&mut |hash,body,role| {
                if role!=super::external_capture::BackupBodyRole::Control {return Err(invalid("payload body is misclassified as an original control"));}
                certified.execute("INSERT INTO controls VALUES(?1,?2)",rusqlite::params![hash,body])?;
                Ok(())
            })?;
        let controls:i64=source.query_row("SELECT COUNT(*) FROM backup_controls",[],|row|row.get(0))?;
        if usize::try_from(controls).ok()!=Some(inventory.controls.len()) {return Err(invalid("unreferenced original control"));}
        for hash in inventory.controls.keys() {
            let present:bool=source.query_row("SELECT EXISTS(SELECT 1 FROM backup_controls WHERE hash=?1)",[hash],|row|row.get(0))?;
            if !present {return Err(invalid("missing original control"));}
        }
        let payloads:i64=source.query_row("SELECT COUNT(*) FROM backup_payloads",[],|row|row.get(0))?;
        if usize::try_from(payloads).ok()!=Some(inventory.payloads.len()) {return Err(invalid("unreferenced original payload"));}
        for (hash,size) in &inventory.payloads {
            let row:Option<(i64,bool)>=source.query_row("SELECT byte_length,record FROM backup_payloads WHERE hash=?1",[hash],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
            let expected_size=size.ok_or_else(||invalid("original payload size is unavailable"))?;
            if row!=Some((i64::try_from(expected_size).map_err(|_|invalid("original payload size exceeds SQLite limit"))?,inventory.record_payloads.contains(hash))) {return Err(invalid("original payload metadata differs"));}
        }
        let mut statement=certified.prepare("SELECT hash,body FROM controls ORDER BY hash")?;
        let mut rows=statement.query([])?;
        while let Some(row)=rows.next()? {
            cancelled(probe)?;
            super::message_pages::put_object(&self.connection,&row.get::<_,String>(0)?,&row.get::<_,Vec<u8>>(1)?)?;
        }
        for (key,value) in &units {
            super::lww::validate_received(&self.connection,key,value)?;
        }
        Ok(units)
    }

    pub(crate) fn capture_portable_records(
        &self,
        lease: &str,
        destination: &mut Connection,
        probe: &dyn CancellationProbe,
    ) -> StoreResult<Vec<RawTableDigest>> {
        let (source, target) = self.read_view(Some(lease))?;
        copy_generation(source, &target.generation, destination, probe)
    }

    /// Prepare raw rows without activating them. The file restore coordinator must additionally
    /// validate payloads, owner manifests and the F0 reference contract, publish recovery, and
    /// obtain the existing replacement fence before using the normal commit API.
    pub(crate) fn portable_object_present(&self, hash: &str, expected_size:u64) -> StoreResult<bool> {
        risunest_sync_wire::validate_hash(hash).map_err(|e| super::StoreError::Validation { message: e.to_string() })?;
        match crate::asset_repository::PayloadCas::new(&self.repository_root)?.stat_object(hash)? {
            None=>Ok(false),
            Some(size) if size==expected_size=>Ok(true),
            Some(_)=>Err(invalid("existing portable payload size mismatch")),
        }
    }

    pub(crate) fn stage_portable_records(
        &mut self,
        source: &Connection,
        probe: &dyn CancellationProbe,
    ) -> StoreResult<super::StagingResult> {
        super::portable_validation::validate_records(source, probe)?;
        let expected = digest_raw_tables(source, probe)?;
        let stage = self.replace_begin()?;
        let result = (|| -> StoreResult<()> {
            let transaction = self.connection.transaction()?;
            for table in TABLES {
                cancelled(probe)?;
                transaction.execute(
                    &format!("DELETE FROM {} WHERE generation=?1", table.name),
                    [&stage.staging_id],
                )?;
                let columns = table.column_list();
                let mut select = source.prepare(&format!(
                    "SELECT {columns} FROM {} ORDER BY {}",
                    table.name, table.order
                ))?;
                let placeholders = (0..=table.columns.len())
                    .map(|_| "?")
                    .collect::<Vec<_>>()
                    .join(",");
                let mut insert = transaction.prepare(&format!(
                    "INSERT INTO {} (generation,{columns}) VALUES ({placeholders})",
                    table.name
                ))?;
                let mut rows = select.query([])?;
                while let Some(row) = rows.next()? {
                    cancelled(probe)?;
                    insert.raw_bind_parameter(1, &stage.staging_id)?;
                    for index in 0..table.columns.len() {
                        insert.raw_bind_parameter(
                            index + 2,
                            rusqlite::types::ToSqlOutput::Borrowed(row.get_ref(index)?),
                        )?;
                    }
                    insert.raw_execute()?;
                }
            }
            if digest_tables(&transaction, Some(&stage.staging_id), probe)? != expected {
                return Err(invalid("portable staging changed raw SQL values"));
            }
            cancelled(probe)?;
            transaction.commit()?;
            Ok(())
        })();
        if let Err(error) = result {
            self.replace_abort(&stage.staging_id)?;
            return Err(error);
        }
        Ok(stage)
    }
}

/// Stages only the records a partial import chose, keeping the settings record and the storage
/// authorities whole. Every row it writes is copied from the archive unchanged except for the
/// ordering columns, which are renumbered because the records around them are gone.
///
/// The selected view is shared by validation, copying and object installation.
pub(crate) fn stage_portable_records_selected(
    store: &mut PersistentStore,
    source: &Connection,
    selection: &crate::portable_backup::ClosedSelection,
    probe: &dyn CancellationProbe,
) -> StoreResult<super::StagingResult> {
    install_selected_views(source, selection, probe)?;
    super::portable_validation::validate_records(source, probe)?;
    let stage = store.replace_begin()?;
    let result = copy_selected(store, source, &stage.staging_id, selection, probe);
    if let Err(error) = result {
        store.replace_abort(&stage.staging_id)?;
        return Err(error);
    }
    Ok(stage)
}

fn install_selected_views(
    source: &Connection,
    selection: &crate::portable_backup::ClosedSelection,
    probe: &dyn CancellationProbe,
) -> StoreResult<()> {
    let query_only: bool = source.pragma_query_value(None, "query_only", |row| row.get(0))?;
    source.pragma_update(None, "query_only", false)?;
    let result = (|| -> StoreResult<()> {
    source.execute_batch("CREATE TEMP TABLE selected_characters(id TEXT PRIMARY KEY); CREATE TEMP TABLE selected_presets(id TEXT PRIMARY KEY); CREATE TEMP TABLE selected_plugins(owner TEXT,storage_key TEXT,PRIMARY KEY(owner,storage_key)); CREATE TEMP TABLE selected_aliases(kind TEXT,logical_key TEXT,PRIMARY KEY(kind,logical_key));")?;
    for id in &selection.characters { source.execute("INSERT OR IGNORE INTO selected_characters VALUES(?1)", [id])?; }
    for id in &selection.presets { source.execute("INSERT OR IGNORE INTO selected_presets VALUES(?1)", [id])?; }
    for key in &selection.plugins { source.execute("INSERT OR IGNORE INTO selected_plugins VALUES(?1,?2)", rusqlite::params![key.owner,key.key])?; }
    for (table, condition) in [
        ("characters", "character_id IN (SELECT id FROM selected_characters)"),
        ("conversations", "character_id IN (SELECT id FROM selected_characters)"),
        ("messages", "character_id IN (SELECT id FROM selected_characters)"),
        ("bot_presets", "preset_id IN (SELECT id FROM selected_presets)"),
        ("plugin_storage", "(owner,storage_key) IN (SELECT owner,storage_key FROM selected_plugins)"),
        ("asset_owner_heads", "owner_kind!='character-additional-assets' OR owner_locator IN (SELECT id FROM selected_characters)"),
    ] {
        source.execute_batch(&format!("CREATE TEMP VIEW {table} AS SELECT * FROM main.{table} WHERE {condition}"))?;
    }
    use crate::lossless_f0::{scan_portable_fragment, PortableFragment};
    for (kind, sql) in [
        ("root", "SELECT value,'','',0 FROM root"),
        ("preset", "SELECT value,'','',configured_index FROM bot_presets"),
        ("plugin", "SELECT value,storage_key,'',0 FROM plugin_storage"),
        ("character", "SELECT detail,character_id,'',conversation_count FROM characters"),
        ("conversation", "SELECT detail,character_id,'',0 FROM conversations"),
        ("message", "SELECT value,character_id,conversation_id,message_index FROM messages"),
    ] {
        let mut statement = source.prepare(sql)?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            cancelled(probe)?;
            let encoded: String = row.get(0)?;
            let value: serde_json::Value = serde_json::from_str(&encoded).map_err(|_| invalid("selected record JSON is invalid"))?;
            let id: String = row.get(1)?;
            let conversation: String = row.get(2)?;
            let index: i64 = row.get(3)?;
            let fragment = match kind {
                "root" => PortableFragment::Root { value: &value, selected_preset: None },
                "preset" => PortableFragment::Preset { value: &value, index },
                "plugin" => PortableFragment::Plugin { value: &value, key: &id },
                "character" => PortableFragment::Character { value: &value, selected_chat: None, has_chats: index > 0 },
                "conversation" => PortableFragment::Conversation { value: &value, character_id: &id },
                _ => PortableFragment::Message { value: &value, character_id: &id, conversation_id: &conversation, index },
            };
            for reference in scan_portable_fragment(fragment).map_err(|_| invalid("selected reference cannot be scanned"))? {
                if matches!(reference.target_kind.as_str(), "asset" | "inlay") {
                    source.execute("INSERT OR IGNORE INTO selected_aliases VALUES(?1,?2)", rusqlite::params![reference.target_kind,reference.target_key])?;
                }
            }
        }
    }
    source.execute_batch("CREATE TEMP TABLE selected_archived_hashes(hash TEXT PRIMARY KEY)")?;
    let mut statement = source.prepare("SELECT archived_object FROM characters WHERE archived_object IS NOT NULL")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        cancelled(probe)?;
        let archived: super::archive::ArchivedObject = serde_json::from_str(&row.get::<_,String>(0)?).map_err(|_| invalid("selected archived payload is invalid"))?;
        for hash in archived.object_roots() {
            source.execute("INSERT OR IGNORE INTO selected_archived_hashes VALUES(?1)", [hash])?;
        }
    }
    source.execute_batch("CREATE TEMP VIEW asset_aliases AS SELECT * FROM main.asset_aliases WHERE (kind,logical_key) IN (SELECT kind,logical_key FROM selected_aliases) OR object_hash IN (SELECT hash FROM selected_archived_hashes)")?;
    Ok(())
    })();
    source.pragma_update(None, "query_only", query_only)?;
    result
}

fn copy_selected(
    store: &mut PersistentStore,
    source: &Connection,
    staging: &str,
    _selection: &crate::portable_backup::ClosedSelection,
    probe: &dyn CancellationProbe,
) -> StoreResult<()> {
    let transaction = store.connection.transaction()?;
    for table in TABLES {
        cancelled(probe)?;
        let columns = table.column_list();
        transaction.execute(
            &format!("DELETE FROM {} WHERE generation=?1", table.name),
            [staging],
        )?;
        let mut select = source.prepare(&format!(
            "SELECT {columns} FROM {} ORDER BY {}",
            table.name,
            table.order
        ))?;
        let placeholders = (0..=table.columns.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let mut insert = transaction.prepare(&format!(
            "INSERT INTO {} (generation,{columns}) VALUES ({placeholders})",
            table.name
        ))?;
        let mut rows = select.query([])?;
        while let Some(row) = rows.next()? {
            cancelled(probe)?;
            insert.raw_bind_parameter(1, staging)?;
            for index in 0..table.columns.len() {
                insert.raw_bind_parameter(
                    index + 2,
                    rusqlite::types::ToSqlOutput::Borrowed(row.get_ref(index)?),
                )?;
            }
            insert.raw_execute()?;
        }
        drop(rows);
        drop(insert);
        drop(select);
    }
    // An owner head belongs to a record; the ones whose character stayed behind have no owner.
    transaction.execute(
        "DELETE FROM asset_owner_heads WHERE generation=?1 AND owner_kind='character-additional-assets'
         AND owner_locator NOT IN (SELECT character_id FROM characters WHERE generation=?1)",
        [staging],
    )?;
    renumber_selected(&transaction, staging)?;
    transaction.commit()?;
    Ok(())
}

/// Closes the gaps the dropped records left in the ordering columns, and points the chosen preset
/// at wherever it landed. Nothing else about the settings record changes.
fn renumber_selected(transaction: &Connection, staging: &str) -> StoreResult<()> {
    for (table, identity_columns, order) in [
        ("characters", &["character_id"][..], "configured_index"),
        ("plugin_storage", &["owner", "storage_key"][..], "ordinal"),
    ] {
        let identity = identity_columns
            .iter()
            .map(|column| format!("\"{column}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let prefixed = identity_columns
            .iter()
            .map(|column| format!("{table}.\"{column}\""))
            .collect::<Vec<_>>()
            .join(", ");
        transaction.execute(
            &format!(
                "UPDATE {table} SET \"{order}\"=(
                     SELECT position-1 FROM (
                         SELECT {identity}, row_number() OVER (ORDER BY \"{order}\") AS position
                         FROM {table} WHERE generation=?1
                     ) ranked WHERE ({identity})=({prefixed})
                 ) WHERE generation=?1",
            ),
            [staging],
        )?;
    }
    let kept: Vec<String> = {
        let mut statement = transaction.prepare(
            "SELECT preset_id FROM bot_presets WHERE generation=?1 ORDER BY configured_index",
        )?;
        let mut rows = statement.query([staging])?;
        let mut kept = Vec::new();
        while let Some(row) = rows.next()? {
            kept.push(row.get(0)?);
        }
        kept
    };
    let selected: Option<String> = transaction
        .query_row(
            "SELECT json_extract(value,'$.botPresetsId') FROM root WHERE generation=?1",
            [staging],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    for (position, preset_id) in kept.iter().enumerate() {
        transaction.execute(
            "UPDATE bot_presets SET configured_index=?3 WHERE generation=?1 AND preset_id=?2",
            rusqlite::params![staging, preset_id, position as i64],
        )?;
    }
    if let Some(selected) = selected {
        let landed = kept
            .iter()
            .find(|preset_id| **preset_id == selected)
            .or_else(|| kept.first());
        if landed != Some(&selected) {
            transaction.execute(
                "UPDATE root SET value=CASE WHEN ?2 IS NULL THEN json_remove(value,'$.botPresetsId') ELSE json_set(value,'$.botPresetsId',?2) END WHERE generation=?1",
                rusqlite::params![staging, landed],
            )?;
        }
    }
    Ok(())
}

pub(crate) fn digest_raw_tables(
    source: &Connection,
    probe: &dyn CancellationProbe,
) -> StoreResult<Vec<RawTableDigest>> {
    digest_tables(source, None, probe)
}

fn digest_tables(
    source: &Connection,
    generation: Option<&str>,
    probe: &dyn CancellationProbe,
) -> StoreResult<Vec<RawTableDigest>> {
    let mut result = Vec::with_capacity(TABLES.len());
    for table in TABLES {
        let mut select = source.prepare(&format!(
            "SELECT {} FROM {} {} ORDER BY {}",
            table.column_list(),
            table.name,
            if generation.is_some() {
                "WHERE generation=?1"
            } else {
                ""
            },
            table.order
        ))?;
        let mut rows = select.query(rusqlite::params_from_iter(generation))?;
        let mut digest = table_hasher(table);
        let mut count = 0_u64;
        while let Some(row) = rows.next()? {
            cancelled(probe)?;
            digest.update([0xf0]);
            for index in 0..table.columns.len() {
                hash_value(&mut digest, row.get_ref(index)?);
            }
            count = count
                .checked_add(1)
                .ok_or_else(|| invalid("portable row count overflow"))?;
        }
        digest.update(count.to_le_bytes());
        result.push(RawTableDigest {
            table: table.name,
            rows: count,
            sha256: digest.finalize().into(),
        });
    }
    cancelled(probe)?;
    Ok(result)
}

fn table_hasher(table: &PortableTable) -> Sha256 {
    let mut hash = Sha256::new();
    hash.update(b"risunest-portable-sql-v1\0");
    hash.update((table.name.len() as u64).to_le_bytes());
    hash.update(table.name.as_bytes());
    hash.update((table.columns.len() as u64).to_le_bytes());
    hash
}

fn hash_value(hash: &mut Sha256, value: ValueRef<'_>) {
    match value {
        ValueRef::Null => hash.update([0]),
        ValueRef::Integer(value) => {
            hash.update([1]);
            hash.update(value.to_le_bytes());
        }
        ValueRef::Real(value) => {
            hash.update([2]);
            hash.update(value.to_bits().to_le_bytes());
        }
        ValueRef::Text(value) => {
            hash.update([3]);
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value);
        }
        ValueRef::Blob(value) => {
            hash.update([4]);
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value);
        }
    }
}

fn validate_live_columns(source: &Connection) -> StoreResult<()> {
    // Operational generation rows are explicitly excluded, never inherited from the live-store
    // cleanup list. A newly introduced family must be classified before claiming full capture.
    let mut tables = source.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
    let mut names = tables.query([])?;
    while let Some(row) = names.next()? {
        let name: String = row.get(0)?;
        let generation: bool = source.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name='generation')",
            [&name],
            |r| r.get(0),
        )?;
        if generation
            && !TABLES.iter().any(|t| t.name == name)
            && !matches!(
                name.as_str(),
                "asset_alias_replacement_candidates"
                    | "snapshot_original_meta"
                    | "snapshot_leases"
                    | "content_change_context"
                    | "content_changes"
                    | "content_change_consumers"
                    | "content_change_floor"
                    | "message_page_indexes"
                    | "message_page_manifests"
            )
        {
            return Err(invalid(&format!(
                "unclassified generation-scoped source table: {name}"
            )));
        }
    }
    for table in TABLES {
        let mut statement = source.prepare(&format!("PRAGMA table_info({})", table.name))?;
        let actual = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut expected = std::iter::once(("generation", "TEXT"))
            .chain(table.columns.iter().copied())
            .map(|(name, kind)| (name.to_owned(), kind.to_owned()))
            .collect::<Vec<_>>();
        if table.name=="messages" {
            expected.extend([("canonical_hash".to_owned(),"TEXT".to_owned()),("canonical_size".to_owned(),"INTEGER".to_owned())]);
        }
        if actual != expected {
            return Err(invalid(
                "portable source columns differ from the reviewed schema",
            ));
        }
    }
    Ok(())
}

fn invalid(message: &str) -> StoreError {
    StoreError::Validation {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn selected_presets_keep_stable_ids_and_selected_identity_when_order_is_compacted() {
        for (selected, remove_all, expected) in [
            ("preset-c", false, Some("preset-c")),
            ("preset-a", false, Some("preset-b")),
            ("preset-c", true, None),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let mut store = PersistentStore::open(directory.path()).unwrap();
            let stage = store.replace_begin().unwrap().staging_id;
            store.replace_put_root(&stage, &serde_json::json!({"botPresetsId":selected})).unwrap();
            store.replace_put_presets(&stage, &[
                serde_json::json!({"id":"preset-a","name":"A"}),
                serde_json::json!({"id":"preset-b","name":"B"}),
                serde_json::json!({"id":"preset-c","name":"C"}),
            ]).unwrap();
            store.connection.execute(
                "DELETE FROM bot_presets WHERE generation=?1 AND (?2 OR preset_id='preset-a')",
                params![stage, remove_all],
            ).unwrap();
            renumber_selected(&store.connection, &stage).unwrap();
            let actual: Vec<(String, i64, String)> = store.connection.prepare(
                "SELECT preset_id,configured_index,value FROM bot_presets WHERE generation=?1 ORDER BY configured_index",
            ).unwrap().query_map([&stage], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)))
                .unwrap().collect::<Result<_,_>>().unwrap();
            let expected_ids: &[&str] = if remove_all { &[] } else { &["preset-b", "preset-c"] };
            assert_eq!(actual.iter().map(|row|row.0.as_str()).collect::<Vec<_>>(), expected_ids);
            for (position, (id, order, raw)) in actual.iter().enumerate() {
                assert_eq!(*order, position as i64);
                assert_eq!(serde_json::from_str::<serde_json::Value>(raw).unwrap()["id"], *id);
            }
            let root: String = store.connection.query_row("SELECT value FROM root WHERE generation=?1", [&stage], |row|row.get(0)).unwrap();
            assert_eq!(serde_json::from_str::<serde_json::Value>(&root).unwrap().get("botPresetsId").and_then(serde_json::Value::as_str), expected);
            store.replace_abort(&stage).unwrap();
        }
    }

    #[test]
    fn selected_views_keep_only_selected_valid_rows_and_owner_qualified_plugins() {
        let source = Connection::open_in_memory().unwrap();
        create_raw_tables(&source).unwrap();
        source.execute("INSERT INTO root VALUES('{}')", []).unwrap();
        for (index, id) in ["a", "b"].into_iter().enumerate() {
            let detail = serde_json::json!({"chaId":id,"name":id,"type":"character","image":format!("assets/{id}")}).to_string();
            source.execute("INSERT INTO characters VALUES(?1,?2,0,0,?1,?3,0,'character',NULL,NULL,?4,NULL)", rusqlite::params![id,index as i64,format!("assets/{id}"),detail]).unwrap();
            source.execute("INSERT INTO asset_aliases VALUES(?1,?2,'asset',1,'application/octet-stream','','',NULL,NULL,NULL,'{}')", rusqlite::params![format!("assets/{id}"),"a".repeat(64)]).unwrap();
            source.execute("INSERT INTO asset_owner_heads VALUES('character-additional-assets',?1,0,NULL,0)", [id]).unwrap();
        }
        source.execute("UPDATE characters SET detail='broken' WHERE character_id='b'", []).unwrap();
        for (index, owner) in ["plugin:a", "plugin:b"].into_iter().enumerate() {
            source.execute("INSERT INTO plugin_storage VALUES(?1,'same/key',2,?2,'{}',NULL,NULL,NULL)", rusqlite::params![owner,index as i64]).unwrap();
        }
        let selection = crate::portable_backup::ClosedSelection {
            characters: vec!["a".into()],
            plugins: vec![crate::portable_backup::PluginKey { owner:"plugin:a".into(),key:"same/key".into() }],
            ..Default::default()
        };
        source.pragma_update(None, "query_only", true).unwrap();
        install_selected_views(&source, &selection, &Never).unwrap();
        assert!(source.pragma_query_value(None, "query_only", |row| row.get::<_,bool>(0)).unwrap());
        for (table, column, expected) in [("characters","character_id","a"),("asset_aliases","logical_key","assets/a"),("asset_owner_heads","owner_locator","a"),("plugin_storage","owner","plugin:a")] {
            let mut statement = source.prepare(&format!("SELECT {column} FROM {table}")).unwrap();
            let values = statement.query_map([], |row| row.get::<_,String>(0)).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
            assert_eq!(values, [expected]);
        }
        let root = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(root.path()).unwrap();
        super::super::portable_validation::validate_records(&source, &Never).unwrap();
        let stage = store.replace_begin().unwrap();
        copy_selected(&mut store, &source, &stage.staging_id, &selection, &Never).unwrap();
        assert_eq!(store.portable_staged_counts(&stage.staging_id).unwrap(), (1,0));
        store.replace_abort(&stage.staging_id).unwrap();
    }

    use super::*;
    use rusqlite::params;
    struct Never;
    impl CancellationProbe for Never {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    #[test]
    fn portable_presence_checks_physical_metadata_without_hashing_present_bodies() {
        let directory=tempfile::tempdir().unwrap();
        let mut store=PersistentStore::open(directory.path()).unwrap();
        let bytes=b"synthetic physically present payload";
        let hash=risunest_sync_wire::hash(bytes);
        store.asset_object_catalog().register(&[super::super::asset_object_catalog::AssetObjectRegistration {object_hash:hash.clone(),byte_size:bytes.len() as u64}],0).unwrap();
        assert!(!store.portable_object_present(&hash,bytes.len() as u64).unwrap());
        let cas=crate::asset_repository::PayloadCas::new(directory.path()).unwrap();
        cas.prepare_bytes(bytes).unwrap();
        crate::asset_repository::body_io::reset_body_io();
        assert!(store.portable_object_present(&hash,bytes.len() as u64).unwrap());
        let observed=crate::asset_repository::body_io::take_body_io();
        assert!(observed.complete());
        assert_eq!(observed.asset_work().opens,0);
        assert_eq!(observed.asset_work().read_bytes,0);
        assert!(observed.asset_work().body_sha.values().all(|work|work.bytes==0));
        assert!(store.portable_object_present(&hash,bytes.len() as u64+1).is_err());
    }

    #[test]
    fn portable_capture_excludes_root_account_but_keeps_other_values() {
        let source = rusqlite::Connection::open_in_memory().unwrap();
        source.execute_batch("CREATE TABLE root(generation TEXT,value TEXT)").unwrap();
        source.execute("INSERT INTO root VALUES('chosen',?1)", [r#"{"account":{"token":"synthetic-secret"},"username":"Synthetic"}"#]).unwrap();
        for table in TABLES.iter().filter(|table| table.name != "root") {
            let columns = table.columns.iter().map(|(name, kind)| format!("{name} {kind}")).collect::<Vec<_>>().join(",");
            source.execute_batch(&format!("CREATE TABLE {} (generation TEXT,{columns})", table.name)).unwrap();
        }
        source.execute_batch("ALTER TABLE messages ADD COLUMN canonical_hash TEXT; ALTER TABLE messages ADD COLUMN canonical_size INTEGER").unwrap();
        let mut output = rusqlite::Connection::open_in_memory().unwrap();
        create_raw_tables(&output).unwrap();
        copy_generation(&source, "chosen", &mut output, &Never).unwrap();
        let encoded: String = output.query_row("SELECT value FROM root", [], |row| row.get(0)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(value, serde_json::json!({"username":"Synthetic"}));
        let original: String = source.query_row("SELECT value FROM root", [], |row| row.get(0)).unwrap();
        assert!(original.contains("synthetic-secret"));
    }

    #[test]
    fn portable_capture_reads_the_leased_revision_after_a_live_commit() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let stage = store.replace_begin().unwrap();
        store
            .replace_put_root(&stage.staging_id, &serde_json::json!({"marker":"before"}))
            .unwrap();
        store.replace_commit(&stage.staging_id, Some(0)).unwrap();
        let lease = store.acquire_revision(1).unwrap().lease;
        let next = store.replace_begin().unwrap();
        store
            .replace_put_root(&next.staging_id, &serde_json::json!({"marker":"after"}))
            .unwrap();
        store.replace_commit(&next.staging_id, Some(1)).unwrap();
        let mut output = Connection::open_in_memory().unwrap();
        create_raw_tables(&output).unwrap();
        let captured = store
            .capture_portable_records(&lease, &mut output, &Never)
            .unwrap();
        assert_eq!(captured, digest_raw_tables(&output, &Never).unwrap());
        assert_eq!(
            output
                .query_row("SELECT value FROM root", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "{\"marker\":\"before\"}"
        );
        store.release_revision(&lease).unwrap();
    }

    #[test]
    fn raw_capture_retains_orphans_bad_json_types_and_exact_text() {
        let mut source = Connection::open_in_memory().unwrap();
        super::super::schema::initialize(&mut source).unwrap();
        source
            .execute(
                "INSERT INTO root VALUES('chosen',?1)",
                ["{ \"z\":null, \"a\":{}, \"empty\":\"\" }"],
            )
            .unwrap();
        source
            .execute("INSERT INTO root VALUES('other','{}')", [])
            .unwrap();
        source
            .execute(
                "INSERT INTO conversations VALUES('chosen','orphan','chat',7,0,'',1,?1)",
                ["not JSON"],
            )
            .unwrap();
        source
            .execute(
                "INSERT INTO messages VALUES('chosen','orphan','chat',9,NULL,?1,'',0)",
                ["{\"data\":\"合成\"}"],
            )
            .unwrap();
        source
            .execute(
                "INSERT INTO plugin_storage VALUES('chosen','synthetic-plugin','blob',3,8,?1,NULL,NULL,NULL)",
                params![vec![0_u8, 0xff, 0x20]],
            )
            .unwrap();
        source
            .execute(
                "INSERT INTO plugin_storage VALUES('chosen','synthetic-plugin','bad-text',1,9,CAST(x'ff' AS TEXT),NULL,NULL,NULL)",
                [],
            )
            .unwrap();
        let mut output = Connection::open_in_memory().unwrap();
        create_raw_tables(&output).unwrap();
        let copied = copy_generation(&source, "chosen", &mut output, &Never).unwrap();
        assert_eq!(copied, digest_raw_tables(&output, &Never).unwrap());
        assert_eq!(
            output
                .query_row("SELECT count(*) FROM root", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            output
                .query_row("SELECT detail FROM conversations", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "not JSON"
        );
        assert_eq!(
            output
                .query_row("SELECT value FROM root", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "{ \"z\":null, \"a\":{}, \"empty\":\"\" }"
        );
        assert_eq!(output.query_row("SELECT typeof(value), hex(value) FROM plugin_storage WHERE storage_key='bad-text'", [], |r| Ok((r.get::<_, String>(0)?,r.get::<_, String>(1)?))).unwrap(), ("text".into(), "FF".into()));
        assert_eq!(
            output
                .query_row(
                    "SELECT typeof(value) FROM plugin_storage WHERE storage_key='blob'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "blob"
        );
        assert_eq!(
            output
                .query_row("SELECT message_id FROM messages", [], |r| r
                    .get::<_, Option<String>>(0))
                .unwrap(),
            None
        );
        assert_eq!(
            source
                .query_row("SELECT count(*) FROM root", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn unknown_source_column_is_not_silently_omitted() {
        let mut source = Connection::open_in_memory().unwrap();
        super::super::schema::initialize(&mut source).unwrap();
        source
            .execute_batch("ALTER TABLE root ADD COLUMN new_data TEXT")
            .unwrap();
        assert!(validate_live_columns(&source).is_err());
    }

    #[test]
    fn digest_distinguishes_sql_classes_null_empty_and_boundaries() {
        fn digest(value: ValueRef<'_>) -> Vec<u8> {
            let mut h = Sha256::new();
            hash_value(&mut h, value);
            h.finalize().to_vec()
        }
        assert_ne!(
            digest(ValueRef::Text(b"same")),
            digest(ValueRef::Blob(b"same"))
        );
        assert_ne!(digest(ValueRef::Null), digest(ValueRef::Text(b"")));
        assert_ne!(digest(ValueRef::Integer(1)), digest(ValueRef::Real(1.0)));
    }

    #[test]
    fn cancellation_rolls_back_partial_tables() {
        use std::cell::Cell;
        struct CancelAfter(Cell<u32>);
        impl CancellationProbe for CancelAfter {
            fn is_cancelled(&self) -> bool {
                self.0.set(self.0.get() + 1);
                self.0.get() > 2
            }
        }
        let mut source = Connection::open_in_memory().unwrap();
        super::super::schema::initialize(&mut source).unwrap();
        source
            .execute("INSERT INTO root VALUES('chosen','{}')", [])
            .unwrap();
        let mut output = Connection::open_in_memory().unwrap();
        create_raw_tables(&output).unwrap();
        assert!(
            copy_generation(&source, "chosen", &mut output, &CancelAfter(Cell::new(0))).is_err()
        );
        assert_eq!(
            output
                .query_row("SELECT count(*) FROM root", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
