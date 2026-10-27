//! Local observations of app-owned remote files, without usage-only network requests.
use super::{
    capabilities::Capabilities,
    connection_store::ConnectionStore,
    contract::*,
    control,
    packaging::RemoteObject,
};
use std::{collections::BTreeSet, path::Path};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ReachableUsage {
    pub snapshot_id: String,
    pub known_direct_objects: u64,
    pub known_direct_bytes: u64,
    pub complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StorageUsage {
    pub provider_physical_bytes: Option<u64>,
    pub observed_at_ms: Option<u64>,
    pub locally_uploaded_objects_lower_bound: u64,
    pub locally_uploaded_bytes_lower_bound: u64,
    pub latest_reachable: Option<ReachableUsage>,
}

fn corrupt() -> ProviderError {
    ProviderError::new(ErrorKind::Corrupt)
}
pub(super) fn direct_reachable(
    snapshot: &RemoteObject,
    document: &control::SnapshotView,
) -> Result<ReachableUsage> {
    direct_objects(
        &document.snapshot_id,
        std::iter::once(&snapshot.receipt),
        super::reachability::document_references(document),
    )
}

pub(super) fn direct_objects<'a>(
    snapshot_id: &str,
    receipts: impl IntoIterator<Item = &'a ObjectReceipt>,
    references: impl IntoIterator<Item = &'a risunest_external_storage_format::snapshot::StoredObject>,
) -> Result<ReachableUsage> {
    let mut identities = BTreeSet::new();
    let mut bytes = 0u64;
    for receipt in receipts {
        let identity = (receipt.locator.collection.clone(), receipt.locator.object.clone());
        if identities.insert(identity) { bytes = bytes.checked_add(receipt.byte_length).ok_or_else(corrupt)?; }
    }
    for object in references {
        let identity = (object.locator.collection.clone(), object.locator.object.clone());
        if identities.insert(identity) { bytes = bytes.checked_add(object.ciphertext_length).ok_or_else(corrupt)?; }
    }
    Ok(ReachableUsage {
        snapshot_id: snapshot_id.into(),
        known_direct_objects: identities.len().try_into().map_err(|_| corrupt())?,
        known_direct_bytes: bytes,
        // Catalog payload packs require traversal; usage never downloads them.
        complete: false,
    })
}

pub(crate) fn summarize(root: &Path, connection_id: &str) -> Result<StorageUsage> {
    let store = ConnectionStore::open(root)?;
    let connection = store.read(connection_id)?;
    let lww_repository_id = super::lww_segment::format_repository_id(&connection.provider_repository_id);
    let mut receipts = super::lww_residency::usage_receipts(root, &connection)?;
    for (directory, repository_id) in [
        (root.join("external-storage").join("package-cache").join(connection_id), lww_repository_id.as_ref()),
        (super::runtime::connection_directory(root, connection_id).join("package-cache"), connection.descriptor.repository_id.as_str()),
    ] {
        receipts.extend(super::package_cache::cached_remote_objects(
            &directory, repository_id, &connection.descriptor_locator.connection_identity,
        )?.into_iter().map(|object| object.receipt));
    }
    store.remember_known_usage(connection_id, &receipts)?;
    store.storage_usage(connection_id)
}

pub(super) fn remember_snapshot(
    root: &Path,
    connection_id: &str,
    snapshot: &RemoteObject,
    document: &control::SnapshotView,
) {
    let result = (|| {
        let summary = direct_reachable(snapshot, document)?;
        ConnectionStore::open(root)?.remember_usage_snapshot(
            connection_id, &snapshot.receipt.locator, document.created_at_ms, &summary,
        )
    })();
    if let Err(error) = result {
        crate::nlog!("warn", "External storage usage snapshot: {}", error);
    }
}

pub(super) fn remember_completed_snapshot(
    root: &Path,
    connection_id: &str,
    completed: &super::packaging::CompletedSnapshot,
    created_at_ms: u64,
) {
    let result = (|| {
        let summary = direct_objects(
            &completed.snapshot_id,
            std::iter::once(&completed.reference.receipt)
                .chain([&completed.record_catalog.receipt, &completed.asset_catalog.receipt]),
            completed.sections.values().map(|section| &section.entries_root),
        )?;
        ConnectionStore::open(root)?.remember_usage_snapshot(
            connection_id, &completed.reference.receipt.locator, created_at_ms, &summary,
        )
    })();
    if let Err(error) = result {
        crate::nlog!("warn", "External storage usage snapshot: {}", error);
    }
}

pub(super) fn wrap(root: &Path, connection_id: &str, provider: std::sync::Arc<dyn Provider>) -> std::sync::Arc<dyn Provider> {
    std::sync::Arc::new(ObservedProvider {
        provider, root: root.to_owned(), connection_id: connection_id.to_owned(), store: std::sync::Mutex::new(None),
    })
}

struct ObservedProvider {
    provider: std::sync::Arc<dyn Provider>,
    root: std::path::PathBuf,
    connection_id: String,
    /// Opened on the first observation and kept for the provider's lifetime;
    /// a failed observation drops it so the next one opens the store again.
    store: std::sync::Mutex<Option<ConnectionStore>>,
}
impl ObservedProvider {
    fn observe(&self, repository: &RepositoryHandle, receipts: &[ObjectReceipt], removed: Option<&RemoteLocator>) {
        let mut store = self.store.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = (|| {
            if store.is_none() { *store = Some(ConnectionStore::open(&self.root)?); }
            store.as_ref().ok_or_else(|| ProviderError::new(ErrorKind::Transient))?
                .observe_usage(&self.connection_id, repository, receipts, removed)
        })();
        if result.is_err() { *store = None; }
        drop(store);
        if let Err(error) = result {
            // A display cache failure must not change the outcome of a remote write.
            crate::nlog!("warn", "External storage usage observation: {}", error);
        }
    }
    fn head(&self, repository: &RepositoryHandle, locator: &RemoteLocator, bytes: &HeadBytes, receipt: &HeadReceipt) {
        if receipt.complete {
            self.observe(repository, &[ObjectReceipt {
                locator: locator.clone(), byte_length: bytes.as_bytes().len() as u64,
                version: receipt.version.clone(), checksum: None, complete: true,
            }], None);
        }
    }
}
impl Provider for ObservedProvider {
    fn transfer_concurrency(&self) -> usize { self.provider.transfer_concurrency() }
    fn progress_stage(&self, stage: &'static str) { self.provider.progress_stage(stage) }
    fn preparation_progress(&self) -> std::sync::Arc<super::phase_progress::PhaseProgress> { self.provider.preparation_progress() }
    fn open_repository<'a>(&'a self, c: &'a ConnectionConfig, s: &'a SecretRef, m: OpenMode, x: &'a Cancellation) -> ProviderFuture<'a, (RepositoryHandle, Capabilities)> { self.provider.open_repository(c,s,m,x) }
    fn begin_upload<'a>(&'a self, r: &'a RepositoryHandle, i: &'a ObjectIntent, x: &'a Cancellation) -> ProviderFuture<'a, Option<ResumeState>> { self.provider.begin_upload(r,i,x) }
    fn read_object<'a>(&'a self, r: &'a RepositoryHandle, l: &'a RemoteLocator, v: Option<&'a VersionToken>, s: &'a mut dyn TransferSink, x: &'a Cancellation) -> ProviderFuture<'a, ReadReceipt> {
        Box::pin(async move {
            let result = self.provider.read_object(r,l,v,s,x).await;
            match &result {
                Ok(ReadReceipt::Body(receipt)) => self.observe(r, std::slice::from_ref(receipt), None),
                Err(error) if error.kind == ErrorKind::NotFound => self.observe(r, &[], Some(l)),
                _ => {}
            }
            result
        })
    }
    fn create_object<'a>(&'a self, r: &'a RepositoryHandle, i: &'a ObjectIntent, s: &'a dyn TransferSource, resume: Option<&'a ResumeState>, x: &'a Cancellation) -> ProviderFuture<'a, ObjectReceipt> {
        Box::pin(async move {
            let receipt = self.provider.create_object(r,i,s,resume,x).await?;
            self.observe(r, std::slice::from_ref(&receipt), None);
            Ok(receipt)
        })
    }
    fn compare_exchange_head<'a>(&'a self, r: &'a RepositoryHandle, l: &'a RemoteLocator, e: &'a ExpectedHead, h: &'a HeadBytes, x: &'a Cancellation) -> ProviderFuture<'a, HeadReceipt> {
        Box::pin(async move {
            let receipt = self.provider.compare_exchange_head(r,l,e,h,x).await?;
            self.head(r,l,h,&receipt);
            Ok(receipt)
        })
    }
    fn replace_head<'a>(&'a self, r: &'a RepositoryHandle, l: &'a RemoteLocator, h: &'a HeadBytes, x: &'a Cancellation) -> ProviderFuture<'a, HeadReceipt> {
        Box::pin(async move {
            let receipt = self.provider.replace_head(r,l,h,x).await?;
            self.head(r,l,h,&receipt);
            Ok(receipt)
        })
    }
    fn replace_descriptor<'a>(&'a self, r: &'a RepositoryHandle, l: &'a RemoteLocator, b: &'a HeadBytes, x: &'a Cancellation) -> ProviderFuture<'a, HeadReceipt> {
        Box::pin(async move {
            let receipt = self.provider.replace_descriptor(r,l,b,x).await?;
            self.head(r,l,b,&receipt);
            Ok(receipt)
        })
    }
    fn list_objects<'a>(&'a self, r: &'a RepositoryHandle, c: Collection, cursor: Option<&'a str>, limit: u16, x: &'a Cancellation) -> ProviderFuture<'a, ObjectPage> {
        Box::pin(async move {
            let page = self.provider.list_objects(r,c,cursor,limit,x).await?;
            if !page.objects.is_empty() { self.observe(r, &page.objects, None); }
            Ok(page)
        })
    }
    fn delete_object<'a>(&'a self, r: &'a RepositoryHandle, l: &'a RemoteLocator, x: &'a Cancellation) -> ProviderFuture<'a, ()> {
        Box::pin(async move {
            let result = self.provider.delete_object(r,l,x).await;
            if result.is_ok() || result.as_ref().is_err_and(|error| error.kind == ErrorKind::NotFound) {
                self.observe(r, &[], Some(l));
            }
            result
        })
    }
    fn supports_incomplete_cleanup(&self) -> bool { self.provider.supports_incomplete_cleanup() }
    fn cleanup_incomplete_objects<'a>(&'a self, r: &'a RepositoryHandle, j: &'a [String], cursor: Option<&'a str>, limit: usize, deleting: &'a (dyn Fn(bool) + Send + Sync), x: &'a Cancellation) -> ProviderFuture<'a, IncompleteCleanupPage> { self.provider.cleanup_incomplete_objects(r,j,cursor,limit,deleting,x) }
    fn delete_empty_container<'a>(&'a self, r: &'a RepositoryHandle, l: &'a RemoteLocator, j: &'a [String], x: &'a Cancellation) -> ProviderFuture<'a, ()> { self.provider.delete_empty_container(r,l,j,x) }
    fn reconcile_upload<'a>(&'a self, r: &'a RepositoryHandle, i: &'a ObjectIntent, s: Option<&'a ResumeState>, x: &'a Cancellation) -> ProviderFuture<'a, UploadResolution> {
        Box::pin(async move {
            let result = self.provider.reconcile_upload(r,i,s,x).await?;
            if let UploadResolution::Complete(receipt) = &result { self.observe(r, std::slice::from_ref(receipt), None); }
            Ok(result)
        })
    }
    fn lookup_metadata<'a>(&'a self, r: &'a RepositoryHandle, i: &'a ObjectIntent, l: Option<&'a RemoteLocator>, x: &'a Cancellation) -> ProviderFuture<'a, Option<ObjectReceipt>> {
        Box::pin(async move {
            let result = self.provider.lookup_metadata(r,i,l,x).await?;
            match &result {
                Some(receipt) => self.observe(r, std::slice::from_ref(receipt), None),
                None if l.is_some() => self.observe(r, &[], l),
                None => {}
            }
            Ok(result)
        })
    }
    fn head_locator(&self, r: &RepositoryHandle) -> Result<RemoteLocator> { self.provider.head_locator(r) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_storage::{
        contract::{ObjectReceipt, ObjectRole},
        fake,
    };

    fn object(
        repository: &super::super::contract::RepositoryHandle,
        id: &str,
        role: ObjectRole,
    ) -> RemoteObject {
        sized_object(repository, id, role, 1)
    }

    fn sized_object(repository: &RepositoryHandle, id: &str, role: ObjectRole, length: u64) -> RemoteObject {
        let mut locator = fake::locator();
        locator.object = id.into();
        let wire_role = super::super::packaging::wire_role(role).unwrap();
        let header = risunest_external_storage_format::snapshot::PublicObjectHeader::new(
            repository.repository_id.clone(),
            id.into(),
            wire_role,
            length,
        )
        .unwrap();
        let bytes = risunest_external_storage_format::snapshot::envelope_length(&header).unwrap();
        RemoteObject {
            repository_id: repository.repository_id.clone(),
            object_id: id.into(),
            role,
            receipt: ObjectReceipt {
                locator,
                byte_length: bytes,
                version: None,
                checksum: None,
                complete: true,
            },
            ciphertext_sha256: "11".repeat(32),
            plaintext_length: length,
            plaintext_sha256: "22".repeat(32),
        }
    }

    fn saved(root: &Path, id: &str) {
        super::super::transfer_factory_tests::save(
            root, id,
            risunest_external_storage_format::format::Descriptor::new(format!("format-{id}"), None).unwrap(),
            fake::locator(),
        );
    }

    fn totals(root: &Path, id: &str) -> (u64, u64) {
        let usage = summarize(root, id).unwrap();
        (usage.locally_uploaded_objects_lower_bound, usage.locally_uploaded_bytes_lower_bound)
    }

    #[test]
    fn usage_includes_existing_sync_backup_and_residency_files_without_retransferring() {
        use super::super::{lww_residency, package_cache::PackageCache, runtime};
        for repository_id in super::super::lww_tests::repository_id_cases() {
        let root = tempfile::tempdir().unwrap();
        saved(root.path(), "connection");
        let mut store = ConnectionStore::open(root.path()).unwrap();
        let mut stored = store.remove("connection").unwrap();
        stored.provider_repository_id = repository_id.clone();
        store.insert(&stored).unwrap();
        let mut repository = fake::repository();
        repository.repository_id = repository_id;
        let mut sync_repository = fake::repository();
        sync_repository.repository_id = super::super::lww_segment::format_repository_id(&repository.repository_id).into_owned();
        let sync_cache = PackageCache::open(&root.path().join("external-storage/package-cache/connection")).unwrap();
        let backup_cache = PackageCache::open(&runtime::connection_directory(root.path(), "connection").join("package-cache")).unwrap();
        let mut objects = (0..64).map(|index| sized_object(&sync_repository, &format!("pack-{index}"), ObjectRole::Pack, 16 * 1024 * 1024)).collect::<Vec<_>>();
        let catalog = object(&sync_repository, "asset-catalog", ObjectRole::Catalog);
        objects.push(catalog.clone());
        sync_cache.put_objects(&repository, &objects).unwrap();
        let mut format_repository = fake::repository();
        format_repository.repository_id = stored.descriptor.repository_id.clone();
        let backup = object(&format_repository, "backup-catalog", ObjectRole::Catalog);
        backup_cache.put_object(&repository, &backup).unwrap();
        // The same physical pack may be retained in both cache namespaces.
        let duplicate = sized_object(&format_repository, "pack-0", ObjectRole::Pack, 16 * 1024 * 1024);
        backup_cache.put_object(&repository, &duplicate).unwrap();
        format_repository.repository_id = "another-library".into();
        let foreign = object(&format_repository, "foreign", ObjectRole::Catalog);
        backup_cache.put_object(&repository, &foreign).unwrap();

        let registry_pack = sized_object(&sync_repository, "registry-pack", ObjectRole::Pack, 1024);
        let source = lww_residency::PackedSource {
            hash: "11".repeat(32), byte_length: 1, library_id: stored.descriptor.repository_id.clone(),
            connection_id: stored.id.clone(), connection_root: root.path().into(), protected_snapshot: "segment".into(),
            catalog: catalog.stored(&repository).unwrap(), chunks: Vec::new(),
            packs: vec![objects[0].stored(&repository).unwrap(), registry_pack.stored(&repository).unwrap()],
        };
        let mut second = source.clone(); second.hash = "22".repeat(32);
        lww_residency::register_verified_packed_many(root.path(), [&source, &second]).unwrap();
        let standalone = lww_residency::Source {
            hash: "33".repeat(32), library_id: stored.descriptor.repository_id.clone(), connection_id: stored.id.clone(),
            connection_root: root.path().into(), protected_segment: "segment".into(),
            body: super::super::lww_segment::LargeBody {
                object_id: "standalone".into(), sha256: "44".repeat(32), byte_length: (24u64 * 1024 * 1024).into(),
                plaintext_byte_length: 1u64.into(), locator: Some(RemoteLocator { object: "standalone".into(), ..fake::locator() }),
            },
        };
        lww_residency::register(root.path(), &standalone).unwrap();
        let mut foreign_source = standalone.clone();
        foreign_source.hash = "55".repeat(32); foreign_source.connection_id = "other-connection".into();
        foreign_source.body.locator.as_mut().unwrap().object = "foreign-standalone".into();
        lww_residency::register(root.path(), &foreign_source).unwrap();
        let observed = (0..7).map(|index| ObjectReceipt {
            locator: RemoteLocator { object: format!("observed-{index}"), ..fake::locator() },
            byte_length: 4 * 1024 * 1024, version: None, checksum: None, complete: true,
        }).collect::<Vec<_>>();
        store.observe_usage("connection", &repository, &observed, None).unwrap();
        let before = store.storage_usage("connection").unwrap();
        assert_eq!((before.locally_uploaded_objects_lower_bound, before.locally_uploaded_bytes_lower_bound), (7, 28 * 1024 * 1024));
        let expected_bytes = objects.iter().map(|object| object.receipt.byte_length).sum::<u64>()
            + backup.receipt.byte_length + registry_pack.receipt.byte_length + standalone.body.byte_length.0 + 28 * 1024 * 1024;
        assert!(expected_bytes > 1024 * 1024 * 1024);
        assert_eq!(totals(root.path(), "connection"), (75, expected_bytes));
        let observed_at = summarize(root.path(), "connection").unwrap().observed_at_ms;
        assert_eq!(totals(root.path(), "connection"), (75, expected_bytes));
        assert_eq!(summarize(root.path(), "connection").unwrap().observed_at_ms, observed_at);

        // Older local references must not resurrect a confirmed deletion.
        store.observe_usage("connection", &repository, &[], Some(&objects[0].receipt.locator)).unwrap();
        assert_eq!(totals(root.path(), "connection"), (74, expected_bytes - objects[0].receipt.byte_length));
        store.observe_usage("connection", &repository, &[objects[0].receipt.clone()], None).unwrap();
        assert_eq!(totals(root.path(), "connection"), (75, expected_bytes));
        }
    }

    #[test]
    fn cached_files_are_visible_before_a_new_sync_and_newer_observations_win() {
        let root = tempfile::tempdir().unwrap();
        saved(root.path(), "connection");
        let repository = fake::repository();
        let cache = super::super::package_cache::PackageCache::open(&root.path().join("external-storage/package-cache/connection")).unwrap();
        let original = object(&repository, "cached", ObjectRole::Catalog);
        cache.put_object(&repository, &original).unwrap();
        let before = summarize(root.path(), "connection").unwrap();
        assert_eq!(before.locally_uploaded_objects_lower_bound, 1);
        assert_eq!(before.locally_uploaded_bytes_lower_bound, original.receipt.byte_length);
        assert_eq!(before.observed_at_ms, Some(1));
        let store = ConnectionStore::open(root.path()).unwrap();
        let mut updated = original.receipt.clone();
        updated.byte_length += 100;
        store.observe_usage("connection", &repository, &[updated.clone()], None).unwrap();
        assert_eq!(totals(root.path(), "connection"), (1, updated.byte_length));

        let removed = object(&repository, "removed-before-query", ObjectRole::Catalog);
        cache.put_object(&repository, &removed).unwrap();
        store.observe_usage("connection", &repository, &[], Some(&removed.receipt.locator)).unwrap();
        assert_eq!(totals(root.path(), "connection"), (1, updated.byte_length));
        // Discarding a reuse cache is not evidence that its remote files were deleted.
        cache.forget_object(&original.repository_id, &repository, &original.object_id).unwrap();
        assert_eq!(totals(root.path(), "connection"), (1, updated.byte_length));
    }

    #[test]
    fn usage_observations_are_durable_scoped_and_deduplicated_by_remote_file() {
        let root = tempfile::tempdir().unwrap();
        saved(root.path(), "first");
        // Different format ids must not affect the provider receipt scope.
        let mut second = ConnectionStore::open(root.path()).unwrap().read("first").unwrap();
        second.id = "second".into();
        second.descriptor = risunest_external_storage_format::format::Descriptor::new("second-format".into(), None).unwrap();
        ConnectionStore::open(root.path()).unwrap().insert(&second).unwrap();
        assert_eq!(summarize(root.path(), "first").unwrap().observed_at_ms, None);
        let repository = fake::repository();
        let mut receipt = ObjectReceipt { locator: fake::locator(), byte_length: 50, version: None, checksum: None, complete: false };
        let store = ConnectionStore::open(root.path()).unwrap();
        store.observe_usage("first", &repository, &[receipt.clone()], None).unwrap();
        assert_eq!(summarize(root.path(), "first").unwrap().observed_at_ms, None);
        receipt.complete = true;
        store.observe_usage("first", &repository, &[receipt.clone(), receipt.clone()], None).unwrap();
        receipt.byte_length = 70;
        store.observe_usage("first", &repository, &[receipt.clone()], None).unwrap();
        let mut other_collection = receipt.clone();
        other_collection.locator.collection = Some("another-release".into());
        store.observe_usage("first", &repository, &[other_collection], None).unwrap();
        store.observe_usage("second", &repository, &[receipt.clone()], None).unwrap();
        assert_eq!(totals(root.path(), "first"), (2, 140));
        assert_eq!(totals(root.path(), "second"), (1, 70));
        let mut invalid = receipt.clone();
        invalid.locator.connection_identity = "foreign".into();
        assert!(store.observe_usage("first", &repository, &[receipt.clone(), invalid], None).is_err());
        assert_eq!(totals(root.path(), "first"), (2, 140));
        store.observe_usage("first", &repository, &[], Some(&receipt.locator)).unwrap();
        store.observe_usage("first", &repository, &[], Some(&receipt.locator)).unwrap();
        assert_eq!(totals(root.path(), "first"), (1, 70));
        assert_eq!(totals(root.path(), "second"), (1, 70));
        ConnectionStore::open(root.path()).unwrap().remove("second").unwrap();
        saved(root.path(), "second");
        assert_eq!(summarize(root.path(), "second").unwrap().observed_at_ms, None);
    }

    #[test]
    fn usage_tracks_uploads_discovery_reads_and_confirmed_deletions_without_extra_requests() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let root = tempfile::tempdir().unwrap();
            saved(root.path(), "connection");
            let raw = std::sync::Arc::new(fake::FakeProvider::new(true));
            let provider = wrap(root.path(), "connection", raw.clone());
            let repository = fake::repository();
            let cancel = Cancellation::default();
            let path = root.path().join("synthetic-upload");
            std::fs::write(&path, b"1234567").unwrap();
            let source = super::super::transfer::SpoolSource::verified(&path, 7, &risunest_sync_wire::hash(b"1234567")).unwrap();
            let intent = ObjectIntent { repository_id: repository.repository_id.clone(), job_id: "job".into(), object_id: "pack".into(), role: ObjectRole::Pack, byte_length: 7, sha256: risunest_sync_wire::hash(b"1234567") };
            let receipt = provider.create_object(&repository, &intent, &source, None, &cancel).await.unwrap();
            provider.create_object(&repository, &intent, &source, None, &cancel).await.unwrap();
            provider.reconcile_upload(&repository, &intent, None, &cancel).await.unwrap();
            assert_eq!(totals(root.path(), "connection"), (1, 7));
            raw.seed("segment", ObjectRole::Segment, vec![1; 11]);
            raw.seed("backup", ObjectRole::BackupBundle, vec![2; 13]);
            let requests = raw.request_count();
            provider.list_objects(&repository, Collection::Segments, None, 100, &cancel).await.unwrap();
            provider.list_objects(&repository, Collection::Snapshots, None, 100, &cancel).await.unwrap();
            assert_eq!(raw.request_count() - requests, 2);
            assert_eq!(totals(root.path(), "connection"), (3, 31));
            let mut sink = super::super::transfer::SpoolSink::create(&root.path().join("read"), 20).unwrap();
            provider.read_object(&repository, &receipt.locator, None, &mut sink, &cancel).await.unwrap();
            assert_eq!(totals(root.path(), "connection"), (3, 31));
            raw.fail_delete("pack", fake::DeleteFault::Transient);
            assert!(provider.delete_object(&repository, &receipt.locator, &cancel).await.is_err());
            assert_eq!(totals(root.path(), "connection"), (3, 31));
            raw.fail_delete("pack", fake::DeleteFault::AppliedThenLost);
            assert!(provider.delete_object(&repository, &receipt.locator, &cancel).await.is_err());
            assert_eq!(totals(root.path(), "connection"), (3, 31));
            assert!(provider.lookup_metadata(&repository, &intent, Some(&receipt.locator), &cancel).await.unwrap().is_none());
            assert_eq!(totals(root.path(), "connection"), (2, 24));
            let segment = RemoteLocator { object: "segment".into(), ..fake::locator() };
            provider.delete_object(&repository, &segment, &cancel).await.unwrap();
            assert_eq!(totals(root.path(), "connection"), (1, 13));
            let head = provider.head_locator(&repository).unwrap();
            provider.replace_head(&repository, &head, &HeadBytes::new(vec![3; 5]).unwrap(), &cancel).await.unwrap();
            provider.replace_head(&repository, &head, &HeadBytes::new(vec![3; 9]).unwrap(), &cancel).await.unwrap();
            assert_eq!(totals(root.path(), "connection"), (2, 22));
            let requests = raw.request_count();
            summarize(root.path(), "connection").unwrap();
            assert_eq!(raw.request_count(), requests);
        });
    }

    #[test]
    fn a_wrapped_provider_opens_its_usage_store_once_and_again_after_a_failure() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let root = tempfile::tempdir().unwrap();
            saved(root.path(), "connection");
            let raw = std::sync::Arc::new(fake::FakeProvider::new(true));
            let provider = wrap(root.path(), "connection", raw.clone());
            let repository = fake::repository();
            let cancel = Cancellation::default();
            for index in 0..4 { raw.seed(&format!("segment-{index}"), ObjectRole::Segment, vec![1; 10]); }
            let opened = super::super::connection_store::opens(root.path());
            for _ in 0..6 { provider.list_objects(&repository, Collection::Segments, None, 100, &cancel).await.unwrap(); }
            assert_eq!(super::super::connection_store::opens(root.path()) - opened, 1);
            let mut store = ConnectionStore::open(root.path()).unwrap();
            let stored = store.remove("connection").unwrap();
            provider.list_objects(&repository, Collection::Segments, None, 100, &cancel).await.unwrap();
            store.insert(&stored).unwrap();
            let opened = super::super::connection_store::opens(root.path());
            raw.seed("segment-4", ObjectRole::Segment, vec![1; 10]);
            provider.list_objects(&repository, Collection::Segments, None, 100, &cancel).await.unwrap();
            assert_eq!(super::super::connection_store::opens(root.path()) - opened, 1);
            assert_eq!(totals(root.path(), "connection").0, 5);
        });
    }

    #[test]
    fn latest_known_backup_uses_observed_bundles_and_survives_an_offline_query() {
        let root = tempfile::tempdir().unwrap();
        saved(root.path(), "connection");
        let repository = fake::repository();
        let store = ConnectionStore::open(root.path()).unwrap();
        for (id, at, length) in [("new", 200, 30), ("old", 100, 20)] {
            let receipt = ObjectReceipt { locator: RemoteLocator { object: id.into(), ..fake::locator() }, byte_length: length, version: None, checksum: None, complete: true };
            store.observe_usage("connection", &repository, &[receipt.clone()], None).unwrap();
            store.remember_usage_snapshot("connection", &receipt.locator, at, &ReachableUsage { snapshot_id: id.into(), known_direct_objects: 3, known_direct_bytes: length + 100, complete: false }).unwrap();
        }
        assert_eq!(summarize(root.path(), "connection").unwrap().latest_reachable.unwrap().snapshot_id, "new");
        store.observe_usage("connection", &repository, &[], Some(&RemoteLocator { object: "new".into(), ..fake::locator() })).unwrap();
        assert_eq!(summarize(root.path(), "connection").unwrap().latest_reachable.unwrap().snapshot_id, "old");
        assert_eq!(totals(root.path(), "connection"), (1, 20));
    }

    /// Invariant 33. Every section the head still names counts as reachable,
    /// including one a library-only publication merely carried forward.
    #[test]
    fn direct_reachable_is_an_explicit_incomplete_lower_bound() {
        use risunest_external_storage_format::{
            section::{SectionKind, SECTION_CODEC},
            snapshot::SectionSnapshotRef,
        };
        let repository = fake::repository();
        let snapshot = object(&repository, "snapshot-root", ObjectRole::SyncState);
        let records_object = object(&repository, "records", ObjectRole::Catalog);
        let assets_object = object(&repository, "assets", ObjectRole::Catalog);
        let section_object = object(&repository, "section-hypa", ObjectRole::Catalog);
        let records = records_object.stored(&repository).unwrap();
        let assets = assets_object.stored(&repository).unwrap();
        let document = control::SnapshotView {
            original_units: None,
            snapshot_id: "snapshot".into(),
            parent_snapshot_id: None,
            library_id: "library".into(),
            created_at_ms: 1,
            revision: "1".into(),
            captured_by_device: None,
            library: risunest_external_storage_format::snapshot::LibrarySnapshotRef {
                record_catalog: records,
                asset_catalog: assets,
                content_fingerprint: [2; 32],
            },
            sections: std::collections::BTreeMap::from([(
                SectionKind::Hypa.id().to_owned(),
                SectionSnapshotRef {
                    kind: SectionKind::Hypa,
                    codec: SECTION_CODEC.into(),
                    generation: risunest_sync_wire::head::Sequence::from(3u64),
                    gc_floor: risunest_sync_wire::head::Sequence::from(0u64),
                    max_write_clock: risunest_sync_wire::head::Sequence::from(9u64),
                    entries_root: section_object.stored(&repository).unwrap(),
                    content_fingerprint: [4; 32],
                },
            )]),
            is_state: true,
        };
        let usage = direct_reachable(&snapshot, &document).unwrap();
        assert!(!usage.complete);
        assert_eq!(usage.known_direct_objects, 4);
        assert_eq!(
            usage.known_direct_bytes,
            snapshot.receipt.byte_length
                + records_object.receipt.byte_length
                + assets_object.receipt.byte_length
                + section_object.receipt.byte_length
        );
    }
}
