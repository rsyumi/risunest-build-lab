use super::*;
use crate::external_storage::{fake, transfer_limit, transfer_factory_tests};
use tokio::io::{AsyncWriteExt, DuplexStream};

struct NetworkSource(Mutex<Option<DuplexStream>>);
impl TransferSource for NetworkSource {
    fn byte_length(&self) -> u64 { 4 }
    fn open<'a>(&'a self, offset: u64, length: u64, _: &'a Cancellation) -> ProviderFuture<'a, Pin<Box<dyn AsyncRead + Send>>> {
        assert_eq!((offset, length), (0, 4));
        Box::pin(async move { Ok(network_reader(Box::pin(self.0.lock().unwrap().take().unwrap()), true)) })
    }
}
struct StreamingProvider { provider: Arc<fake::FakeProvider>, downloads: Mutex<BTreeMap<String, DuplexStream>> }
impl Provider for StreamingProvider {
    fn transfer_concurrency(&self) -> usize { self.provider.transfer_concurrency() }
    fn progress_stage(&self, stage: &'static str) { self.provider.progress_stage(stage) }
    fn preparation_progress(&self) -> Arc<PhaseProgress> { self.provider.preparation_progress() }
    fn open_repository<'a>(&'a self, c: &'a ConnectionConfig, s: &'a SecretRef, m: OpenMode, x: &'a Cancellation) -> ProviderFuture<'a, (RepositoryHandle, Capabilities)> { self.provider.open_repository(c,s,m,x) }
    fn read_object<'a>(&'a self, r: &'a RepositoryHandle, l: &'a RemoteLocator, _: Option<&'a VersionToken>, s: &'a mut dyn TransferSink, x: &'a Cancellation) -> ProviderFuture<'a, ReadReceipt> {
        Box::pin(async move {
            x.check()?; l.validate_for(r)?;
            let stream = self.downloads.lock().unwrap().remove(&l.object).unwrap();
            let mut reader = network_reader(Box::pin(stream), false);
            let mut writer = s.open(0, 4, x).await?;
            let bytes = tokio::io::copy(&mut reader, &mut writer).await.unwrap();
            writer.shutdown().await.unwrap(); drop(writer);
            s.finish(bytes, &risunest_sync_wire::hash(b"test")).await?;
            Ok(ReadReceipt::Body(ObjectReceipt { locator: l.clone(), byte_length: bytes, version: None, checksum: None, complete: true }))
        })
    }
    fn begin_upload<'a>(&'a self, r: &'a RepositoryHandle, i: &'a ObjectIntent, x: &'a Cancellation) -> ProviderFuture<'a, Option<ResumeState>> { self.provider.begin_upload(r,i,x) }
    fn create_object<'a>(&'a self, r: &'a RepositoryHandle, i: &'a ObjectIntent, s: &'a dyn TransferSource, resume: Option<&'a ResumeState>, x: &'a Cancellation) -> ProviderFuture<'a, ObjectReceipt> {
        Box::pin(async move {
            self.provider.create_object(r,i,s,resume,x).await
        })
    }
    fn compare_exchange_head<'a>(&'a self, r: &'a RepositoryHandle, l: &'a RemoteLocator, e: &'a ExpectedHead, h: &'a HeadBytes, x: &'a Cancellation) -> ProviderFuture<'a, HeadReceipt> { self.provider.compare_exchange_head(r,l,e,h,x) }
    fn replace_head<'a>(&'a self, r: &'a RepositoryHandle, l: &'a RemoteLocator, h: &'a HeadBytes, x: &'a Cancellation) -> ProviderFuture<'a, HeadReceipt> { self.provider.replace_head(r,l,h,x) }
    fn list_objects<'a>(&'a self, r: &'a RepositoryHandle, c: Collection, cursor: Option<&'a str>, limit: u16, x: &'a Cancellation) -> ProviderFuture<'a, ObjectPage> { self.provider.list_objects(r,c,cursor,limit,x) }
    fn delete_object<'a>(&'a self, r: &'a RepositoryHandle, l: &'a RemoteLocator, x: &'a Cancellation) -> ProviderFuture<'a, ()> { self.provider.delete_object(r,l,x) }
    fn delete_empty_container<'a>(&'a self, r: &'a RepositoryHandle, l: &'a RemoteLocator, j: &'a [String], x: &'a Cancellation) -> ProviderFuture<'a, ()> { self.provider.delete_empty_container(r,l,j,x) }
    fn reconcile_upload<'a>(&'a self, r: &'a RepositoryHandle, i: &'a ObjectIntent, s: Option<&'a ResumeState>, x: &'a Cancellation) -> ProviderFuture<'a, UploadResolution> { self.provider.reconcile_upload(r,i,s,x) }
    fn lookup_metadata<'a>(&'a self, r: &'a RepositoryHandle, i: &'a ObjectIntent, l: Option<&'a RemoteLocator>, x: &'a Cancellation) -> ProviderFuture<'a, Option<ObjectReceipt>> { self.provider.lookup_metadata(r,i,l,x) }
    fn head_locator(&self, r: &RepositoryHandle) -> Result<RemoteLocator> { self.provider.head_locator(r) }
}

#[test]
fn concurrent_observed_body_streams_keep_bytes_and_logical_completion_exact() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let root = tempfile::tempdir().unwrap();
        transfer_factory_tests::save(root.path(), "connection", risunest_external_storage_format::format::Descriptor::new("library".into(), None).unwrap(), fake::locator());
        transfer_limit::set_limit(root.path(), "connection", 3).unwrap();
        let inner = Arc::new(fake::FakeProvider::new(false));
        let (read_stream, mut read_writer) = tokio::io::duplex(16);
        let provider = Arc::new(StreamingProvider { provider: inner.clone(), downloads: Mutex::new(BTreeMap::from([("read".into(), read_stream)])) });
        let observer = Observer::default();
        let snapshots = Arc::new(Mutex::new(Vec::new())); let saved = snapshots.clone();
        let guard = observer.begin_sink(Some(Box::new(move |snapshot| saved.lock().unwrap().push(snapshot))));
        let observed = observer.wrap(transfer_limit::wrap(root.path(), "connection", provider).unwrap());
        let repository = fake::repository(); let cancel = Cancellation::default();
        let intent = |id: &str| ObjectIntent { repository_id: repository.repository_id.clone(), job_id: "job".into(), object_id: id.into(), role: ObjectRole::Pack, byte_length: 4, sha256: risunest_sync_wire::hash(b"test") };
        let first_intent = intent("first"); let second_intent = intent("second");
        let (first_stream, mut first_writer) = tokio::io::duplex(16); let first_source = NetworkSource(Mutex::new(Some(first_stream)));
        let (second_stream, mut second_writer) = tokio::io::duplex(16); let second_source = NetworkSource(Mutex::new(Some(second_stream)));
        first_writer.write_all(b"te").await.unwrap(); second_writer.write_all(b"te").await.unwrap(); read_writer.write_all(b"te").await.unwrap();
        let mut first = observed.create_object(&repository, &first_intent, &first_source, None, &cancel);
        let mut second = observed.create_object(&repository, &second_intent, &second_source, None, &cancel);
        let locator = RemoteLocator { connection_identity: repository.connection_identity.clone(), collection: None, object: "read".into() };
        let read_intent = intent("read");
        let mut sink = IdentitySink { intent: &read_intent };
        let mut read = observed.read_object(&repository, &locator, None, &mut sink, &cancel);
        assert!(matches!(futures::poll!(first.as_mut()), Poll::Pending));
        assert!(matches!(futures::poll!(second.as_mut()), Poll::Pending));
        assert!(matches!(futures::poll!(read.as_mut()), Poll::Pending));
        {
            let run = observer.current().unwrap(); let reading = run.reading.lock().unwrap();
            assert_eq!((reading.sending, reading.receiving), (2, 1));
            assert_eq!((reading.network_sent, reading.network_received), (4, 2));
            assert_eq!((reading.uploads.len(), reading.downloads.len()), (0, 0));
        }
        second_writer.write_all(b"st").await.unwrap(); second_writer.shutdown().await.unwrap(); second.await.unwrap();
        read_writer.write_all(b"st").await.unwrap(); read_writer.shutdown().await.unwrap(); read.await.unwrap();
        first_writer.write_all(b"st").await.unwrap(); first_writer.shutdown().await.unwrap(); first.await.unwrap();
        let (retry_stream, mut retry_writer) = tokio::io::duplex(16); retry_writer.write_all(b"test").await.unwrap(); retry_writer.shutdown().await.unwrap();
        observed.create_object(&repository, &first_intent, &NetworkSource(Mutex::new(Some(retry_stream))), None, &cancel).await.unwrap();
        assert!(matches!(observed.reconcile_upload(&repository, &first_intent, None, &cancel).await.unwrap(), UploadResolution::Complete(_)));
        observed.progress_stage("applying"); drop(guard);
        let snapshots = snapshots.lock().unwrap(); let last = snapshots.last().unwrap();
        assert_eq!((&last.network.sent_bytes, &last.network.received_bytes), (&"12".into(), &"4".into()));
        assert_eq!((&last.uploaded_bytes, &last.uploaded_objects), (&"8".into(), &"2".into()));
        assert_eq!((&last.downloaded_bytes, &last.downloaded_objects), (&"4".into(), &"1".into()));
        assert!(!last.network.sending && !last.network.receiving);
    });
}
