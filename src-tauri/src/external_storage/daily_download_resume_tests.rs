use super::{
    capabilities::Capabilities,
    capture::CaptureCatalog,
    contract::*,
    durable_quota::MyboxBudget,
    fake::{self, FakeProvider, FixedClock},
    http::Clock,
    journal::{JobIdentity, TransferJournal},
    packaging::{self, CompletedSnapshot, PackageLimits, SnapshotMetadata},
    quota::AccountKey,
    quota_profiles::{MyboxCharge, MyboxCounter, MyboxPlan},
    snapshot_restore,
};
use crate::persistent_store::{
    content_capture::ContentCaptureSink, external_capture::CapturedSnapshot,
    sync_selection::CaptureIdentity,
};
use risunest_external_storage_format::{
    content_identity::{hash, hash_reader},
    pack::ENTRY_OVERHEAD,
    snapshot as wire,
};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

const ACCOUNT: &str = "synthetic-account";
const FIRST_RESET_MS: u64 = 54_000_000;
const PACKS: usize = 520;
const INTERRUPT_AFTER: u64 = 300;

struct QuotaProvider {
    inner: Arc<FakeProvider>,
    budget: MyboxBudget,
    account: AccountKey,
    clock: Arc<FixedClock>,
    pack_reads: AtomicU64,
    interrupt_after: Option<u64>,
    interrupt: Cancellation,
}

impl QuotaProvider {
    fn new(
        inner: Arc<FakeProvider>,
        budget: MyboxBudget,
        clock: Arc<FixedClock>,
        interrupt_after: Option<u64>,
        interrupt: Cancellation,
    ) -> Self {
        Self {
            inner,
            budget,
            account: AccountKey::new(
                "mybox",
                &url::Url::parse("https://synthetic.invalid").unwrap(),
                ACCOUNT,
            )
            .unwrap(),
            clock,
            pack_reads: AtomicU64::new(0),
            interrupt_after,
            interrupt,
        }
    }

    fn pack_reads(&self) -> u64 {
        self.pack_reads.load(Ordering::SeqCst)
    }
}

fn download_charge() -> MyboxCharge {
    MyboxCharge {
        plan: MyboxPlan::Plan30gb,
        counters: vec![MyboxCounter::DownloadDay],
    }
}

impl Provider for QuotaProvider {
    fn open_repository<'a>(
        &'a self,
        config: &'a ConnectionConfig,
        secret: &'a SecretRef,
        mode: OpenMode,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, (RepositoryHandle, Capabilities)> {
        self.inner.open_repository(config, secret, mode, cancel)
    }

    fn read_object<'a>(
        &'a self,
        repository: &'a RepositoryHandle,
        locator: &'a RemoteLocator,
        unchanged: Option<&'a VersionToken>,
        sink: &'a mut dyn TransferSink,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, ReadReceipt> {
        Box::pin(async move {
            let pack = locator.object.starts_with("pack-");
            if pack {
                self.budget
                    .reserve(&self.account, &download_charge(), self.clock.now_ms())?;
            }
            let receipt = self
                .inner
                .read_object(repository, locator, unchanged, sink, cancel)
                .await?;
            if pack {
                let completed = self.pack_reads.fetch_add(1, Ordering::SeqCst) + 1;
                if self.interrupt_after == Some(completed) {
                    self.interrupt.cancel();
                }
            }
            Ok(receipt)
        })
    }

    fn begin_upload<'a>(
        &'a self,
        repository: &'a RepositoryHandle,
        intent: &'a ObjectIntent,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, Option<ResumeState>> {
        self.inner.begin_upload(repository, intent, cancel)
    }

    fn create_object<'a>(
        &'a self,
        repository: &'a RepositoryHandle,
        intent: &'a ObjectIntent,
        source: &'a dyn TransferSource,
        resume: Option<&'a ResumeState>,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, ObjectReceipt> {
        self.inner
            .create_object(repository, intent, source, resume, cancel)
    }

    fn compare_exchange_head<'a>(
        &'a self,
        repository: &'a RepositoryHandle,
        locator: &'a RemoteLocator,
        expected: &'a ExpectedHead,
        head: &'a HeadBytes,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, HeadReceipt> {
        self.inner
            .compare_exchange_head(repository, locator, expected, head, cancel)
    }

    fn replace_head<'a>(
        &'a self,
        repository: &'a RepositoryHandle,
        locator: &'a RemoteLocator,
        head: &'a HeadBytes,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, HeadReceipt> {
        self.inner.replace_head(repository, locator, head, cancel)
    }

    fn delete_object<'a>(
        &'a self,
        repository: &'a RepositoryHandle,
        locator: &'a RemoteLocator,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, ()> {
        self.inner.delete_object(repository, locator, cancel)
    }

    fn list_objects<'a>(
        &'a self,
        repository: &'a RepositoryHandle,
        collection: Collection,
        cursor: Option<&'a str>,
        limit: u16,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, ObjectPage> {
        self.inner
            .list_objects(repository, collection, cursor, limit, cancel)
    }

    fn reconcile_upload<'a>(
        &'a self,
        repository: &'a RepositoryHandle,
        intent: &'a ObjectIntent,
        resume: Option<&'a ResumeState>,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, UploadResolution> {
        self.inner
            .reconcile_upload(repository, intent, resume, cancel)
    }

    fn head_locator(&self, repository: &RepositoryHandle) -> Result<RemoteLocator> {
        self.inner.head_locator(repository)
    }

}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn pack_plaintext_limit(limits: PackageLimits) -> u64 {
    let available = limits.max_stored_bytes - limits.sdk_overhead_bytes;
    let mut low = 0;
    let mut high = available;
    while low < high {
        let middle = low + (high - low + 1) / 2;
        let header = wire::PublicObjectHeader::new(
            "format-repository".into(),
            format!("pack-{}", "0".repeat(64)),
            wire::ObjectRole::Pack,
            middle,
        )
        .unwrap();
        if wire::envelope_length(&header).unwrap() <= available {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    low
}

fn incompressible_bytes(length: usize) -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

fn captured(root: &Path, bytes: &[u8]) -> CapturedSnapshot {
    let directory = root
        .join("external-storage")
        .join("captures")
        .join("capture-daily-resume");
    let external = root.join("external-storage");
    let mut catalog = CaptureCatalog::create(&directory, &external, None).unwrap();
    let identity = CaptureIdentity {
        store_id: "device".into(),
        library_epoch: "library".into(),
        generation: "generation".into(),
        selection_epoch: "selection".into(),
        revision: 10,
    };
    catalog.begin(&identity, None).unwrap();
    catalog.record("root", bytes).unwrap();
    catalog.finish().unwrap();
    CapturedSnapshot {
        id: "capture-daily-resume".into(),
        identity,
        catalog,
        projected_records: 1,
        shared: false,
    }
}

fn metadata(capture: &CapturedSnapshot) -> SnapshotMetadata {
    SnapshotMetadata {
        snapshot_id: "latest".into(),
        repository_id: "format-repository".into(),
        library_id: capture.identity.library_epoch.clone(),
        author_device_id: capture.identity.store_id.clone(),
        created_at_ms: 10,
        logical_revision: 10,
        purpose: crate::external_storage::packaging::SnapshotPurpose::SyncState {
            epoch: "epoch".into(),
            generation: risunest_sync_wire::head::Sequence::from(1u64),
            parent_sections: std::collections::BTreeMap::new(),
        },
        parent_snapshot_id: None,
        content_fingerprint: capture.catalog.content_fingerprint(&risunest_external_storage_format::format::library_fingerprint_domain()).unwrap(),
    }
}

fn journal(root: &Path, capture: &CapturedSnapshot) -> TransferJournal {
    TransferJournal::open(
        root,
        JobIdentity {
            job_id: "publish-latest".into(),
            connection_id: "connection".into(),
            repository_id: fake::repository().repository_id,
            capture_id: capture.id.clone(),
            capture: capture.identity.clone(),
        },
    )
    .unwrap()
}

fn budget(path: &Path) -> MyboxBudget {
    MyboxBudget::new(path.to_path_buf())
}

fn used(path: &Path, now_ms: u64) -> u64 {
    MyboxBudget::new(path.to_path_buf())
        .snapshot(
            &AccountKey::new(
                "mybox",
                &url::Url::parse("https://synthetic.invalid").unwrap(),
                ACCOUNT,
            )
            .unwrap(),
            MyboxPlan::Plan30gb,
            now_ms,
        )
        .unwrap()
        .into_iter()
        .find(|counter| counter.id == MyboxCounter::DownloadDay.id())
        .unwrap()
        .used
}

/// Packs an interrupted download placed and will not read again.
fn placed_packs(staging: &Path) -> usize {
    std::fs::read_dir(staging.join("turnover")).map_or(0, |entries| entries.count())
}

/// One record spread over `PACKS` packs, published once.
struct Published {
    completed: CompletedSnapshot,
    provider: Arc<FakeProvider>,
    repository: RepositoryHandle,
    root_key: [u8; 32],
    source: Vec<u8>,
}

async fn published(temp: &Path) -> Published {
    let repository_root = temp.join("repository");
    let cache_root = temp.join("package-cache");
    let transfer_root = temp.join("transfer");
    let limits = PackageLimits {
        max_stored_bytes: 8 * 1024,
        sdk_overhead_bytes: 0,
        target_plaintext_bytes: 8 * 1024,
        maintenance: Default::default(),
    };
    let chunk_bytes = pack_plaintext_limit(limits) - ENTRY_OVERHEAD - 1;
    let source = incompressible_bytes(chunk_bytes as usize * PACKS);
    let capture = captured(&repository_root, &source);
    let metadata = metadata(&capture);
    let mut journal = journal(&transfer_root, &capture);
    let provider = Arc::new(FakeProvider::new(true));
    let repository = fake::repository();
    let root_key = [7; 32];
    let completed = packaging::package_and_upload(
        capture,
        Vec::new(),
        &repository_root,
        &cache_root,
        metadata,
        &root_key,
        limits,
        None,
        &mut journal,
        provider.as_ref(),
        &repository,
        &crate::external_storage::phase_progress::PhaseProgress::silent(),
        &Cancellation::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        completed
            .referenced_objects
            .iter()
            .filter(|object| object.role == ObjectRole::Pack)
            .count(),
        PACKS
    );
    Published {
        completed,
        provider,
        repository,
        root_key,
        source,
    }
}

#[test]
fn daily_download_resume_preserves_placed_packs_and_durable_budget() {
    runtime().block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let staging_root = temp.path().join("restore-stage");
        let quota_path = temp.path().join("quota.sqlite");
        let Published {
            completed,
            provider,
            repository,
            root_key,
            source,
        } = published(temp.path()).await;
        let source_hash = hex::encode(hash(&source));
        snapshot_restore::reset_test_turnover(&staging_root);

        let first_cancel = Cancellation::default();
        let first_clock = Arc::new(FixedClock::at(1));
        let first = QuotaProvider::new(
            provider.clone(),
            budget(&quota_path),
            first_clock,
            Some(INTERRUPT_AFTER),
            first_cancel.clone(),
        );
        let error = snapshot_restore::download_snapshot(
            &completed.reference,
            &staging_root,
            &root_key,
            None,
            snapshot_restore::SourceTrust::Downloaded, &first,
            &repository,
            &crate::external_storage::phase_progress::PhaseProgress::silent(),
            &first_cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(first.pack_reads(), INTERRUPT_AFTER);
        assert_eq!(placed_packs(&staging_root), 300);
        assert_eq!(
            used(&quota_path, 1),
            300
        );

        let second_cancel = Cancellation::default();
        let second = QuotaProvider::new(
            provider.clone(),
            budget(&quota_path),
            Arc::new(FixedClock::at(2)),
            None,
            second_cancel.clone(),
        );
        let error = snapshot_restore::download_snapshot(
            &completed.reference,
            &staging_root,
            &root_key,
            None,
            snapshot_restore::SourceTrust::Downloaded, &second,
            &repository,
            &crate::external_storage::phase_progress::PhaseProgress::silent(),
            &second_cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::DailyQuotaExhausted);
        assert_eq!(second.pack_reads(), 200);
        assert_eq!(placed_packs(&staging_root), 500);
        assert_eq!(
            used(&quota_path, 2),
            500
        );

        let third_cancel = Cancellation::default();
        let third = QuotaProvider::new(
            provider,
            budget(&quota_path),
            Arc::new(FixedClock::at(FIRST_RESET_MS)),
            None,
            third_cancel.clone(),
        );
        let prepared = snapshot_restore::download_snapshot(
            &completed.reference,
            &staging_root,
            &root_key,
            None,
            snapshot_restore::SourceTrust::Downloaded, &third,
            &repository,
            &crate::external_storage::phase_progress::PhaseProgress::silent(),
            &third_cancel,
        )
        .await
        .unwrap();
        assert_eq!(third.pack_reads(), 20);
        // Nothing is left to resume from, and no pack was ever held beside
        // another one.
        assert_eq!(placed_packs(&staging_root), 0);
        assert!(!staging_root.join("assembly").exists());
        assert_eq!(
            snapshot_restore::take_test_turnover(&staging_root),
            snapshot_restore::TestTurnover { ciphertexts: 1, plaintexts: 1 }
        );
        assert_eq!(prepared.snapshot_id, "latest");
        assert_eq!(prepared.records.len(), 1);
        assert_eq!(prepared.records[0].content_hash, source_hash);
        assert_eq!(prepared.records[0].byte_length, source.len() as u64);
        let restored = prepared.record_body(0);
        assert_eq!(
            hex::encode(hash_reader(&mut restored.as_slice(), source.len() as u64).unwrap()),
            source_hash
        );
        assert_eq!(
            used(&quota_path, FIRST_RESET_MS),
            20
        );
    });
}

async fn download(
    publication: &Published,
    staging_root: &Path,
    provider: &dyn Provider,
    cancel: &Cancellation,
) -> Result<snapshot_restore::PreparedRemoteSnapshot> {
    snapshot_restore::download_snapshot(
        &publication.completed.reference,
        staging_root,
        &publication.root_key,
        None,
        snapshot_restore::SourceTrust::Downloaded,
        provider,
        &publication.repository,
        &crate::external_storage::phase_progress::PhaseProgress::silent(),
        cancel,
    )
    .await
}

/// An assembly file whose packs are all marked but whose bytes are not the
/// entry any more fails the download, and takes the markers of exactly its
/// packs with it, so the retry reads those packs again and succeeds.
#[test]
fn a_damaged_assembly_is_read_again_from_its_packs() {
    runtime().block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let staging_root = temp.path().join("restore-stage");
        let publication = published(temp.path()).await;
        let provider = publication.provider.clone();
        let cancel = Cancellation::default();
        let interrupted = QuotaProvider::new(
            provider.clone(),
            budget(&temp.path().join("quota.sqlite")),
            Arc::new(FixedClock::at(1)),
            Some(INTERRUPT_AFTER),
            cancel.clone(),
        );
        let download = |provider, cancel| download(&publication, &staging_root, provider, cancel);
        assert_eq!(download(&interrupted, &cancel).await.unwrap_err().kind, ErrorKind::Cancelled);
        assert_eq!(placed_packs(&staging_root), INTERRUPT_AFTER as usize);
        let assembly = std::fs::read_dir(staging_root.join("assembly")).unwrap()
            .next().unwrap().unwrap().path();
        let mut bytes = std::fs::read(&assembly).unwrap();
        bytes[0] ^= 0xff;
        std::fs::write(&assembly, &bytes).unwrap();

        let cancel = Cancellation::default();
        snapshot_restore::reset_test_read_counts(&staging_root);
        assert_eq!(download(provider.as_ref(), &cancel).await.unwrap_err().kind, ErrorKind::Transient);
        assert_eq!(snapshot_restore::take_test_read_counts(&staging_root).packs,
            PACKS as u64 - INTERRUPT_AFTER);
        assert_eq!(placed_packs(&staging_root), 0);
        assert!(!assembly.exists());

        snapshot_restore::reset_test_read_counts(&staging_root);
        let prepared = download(provider.as_ref(), &cancel).await.unwrap();
        assert_eq!(snapshot_restore::take_test_read_counts(&staging_root).packs, PACKS as u64);
        assert_eq!(prepared.records[0].content_hash, hex::encode(hash(&publication.source)));
        assert_eq!(prepared.record_body(0), publication.source);
    });
}
