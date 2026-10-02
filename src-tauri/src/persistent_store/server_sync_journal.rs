use super::PersistentStore;
use crate::server_sync::{credentials::StoredConfig, Result, SyncError};
use rusqlite::{Connection, OptionalExtension};
impl PersistentStore {
    pub(crate) fn server_asset_policy(&self)->Result<crate::server_sync::residency::AssetPolicy> {Ok(self.device_store()?.asset_residency_policy()?)}
    pub(crate) fn server_outbox_for_repair(&self,authority:risunest_sync_wire::stamp::DecimalU64)->Result<Vec<super::lww::OutboxEntry>> {
        if self.lww_binding_authority()?!=authority{return Err(SyncError::new("binding-authority-changed",409));}
        let mut entries=Vec::new();
        for db in [&self.connection,self.device_store()?.connection()] {
            let mut query=db.prepare("SELECT key,stamp,value,version FROM lww_outbox WHERE authority=?1 ORDER BY key")?;
            let rows=query.query_map([authority.0.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?;
            for row in rows {let (key,stamp,value,version)=row?;entries.push(super::lww::OutboxEntry{key:key.try_into()?,stamp:serde_json::from_str(&stamp).map_err(|_|SyncError::new("outbox-integrity",409))?,value:serde_json::from_str(&value).map_err(|_|SyncError::new("outbox-integrity",409))?,version,target_authority:authority});}
        }Ok(entries)
    }
    pub(crate) fn server_assert_receive_finished(&self, request:&super::lww::StageReceive) -> Result<()> {
        if self.lww_binding_authority()?!=request.header.binding_authority {return Err(SyncError::new("binding-authority-changed",409));}
        let body:Option<String>=self.device_store()?.connection().query_row("SELECT body FROM lww_receive WHERE request_id=?1 AND authority=?2 AND applied=1 AND finished=1",rusqlite::params![request.header.request_id,request.header.binding_authority.0.to_string()],|r|r.get(0)).optional()?;
        if body.as_deref()!=Some(&serde_json::to_string(request).map_err(|_|SyncError::new("receive-page-integrity",409))?) {return Err(SyncError::new("receive-not-durable",409));}Ok(())
    }
    pub(crate) fn server_save_config(&self, config: &StoredConfig) -> Result<()> {
        self.connection.execute("INSERT INTO server_sync_state VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET config=excluded.config", [serde_json::to_string(config).map_err(|_|SyncError::new("invalid-local-server-config",409))?])?;
        self.server_repair_residency_access()
    }
    pub(crate) fn server_stored_config(&self) -> Result<Option<StoredConfig>> {
        let text: Option<String> = self
            .connection
            .query_row(
                "SELECT config FROM server_sync_state WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        text.map(|s| {
            serde_json::from_str(&s).map_err(|_| SyncError::new("invalid-local-server-config", 409))
        })
        .transpose()
    }
    pub(crate) fn server_repair_residency_access(&self) -> Result<()> {
        if let Some(config)=self.server_stored_config()? {
            if crate::server_sync::residency::Residency::exists(&self.repository_root) { crate::server_sync::residency::Residency::open(&self.repository_root)?.replace_access_config(&config)?; }
        } Ok(())
    }


}
pub(super) fn create_schema(db: &Connection) -> super::StoreResult<()> {
    db.execute_batch("CREATE TABLE server_sync_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1),config TEXT NOT NULL);")?; Ok(())
}
pub(super) fn validate_schema(db: &Connection) -> super::StoreResult<()> {
    let actual: Option<String> = db.query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name='server_sync_state'", [], |row| row.get(0)).optional()?;
    if actual.as_deref() != Some("CREATE TABLE server_sync_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1),config TEXT NOT NULL)") { return Err(super::StoreError::Validation { message: "Server connection schema is incompatible".into() }); }
    Ok(())
}
