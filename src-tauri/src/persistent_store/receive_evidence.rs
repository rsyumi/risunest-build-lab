use super::{device_store::plugin_gc, lww, sync_selection, PersistentStore, StoreError, StoreResult};
use risunest_sync_wire::stamp::DecimalU64;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DevicePluginFence {
    pub instance_id: String,
    pub revision: DecimalU64,
}

pub(super) fn device_fence(db: &Connection) -> StoreResult<DevicePluginFence> {
    let fence = plugin_gc::read_fence(db)?;
    Ok(DevicePluginFence { instance_id: fence.instance_id, revision: DecimalU64(fence.revision as u64) })
}

pub(super) fn device_revision(db: &Connection) -> StoreResult<DecimalU64> {
    let revision: i64 = db.query_row("SELECT revision FROM device_meta WHERE singleton=1", [], |row| row.get(0))?;
    Ok(DecimalU64(u64::try_from(revision).map_err(|_| StoreError::Validation {
        message: "receive evidence device revision is invalid".into(),
    })?))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeReceiveEvidence {
    pub identity: sync_selection::CaptureIdentity,
    pub binding_authority: DecimalU64,
    pub device_plugins: DevicePluginFence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_revision: Option<DecimalU64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecordedApplyReceive {
    #[serde(flatten)]
    pub apply: lww::ApplyReceive,
    #[serde(default)]
    pub native_baseline: Option<NativeReceiveEvidence>,
}

impl PersistentStore {
    pub(crate) fn read_native_receive_evidence(&self, lease: &str) -> StoreResult<NativeReceiveEvidence> {
        let (source, _) = self.read_view(Some(lease))?;
        let identity = sync_selection::identity(source)?;
        // Job stores have independent connections. Hold both writer boundaries only while sampling.
        let device = Transaction::new_unchecked(self.device_store()?.connection(), TransactionBehavior::Immediate)?;
        let library = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let current = sync_selection::identity(&library)?;
        if identity.store_id != current.store_id || identity.library_epoch != current.library_epoch ||
            identity.generation != current.generation || identity.selection_epoch != current.selection_epoch {
            return Err(StoreError::Validation { message: "receive evidence library identity changed".into() });
        }
        if identity.revision != current.revision {
            return Err(StoreError::RevisionConflict { expected: identity.revision, actual: current.revision });
        }
        Ok(NativeReceiveEvidence { identity, binding_authority: lww::authority(&device)?, device_plugins: device_fence(&device)?, device_revision: Some(device_revision(&device)?) })
    }
}

pub(super) struct ReceiveProvenance {
    expected: Option<NativeReceiveEvidence>,
    pub exact: bool,
    pub post_device: Option<DevicePluginFence>,
    post_device_revision: Option<DecimalU64>,
}

impl ReceiveProvenance {
    pub fn new(baseline: Option<&NativeReceiveEvidence>, header: &lww::Header) -> Self {
        Self { expected: baseline.cloned(), exact: baseline.is_some_and(|value|
            value.binding_authority == header.binding_authority && value.device_revision.is_some()),
            post_device: None, post_device_revision: None }
    }

    pub fn check_library(&mut self, db: &Connection) -> StoreResult<()> {
        if let Some(expected) = &self.expected {
            self.exact &= sync_selection::identity(db)? == expected.identity;
        }
        Ok(())
    }

    pub fn committed_library(&mut self, revision: i64) {
        if let Some(expected) = &mut self.expected { expected.identity.revision = revision; }
    }

    pub fn check_device(&mut self, db: &Connection) -> StoreResult<()> {
        if let Some(expected) = &self.expected {
            self.exact &= lww::authority(db)? == expected.binding_authority &&
                device_fence(db)? == *self.post_device.as_ref().unwrap_or(&expected.device_plugins) &&
                Some(device_revision(db)?) == self.post_device_revision.or(expected.device_revision);
        }
        Ok(())
    }

    pub fn committed_device(&mut self, db: &Connection) -> StoreResult<()> {
        self.post_device = Some(device_fence(db)?);
        self.post_device_revision = Some(device_revision(db)?);
        Ok(())
    }
}

#[cfg(test)]
pub(super) mod test_hooks {
    use std::cell::RefCell;
    thread_local! {
        static DEVICE: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
        static ARCHIVE: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
    }
    pub fn set_after_device(action: impl FnOnce() + 'static) { DEVICE.with(|slot| *slot.borrow_mut() = Some(Box::new(action))); }
    pub fn set_after_archive(action: impl FnOnce() + 'static) { ARCHIVE.with(|slot| *slot.borrow_mut() = Some(Box::new(action))); }
    pub fn after_device() { let action = DEVICE.with(|slot| slot.borrow_mut().take()); if let Some(action) = action { action(); } }
    pub fn after_archive() { let action = ARCHIVE.with(|slot| slot.borrow_mut().take()); if let Some(action) = action { action(); } }
}
