use super::{
    error, is_device, parent_status, projection, put_unit, validate_header, verify, wire, Change,
    DecimalU64, Header, PersistentStore, StoreResult, UnitValue,
};
use crate::persistent_store::{message_pages, schema, GENERATION_TABLES};
use risunest_sync_wire::{
    canonical,
    descriptor::{ReferencePage, MAX_DESCRIPTOR_REFERENCES, MAX_TREE_DEPTH},
    MAX_METADATA_BYTES,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, io::Write};

pub(in crate::persistent_store) const BINDING_STAGE_SCHEMA: &str = r#"CREATE TABLE lww_binding_sources(request_id TEXT PRIMARY KEY,staging_id TEXT NOT NULL UNIQUE,authority TEXT NOT NULL,inspection_id TEXT NOT NULL,source_digest TEXT NOT NULL,admitted_upper_ms TEXT NOT NULL,status_digest TEXT NOT NULL,catalog_digest TEXT NOT NULL);
CREATE TABLE lww_binding_source_units(staging_id TEXT NOT NULL,key TEXT NOT NULL,stamp TEXT NOT NULL,value TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('ready','held','retired')),PRIMARY KEY(staging_id,key));
"#;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BindingUnitStage {
    pub staging_id: String,
    pub source_digest: String,
}

struct HashWriter(Sha256, #[cfg(test)] &'static str);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        #[cfg(test)]
        crate::persistent_store::hash_work::update(self.1, bytes.len());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
fn source_digest(changes: &[&Change]) -> StoreResult<String> {
    let mut writer = HashWriter(Sha256::new(), #[cfg(test)] "binding_source_proof");
    #[cfg(test)]
    crate::persistent_store::hash_work::begin("binding_source_proof");
    writer.0.update(b"risunest.lww-binding-source/v1\0");
    #[cfg(test)]
    crate::persistent_store::hash_work::update("binding_source_proof", b"risunest.lww-binding-source/v1\0".len());
    serde_json::to_writer(&mut writer, changes)?;
    Ok(hex::encode(writer.0.finalize()))
}

#[cfg(test)]
fn sorted_source(changes: &[Change]) -> StoreResult<Vec<&Change>> {
    let mut changes = changes.iter().collect::<Vec<_>>();
    changes.sort_by(|a, b| a.key.cmp(&b.key));
    if changes.windows(2).any(|pair| pair[0].key == pair[1].key) {
        return Err(error("duplicate-unit-key"));
    }
    Ok(changes)
}

#[cfg(test)]
fn status_digest(statuses: &[(&str, &str)]) -> StoreResult<String> {
    let mut writer = HashWriter(Sha256::new(), #[cfg(test)] "binding_status_proof");
    #[cfg(test)]
    crate::persistent_store::hash_work::begin("binding_status_proof");
    writer.0.update(b"risunest.lww-binding-status/v1\0");
    #[cfg(test)]
    crate::persistent_store::hash_work::update("binding_status_proof", b"risunest.lww-binding-status/v1\0".len());
    serde_json::to_writer(&mut writer, statuses)?;
    Ok(hex::encode(writer.0.finalize()))
}

fn catalog_columns(db: &Connection, table: &str) -> StoreResult<Vec<String>> {
    let mut stmt = db.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?.collect::<Result<Vec<_>, _>>()?;
    Ok(names.into_iter().filter(|name| name != "generation").collect())
}

pub(in crate::persistent_store) fn catalog_digest(db: &Connection, staging_id: &str) -> StoreResult<String> {
    let mut writer = HashWriter(Sha256::new(), #[cfg(test)] "binding_catalog_proof");
    #[cfg(test)]
    crate::persistent_store::hash_work::begin("binding_catalog_proof");
    writer.0.update(b"risunest.lww-binding-catalog/v1\0");
    #[cfg(test)]
    crate::persistent_store::hash_work::update("binding_catalog_proof", b"risunest.lww-binding-catalog/v1\0".len());
    for &(table, _) in GENERATION_TABLES {
        let columns = catalog_columns(db, table)?;
        serde_json::to_writer(&mut writer, &(table, &columns))?;
        let columns_sql = columns.join(",");
        let order = {
            let mut stmt = db.prepare(&format!("PRAGMA table_info({table})"))?;
            let columns = stmt.query_map([], |row| Ok((row.get::<_,String>(1)?,row.get::<_,u32>(5)?)))?.collect::<Result<Vec<_>,_>>()?;
            let mut keys = columns.into_iter().filter(|(name,position)| name != "generation" && *position > 0).collect::<Vec<_>>();
            keys.sort_by_key(|(_,position)| *position);
            if keys.is_empty() { String::new() } else { format!(" ORDER BY {}",keys.into_iter().map(|(name,_)| name).collect::<Vec<_>>().join(",")) }
        };
        let mut stmt = db.prepare(&format!("SELECT {columns_sql} FROM {table} WHERE generation=?1{order}"))?;
        let mut rows = stmt.query([staging_id])?;
        while let Some(row) = rows.next()? {
            let mut values = Vec::with_capacity(columns.len());
            for index in 0..columns.len() {
                use rusqlite::types::Value;
                values.push(match row.get::<_, Value>(index)? {
                    Value::Null => serde_json::Value::Null,
                    Value::Integer(value) => serde_json::json!(["integer",value]),
                    Value::Real(value) => serde_json::json!(["real",value]),
                    Value::Text(value) => serde_json::json!(["text",value]),
                    Value::Blob(value) => {
                        use base64::Engine;
                        serde_json::json!(["blob",base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value)])
                    }
                });
            }
            serde_json::to_writer(&mut writer, &values)?;
        }
    }
    Ok(hex::encode(writer.0.finalize()))
}

fn binding_copy_insert_sql(table: &str, columns: &[String]) -> String {
    let placeholders = (0..=columns.len()).map(|_| "?").collect::<Vec<_>>().join(",");
    format!("INSERT INTO {table}(generation,{}) VALUES ({placeholders})", columns.join(","))
}

fn copy_binding_table(tx: &Transaction<'_>, table: &str, staging_id: &str) -> StoreResult<()> {
    let columns = catalog_columns(tx, table)?;
    let mut select = tx.prepare(&format!(
        "SELECT {} FROM binding_incoming.{table} WHERE generation='incoming'", columns.join(",")
    ))?;
    let mut insert = tx.prepare(&binding_copy_insert_sql(table, &columns))?;
    let mut rows = select.query([])?;
    // INSERT SELECT on a triggered table buffers every source row in a temporary B-tree.
    // Android forces that B-tree into memory; single-row VALUES retains triggers and rollback.
    while let Some(row) = rows.next()? {
        insert.raw_bind_parameter(1, staging_id)?;
        for index in 0..columns.len() {
            insert.raw_bind_parameter(index + 2, rusqlite::types::ToSqlOutput::Borrowed(row.get_ref(index)?))?;
        }
        insert.raw_execute()?;
    }
    Ok(())
}

fn validate_inspection(store: &PersistentStore, header: &Header, inspection_id: &str) -> StoreResult<()> {
    validate_header(header)?;
    verify(store.device_store()?.connection(), header.binding_authority)?;
    let inspection: Option<(String, String)> = store.connection.query_row(
        "SELECT source_authority,source_epoch FROM lww_binding_inspections WHERE inspection_id=?1",
        [inspection_id], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    if inspection != Some((header.binding_authority.0.to_string(), store.lww_binding_state()?.selection_epoch)) {
        return Err(error("stale-binding-inspection"));
    }
    Ok(())
}

impl PersistentStore {
    // A native transport supplies a complete verified winning map, never a renderer database.
    pub(crate) fn lww_stage_binding_units(
        &mut self, header: &Header, inspection_id: &str, changes: &[Change], admitted_upper_ms: DecimalU64,
    ) -> StoreResult<super::BindingUnitStage> {
        self.lww_stage_binding_units_stream(header, inspection_id, admitted_upper_ms, |emit| {
            for change in changes { emit(change.clone())?; }
            Ok(())
        })
    }

    pub(crate) fn lww_stage_binding_units_stream(
        &mut self,
        header: &Header,
        inspection_id: &str,
        admitted_upper_ms: DecimalU64,
        produce: impl FnOnce(&mut dyn FnMut(Change) -> StoreResult<()>) -> StoreResult<()>,
    ) -> StoreResult<super::BindingUnitStage> {
        validate_inspection(self, header, inspection_id)?;
        let changes = super::FrozenRows::new()?;
        produce(&mut |change| {
            wire(change.stamp.validate())?;
            wire({
                let result = change.value.validate();
                #[cfg(test)]
                crate::persistent_store::hash_work::validation(&change.value);
                result
            })?;
            if change.stamp.physical_ms > admitted_upper_ms { return Err(error("incoming-clock-skew")); }
            if changes.db.query_row("SELECT EXISTS(SELECT 1 FROM rows WHERE key=?1)", [change.key.as_str()], |row| row.get::<_,bool>(0))? {
                return Err(error("duplicate-unit-key"));
            }
            changes.insert(super::intent_rows::target_row(&change)?)
        })?;
        let digest = source_digest_rows(&changes)?;
        let existing: Option<(String,String,String,String,String)> = self.connection.query_row(
            "SELECT staging_id,authority,inspection_id,source_digest,admitted_upper_ms FROM lww_binding_sources WHERE request_id=?1",
            [&header.request_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
        ).optional()?;
        if existing.is_none() && self.connection.query_row("SELECT EXISTS(SELECT 1 FROM lww_binding_stages WHERE receive_id=?1)", [&header.request_id], |row| row.get::<_,bool>(0))? {
            return Err(error("binding-source-missing"));
        }
        let staging_id = if let Some((id, authority, inspection, old_digest, upper)) = existing {
            if authority != header.binding_authority.0.to_string() || inspection != inspection_id || old_digest != digest || upper != admitted_upper_ms.0.to_string() {
                return Err(error("request-id-integrity"));
            }
            validate_binding_source_rows(&self.connection, &id, header, &changes)?;
            crate::persistent_store::sync_selection::validate_binding_stage_content(&self.connection, &id)?;
            id
        } else {
            let temporary = crate::external_storage::leftovers::local_scratch(&self.repository_root, "binding-stage-")?;
            #[cfg(test)]
            STAGE_SCRATCH.with(|scratch| scratch.replace(Some(temporary.path().to_owned())));
            let path = temporary.path().join("incoming.sqlite");
            let mut isolated = crate::sqlite_open::open(&path)?;
            schema::initialize(&mut isolated)?;
            isolated.execute_batch("INSERT INTO meta VALUES('activeGeneration','\"incoming\"'),('currentRevision','0'); INSERT INTO root VALUES('incoming','{}');")?;
            changes.visit(false, |key, stamp, value, _| {
                let change = Change { key, stamp: stamp.ok_or_else(|| error("request-id-integrity"))?, value };
                copy_controls(&self.connection, &isolated, &change)?;
                projection::validate_received(&isolated, &change.key, &change.value)
            })?;
            project_source(&mut isolated, &changes, &header.request_id)?;
            let statuses = stored_status_digest(&isolated, "incoming")?;
            isolated.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
            drop(isolated);
            let id = format!("staging-{}", uuid::Uuid::new_v4());
            self.connection.execute("ATTACH DATABASE ?1 AS binding_incoming", [path.to_string_lossy().as_ref()])?;
            let copied = (|| -> StoreResult<()> {
                let tx = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                super::commit::begin_generation(&tx, &id)?;
                for &(table, _) in GENERATION_TABLES { copy_binding_table(&tx, table, &id)?; }
                let catalog_digest = catalog_digest(&tx, &id)?;
                tx.execute("INSERT INTO lww_binding_sources VALUES(?1,?2,?3,?4,?5,?6,?7,?8)", params![header.request_id,id,header.binding_authority.0.to_string(),inspection_id,digest,admitted_upper_ms.0.to_string(),statuses,catalog_digest])?;
                tx.execute("INSERT INTO lww_binding_source_units SELECT ?1,key,stamp,value,status FROM binding_incoming.lww_binding_source_units WHERE staging_id='incoming'", [&id])?;
                tx.execute("INSERT INTO lww_binding_stages VALUES(?1,?2,?3,?4,NULL,?5)", params![id,inspection_id,header.request_id,digest,catalog_digest])?;
                tx.commit()?;
                Ok(())
            })();
            let detached = self.connection.execute_batch("DETACH DATABASE binding_incoming");
            copied?;
            detached?;
            id
        };
        #[cfg(test)]
        if FAIL_AFTER_STAGE_COPY.with(|fail| fail.replace(false)) { return Err(error("synthetic-after-binding-copy")); }
        self.prepare_replace_commit(&staging_id, None)?;
        Ok(BindingUnitStage { staging_id, source_digest: digest })
    }
}

fn project_source(db: &mut Connection, changes: &super::FrozenRows, version: &str) -> StoreResult<()> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    changes.visit(false, |key, stamp, value, _| {
        if !is_device(&key) { put_unit(&tx, &key, &stamp.ok_or_else(|| error("request-id-integrity"))?, &value, version, None)?; }
        Ok(())
    })?;
    let mut statement = changes.db.prepare("SELECT key,value FROM rows ORDER BY CASE WHEN json_extract(key,'$[0]')='exists' AND json_extract(key,'$[1]')<>'conversation' THEN 0 WHEN json_extract(key,'$[0]')='archive' THEN 2 WHEN json_extract(key,'$[0]')='exists' THEN 3 WHEN json_extract(key,'$[0]') IN ('conversation','messages') THEN 4 ELSE 1 END,key")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let key = wire(row.get::<_,String>(0)?.try_into())?;
        let value: UnitValue = serde_json::from_str(&row.get::<_,String>(1)?)?;
        if is_device(&key) { continue; }
        let hard_delete = key.components()[0] == "exists" && matches!(value, UnitValue::Deleted);
        if parent_status(&tx, &key)? == "ready" || hard_delete { projection::apply(&tx, "incoming", &key, &value)?; }
    }
    let affected = super::FrozenRows::new()?;
    changes.visit(false, |key, stamp, value, _| {
        let status = if is_device(&key) { "ready" } else { parent_status(&tx, &key)? };
        if status == "ready" && projection::known(&key) && !is_device(&key) {
            affected.insert(super::intent_rows::replacement_row(&key, &value, false)?)?;
        }
        tx.execute("INSERT INTO lww_binding_source_units VALUES('incoming',?1,?2,?3,?4)", params![key.as_str(),serde_json::to_string(&stamp.ok_or_else(|| error("request-id-integrity"))?)?,serde_json::to_string(&value)?,status])?;
        Ok(())
    })?;
    affected.refresh_orders(&tx, "incoming")?;
    tx.commit()?;
    Ok(())
}

fn copy_required_object(source: &Connection, target: &Connection, hash: &str) -> StoreResult<Vec<u8>> {
    let bytes = message_pages::object_body(source, hash)?.ok_or_else(|| error("binding-control-missing"))?;
    message_pages::put_object(target, hash, &bytes)?;
    Ok(bytes)
}

fn copy_controls(source: &Connection, target: &Connection, change: &Change) -> StoreResult<()> {
    let UnitValue::Object { descriptor, .. } = &change.value else { return Ok(()); };
    for (root, relations) in [(&descriptor.dependency_root, false), (&descriptor.relation_root, true)] {
        if let Some(root) = root {
            copy_reference_tree(source, target, root, relations, 0, &mut BTreeSet::new())?;
        }
    }
    if !projection::known(&change.key) { return Ok(()); }
    match change.key.components()[0].as_str() {
        "messages" => {
            let bytes = copy_required_object(source, target, &descriptor.object_hash)?;
            let manifest = {
                let result = risunest_external_storage_format::message_pages::MessageManifest::decode(&bytes);
                #[cfg(test)]
                crate::persistent_store::hash_work::decoded("native_manifest_decode_identity", &bytes, &result);
                result
            }.map_err(error)?;
            for page in manifest.pages { copy_required_object(source, target, &page.hash)?; }
        }
        _ => { copy_required_object(source, target, &descriptor.object_hash)?; }
    }
    Ok(())
}

fn copy_reference_tree(
    source: &Connection, target: &Connection, hash: &str, relations: bool, depth: usize,
    seen: &mut BTreeSet<String>,
) -> StoreResult<(String, String, usize)> {
    if depth >= MAX_TREE_DEPTH || !seen.insert(hash.into()) || seen.len() > MAX_DESCRIPTOR_REFERENCES {
        return Err(error("invalid-reference-tree"));
    }
    let body = copy_required_object(source, target, hash)?;
    let page: ReferencePage = wire(canonical::decode(&body, MAX_METADATA_BYTES))?;
    wire(page.validate())?;
    match page {
        ReferencePage::Branches { children } => {
            let mut first = None;
            let mut last: Option<String> = None;
            let mut count = 0usize;
            for child in children {
                let (child_first, child_last, child_count) = copy_reference_tree(source, target, &child, relations, depth + 1, seen)?;
                if last.as_ref().is_some_and(|last| last >= &child_first) { return Err(error("unordered-references")); }
                count = count.checked_add(child_count).filter(|count| *count <= MAX_DESCRIPTOR_REFERENCES).ok_or_else(|| error("invalid-reference-tree"))?;
                first.get_or_insert(child_first);
                last = Some(child_last);
            }
            Ok((first.unwrap(), last.unwrap(), count))
        }
        ReferencePage::Objects { hashes } if !relations => Ok((hashes.first().unwrap().clone(), hashes.last().unwrap().clone(), hashes.len())),
        ReferencePage::Relations { keys } if relations => Ok((keys.first().unwrap().clone(), keys.last().unwrap().clone(), keys.len())),
        _ => Err(error("reference-kind-mismatch")),
    }
}

/// A stage that a binding received; only a target replacement may activate it.
pub(in crate::persistent_store) fn is_binding_stage(db: &Connection, staging_id: &str) -> StoreResult<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM lww_binding_stages WHERE staging_id=?1) OR EXISTS(SELECT 1 FROM lww_binding_sources WHERE staging_id=?1)",
        [staging_id], |row| row.get(0),
    )?)
}

pub(in crate::persistent_store) fn binding_source_rows(db: &Connection, staging_id: &str) -> StoreResult<super::FrozenRows> {
    let frozen = super::FrozenRows::new()?;
    let mut statement = db.prepare("SELECT key,stamp,value FROM lww_binding_source_units WHERE staging_id=?1 ORDER BY key")?;
    let mut rows = statement.query([staging_id])?;
    while let Some(row) = rows.next()? {
        frozen.insert(super::intent_rows::RowText { key: row.get(0)?, stamp: Some(row.get(1)?), value: row.get(2)?, source_override: false })?;
    }
    Ok(frozen)
}

pub(in crate::persistent_store) fn source_digest_rows(changes: &super::FrozenRows) -> StoreResult<String> {
    let mut writer = HashWriter(Sha256::new(), #[cfg(test)] "binding_source_proof");
    #[cfg(test)]
    crate::persistent_store::hash_work::begin("binding_source_proof");
    writer.write_all(b"risunest.lww-binding-source/v1\0")?;
    writer.write_all(b"[")?;
    let mut first = true;
    changes.visit(false, |key, stamp, value, _| {
        if !first { writer.write_all(b",")?; }
        first = false;
        serde_json::to_writer(&mut writer, &Change { key, stamp: stamp.ok_or_else(|| error("request-id-integrity"))?, value })?;
        Ok(())
    })?;
    writer.write_all(b"]")?;
    Ok(hex::encode(writer.0.finalize()))
}

fn stored_status_digest(db: &Connection, staging_id: &str) -> StoreResult<String> {
    let mut writer = HashWriter(Sha256::new(), #[cfg(test)] "binding_status_proof");
    #[cfg(test)]
    crate::persistent_store::hash_work::begin("binding_status_proof");
    writer.write_all(b"risunest.lww-binding-status/v1\0")?;
    writer.write_all(b"[")?;
    let mut first = true;
    let mut statement = db.prepare("SELECT key,status FROM lww_binding_source_units WHERE staging_id=?1 ORDER BY key")?;
    let mut rows = statement.query([staging_id])?;
    while let Some(row) = rows.next()? {
        if !first { writer.write_all(b",")?; }
        first = false;
        serde_json::to_writer(&mut writer, &(row.get::<_,String>(0)?, row.get::<_,String>(1)?))?;
    }
    writer.write_all(b"]")?;
    Ok(hex::encode(writer.0.finalize()))
}

pub(in crate::persistent_store) fn validate_binding_source(db: &Connection, staging_id: &str, header: &Header, received: &[Change]) -> StoreResult<()> {
    let rows = super::FrozenRows::new()?;
    for change in received { rows.insert(super::intent_rows::target_row(change)?)?; }
    validate_binding_source_rows(db, staging_id, header, &rows)
}

pub(in crate::persistent_store) fn validate_binding_source_rows(db: &Connection, staging_id: &str, header: &Header, received: &super::FrozenRows) -> StoreResult<()> {
    let source: Option<(String,String,String,String,String)> = db.query_row("SELECT request_id,source_digest,status_digest,authority,inspection_id FROM lww_binding_sources WHERE staging_id=?1", [staging_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?;
    let (request_id,digest,expected_status,authority,inspection) = source.ok_or_else(|| error("binding-source-missing"))?;
    let receipt: Option<(String,String,String,String)> = db.query_row("SELECT s.receive_id,s.inspection_id,i.source_authority,s.changes FROM lww_binding_stages s JOIN lww_binding_inspections i USING(inspection_id) WHERE s.staging_id=?1", [staging_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional()?;
    let (receive_id,receipt_inspection,source_authority,encoded_digest) = receipt.ok_or_else(|| error("binding-source-association"))?;
    let original_authority: DecimalU64 = wire(authority.clone().try_into())?;
    if receive_id != request_id || receipt_inspection != inspection || source_authority != authority || encoded_digest != digest
        || (header.binding_authority != original_authority && original_authority.0.checked_add(1) != Some(header.binding_authority.0)) {
        return Err(error("binding-source-association"));
    }
    if request_id != header.request_id || source_digest_rows(received)? != digest
        || source_digest_rows(&binding_source_rows(db, staging_id)?)? != digest
        || stored_status_digest(db, staging_id)? != expected_status {
        return Err(error("binding-source-integrity"));
    }
    Ok(())
}

#[cfg(test)]
thread_local! { static FAIL_AFTER_STAGE_COPY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(test)]
thread_local! { static STAGE_SCRATCH: std::cell::RefCell<Option<std::path::PathBuf>> = const { std::cell::RefCell::new(None) }; }

pub(in crate::persistent_store) fn seed_binding_holds(tx: &Transaction<'_>, staging_id: &str, header: &Header) -> StoreResult<()> {
    tx.execute("INSERT INTO lww_receive_rows(request_id,key,stamp,value,status) SELECT ?2,key,stamp,value,'held' FROM lww_binding_source_units WHERE staging_id=?1 AND status='held'", params![staging_id,header.request_id])?;
    Ok(())
}

#[cfg(test)]
#[path = "lww_binding_stage_tests.rs"]
mod tests;

#[cfg(test)]
mod hash_work_tests {
    use super::*;
    use crate::persistent_store::hash_work::{reset_hash_work, take_hash_work, DomainWork};
    #[test]
    fn digest_prefixes_streamed_json_and_repeated_invocations_are_counted() {
        let changes: Vec<&Change> = vec![];
        let statuses = [("key", "held")];
        let source_bytes = b"risunest.lww-binding-source/v1\0".len() + serde_json::to_vec(&changes).unwrap().len();
        let status_bytes = b"risunest.lww-binding-status/v1\0".len() + serde_json::to_vec(&statuses).unwrap().len();
        reset_hash_work();
        source_digest(&changes).unwrap(); source_digest(&changes).unwrap(); status_digest(&statuses).unwrap();
        let work = take_hash_work();
        assert_eq!(work.domains["binding_source_proof"], DomainWork { calls: 2, bytes: 2 * source_bytes as u64 });
        assert_eq!(work.domains["binding_status_proof"], DomainWork { calls: 1, bytes: status_bytes as u64 });
        assert!(work.incomplete.is_empty());
        assert!(take_hash_work().domains.is_empty());
    }
}
