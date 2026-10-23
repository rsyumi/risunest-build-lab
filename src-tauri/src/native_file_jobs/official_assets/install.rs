//! Installs a batch of spooled bodies into the payload store and commits their
//! aliases in one revision of the library the restore started in.
use super::download::Spooled;
use crate::asset_repository::import_payloads::{prepare_import_payloads, ImportPayload, ImportPinning};
use crate::asset_repository::job_pins::{CasJobKind, CasJobOwner, CasReleaseOutcome, DurableCasJob};
use crate::asset_repository::{PayloadCas, PreparedPayload};
use crate::persistent_store::{AssetAlias, PersistentStore, StoreError, WorkingSetCommit};

const COMMIT_ATTEMPTS: usize = 8;

#[derive(Debug)]
pub(super) struct Body {
    pub(super) index: usize,
    pub(super) key: String,
    pub(super) spooled: Spooled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BatchError {
    LibraryChanged,
    Cancelled,
    Local,
}

#[derive(Debug)]
pub(super) struct Installed {
    pub(super) revision: i64,
    pub(super) cleanup_failed: bool,
}

pub(super) struct Installer {
    store: PersistentStore,
    cas: PayloadCas,
    job_id: String,
    expected_library: String,
    batches: u64,
}

impl Installer {
    pub(super) fn new(store: PersistentStore, job_id: String, expected_library: String) -> std::io::Result<Self> {
        let cas = PayloadCas::new(store.repository_root())?;
        Ok(Self { store, cas, job_id, expected_library, batches: 0 })
    }

    pub(super) fn install(&mut self, bodies: &[Body], cancelled: &dyn Fn() -> bool) -> Result<Installed, BatchError> {
        self.batches += 1;
        let mut pins = DurableCasJob::begin(
            self.store.repository_root(),
            &format!("{}-{}", self.job_id, self.batches),
            CasJobKind::LocalBackupRestore,
            CasJobOwner::native_file_job(&self.job_id),
            now_ms(),
        )
        .map_err(|error| local("begin", error))?;
        let committed = self.commit(&mut pins, bodies, cancelled);
        let released = pins.release(if committed.is_ok() { CasReleaseOutcome::Committed } else { CasReleaseOutcome::Aborted });
        committed.map(|revision| Installed { revision, cleanup_failed: released.is_err() })
    }

    fn commit(&mut self, pins: &mut DurableCasJob, bodies: &[Body], cancelled: &dyn Fn() -> bool) -> Result<i64, BatchError> {
        let inputs = bodies
            .iter()
            .map(|body| ImportPayload {
                path: body.spooled.path.clone(),
                hash: body.spooled.sha256.clone(),
                size: body.spooled.size,
                sync_required: true,
                progress_items: 1,
            })
            .collect::<Vec<_>>();
        let prepared = prepare_import_payloads(&self.cas, pins, &inputs, ImportPinning::DuringPreparation, cancelled, |_, _, _| {})
            .map_err(|error| if cancelled() { BatchError::Cancelled } else { local("prepare", error) })?;
        let aliases = bodies
            .iter()
            .zip(&prepared)
            .map(|(body, payload)| alias(&body.key, payload))
            .collect::<Result<Vec<_>, _>>()?;
        pins.seal(&mut self.store, now_ms()).map_err(|error| local("seal", error))?;
        for _ in 0..COMMIT_ATTEMPTS {
            if cancelled() {
                return Err(BatchError::Cancelled);
            }
            let revision = self.store.revision().map_err(|error| local("revision", error))?;
            if self.store.library_identity().map_err(|error| local("library", error))? != self.expected_library {
                return Err(BatchError::LibraryChanged);
            }
            match self.store.commit_with_asset_aliases(&WorkingSetCommit { expected_revision: revision, ..Default::default() }, &aliases) {
                Ok(committed) => return Ok(committed.revision),
                Err(StoreError::RevisionConflict { .. }) => continue,
                Err(error) => return Err(local("commit", error)),
            }
        }
        Err(BatchError::Local)
    }
}

/// The alias metadata the renderer's verified asset write records.
fn alias(key: &str, payload: &PreparedPayload) -> Result<AssetAlias, BatchError> {
    let normalized = key.replace('\\', "/");
    Ok(AssetAlias {
        key: key.to_owned(),
        object_hash: Some(payload.content_hash.clone()),
        kind: "asset".to_owned(),
        size: i64::try_from(payload.byte_size).map_err(|_| BatchError::Local)?,
        mime: String::new(),
        name: normalized.rsplit('/').next().unwrap_or(&normalized).to_owned(),
        ext: key.rsplit('.').next().unwrap_or(key).to_owned(),
        inlay_type: None,
        width: None,
        height: None,
        metadata: serde_json::json!({}),
    })
}

fn local(step: &str, error: impl std::fmt::Display) -> BatchError {
    crate::nlog!("warn", "official asset install failed at {step}: {error}");
    BatchError::Local
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
