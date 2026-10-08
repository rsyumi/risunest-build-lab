//! User-selected standalone archive export for authenticated history entries.
//! Remote bytes are fully downloaded and verified before the archive is built.
use super::{
    capabilities::Capabilities,
    connection_commands, connection_store::ConnectionStore,
    contract::{Cancellation, ErrorKind, LeaseKind, ProviderError, Result},
    control, leases,
    runtime, snapshot_export, snapshot_restore,
};
use crate::native_log::logged;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_fs::{FilePath, FsExt, OpenOptions};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ExportSnapshotRequest {
    connection_id: String,
    snapshot_id: String,
    export_id: String,
}

static EXPORTS: std::sync::LazyLock<std::sync::Mutex<std::collections::BTreeMap<String, Cancellation>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::BTreeMap::new()));
struct ExportClaim(String);
impl Drop for ExportClaim {
    fn drop(&mut self) {
        if let Ok(mut exports) = EXPORTS.lock() { exports.remove(&self.0); }
    }
}
fn claim_export(id: &str) -> Result<(ExportClaim, Cancellation)> {
    if uuid::Uuid::parse_str(id).ok().is_none_or(|parsed| parsed.to_string() != id) {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    let mut exports = EXPORTS.lock().map_err(runtime::local_error)?;
    if !exports.is_empty() { return Err(ProviderError::new(ErrorKind::PreconditionFailed)); }
    let cancel = Cancellation::default();
    exports.insert(id.to_owned(), cancel.clone());
    Ok((ExportClaim(id.to_owned()), cancel))
}
#[tauri::command]
pub(crate) fn external_storage_cancel_export(export_id: String) -> Result<()> {
    let exports = logged("external_storage_cancel_export", EXPORTS.lock().map_err(runtime::local_error))?;
    if let Some(cancel) = exports.get(&export_id) { cancel.cancel(); }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExportSnapshotResponse {
    cancelled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    destination: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
}


fn valid_id(value: &str) -> bool { !value.is_empty() && value.len() <= 1024 && !value.contains('\0') }

fn archive_name(snapshot_id: &str) -> String {
    let safe: String = snapshot_id
        .chars()
        .filter(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
        .take(64)
        .collect();
    format!(
        "RisuNest-{}.risunest",
        if safe.is_empty() { "snapshot" } else { &safe }
    )
}

fn selected_path(app: &AppHandle, snapshot_id: &str) -> Option<FilePath> {
    app.dialog()
        .file()
        .add_filter("RisuNest backup", &["risunest"])
        .set_file_name(archive_name(snapshot_id))
        .blocking_save_file()
}

fn publish_uri_destination(
    app: &AppHandle,
    selected: FilePath,
    candidate: &std::path::Path,
    expected_hash: &str,
    cancel: &Cancellation,
) -> Result<()> {
    let mut source = File::open(candidate).map_err(|_| ProviderError::new(ErrorKind::Transient))?;
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    let mut output = app
        .fs()
        .open(selected.clone(), options)
        .map_err(|_| ProviderError::new(ErrorKind::Transient))?;
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        cancel.check()?;
        let count = source
            .read(&mut buffer)
            .map_err(|_| ProviderError::new(ErrorKind::Transient))?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(|_| ProviderError::new(ErrorKind::Transient))?;
    }
    output
        .flush()
        .map_err(|_| ProviderError::new(ErrorKind::Transient))?;
    output
        .sync_all()
        .map_err(|_| ProviderError::new(ErrorKind::Transient))?;
    drop(output);
    let mut options = OpenOptions::new();
    options.read(true);
    let mut output = app
        .fs()
        .open(selected, options)
        .map_err(|_| ProviderError::new(ErrorKind::Transient))?;
    let mut digest = Sha256::new();
    loop {
        cancel.check()?;
        let count = output
            .read(&mut buffer)
            .map_err(|_| ProviderError::new(ErrorKind::Transient))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    if hex::encode(digest.finalize()) != expected_hash {
        return Err(ProviderError::new(ErrorKind::Corrupt));
    }
    Ok(())
}

/// Announces this command in the repository, runs its remote work and hands
/// the announcement back. A command has no durable job to name it, so the
/// identity is drawn per call and lives only as long as the call does.
/// An interrupted command leaves only a finite lease behind.
async fn with_export_lease<T>(
    context: &leases::LeaseContext<'_>,
    capabilities: &Capabilities,
    cancel: &Cancellation,
    body: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    if !capabilities.lease_operations {
        return body.await;
    }
    let lease_id = uuid::Uuid::new_v4().to_string();
    match leases::admit(context, &lease_id, LeaseKind::Work, cancel).await? {
        leases::Admission::Admitted(owner) => owner.run(context, cancel, body).await,
        leases::Admission::Yield { reason, .. } => Err(leases::yield_error(reason)),
        leases::Admission::UnsupportedProtection => Err(ProviderError::new(ErrorKind::Unsupported)),
    }
}

fn publish_prepared_snapshot(
    app: &AppHandle,
    selected: FilePath,
    prepared: snapshot_restore::PreparedRemoteSnapshot,
    sections: &[super::sections::CapturedSection],
    staging: &std::path::Path,
    cancel: &Cancellation,
) -> Result<ExportSnapshotResponse> {
    let path_destination = selected.clone().into_path().ok();
    let local_destination = path_destination
        .clone()
        .unwrap_or_else(|| staging.join("selected-snapshot.risunest"));
    let receipt = snapshot_export::export_verified_snapshot(
        prepared,
        sections,
        &local_destination,
        &staging.join("export"),
        cancel,
        &app.package_info().version.to_string(),
    )?;
    if path_destination.is_none() {
        publish_uri_destination(
            app,
            selected.clone(),
            &local_destination,
            &receipt.sha256,
            cancel,
        )?;
    }
    Ok(ExportSnapshotResponse {
        cancelled: false,
        destination: Some(selected.to_string()),
        sha256: Some(receipt.sha256),
    })
}


/// Prepares a source that was resolved from the native conflict-source
/// registry. The returned wrapper owns the source claim until the caller has
/// consumed the prepared snapshot or explicitly keeps the claim from
/// `into_parts` while staging it.


/// Exports a source that was resolved from the native conflict-source
/// registry. The local variant never opens a provider connection; the remote
/// variant validates its stored locator against the currently opened handle.


#[tauri::command(async)]
pub(crate) async fn external_storage_export_snapshot(
    app: AppHandle,
    request: ExportSnapshotRequest,
    progress: tauri::ipc::Channel<serde_json::Value>,
) -> Result<ExportSnapshotResponse> {
    logged("external_storage_export_snapshot", Box::pin(async move {
        if !valid_id(&request.connection_id) || !valid_id(&request.snapshot_id) {
            return Err(ProviderError::new(ErrorKind::Corrupt));
        }
        let (_claim, cancel) = claim_export(&request.export_id)?;
        let counters = super::phase_progress::PhaseProgress::new(move |value| {
            let _ = progress.send(serde_json::json!({
                "completedBytes": value.bytes.to_string(), "totalBytes": value.total_bytes.to_string(),
                "completedItems": value.items.to_string(), "totalItems": value.total_items.to_string(),
            }));
        });
        let Some(selected) = selected_path(&app, &request.snapshot_id) else {
            return Ok(ExportSnapshotResponse {
                cancelled: true,
                destination: None,
                sha256: None,
            });
        };
        let root = runtime::root(&app)?;
        let staging = super::leftovers::managed_scratch(&root, "external-snapshot-download-")?;
        cancel.check()?;
        let connected = connection_commands::open_connected_with_cancel(&app, &request.connection_id, &cancel).await?;
        let known = match ConnectionStore::open(&root)?.discovery_snapshot(
            &request.connection_id,
            &request.snapshot_id,
        ) {
            Ok(value) => Some(value),
            Err(error) if error.kind == ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let writer_id = crate::persistent_store::commands::with_store_mut(app.state(), |store| {
            store.external_identity()
        })
        .map_err(runtime::local_error)?
        .store_id;
        let context = leases::LeaseContext {
            root: root.as_path(),
            connection_id: &connected.stored.id,
            writer_id: &writer_id,
            descriptor: &connected.stored.descriptor,
            root_key: &connected.root_key,
            provider: connected.provider.as_ref(),
            repository: &connected.handle,
            clock: leases::system_clock(),
            protection_supported: connected.stored.capabilities.lease_operations,
            ledger: Some(app.state::<super::job_store::JobCommandState>().lease_ledger()?),
        };
        let (prepared, sections) = with_export_lease(
            &context,
            &connected.stored.capabilities,
            &cancel,
            async {
                let cache_root = root.clone();
                let cache_connection = request.connection_id.clone();
                let cache_id = request.snapshot_id.clone();
                let remote = control::find_snapshot_with_locator_invalidation(
                    &connected,
                    &request.snapshot_id,
                    known.as_ref(),
                    move || {
                        ConnectionStore::open(&cache_root)?
                            .forget_discovery(&cache_connection, &cache_id)
                    },
                    &cancel,
                )
                .await?;
                ConnectionStore::open(&root)?.remember_discovery(
                    &request.connection_id,
                    &request.snapshot_id,
                    &remote,
                )?;
                let prepared = snapshot_restore::download_snapshot(
                    &remote,
                    &staging.path().join("verified"),
                    &connected.root_key,
                    None,
                    snapshot_restore::SourceTrust::Downloaded,
                    connected.provider.as_ref(),
                    &connected.handle,
                    &counters,
                    &cancel,
                )
                .await?;
                if prepared.snapshot_id != request.snapshot_id {
                    return Err(ProviderError::new(ErrorKind::Corrupt));
                }
                let sections = snapshot_export::download_snapshot_sections(
                    &remote,
                    &prepared,
                    &staging.path().join("verified"),
                    &connected.root_key,
                    connected.provider.as_ref(),
                    &connected.handle,
                    &counters,
                    &cancel,
                )
                .await?;
                Ok((prepared, sections))
            },
        )
        .await?;
        counters.flush();
        cancel.check()?;
        publish_prepared_snapshot(&app, selected, prepared, &sections, staging.path(), &cancel)
    }).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_storage::{
        contract::{lease_object_id, LeaseKind, ObjectRole, RepositoryHandle},
        fake::{self, FakeLeaseClock, FakeProvider},
    };
    use risunest_external_storage_format::format::{Descriptor, Strategy};
    use std::{
        cell::RefCell,
        path::PathBuf,
        sync::atomic::{AtomicBool, Ordering},
    };

    const DAY: u64 = 24 * 60 * 60 * 1000;
    const NOW: u64 = 1_000 * DAY;
    const CONNECTION: &str = "connection";

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn cleanup_ready() -> Capabilities {
        fake::capabilities(true)
    }

    struct Harness {
        _directory: tempfile::TempDir,
        root: PathBuf,
        clock: FakeLeaseClock,
        provider: FakeProvider,
        repository: RepositoryHandle,
        descriptor: Descriptor,
        root_key: [u8; 32],
    }

    impl Harness {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().to_path_buf();
            Self {
                clock: FakeLeaseClock::new(NOW),
                _directory: directory,
                root,
                provider: FakeProvider::new(true),
                repository: fake::repository(),
                descriptor: Descriptor::new("synthetic-descriptor".into(), Some(Strategy::Cas))
                    .unwrap(),
                root_key: [7; 32],
            }
        }
        fn context(&self) -> leases::LeaseContext<'_> {
            leases::LeaseContext {
                root: &self.root,
                connection_id: CONNECTION,
                writer_id: "writer",
                descriptor: &self.descriptor,
                root_key: &self.root_key,
                provider: &self.provider,
                repository: &self.repository,
                clock: &self.clock,
                protection_supported: true,
                ledger: None,
            }
        }
        /// The work leases the repository shows, whoever placed them.
        fn work_leases(&self) -> Vec<String> {
            self.provider
                .state
                .lock()
                .unwrap()
                .objects
                .keys()
                .filter(|object| object.starts_with("work-"))
                .cloned()
                .collect()
        }
        /// Counts every object the fake ever created, so a lease that was
        /// placed and given back inside one call is still visible afterwards.
        fn creations(&self) -> u64 {
            self.provider.state.lock().unwrap().next_version
        }
    }

    /// GC29: the lease is confirmed in the repository before the command asks
    /// for any data, and it is gone once the command has finished.
    #[test]
    fn export_claim_cancellation_and_release_cover_picker_and_worker_lifetime() {
        let id=uuid::Uuid::new_v4().to_string();
        let (claim,cancel)=claim_export(&id).unwrap();
        assert!(claim_export(&uuid::Uuid::new_v4().to_string()).is_err());
        external_storage_cancel_export(uuid::Uuid::new_v4().to_string()).unwrap();
        assert!(cancel.check().is_ok());
        external_storage_cancel_export(id.clone()).unwrap();
        assert_eq!(cancel.check().unwrap_err().kind,ErrorKind::Cancelled);
        drop(claim);
        let (next,cancel)=claim_export(&id).unwrap();
        assert!(cancel.check().is_ok());
        drop(next);
        assert!(claim_export("invalid").is_err());
    }

    #[test]
    fn an_export_holds_a_confirmed_lease_while_it_reads_and_gives_it_back() {
        let harness = Harness::new();
        let cancel = Cancellation::default();
        let held = RefCell::new(Vec::new());
        block_on(async {
            let context = harness.context();
            let value = with_export_lease(&context, &cleanup_ready(), &cancel, async {
                let rows = leases::survey(&context, &cancel).await.unwrap().leases;
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].document.as_ref().unwrap().kind, risunest_external_storage_format::control::LeaseKind::Work);
                assert_eq!(rows[0].document.as_ref().unwrap().expires_at_ms, NOW + leases::LEASE_TTL_MS);
                assert!(harness.provider.holds(&rows[0].locator.object));
                *held.borrow_mut() = harness.work_leases();
                Ok(7u8)
            })
            .await
            .unwrap();
            assert_eq!(value, 7);
        });
        assert_eq!(held.borrow().len(), 1);
        assert!(harness.work_leases().is_empty());
    }

    /// An unverifiable delete marker stops data requests before admission.
    #[test]
    fn an_export_refuses_an_unverifiable_delete_marker() {
        let harness = Harness::new();
        let cancel = Cancellation::default();
        let marker = lease_object_id(LeaseKind::Deleting, &"a".repeat(32)).unwrap();
        harness
            .provider
            .seed(&marker, ObjectRole::Lease, b"foreign".to_vec());
        let placed = harness.creations();
        let started = AtomicBool::new(false);
        block_on(async {
            let context = harness.context();
            let error =
                with_export_lease(&context, &cleanup_ready(), &cancel, async {
                    started.store(true, Ordering::SeqCst);
                    Ok(())
                })
                .await
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Transient);
        });
        assert!(!started.load(Ordering::SeqCst));
        // The initial survey yields before announcing this export.
        assert_eq!(harness.creations(), placed);
        assert!(harness.provider.holds(&marker));
        assert!(harness.work_leases().is_empty());
    }

    #[test]
    fn an_export_never_reads_through_an_unclassified_lease() {
        let harness = Harness::new();
        let cancel = Cancellation::default();
        harness.provider.seed("not-a-lease", ObjectRole::Lease, b"unknown".to_vec());
        let started = AtomicBool::new(false);
        block_on(async {
            let context = harness.context();
            let error = with_export_lease(&context, &cleanup_ready(), &cancel, async {
                started.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await
            .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Transient);
        });
        assert!(!started.load(Ordering::SeqCst));
        assert!(harness.provider.holds("not-a-lease"));
        assert_eq!(harness.provider.delete_attempts("not-a-lease"), 0);
        assert!(harness.work_leases().is_empty());
    }

    /// GC29: a body that fails or reports cancellation gives its lease back.
    #[test]
    fn an_export_that_fails_or_is_cancelled_still_returns_its_lease() {
        for kind in [ErrorKind::Cancelled, ErrorKind::Corrupt] {
            let harness = Harness::new();
            let cancel = Cancellation::default();
            block_on(async {
                let context = harness.context();
                let error = with_export_lease(&context, &cleanup_ready(), &cancel, async {
                    Err::<(), _>(ProviderError::new(kind))
                })
                .await
                .unwrap_err();
                assert_eq!(error.kind, kind);
            });
            assert!(harness.work_leases().is_empty());
        }
    }

    /// A repository whose removals cannot be trusted never runs a cleanup, so
    /// the command announces nothing and the export proceeds as before.
    #[test]
    fn an_export_announces_nothing_where_cleanup_is_unavailable() {
        let harness = Harness::new();
        let cancel = Cancellation::default();
        let started = AtomicBool::new(false);
        block_on(async {
            let context = harness.context();
            let unavailable = Capabilities { lease_operations: false, ..fake::capabilities_without_cleanup(true) };
            with_export_lease(&context, &unavailable, &cancel, async {
                started.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await
            .unwrap();
        });
        assert!(started.load(Ordering::SeqCst));
        assert_eq!(harness.creations(), 0);
    }

    #[test]
    fn request_ids_are_bounded_before_dialog_or_network() {
        assert!(valid_id("connection"));
        assert!(!valid_id(""));
        assert!(!valid_id(&"x".repeat(1025)));
        assert!(!valid_id("bad\0id"));
        assert_eq!(
            archive_name("../snapshot/id"),
            "RisuNest-snapshotid.risunest"
        );
    }


}
