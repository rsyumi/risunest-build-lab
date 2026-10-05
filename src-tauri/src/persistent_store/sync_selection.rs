//! Local operational authority. Never exported as library content or connection QR.
use super::{active_generation, current_revision, StoreError, StoreResult};
use rusqlite::{params, Connection, Transaction, OptionalExtension};
use serde::{Deserialize, Serialize};
use crate::native_log::logged;

pub(super) const SCHEMA: &str = r#"
CREATE TABLE library_sync_selection(singleton INTEGER PRIMARY KEY CHECK(singleton=1),target TEXT NOT NULL CHECK(target IN ('none','server','external')),connection_id TEXT,selection_epoch TEXT NOT NULL,paused INTEGER NOT NULL CHECK(paused IN (0,1)),CHECK((target='none' AND connection_id IS NULL) OR (target!='none' AND length(connection_id)>0)));
CREATE TABLE local_library_identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1),store_id TEXT NOT NULL,library_epoch TEXT NOT NULL);
CREATE TABLE lww_binding_switch_requests(request_id TEXT PRIMARY KEY,body TEXT NOT NULL,change TEXT NOT NULL);
CREATE TABLE lww_binding_switch_retained(request_id TEXT PRIMARY KEY);
CREATE TABLE lww_initial_publication(singleton INTEGER PRIMARY KEY CHECK(singleton=1),authority TEXT NOT NULL);
CREATE TABLE lww_binding_identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1),library_id TEXT,target_id TEXT);
CREATE TABLE lww_binding_inspections(inspection_id TEXT PRIMARY KEY,target TEXT NOT NULL,target_id TEXT NOT NULL,library_id TEXT NOT NULL,source_authority TEXT NOT NULL,source_epoch TEXT NOT NULL);
CREATE TABLE lww_binding_stages(staging_id TEXT PRIMARY KEY,inspection_id TEXT NOT NULL,receive_id TEXT NOT NULL UNIQUE,changes TEXT NOT NULL,activation_epoch TEXT,library_digest TEXT NOT NULL);
"#;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CaptureIdentity {
    pub store_id: String,
    pub library_epoch: String,
    pub generation: String,
    pub selection_epoch: String,
    // Decimal strings are used by the UI bridge; SQLite uses signed 64-bit integers.
    pub revision: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "connectionId", rename_all = "lowercase")]
pub(crate) enum SyncTarget {
    None,
    Server(String),
    External(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selection {
    pub target: SyncTarget,
    pub epoch: String,
    pub paused: bool,
}

fn invalid(message: &str) -> StoreError {
    StoreError::Validation {
        message: message.into(),
    }
}

pub(super) fn create_schema(db: &Connection) -> StoreResult<()> {
    db.execute_batch(SCHEMA)?;
    db.execute(
        "INSERT INTO library_sync_selection VALUES(1,'none',NULL,?1,0)",
        [uuid::Uuid::new_v4().to_string()],
    )?;
    db.execute(
        "INSERT INTO local_library_identity VALUES(1,?1,?2)",
        params![
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string()
        ],
    )?;
    db.execute("INSERT INTO lww_binding_identity VALUES(1,NULL,NULL)", [])?;
    Ok(())
}

pub(crate) fn read(db: &Connection) -> StoreResult<Selection> {
    let (kind,id,epoch,paused):(String,Option<String>,String,bool)=db.query_row("SELECT target,connection_id,selection_epoch,paused FROM library_sync_selection WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    let target = match (kind.as_str(), id) {
        ("none", None) => SyncTarget::None,
        ("server", Some(id)) => SyncTarget::Server(id),
        ("external", Some(id)) => SyncTarget::External(id),
        _ => return Err(invalid("Invalid sync selection")),
    };
    Ok(Selection {
        target,
        epoch,
        paused,
    })
}

pub(crate) fn identity(db: &Connection) -> StoreResult<CaptureIdentity> {
    let (store_id, library_epoch) = db.query_row(
        "SELECT store_id,library_epoch FROM local_library_identity WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(CaptureIdentity {
        store_id,
        library_epoch,
        generation: active_generation(db)?,
        selection_epoch: read(db)?.epoch,
        revision: current_revision(db)?,
    })
}

/// Selection changes require the existing exclusive replacement reservation/permit.
/// The caller must settle server operations before entering this local transaction.
pub(crate) fn select(
    tx: &Transaction<'_>,
    expected_epoch: &str,
    target: &SyncTarget,
) -> StoreResult<Selection> {
    if read(tx)?.epoch != expected_epoch {
        return Err(invalid("Sync selection changed"));
    }
    require_no_pending_publication(tx)?;
    let (kind, id) = match target {
        SyncTarget::None => ("none", None),
        SyncTarget::Server(id) => ("server", Some(id)),
        SyncTarget::External(id) => ("external", Some(id)),
    };
    if id.is_some_and(|id| id.is_empty()) {
        return Err(invalid("Missing sync connection identity"));
    }
    tx.execute("UPDATE library_sync_selection SET target=?1,connection_id=?2,selection_epoch=?3,paused=0 WHERE singleton=1",params![kind,id,uuid::Uuid::new_v4().to_string()])?;
    tx.execute(
        "UPDATE external_storage_jobs SET phase='stale' WHERE role IN ('sync','restore') AND phase IN ('preparing','ready')",
        [],
    )?;
    read(tx)
}

pub(crate) fn set_paused(tx: &Transaction<'_>, expected_epoch: &str, paused: bool) -> StoreResult<Selection> {
    let current = read(tx)?;
    if current.epoch != expected_epoch || !matches!(current.target, SyncTarget::External(_)) {
        return Err(invalid("Sync selection changed"));
    }
    tx.execute("UPDATE library_sync_selection SET paused=?1 WHERE singleton=1", [paused])?;
    read(tx)
}

pub(crate) fn require_no_pending_publication(db: &Connection) -> StoreResult<()> {
    let pending:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM external_storage_jobs WHERE phase='applying')",[],|r|r.get(0))?;
    if pending {
        return Err(invalid(
            "Unsettled sync operation blocks library replacement",
        ));
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BindingContent {
    pub library: serde_json::Value,
    pub character_count: risunest_sync_wire::stamp::DecimalU64,
    pub opaque_shared_unit_count: risunest_sync_wire::stamp::DecimalU64,
    pub shared_variables: serde_json::Value,
    pub protected_values: serde_json::Value,
    pub managed_alias_count: risunest_sync_wire::stamp::DecimalU64,
    pub ordinary_plugin_value_count: risunest_sync_wire::stamp::DecimalU64,
    pub hypa_value_count: risunest_sync_wire::stamp::DecimalU64,
    pub plugin_local_value_count: risunest_sync_wire::stamp::DecimalU64,
    pub plugin_local_participating: bool,
}

impl super::PersistentStore {
    pub(crate) fn lww_binding_content(&self) -> StoreResult<BindingContent> {
        let generation = active_generation(&self.connection)?;
        let ordinary = self.connection.query_row("SELECT COUNT(*) FROM plugin_storage WHERE generation=?1", [&generation], |row| row.get::<_, i64>(0))?;
        let aliases = self.connection.query_row("SELECT COUNT(*) FROM asset_aliases WHERE generation=?1 AND object_hash IS NOT NULL", [&generation], |row| row.get::<_, i64>(0))?;
        let device = self.device_store()?;
        let mut plugin_local_keys = std::collections::BTreeSet::new();
        let mut values = device.connection().prepare("SELECT owner,space,key FROM plugin_device_storage WHERE tombstone=0")?;
        for row in values.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))? { plugin_local_keys.insert(row?); }
        let mut opaque = device.connection().prepare("SELECT key,value FROM lww_units")?;
        for row in opaque.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))? {
            let (key,value) = row?;
            let key:risunest_sync_wire::unit::UnitKey = key.try_into().map_err(|e:risunest_sync_wire::WireError| invalid(&e.to_string()))?;
            let value:risunest_sync_wire::unit::UnitValue = serde_json::from_str(&value)?;
            let parts = key.components();
            if parts[0] == "plugin-local" && !matches!(value,risunest_sync_wire::unit::UnitValue::Deleted) {
                plugin_local_keys.insert((parts[1].clone(),parts[2].clone(),parts[3].clone()));
            }
        }
        let plugin_local = plugin_local_keys.len();
        let (opaque_shared_unit_count,shared_variables) = self.lww_binding_extra_content()?;
        let mut protected = serde_json::Map::new();
        let mut statement = self.connection.prepare("SELECT key,value FROM lww_units")?;
        let rows = statement.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?;
        for row in rows {
            let (key,value) = row?;
            let key:risunest_sync_wire::unit::UnitKey = key.try_into().map_err(|e:risunest_sync_wire::WireError| invalid(&e.to_string()))?;
            if key.components()[0] == "preset-protected" {
                let value:risunest_sync_wire::unit::UnitValue = serde_json::from_str(&value)?;
                if let Some(value) = super::lww::json_value_resolved(&self.connection,&value)? { protected.insert(key.components()[1].clone(),value); }
            }
        }
        // Binding compares this with a factory library, which has no characters, so a count
        // decides for them, and plugin storage values are counted above. The response then
        // grows only with root fields and presets.
        let (library, character_count) = self.binding_library()?;
        Ok(BindingContent {
            library,
            character_count: character_count.into(),
            opaque_shared_unit_count: opaque_shared_unit_count.into(),
            shared_variables,
            protected_values:serde_json::Value::Object(protected),
            managed_alias_count: (aliases as u64).into(),
            ordinary_plugin_value_count: (ordinary as u64).into(),
            hypa_value_count: device.hypa_embedding_usage()?.0.into(),
            plugin_local_value_count: (plugin_local as u64).into(),
            plugin_local_participating: device.connection().query_row("SELECT participating FROM device_sections WHERE section='local-plugins'", [], |r| r.get(0))?,
        })
    }
}

#[tauri::command]
pub(crate) fn pds_lww_binding_content(state: tauri::State<'_, super::commands::PersistentStoreState>) -> StoreResult<BindingContent> {
    logged("pds_lww_binding_content", super::commands::with_store(state, |store| store.lww_binding_content()))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BindingSelectionChange {
    pub expected_epoch: String,
    pub new_epoch: String,
    pub target: SyncTarget,
    pub library_id: Option<String>,
    pub target_id: Option<String>,
    pub inspection_id: Option<String>,
    pub initial_publication: bool,
}

pub(crate) fn apply_binding_selection(tx: &Transaction<'_>, change: &BindingSelectionChange) -> StoreResult<Selection> {
    let current = read(tx)?;
    if current.epoch == change.new_epoch {
        let (library, target): (Option<String>, Option<String>) = tx.query_row("SELECT library_id,target_id FROM lww_binding_identity WHERE singleton=1", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
        if current.target != change.target || (!matches!(change.target, SyncTarget::None) && (library != change.library_id || target != change.target_id)) {
            return Err(invalid("Binding switch identity changed"));
        }
        return Ok(current);
    }
    if current.epoch != change.expected_epoch || change.new_epoch.is_empty() || change.new_epoch == change.expected_epoch {
        return Err(invalid("Sync selection changed"));
    }
    let (kind, connection) = match &change.target {
        SyncTarget::None => ("none", None),
        SyncTarget::Server(id) => ("server", Some(id)),
        SyncTarget::External(id) => ("external", Some(id)),
    };
    if connection.is_some_and(|id| id.is_empty()) || (connection.is_some() && (change.library_id.as_deref().is_none_or(str::is_empty) || change.target_id.as_deref().is_none_or(str::is_empty))) {
        return Err(invalid("Missing binding identity"));
    }
    tx.execute("UPDATE library_sync_selection SET target=?1,connection_id=?2,selection_epoch=?3,paused=0 WHERE singleton=1", params![kind, connection, change.new_epoch])?;
    if !matches!(change.target, SyncTarget::None) {
        tx.execute("UPDATE lww_binding_identity SET library_id=?1,target_id=?2 WHERE singleton=1", params![change.library_id, change.target_id])?;
    }
    if let Some(inspection) = &change.inspection_id {
        tx.execute("UPDATE lww_binding_stages SET activation_epoch=?1 WHERE inspection_id=?2", params![change.new_epoch, inspection])?;
    }
    tx.execute("UPDATE external_storage_jobs SET phase='stale' WHERE role IN ('sync','restore') AND phase IN ('preparing','ready')", [])?;
    read(tx)
}

#[cfg(test)]
mod binding_tests {
    use super::*;

    #[test]
    fn a_switch_request_reads_the_renderer_body_and_requires_the_initial_publication_flag() {
        let body = serde_json::json!({
            "bindingAuthority":"4","requestId":"synthetic-switch","expectedSelectionEpoch":"epoch",
            "target":{"kind":"server","connectionId":"server"},"inspectionId":"inspection","initialPublication":true
        });
        let request: SwitchBindingRequest = serde_json::from_value(body.clone()).unwrap();
        assert!(request.initial_publication);
        assert_eq!(request.target, SyncTarget::Server("server".into()));
        let mut missing = body;
        missing.as_object_mut().unwrap().remove("initialPublication");
        assert!(serde_json::from_value::<SwitchBindingRequest>(missing).is_err());
    }

    #[test]
    fn binding_content_count_dto_uses_only_canonical_decimal_strings() {
        let encoded = serde_json::json!({
            "library":{},"sharedVariables":{},"protectedValues":{},
            "characterCount":"3","opaqueSharedUnitCount":"0","managedAliasCount":"18446744073709551615",
            "ordinaryPluginValueCount":"9007199254740993","hypaValueCount":"1","pluginLocalValueCount":"2",
            "pluginLocalParticipating":false
        });
        let decoded:BindingContent = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded.managed_alias_count.0,u64::MAX);
        assert_eq!(serde_json::to_value(decoded).unwrap(),encoded);
        for key in ["characterCount","opaqueSharedUnitCount","managedAliasCount","ordinaryPluginValueCount","hypaValueCount","pluginLocalValueCount"] {
            for invalid in [serde_json::json!(0),serde_json::Value::Null,serde_json::json!(""),serde_json::json!("00"),serde_json::json!("01"),serde_json::json!("+1"),serde_json::json!("-1"),serde_json::json!("1.0"),serde_json::json!("1e3"),serde_json::json!(" 1"),serde_json::json!("18446744073709551616")] {
                let mut input=encoded.clone();input[key]=invalid;
                assert!(serde_json::from_value::<BindingContent>(input).is_err(),"accepted invalid {key}");
            }
        }
    }
    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        create_schema(&db).unwrap();
        db.execute_batch("CREATE TABLE external_storage_jobs(role TEXT,phase TEXT); INSERT INTO external_storage_jobs VALUES('backup','ready'),('sync','ready'),('restore','preparing');").unwrap();
        db
    }
    fn change(db: &Connection) -> BindingSelectionChange {
        BindingSelectionChange { initial_publication: false, expected_epoch: read(db).unwrap().epoch, new_epoch: "new".into(), target: SyncTarget::Server("connection".into()), library_id: Some("library".into()), target_id: Some("target".into()), inspection_id: None }
    }
    #[test]
    fn binding_content_counts_orphan_values_in_both_namespaces_when_participation_is_off() {
        use super::super::device_store::plugin_values::PluginDeviceMutation;
        let dir = tempfile::tempdir().unwrap();
        let mut store = super::super::PersistentStore::open(dir.path()).unwrap();
        let device = store.device_store_mut().unwrap();
        device.write_plugin_device_values("orphan", &[
            PluginDeviceMutation::Set { space: "json".into(), key: "empty-json".into(), value: "null".into() },
            PluginDeviceMutation::Set { space: "string".into(), key: "empty-string".into(), value: "".into() },
        ]).unwrap();
        let participation: bool = device.connection().query_row("SELECT participating FROM device_sections WHERE section='local-plugins'", [], |r| r.get(0)).unwrap();
        assert!(!participation);
        let content = store.lww_binding_content().unwrap();
        assert_eq!(content.plugin_local_value_count.0, 2);
        assert!(!content.plugin_local_participating);
        assert_eq!(content.hypa_value_count.0, 0);
        assert_eq!(content.ordinary_plugin_value_count.0, 0);
    }
    #[test]
    fn binding_content_library_is_the_materialized_library_without_characters_or_plugin_values() {
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let mut store = super::super::PersistentStore::open(dir.path()).unwrap();
        let staging = store.replace_begin().unwrap().staging_id;
        store.replace_put_root(&staging, &json!({"plugins": [{"name": "Plugin"}], "pluginCustomStorage": {"plugin": {"enabled": true}}})).unwrap();
        store.replace_put_presets(&staging, &[json!({"id": "preset-a", "name": "Preset A"})]).unwrap();
        let revision = store.replace_commit(&staging, Some(0)).unwrap().revision;
        let mut expected = store.materialize(None).unwrap();
        assert_eq!(expected["pluginCustomStorage"], json!({"plugin": {"enabled": true}}));
        expected.as_object_mut().unwrap().shift_remove("pluginCustomStorage");
        let content = store.lww_binding_content().unwrap();
        assert_eq!(content.library, expected);
        assert_eq!(content.library["characters"], json!([]));
        assert_eq!(content.character_count.0, 0);
        assert_eq!(content.ordinary_plugin_value_count.0, 1);

        let staging = store.replace_begin().unwrap().staging_id;
        store.replace_put_root(&staging, &json!({"plugins": [{"name": "Plugin"}]})).unwrap();
        store.replace_put_presets(&staging, &[json!({"id": "preset-a", "name": "Preset A"})]).unwrap();
        store.replace_add_characters(&staging, &[json!({
            "type": "character", "chaId": "character-a", "name": "Character A",
            "chats": [{"id": "chat-a", "name": "Chat A", "note": "kept out", "message": [
                {"role": "user", "data": "first", "chatId": "message-1"},
                {"role": "char", "data": "second", "chatId": "message-2"}
            ]}]
        })]).unwrap();
        store.replace_commit(&staging, Some(revision)).unwrap();
        let mut expected = store.materialize(None).unwrap();
        assert_eq!(expected["characters"][0]["chats"][0]["message"].as_array().unwrap().len(), 2);
        expected["characters"] = json!([]);
        expected.as_object_mut().unwrap().shift_remove("pluginCustomStorage");
        let content = store.lww_binding_content().unwrap();
        assert_eq!(content.library, expected);
        assert_eq!(content.character_count.0, 1);
    }
    fn staged(store: &mut super::super::PersistentStore, inspection: &str, changes: &[super::super::lww::Change]) -> (String,String) {
        let receive = uuid::Uuid::new_v4().to_string();
        let header = super::super::lww::Header { binding_authority:store.lww_binding_authority().unwrap(),request_id:receive.clone() };
        let staging = store.lww_stage_binding_units(&header,inspection,changes,risunest_sync_wire::stamp::DecimalU64(u64::MAX)).unwrap().staging_id;
        (staging,receive)
    }
    fn inspect(store: &super::super::PersistentStore, target: &SyncTarget) -> String {
        inspect_as(store,target,"remote-target","remote-library")
    }
    fn inspect_as(store: &super::super::PersistentStore, target: &SyncTarget, target_id: &str, library_id: &str) -> String {
        store.register_lww_binding_inspection(store.lww_binding_authority().unwrap(),target,target_id,library_id).unwrap()
    }
    fn unbind(store: &mut super::super::PersistentStore) -> BindingState {
        let before = store.lww_binding_state().unwrap();
        store.switch_lww_binding(&SwitchBindingRequest { initial_publication: false, header:super::super::lww::Header{binding_authority:before.target_authority,request_id:uuid::Uuid::new_v4().to_string()},expected_selection_epoch:before.selection_epoch,target:SyncTarget::None,inspection_id:None }).unwrap()
    }
    fn outbox_keys(store: &super::super::PersistentStore, authority: risunest_sync_wire::stamp::DecimalU64) -> Vec<String> {
        store.lww_read_outbox(authority,100).unwrap().entries.into_iter().map(|entry|entry.key.as_str().to_string()).collect()
    }
    fn pending_plugin_value(store: &mut super::super::PersistentStore, key: &str) {
        use super::super::device_store::plugin_values::PluginDeviceMutation;
        store.device_store_mut().unwrap().write_plugin_device_values("owner",&[PluginDeviceMutation::Set{space:"string".into(),key:key.into(),value:"local".into()}]).unwrap();
    }
    fn pending_root_value(store: &mut super::super::PersistentStore, value: &str) {
        store.commit(&super::super::WorkingSetCommit { expected_revision:current_revision(&store.connection).unwrap(),unit_mutations:Some(vec![super::super::lww::UnitMutation::Set{key:risunest_sync_wire::unit::UnitKey::new(&["root","language"]).unwrap(),value:serde_json::json!(value)}]),..Default::default() }).unwrap();
    }
    fn switch(store: &mut super::super::PersistentStore, inspection: &str, target: &SyncTarget) -> BindingState {
        let before = store.lww_binding_state().unwrap();
        store.switch_lww_binding(&SwitchBindingRequest { initial_publication: false, header:super::super::lww::Header{binding_authority:before.target_authority,request_id:uuid::Uuid::new_v4().to_string()},expected_selection_epoch:before.selection_epoch,target:target.clone(),inspection_id:Some(inspection.into()) }).unwrap()
    }
    fn replacement(state: &BindingState, staging: &str, receive: &str) -> ReplaceBindingRequest {
        ReplaceBindingRequest { header:super::super::lww::Header{binding_authority:state.target_authority,request_id:receive.into()},expected_selection_epoch:state.selection_epoch.clone(),staging_id:staging.into(),receive_id:receive.into(),target_id:"remote-target".into(),library_id:"remote-library".into() }
    }
    #[test]
    fn binding_rejects_stale_or_wrong_target_inspection_before_switching() {
        let dir=tempfile::tempdir().unwrap();let mut store=super::super::PersistentStore::open(dir.path()).unwrap();
        let target=SyncTarget::Server("server".into());let inspection=inspect(&store,&target);
        let before=store.lww_binding_state().unwrap();
        let request=SwitchBindingRequest { initial_publication: false, header:super::super::lww::Header{binding_authority:before.target_authority,request_id:uuid::Uuid::new_v4().to_string()},expected_selection_epoch:before.selection_epoch.clone(),target:SyncTarget::External("other".into()),inspection_id:Some(inspection) };
        assert!(store.switch_lww_binding(&request).is_err());
        assert_eq!(store.lww_binding_state().unwrap().target_authority,before.target_authority);
        assert_eq!(store.lww_binding_state().unwrap().selection_epoch,before.selection_epoch);
    }
    #[test]
    fn binding_rejects_mutated_stage_before_authority_or_values_change() {
        let dir=tempfile::tempdir().unwrap();let mut store=super::super::PersistentStore::open(dir.path()).unwrap();
        let target=SyncTarget::Server("server".into());let inspection=inspect(&store,&target);
        let (staging,receive)=staged(&mut store,&inspection,&[]);
        store.replace_put_root(&staging,&serde_json::json!({"language":"changed"})).unwrap();
        assert!(store.register_lww_binding_stage(&inspection,&staging,&receive,&[]).is_err());
        let before=store.lww_binding_state().unwrap();
        let request=SwitchBindingRequest { initial_publication: false, header:super::super::lww::Header{binding_authority:before.target_authority,request_id:uuid::Uuid::new_v4().to_string()},expected_selection_epoch:before.selection_epoch.clone(),target,inspection_id:Some(inspection) };
        assert!(store.switch_lww_binding(&request).is_err());
        assert_eq!(store.lww_binding_state().unwrap().target_authority,before.target_authority);
    }
    #[test]
    fn binding_replacement_resets_all_owners_without_deletions_and_preserves_operational_state() {
        use super::super::device_store::plugin_values::PluginDeviceMutation;
        let dir=tempfile::tempdir().unwrap();let mut store=super::super::PersistentStore::open(dir.path()).unwrap();
        let writer=store.device_store().unwrap().writer_id().unwrap();
        store.device_store_mut().unwrap().write_setting("risuNestDeviceSettings",&serde_json::json!({"synthetic":true})).unwrap();
        let old:super::super::WorkingSetCommit=serde_json::from_value(serde_json::json!({
            "expectedRevision":current_revision(&store.connection).unwrap(),
            "root":{"statics":{"messages":7},"loreBookPage":3,"vertexAccessToken":"synthetic"},
            "pluginStorage":[{"type":"set","owner":"orphan","key":"ordinary","value":"old-value"}]
        })).unwrap();
        store.commit(&old).unwrap();
        store.device_store_mut().unwrap().write_hypa_embeddings(&[super::super::device_store::hypa::HypaEmbeddingWrite {
            cache_key:"synthetic-hypa".into(),producer:"test".into(),model:"test".into(),endpoint:None,
            preprocess_version:0,dimensions:1,vector:1.0f32.to_le_bytes().to_vec(),metadata:None,
        }]).unwrap();
        let prior_clock=store.lww_clock_state().unwrap();
        for owner in ["installed","orphan"] {
            store.device_store_mut().unwrap().write_plugin_device_values(owner,&[
                PluginDeviceMutation::Set{space:"json".into(),key:"json".into(),value:"null".into()},
                PluginDeviceMutation::Set{space:"string".into(),key:"string".into(),value:"synthetic".into()},
            ]).unwrap();
        }
        let target=SyncTarget::Server("server".into());let inspection=inspect(&store,&target);
        let (staging,receive)=staged(&mut store,&inspection,&[]);let state=switch(&mut store,&inspection,&target);
        let request=replacement(&state,&staging,&receive);
        store.replace_lww_binding(&request).unwrap();
        let content=store.lww_binding_content().unwrap();
        assert_eq!(content.plugin_local_value_count.0,0);
        assert_eq!(content.hypa_value_count.0,0);
        assert_eq!(content.ordinary_plugin_value_count.0,0);
        assert_eq!(store.read_root(None).unwrap().value["statics"],serde_json::json!({"messages":7}));
        assert_eq!(store.read_root(None).unwrap().value["loreBookPage"],3);
        assert_eq!(store.read_root(None).unwrap().value["vertexAccessToken"],"synthetic");
        assert!(store.lww_clock_state().unwrap().issued >= prior_clock.issued);
        assert_eq!(store.device_store().unwrap().writer_id().unwrap(),writer);
        assert_eq!(store.device_store().unwrap().read_setting("risuNestDeviceSettings").unwrap(),Some(serde_json::json!({"synthetic":true})));
        assert_eq!(store.lww_binding_authority().unwrap(),state.target_authority);
        assert!(store.lww_read_outbox(state.target_authority,100).unwrap().entries.is_empty());
        store.device_store_mut().unwrap().write_plugin_device_values("orphan",&[PluginDeviceMutation::Set{space:"string".into(),key:"after".into(),value:"kept".into()}]).unwrap();
        store.replace_lww_binding(&request).unwrap();
        assert_eq!(store.lww_binding_content().unwrap().plugin_local_value_count.0,1);
    }
    #[test]
    fn binding_target_plugin_values_are_active_only_with_participation() {
        for enabled in [false,true] {
            let dir=tempfile::tempdir().unwrap();let mut store=super::super::PersistentStore::open(dir.path()).unwrap();
            store.device_store().unwrap().connection().execute("UPDATE device_sections SET participating=?1 WHERE section='local-plugins'",[enabled]).unwrap();
            let value=risunest_sync_wire::unit::UnitValue::inline(&serde_json::to_vec(&serde_json::json!("remote-value")).unwrap()).unwrap();
            let changes=[super::super::lww::Change{ key:risunest_sync_wire::unit::UnitKey::new(&["plugin-local","remote-owner","string","remote"]).unwrap(), stamp:risunest_sync_wire::stamp::Stamp{physical_ms:risunest_sync_wire::stamp::DecimalU64(1),logical:0,writer_id:uuid::Uuid::new_v4().to_string()},value }];
            let target=SyncTarget::Server("server".into());let inspection=inspect(&store,&target);
            let (staging,receive)=staged(&mut store,&inspection,&changes);let state=switch(&mut store,&inspection,&target);
            store.replace_lww_binding(&replacement(&state,&staging,&receive)).unwrap();
            let active:i64=store.device_store().unwrap().connection().query_row("SELECT COUNT(*) FROM plugin_device_storage WHERE tombstone=0",[],|r|r.get(0)).unwrap();
            assert_eq!(active,i64::from(enabled));
            let opaque:i64=store.device_store().unwrap().connection().query_row("SELECT COUNT(*) FROM lww_units",[],|r|r.get(0)).unwrap();
            assert_eq!(opaque,1);
            assert_eq!(store.lww_binding_content().unwrap().plugin_local_value_count.0,1);
            assert_eq!(store.lww_binding_content().unwrap().plugin_local_participating,enabled);
        }
    }

    #[test]
    fn unbinding_retains_library_for_rebind() {
        let mut db=database();let initial=change(&db);let tx=db.transaction().unwrap();
        apply_binding_selection(&tx,&initial).unwrap();
        let unbind=BindingSelectionChange { initial_publication: false,expected_epoch:initial.new_epoch,new_epoch:"unbound".into(),target:SyncTarget::None,library_id:None,target_id:None,inspection_id:None};
        apply_binding_selection(&tx,&unbind).unwrap();
        apply_binding_selection(&tx,&unbind).unwrap();
        let library:Option<String>=tx.query_row("SELECT library_id FROM lww_binding_identity WHERE singleton=1",[],|r|r.get(0)).unwrap();
        assert_eq!(library,Some("library".into()));
    }

    #[test]
    fn binding_switch_fences_old_mutations_and_never_relays_pending_versions() {
        use super::super::device_store::plugin_values::PluginDeviceMutation;
        let dir=tempfile::tempdir().unwrap();let mut store=super::super::PersistentStore::open(dir.path()).unwrap();
        store.device_store().unwrap().connection().execute("UPDATE device_sections SET participating=1 WHERE section='local-plugins'",[]).unwrap();
        let first=SyncTarget::Server("first".into());let first_inspection=inspect(&store,&first);let old=switch(&mut store,&first_inspection,&first);
        store.device_store_mut().unwrap().write_plugin_device_values("owner",&[PluginDeviceMutation::Set{space:"string".into(),key:"pending".into(),value:"local".into()}]).unwrap();
        assert!(!store.lww_read_outbox(old.target_authority,100).unwrap().entries.is_empty());
        let second=SyncTarget::External("second".into());let second_inspection=inspect_as(&store,&second,"second-target","remote-library");let next=switch(&mut store,&second_inspection,&second);
        assert!(store.lww_read_outbox(old.target_authority,100).is_err());
        assert!(store.lww_read_outbox(next.target_authority,100).unwrap().entries.is_empty());
        let device_value=store.device_store().unwrap().read_plugin_device_value("owner","string","pending").unwrap();
        assert_eq!(device_value,Some("local".into()));
    }

    #[test]
    fn unbinding_and_rebinding_the_same_target_keeps_unpublished_versions_and_progress() {
        let dir=tempfile::tempdir().unwrap();let mut store=super::super::PersistentStore::open(dir.path()).unwrap();
        store.device_store().unwrap().connection().execute("UPDATE device_sections SET participating=1 WHERE section='local-plugins'",[]).unwrap();
        let target=SyncTarget::Server("server".into());let inspection=inspect(&store,&target);let bound=switch(&mut store,&inspection,&target);
        pending_plugin_value(&mut store,"bound");pending_root_value(&mut store,"bound");
        store.device_store().unwrap().connection().execute("INSERT INTO lww_progress VALUES('server','',?1,?2)",["7".to_string(),bound.target_authority.0.to_string()]).unwrap();
        let bound_keys=outbox_keys(&store,bound.target_authority);
        assert_eq!(bound_keys.len(),2);
        let unbound=unbind(&mut store);
        assert_eq!(outbox_keys(&store,unbound.target_authority),bound_keys);
        pending_plugin_value(&mut store,"offline");
        let inspection=inspect(&store,&target);let rebound=switch(&mut store,&inspection,&target);
        let keys=outbox_keys(&store,rebound.target_authority);
        assert_eq!(keys.len(),3);
        for key in &bound_keys { assert!(keys.contains(key)); }
        assert_eq!(rebound.progress,serde_json::json!([{"kind":"server","writerId":"","cursor":"7"}]));
        assert!(store.lww_read_outbox(bound.target_authority,100).is_err());
    }
    #[test]
    fn rebinding_a_different_target_or_a_replacement_stage_after_unbinding_drops_old_versions() {
        for staged_replacement in [false,true] {
            let dir=tempfile::tempdir().unwrap();let mut store=super::super::PersistentStore::open(dir.path()).unwrap();
            let target=SyncTarget::Server("server".into());let inspection=inspect(&store,&target);let bound=switch(&mut store,&inspection,&target);
            pending_root_value(&mut store,"bound");
            store.device_store().unwrap().connection().execute("INSERT INTO lww_progress VALUES('server','',?1,?2)",["7".to_string(),bound.target_authority.0.to_string()]).unwrap();
            unbind(&mut store);
            pending_root_value(&mut store,"offline");
            let (next,inspection)=if staged_replacement {
                let inspection=inspect(&store,&target);staged(&mut store,&inspection,&[]);(target.clone(),inspection)
            } else {
                let next=SyncTarget::External("other".into());let inspection=inspect_as(&store,&next,"other-target","remote-library");(next,inspection)
            };
            let rebound=switch(&mut store,&inspection,&next);
            assert!(outbox_keys(&store,rebound.target_authority).is_empty());
            assert_eq!(rebound.progress,serde_json::json!([]));
        }
    }
    #[test]
    fn the_same_target_needs_both_the_retained_library_and_target_identity() {
        let dir=tempfile::tempdir().unwrap();let mut store=super::super::PersistentStore::open(dir.path()).unwrap();
        assert!(!store.lww_previously_bound("remote-library","remote-target").unwrap());
        let target=SyncTarget::Server("server".into());let inspection=inspect(&store,&target);switch(&mut store,&inspection,&target);
        unbind(&mut store);
        assert!(store.lww_previously_bound("remote-library","remote-target").unwrap());
        assert!(!store.lww_previously_bound("remote-library","other-target").unwrap());
        assert!(!store.lww_previously_bound("other-library","remote-target").unwrap());
    }
    #[test]
    fn binding_selection_replays_exactly_and_preserves_backup_jobs() {
        let mut db = database(); let change = change(&db);
        let tx = db.transaction().unwrap();
        let result = apply_binding_selection(&tx, &change).unwrap();
        assert_eq!(apply_binding_selection(&tx, &change).unwrap(), result);
        let backup: String = tx.query_row("SELECT phase FROM external_storage_jobs WHERE role='backup'", [], |r| r.get(0)).unwrap();
        assert_eq!(backup, "ready");
        let stale: i64 = tx.query_row("SELECT COUNT(*) FROM external_storage_jobs WHERE phase='stale'", [], |r| r.get(0)).unwrap();
        assert_eq!(stale, 2);
    }
    #[test]
    fn binding_selection_rejects_stale_and_altered_replays() {
        let mut db = database(); let mut change = change(&db);
        let tx = db.transaction().unwrap();
        apply_binding_selection(&tx, &change).unwrap();
        change.library_id = Some("other".into());
        assert!(apply_binding_selection(&tx, &change).is_err());
        change.new_epoch = "another".into();
        assert!(apply_binding_selection(&tx, &change).is_err());
    }
    #[test]
    fn binding_selection_rejects_missing_identity_without_mutation() {
        let mut db = database(); let mut change = change(&db); change.target_id = None;
        let tx = db.transaction().unwrap();
        assert!(apply_binding_selection(&tx, &change).is_err());
        assert_eq!(read(&tx).unwrap().epoch, change.expected_epoch);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BindingState {
    pub target: SyncTarget,
    pub target_authority: risunest_sync_wire::stamp::DecimalU64,
    pub selection_epoch: String,
    pub library_id: Option<String>,
    pub progress: serde_json::Value,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SwitchBindingRequest {
    #[serde(flatten)]
    pub header: super::lww::Header,
    pub expected_selection_epoch: String,
    pub target: SyncTarget,
    pub inspection_id: Option<String>,
    /// Whether the switch leaves every unit of this library owed to an empty target.
    pub initial_publication: bool,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReplaceBindingRequest {
    #[serde(flatten)]
    pub header: super::lww::Header,
    pub expected_selection_epoch: String,
    pub staging_id: String,
    pub receive_id: String,
    pub target_id: String,
    pub library_id: String,
}

impl super::PersistentStore {
    pub(crate) fn lww_binding_state(&self) -> StoreResult<BindingState> {
        let selection = read(&self.connection)?;
        let library_id = self.connection.query_row("SELECT library_id FROM lww_binding_identity WHERE singleton=1", [], |r| r.get(0))?;
        let mut statement = self.device_store()?.connection().prepare("SELECT kind,writer_id,cursor FROM lww_progress WHERE authority=?1 ORDER BY kind,writer_id")?;
        let rows = statement.query_map([self.lww_binding_authority()?.0.to_string()], |r| Ok(serde_json::json!({"kind":r.get::<_,String>(0)?,"writerId":r.get::<_,String>(1)?,"cursor":r.get::<_,String>(2)?})))?;
        let progress = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(BindingState { target: selection.target, target_authority: self.lww_binding_authority()?, selection_epoch: selection.epoch, library_id, progress: serde_json::Value::Array(progress) })
    }

    pub(crate) fn lww_previously_bound(&self, library_id: &str, target_id: &str) -> StoreResult<bool> {
        let (library,target):(Option<String>,Option<String>) = self.connection.query_row("SELECT library_id,target_id FROM lww_binding_identity WHERE singleton=1", [], |r| Ok((r.get(0)?,r.get(1)?)))?;
        Ok(library.as_deref() == Some(library_id) && target.as_deref() == Some(target_id))
    }

    // Native transport adapters record verified remote identities before returning an inspection token.
    pub(crate) fn register_lww_binding_inspection(&self, source_authority: risunest_sync_wire::stamp::DecimalU64, target: &SyncTarget, target_id: &str, library_id: &str) -> StoreResult<String> {
        if self.lww_binding_authority()? != source_authority || matches!(target, SyncTarget::None) || target_id.is_empty() || library_id.is_empty() {
            return Err(invalid("Invalid binding inspection"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.connection.execute("INSERT INTO lww_binding_inspections VALUES(?1,?2,?3,?4,?5,?6)", params![id, serde_json::to_string(target)?, target_id, library_id, source_authority.0.to_string(), read(&self.connection)?.epoch])?;
        Ok(id)
    }

    #[cfg(test)]
    pub(crate) fn register_lww_binding_stage(&self, inspection_id: &str, staging_id: &str, receive_id: &str, changes: &[super::lww::Change]) -> StoreResult<()> {
        self.register_lww_binding_stage_with(inspection_id, staging_id, receive_id, changes, true)
    }

    pub(super) fn register_lww_binding_stage_with(&self, inspection_id: &str, staging_id: &str, receive_id: &str, changes: &[super::lww::Change], prove_content: bool) -> StoreResult<()> {
        if receive_id.is_empty() { return Err(invalid("Missing binding receive identity")); }
        let (authority,epoch):(String,String) = self.connection.query_row("SELECT source_authority,source_epoch FROM lww_binding_inspections WHERE inspection_id=?1", [inspection_id], |r| Ok((r.get(0)?,r.get(1)?)))?;
        if authority != self.lww_binding_authority()?.0.to_string() || epoch != read(&self.connection)?.epoch { return Err(invalid("Stale binding inspection")); }
        self.prepare_replace_commit(staging_id, None)?;
        for change in changes {
            change.stamp.validate().map_err(|e| invalid(&e.to_string()))?;
            {
                let result = change.value.validate();
                #[cfg(test)]
                crate::persistent_store::hash_work::validation(&change.value);
                result
            }.map_err(|e| invalid(&e.to_string()))?;
        }
        let frozen = super::lww::FrozenRows::new()?;
        for change in changes { frozen.insert(super::lww::intent_rows::target_row(change)?)?; }
        let encoded = super::lww::source_digest_rows(&frozen)?;
        let existing: Option<(String,String,String)> = self.connection.query_row("SELECT inspection_id,receive_id,changes FROM lww_binding_stages WHERE staging_id=?1", [staging_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some(existing) = existing {
            if existing != (inspection_id.into(),receive_id.into(),encoded) { return Err(invalid("Binding stage identity changed")); }
            if prove_content { validate_binding_stage_content(&self.connection,staging_id)?; }
            return Ok(());
        }
        let digest = binding_stage_digest(&self.connection, staging_id)?;
        self.connection.execute("INSERT INTO lww_binding_stages VALUES(?1,?2,?3,?4,NULL,?5)", params![staging_id,inspection_id,receive_id,encoded,digest])?;
        Ok(())
    }

    pub(crate) fn switch_lww_binding(&mut self, request: &SwitchBindingRequest) -> StoreResult<BindingState> {
        let body = serde_json::to_string(request)?;
        let existing:Option<(String,String)> = self.connection.query_row("SELECT body,change FROM lww_binding_switch_requests WHERE request_id=?1", [&request.header.request_id], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((original,encoded)) = existing {
            if original != body { return Err(invalid("Binding switch request identity changed")); }
            let change:BindingSelectionChange = serde_json::from_str(&encoded)?;
            let current = self.lww_binding_state()?;
            if current.selection_epoch != change.expected_epoch && current.selection_epoch != change.new_epoch {
                return Err(invalid("Stale binding switch completion"));
            }
            if current.selection_epoch == change.expected_epoch {
                if let Some(inspection)=change.inspection_id.as_deref() {
                    validate_binding_inspection_stages(self,inspection,request.header.binding_authority)?;
                }
            }
            self.lww_switch_target(&request.header,&change)?;
            return self.lww_binding_state();
        }
        let state = self.lww_binding_state()?;
        if state.target_authority != request.header.binding_authority || state.selection_epoch != request.expected_selection_epoch { return Err(invalid("Sync binding changed")); }
        // Unbinding, and returning to the target this library was last bound to without replacing it,
        // carry unpublished versions, receive progress and pending publications to the new authority.
        let (library_id,target_id,retain) = if matches!(request.target, SyncTarget::None) {
            if request.inspection_id.is_some() || request.initial_publication { return Err(invalid("Unbinding has no target inspection")); }
            (None,None,true)
        } else {
            let inspection = request.inspection_id.as_deref().ok_or_else(|| invalid("Missing binding inspection"))?;
            let (target,library,target_id,authority,epoch):(String,String,String,String,String) = self.connection.query_row("SELECT target,library_id,target_id,source_authority,source_epoch FROM lww_binding_inspections WHERE inspection_id=?1", [inspection], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
            if serde_json::from_str::<SyncTarget>(&target)? != request.target || authority != request.header.binding_authority.0.to_string() || epoch != request.expected_selection_epoch { return Err(invalid("Wrong target binding inspection")); }
            validate_binding_inspection_stages(self,inspection,request.header.binding_authority)?;
            let staged:bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM lww_binding_stages WHERE inspection_id=?1)", [inspection], |r| r.get(0))?;
            if staged && request.initial_publication { return Err(invalid("A staged binding has no initial publication")); }
            let retain = !staged && self.lww_previously_bound(&library,&target_id)?;
            (Some(library),Some(target_id),retain)
        };
        let change = BindingSelectionChange { expected_epoch: request.expected_selection_epoch.clone(),new_epoch:uuid::Uuid::new_v4().to_string(),target:request.target.clone(),library_id,target_id,inspection_id:request.inspection_id.clone(),initial_publication:request.initial_publication };
        let tx = self.connection.transaction()?;
        tx.execute("INSERT INTO lww_binding_switch_requests VALUES(?1,?2,?3)", params![request.header.request_id,body,serde_json::to_string(&change)?])?;
        if retain { tx.execute("INSERT INTO lww_binding_switch_retained VALUES(?1)", [&request.header.request_id])?; }
        tx.commit()?;
        self.lww_switch_target(&request.header,&change)?;
        self.lww_binding_state()
    }

    pub(crate) fn replace_lww_binding(&mut self, request: &ReplaceBindingRequest) -> StoreResult<super::RevisionResult> {
        let state = self.lww_binding_state()?;
        if state.target_authority != request.header.binding_authority || state.selection_epoch != request.expected_selection_epoch || matches!(state.target,SyncTarget::None) { return Err(invalid("Sync binding changed")); }
        let (library,target):(Option<String>,Option<String>) = self.connection.query_row("SELECT library_id,target_id FROM lww_binding_identity WHERE singleton=1", [], |r| Ok((r.get(0)?,r.get(1)?)))?;
        if library.as_deref() != Some(request.library_id.as_str()) || target.as_deref() != Some(request.target_id.as_str()) { return Err(invalid("Wrong replacement target")); }
        let (receive,_encoded,epoch,stage_library,stage_target):(String,String,Option<String>,String,String) = self.connection.query_row("SELECT s.receive_id,s.changes,s.activation_epoch,i.library_id,i.target_id FROM lww_binding_stages s JOIN lww_binding_inspections i USING(inspection_id) WHERE s.staging_id=?1", [&request.staging_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        if receive != request.receive_id || epoch.as_deref() != Some(request.expected_selection_epoch.as_str()) || stage_library != request.library_id || stage_target != request.target_id { return Err(invalid("Stale or wrong-target replacement stage")); }
        if request.header.request_id != request.receive_id { return Err(invalid("Binding receive request identity changed")); }
        let library_committed:bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM lww_requests WHERE request_id=?1)", [&request.header.request_id], |r| r.get(0))?;
        let changes = super::lww::binding_source_rows(&self.connection, &request.staging_id)?;
        // The stage catalog is proved once, inside the activation transaction.
        if !library_committed {
            super::lww::validate_binding_source_rows(&self.connection,&request.staging_id,&request.header,&changes)?;
        }
        self.lww_replace_target_rows(&request.header,&request.staging_id,&changes)
    }
}

pub(super) fn switch_retains_binding_state(db: &Connection, request_id: &str) -> StoreResult<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM lww_binding_switch_retained WHERE request_id=?1)", [request_id], |r| r.get(0))?)
}

fn validate_binding_inspection_stages(store: &super::PersistentStore, inspection: &str, authority: risunest_sync_wire::stamp::DecimalU64) -> StoreResult<()> {
    let mut stages = store.connection.prepare("SELECT staging_id,receive_id,changes FROM lww_binding_stages WHERE inspection_id=?1")?;
    for staging in stages.query_map([inspection], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))? {
        let (staging,receive,_encoded) = staging?;
        store.prepare_replace_commit(&staging,None)?;
        validate_binding_stage_content(&store.connection,&staging)?;
        let source_header = super::lww::Header { binding_authority:authority,request_id:receive };
        let changes = super::lww::binding_source_rows(&store.connection, &staging)?;
        super::lww::validate_binding_source_rows(&store.connection,&staging,&source_header,&changes)?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn pds_lww_binding_state(state: tauri::State<'_, super::commands::PersistentStoreState>) -> StoreResult<BindingState> {
    logged("pds_lww_binding_state", super::commands::with_store_mut(state, |store| { store.lww_recover_intents()?; store.lww_binding_state() }))
}
#[tauri::command]
pub(crate) fn pds_lww_switch_target(state: tauri::State<'_, super::commands::PersistentStoreState>, request: SwitchBindingRequest) -> StoreResult<BindingState> {
    logged("pds_lww_switch_target", super::commands::with_store_mut(state, |store| store.switch_lww_binding(&request)))
}
#[tauri::command]
pub(crate) fn pds_lww_replace_from_target(state: tauri::State<'_, super::commands::PersistentStoreState>, request: ReplaceBindingRequest) -> StoreResult<super::RevisionResult> {
    logged("pds_lww_replace_from_target", super::commands::with_store_mut(state, |store| store.replace_lww_binding(&request)))
}

pub(crate) fn binding_stage_digest(db: &Connection, staging_id: &str) -> StoreResult<String> {
    super::lww::catalog_digest(db, staging_id)
}

/// Proves the stage catalog once against both digests recorded for it.
pub(crate) fn validate_binding_stage_content(db: &Connection, staging_id: &str) -> StoreResult<()> {
    let expected:Option<String> = db.query_row("SELECT library_digest FROM lww_binding_stages WHERE staging_id=?1", [staging_id], |r| r.get(0)).optional()?;
    let source:Option<String> = db.query_row("SELECT catalog_digest FROM lww_binding_sources WHERE staging_id=?1", [staging_id], |r| r.get(0)).optional()?;
    if expected.is_none() && source.is_none() { return Ok(()); }
    let actual = binding_stage_digest(db,staging_id)?;
    if expected.is_some_and(|expected| expected != actual) { return Err(invalid("Binding stage content changed")); }
    if source.is_some_and(|source| source != actual) { return Err(super::lww::error("binding-source-integrity")); }
    Ok(())
}
