//! Bodies a publication references that this device does not hold. The server
//! being published to is asked first; only what it lacks is fetched from the
//! server or external storage that holds it, and never into the library.
use super::{
    cache::Cache,
    client::ServerClient,
    credentials::StoredConfig,
    residency::{open_transient_server_proof_with_check, Residency},
    transfer::HeldBodies,
    Result, SyncError,
};
use crate::persistent_store::PersistentStore;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc},
};

pub(crate) struct PreviousStorage<'a> {
    root: PathBuf,
    sizes: BTreeMap<String, u64>,
    check: &'a dyn Fn() -> Result<()>,
    cancelled: Option<Arc<AtomicBool>>,
}

impl<'a> PreviousStorage<'a> {
    pub(crate) fn new(
        root: &Path,
        check: &'a dyn Fn() -> Result<()>,
        cancelled: Option<Arc<AtomicBool>>,
    ) -> Self {
        Self { root: root.to_owned(), sizes: BTreeMap::new(), check, cancelled }
    }
    fn custody(&self, hash: &str) -> Result<Option<super::residency::RemoteObject>> {
        if !Residency::exists(&self.root) {
            return Ok(None);
        }
        Residency::open(&self.root)?.object(hash, None)
    }
    /// Records the size of a body this device does not hold. A body that no
    /// catalog or storage knows stays unrecorded and fails the upload.
    pub(crate) fn observe(&mut self, store: &PersistentStore, hash: &str) -> Result<()> {
        let size = match store.asset_object_byte_size(hash)? {
            Some(size) => Some(size),
            None => match self.custody(hash)? {
                Some(object) => Some(object.size),
                None => crate::external_storage::lww_residency::stat(&self.root, hash)?,
            },
        };
        if let Some(size) = size {
            self.sizes.insert(hash.to_owned(), size);
        }
        Ok(())
    }
    /// Records custody on the server this device publishes to, so later
    /// downloads of these bodies come from it while the previous storage keeps
    /// its copy.
    pub(crate) fn retain(&self, client: &ServerClient, config: &StoredConfig) -> Result<()> {
        let mut objects = Vec::new();
        for (hash, size) in &self.sizes {
            if self.custody(hash)?.is_some_and(|object| object.config.library_id == config.library_id) {
                continue;
            }
            objects.push((hash.clone(), Some(*size)));
        }
        if objects.is_empty() {
            return Ok(());
        }
        (self.check)()?;
        let head = client.resolve_identity()?;
        Residency::open(&self.root)?.retain(client, config, &head, &objects)
    }
    fn unavailable(&self, cause: String) -> SyncError {
        if let Err(stopped) = (self.check)() {
            return stopped;
        }
        SyncError { cause: Some(cause), ..SyncError::new("previous-storage-unavailable", 409) }
    }
}

impl HeldBodies for PreviousStorage<'_> {
    fn size(&self, hash: &str) -> Option<u64> {
        self.sizes.get(hash).copied()
    }
    fn fetch(&self, hash: &str, size: u64, destination: &Cache) -> Result<bool> {
        (self.check)()?;
        let scratch = self.scratch()?;
        if let Some(proof) = self.custody(hash)? {
            let mut body = open_transient_server_proof_with_check(&self.root, scratch.path(), &proof, self.check)
                .map_err(|error| self.unavailable(error.code))?
                .ok_or_else(|| self.unavailable("server-body-missing".into()))?;
            destination.cas.prepare_reader_expected(&mut body, hash, size)?;
            return Ok(true);
        }
        use crate::external_storage::contract::{Cancellation, ErrorKind};
        let cancel = match &self.cancelled {
            Some(flag) => Cancellation::with_external_flag(flag.clone()),
            None => Cancellation::default(),
        };
        let spooled = tauri::async_runtime::block_on(crate::external_storage::lww_residency::spool_verified_remote_body(
            &self.root, hash, scratch.path(), &cancel,
        ));
        match spooled {
            Ok(Some(mut file)) => {
                destination.cas.prepare_reader_expected(file.as_file_mut(), hash, size)?;
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(error) if error.kind == ErrorKind::Cancelled => Err(SyncError::new("cancelled", 409)),
            Err(error) => Err(self.unavailable(format!("{:?}", error.kind))),
        }
    }
    fn scratch(&self) -> Result<tempfile::TempDir> {
        Ok(tempfile::Builder::new().prefix("asset-transient-").tempdir_in(&self.root)?)
    }
}
