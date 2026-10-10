use super::*;
use super::tests::{identity, prepare_wave, runtime};
use crate::external_storage::fake::{self, FakeProvider};

pub(crate) struct GateProvider {
    pub(crate) inner: std::sync::Arc<FakeProvider>,
    pub(crate) minimum_bytes: u64,
    entered: tokio::sync::mpsc::UnboundedSender<String>,
    gates: std::sync::Mutex<std::collections::BTreeMap<String, std::sync::Arc<tokio::sync::Semaphore>>>,
    pub(crate) fail: std::sync::Mutex<Option<String>>,
    fail_kind: ErrorKind,
}
impl GateProvider {
    pub(crate) fn new() -> (Self, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let (entered, receiver) = tokio::sync::mpsc::unbounded_channel();
        (Self { inner: std::sync::Arc::new(FakeProvider::new(false)), minimum_bytes: 0, entered, gates: Default::default(), fail: Default::default(), fail_kind: ErrorKind::RateLimited }, receiver)
    }
    pub(crate) fn release(&self, object: &str) {
        self.gates.lock().unwrap().get(object).unwrap().add_permits(1);
    }
}
        impl Provider for GateProvider {
            fn open_repository<'a>(
                &'a self,
                config: &'a ConnectionConfig,
                secret: &'a SecretRef,
                mode: OpenMode,
                cancel: &'a Cancellation,
            ) -> ProviderFuture<'a, (RepositoryHandle, crate::external_storage::capabilities::Capabilities)> {
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
                self.inner.read_object(repository, locator, unchanged, sink, cancel)
            }
            fn begin_upload<'a>(
                &'a self,
                repository: &'a RepositoryHandle,
                intent: &'a ObjectIntent,
                cancel: &'a Cancellation,
            ) -> ProviderFuture<'a, Option<ResumeState>> {
                self.inner.begin_upload(repository, intent, cancel)
            }
            fn transfer_concurrency(&self) -> usize { 4 }
            fn create_object<'a>(&'a self, repository: &'a RepositoryHandle, intent: &'a ObjectIntent,
                source: &'a dyn TransferSource, resume: Option<&'a ResumeState>, cancel: &'a Cancellation,
            ) -> ProviderFuture<'a, ObjectReceipt> {
                Box::pin(async move {
                    let receipt = self.inner.create_object(repository, intent, source, resume, cancel).await?;
                    if intent.role == ObjectRole::Pack && intent.byte_length >= self.minimum_bytes {
                        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
                        self.gates.lock().unwrap().insert(intent.object_id.clone(), gate.clone());
                        self.entered.send(intent.object_id.clone()).unwrap();
                        tokio::select! {
                            permit = gate.acquire() => permit.unwrap().forget(),
                            _ = cancel.cancelled() => return Err(ProviderError::new(ErrorKind::Cancelled)),
                        }
                        if self.fail.lock().unwrap().as_deref() == Some(&intent.object_id) {
                            return Err(ProviderError::new(self.fail_kind));
                        }
                    }
                    Ok(receipt)
                })
            }
            fn compare_exchange_head<'a>(
                &'a self,
                repository: &'a RepositoryHandle,
                locator: &'a RemoteLocator,
                expected: &'a ExpectedHead,
                head: &'a HeadBytes,
                cancel: &'a Cancellation,
            ) -> ProviderFuture<'a, HeadReceipt> {
                self.inner.compare_exchange_head(repository, locator, expected, head, cancel)
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
            fn list_objects<'a>(
                &'a self,
                repository: &'a RepositoryHandle,
                collection: Collection,
                cursor: Option<&'a str>,
                limit: u16,
                cancel: &'a Cancellation,
            ) -> ProviderFuture<'a, ObjectPage> {
                self.inner.list_objects(repository, collection, cursor, limit, cancel)
            }
            fn delete_object<'a>(
                &'a self,
                repository: &'a RepositoryHandle,
                locator: &'a RemoteLocator,
                cancel: &'a Cancellation,
            ) -> ProviderFuture<'a, ()> {
                self.inner.delete_object(repository, locator, cancel)
            }
            fn reconcile_upload<'a>(&'a self, repository: &'a RepositoryHandle, intent: &'a ObjectIntent,
                resume: Option<&'a ResumeState>, cancel: &'a Cancellation,
            ) -> ProviderFuture<'a, UploadResolution> {
                self.inner.reconcile_upload(repository, intent, resume, cancel)
            }
            fn lookup_metadata<'a>(
                &'a self,
                repository: &'a RepositoryHandle,
                intent: &'a ObjectIntent,
                known: Option<&'a RemoteLocator>,
                cancel: &'a Cancellation,
            ) -> ProviderFuture<'a, Option<ObjectReceipt>> {
                self.inner.lookup_metadata(repository, intent, known, cancel)
            }
            fn head_locator(&self, repository: &RepositoryHandle) -> Result<RemoteLocator> {
                self.inner.head_locator(repository)
            }
        }
#[test]
fn concurrent_wave_refills_and_settles_siblings_before_returning_error() {
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let (mut journal, members) = prepare_wave(root.path(), 8);
        let (provider, mut entered) = GateProvider::new();
        let repository = fake::repository();
        let cancel = Cancellation::default();
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            tokio::join!(
                upload_wave(&mut journal, &members, "format-repository", &[8; 32], &provider, &repository, &cancel),
                async {
                    let mut first = Vec::new();
                    for _ in 0..4 { first.push(entered.recv().await.unwrap()); }
                    assert!(entered.try_recv().is_err());
                    provider.release(&first[2]);
                    let refill = entered.recv().await.unwrap();
                    assert_eq!(refill, members[4].object_id);
                    *provider.fail.lock().unwrap() = Some(first[0].clone());
                    provider.release(&first[0]);
                    // Let the owner observe the stopping error before siblings return.
                    tokio::task::yield_now().await;
                    provider.release(&first[1]);
                    provider.release(&first[3]);
                    provider.release(&refill);
                }
            )
        }).await.expect("body futures did not drain");
        assert_eq!(result.unwrap_err().kind, ErrorKind::RateLimited);
        assert_eq!(provider.inner.upload_attempts(&members[5].object_id), 0);
        for index in [1, 2, 3, 4] {
            let row = journal.record(&members[index].object_id).unwrap().unwrap();
            assert!(row.released);
            assert_eq!(row.receipt.unwrap().locator.object, members[index].object_id);
            assert!(!journal.spool_path(&members[index].object_id).exists());
        }
        drop(journal);
        let mut journal = TransferJournal::open(root.path(), identity()).unwrap();
        let uncertain = journal.record(&members[0].object_id).unwrap().unwrap();
        assert!(uncertain.attempted && uncertain.receipt.is_none());
        upload(&mut journal, &members[0].object_id, &provider, &repository, &cancel).await.unwrap();
        assert_eq!(provider.inner.upload_attempts(&members[0].object_id), 1);
        assert!(journal.record(&members[0].object_id).unwrap().unwrap().released);
    });
}

#[test]
fn lowered_limit_drains_uploads_before_recovery_reads_and_resumes_ready_members() {
    use crate::external_storage::{transfer_factory_tests, transfer_limit};
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let (mut journal, members) = prepare_wave(&root.path().join("job"), 6);
        transfer_factory_tests::save(root.path(), "connection",
            Descriptor::new("format-repository".into(), None).unwrap(), fake::locator());
        let (mut gate, mut entered) = GateProvider::new();
        gate.fail_kind = ErrorKind::Transient;
        let gate = std::sync::Arc::new(gate);
        let provider = transfer_limit::wrap(root.path(), "connection", gate.clone()).unwrap();
        let repository = fake::repository();
        let cancel = Cancellation::default();
        let receipts = {
            let wave = upload_wave(&mut journal, &members, "format-repository", &[8; 32],
                provider.as_ref(), &repository, &cancel);
            tokio::pin!(wave);
            let first = tokio::time::timeout(std::time::Duration::from_secs(20), async {
                let mut first = Vec::new();
                while first.len() < 4 {
                    tokio::select! {
                        result = &mut wave => panic!("wave ended before four active bodies: {result:?}"),
                        object = entered.recv() => first.push(object.unwrap()),
                    }
                }
                first
            }).await.unwrap();
            transfer_limit::set_limit(root.path(), "connection", 1).unwrap();
            *gate.fail.lock().unwrap() = Some(first[0].clone());
            gate.release(&first[0]);
            assert!(matches!(futures::poll!(wave.as_mut()), std::task::Poll::Pending));
            assert_eq!(gate.inner.read_attempts(&first[0]), 0);
            for object in &first[1..] { gate.release(object); }
            let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(20), async {
                tokio::join!(wave.as_mut(), async {
                    for member in &members[4..] {
                        let object = entered.recv().await.unwrap();
                        assert_eq!(object, member.object_id);
                        assert!(entered.try_recv().is_err());
                        gate.release(&object);
                    }
                })
            }).await.expect("failed upload recovery retained unpolled sibling slots");
            assert_eq!(gate.inner.read_attempts(&first[0]), 1);
            result.unwrap()
        };
        drop(journal);
        let journal = TransferJournal::open(&root.path().join("job"), identity()).unwrap();
        for (member, receipt) in members.iter().zip(receipts) {
            assert_eq!(receipt.locator.object, member.object_id);
            assert_eq!(gate.inner.upload_attempts(&member.object_id), 1);
            let row = journal.record(&member.object_id).unwrap().unwrap();
            assert!(row.released);
            assert_eq!(row.receipt.unwrap().locator.object, member.object_id);
            assert!(!journal.spool_path(&member.object_id).exists());
        }
    });
}
