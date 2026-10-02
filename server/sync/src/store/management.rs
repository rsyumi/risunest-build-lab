use super::Store;
use crate::{Error, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedDevice {
    pub id: String,
    pub name: String,
    pub revoked: bool,
    pub pending: bool,
    pub registration_request: Option<String>,
    pub last_ack: Option<i64>,
    pub retained: u64,
    pub pending_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementConnection {
    pub endpoint: Option<String>,
    pub cloudflared: Option<PathBuf>,
    pub registry_url: Option<String>,
    pub registry_enabled: bool,
    pub uuid: Option<String>,
}

impl Store {
    pub fn managed_devices(&self) -> Result<Vec<ManagedDevice>> {
        let db = self.reader()?;
        let mut query = db.prepare("SELECT id,name,revoked,EXISTS(SELECT 1 FROM upload_jobs j JOIN uploads u ON u.id=j.upload WHERE u.device=devices.id AND j.terminal=0),registration_request,last_ack,(SELECT count(*) FROM object_custody WHERE device=devices.id),(SELECT j.error FROM upload_jobs j JOIN uploads u ON u.id=j.upload WHERE u.device=devices.id AND j.terminal=0 AND j.error IS NOT NULL LIMIT 1) FROM devices ORDER BY rowid")?;
        let rows = query.query_map([], |row| {
            Ok(ManagedDevice {
                id: row.get(0)?,
                name: row.get(1)?,
                revoked: row.get(2)?,
                pending: row.get(3)?,
                registration_request: row.get(4)?,
                last_ack: row.get(5)?,
                retained: row.get::<_, i64>(6)? as u64,
                pending_error: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn forget_revoked_device(&self, id: &str) -> Result<()> {
        risunest_sync_wire::validate_id(id)?;
        let mut db = self.db()?;
        let tx = db.transaction()?;
        let revoked: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM devices WHERE id=?1 AND revoked=1)",
            [id],
            |row| row.get(0),
        )?;
        if !revoked {
            return Err(Error::new("revoked-device-required", 409));
        }
        tx.execute("DELETE FROM object_custody WHERE device=?1", [id])?;
        tx.commit()?;
        Ok(())
    }
    pub fn management_connection(&self) -> Result<ManagementConnection> {
        let _gate = self
            .connection_gate
            .lock()
            .map_err(|_| Error::new("connection-state-unavailable", 503))?;
        let state = self.read_connection()?;
        Ok(ManagementConnection {
            endpoint: state.endpoint,
            cloudflared: state.cloudflared,
            registry_url: state.directory.as_ref().map(|d| d.base_url.clone()),
            registry_enabled: state.directory_enabled,
            uuid: state.directory.as_ref().map(|d| d.uuid.clone()),
        })
    }
    pub fn data_path(&self) -> &Path {
        &self.root
    }
}

pub(crate) fn protect(bytes: &[u8], seal: bool) -> Result<Vec<u8>> {
    super::connection::protect(bytes, seal)
}
