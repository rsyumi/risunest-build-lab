use super::{connection_commands::{self, ConnectedRepository}, connection_store::{ConnectionStore, StoredConnection}, contract::*, fake, providers::Dependencies, transfer_limit};
use std::{collections::BTreeMap, path::{Path, PathBuf}, pin::Pin, sync::{Arc, Mutex, OnceLock}, task::Poll};
use tauri::Manager;
use zeroize::Zeroizing;

#[derive(Clone)]
pub(super) struct Inputs { pub provider: Arc<dyn Provider>, pub dependencies: Dependencies, pub root_key: Zeroizing<[u8; 32]> }
type InputRegistry = BTreeMap<(PathBuf, String), Inputs>;
fn registry() -> &'static Mutex<InputRegistry> {
    static INPUTS: OnceLock<Mutex<InputRegistry>> = OnceLock::new();
    INPUTS.get_or_init(Mutex::default)
}
pub(super) fn inputs(root: &Path, id: &str) -> Option<Inputs> { registry().lock().unwrap().get(&(root.to_owned(), id.into())).cloned() }
struct Installed((PathBuf, String));
impl Drop for Installed { fn drop(&mut self) { registry().lock().unwrap().remove(&self.0); } }

pub(super) struct HeldSource;
impl TransferSource for HeldSource {
    fn byte_length(&self) -> u64 { 4 }
    fn open<'a>(&'a self, _: u64, _: u64, _: &'a Cancellation) -> ProviderFuture<'a, Pin<Box<dyn tokio::io::AsyncRead + Send>>> { Box::pin(std::future::pending()) }
}
struct HeldSink;
impl TransferSink for HeldSink {
    fn open<'a>(&'a mut self, _: u64, _: u64, _: &'a Cancellation) -> ProviderFuture<'a, Pin<Box<dyn tokio::io::AsyncWrite + Send>>> { Box::pin(std::future::pending()) }
    fn finish<'a>(&'a mut self, _: u64, _: &'a str) -> ProviderFuture<'a, ()> { Box::pin(async { Ok(()) }) }
}

pub(super) fn save(root: &Path, id: &str, descriptor: risunest_external_storage_format::format::Descriptor, locator: RemoteLocator) -> StoredConnection {
    let stored = StoredConnection {
        id: id.into(), config: ConnectionConfig { provider: "synthetic".into(), profile: None, endpoint: "https://synthetic.invalid".into(), account_id: "account".into(), location: Default::default(), oauth_profile: None },
        descriptor, descriptor_locator: locator, provider_repository_id: fake::repository().repository_id,
        credential_ref: "credential".into(), root_key_ref: "root-key".into(), recovery_key_ref: "recovery-key".into(),
        capabilities: fake::capabilities(false), retention_policy: None, transfer_concurrency: None,
        created_at_ms: 1, verified_at_ms: 1, last_sync_at_ms: None, last_backup_at_ms: None,
    };
    ConnectionStore::open(root).unwrap().insert(&stored).unwrap(); stored
}

#[test]
fn both_saved_factories_share_the_same_body_admission() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let provider = Arc::new(fake::FakeProvider::new(false));
        let repository = fake::repository();
        let cancel = Cancellation::default();
        let descriptor = risunest_external_storage_format::format::Descriptor::new("library".into(), None).unwrap();
        let locator = super::descriptor::upload(root.path(), provider.as_ref(), &repository, &descriptor, &[7; 32], &cancel).await.unwrap();
        let stored = save(root.path(), "connection", descriptor, locator);
        let key = (root.path().to_owned(), stored.id.clone());
        registry().lock().unwrap().insert(key.clone(), Inputs { provider: provider.clone(), dependencies: fake::loopback_dependencies(fake::MemoryVault::default(), 1_000).dependencies, root_key: Zeroizing::new([7; 32]) });
        let _installed = Installed(key);
        let app = tauri::test::mock_builder().build(tauri::test::mock_context(tauri::test::noop_assets())).unwrap();
        let jobs = super::job_store::JobCommandState::default(); jobs.root.set(root.path().to_owned()).unwrap(); app.manage(jobs);
        let ConnectedRepository { provider: regular, handle, .. } = connection_commands::open_connected_with_cancel(app.handle(), &stored.id, &cancel).await.unwrap();
        let (source, source_handle, _, source_stored) = super::lww_residency::open_source_connection(root.path(), &stored.id, &stored.descriptor.repository_id, &cancel).await.unwrap();
        assert_eq!(source_stored.id, stored.id);
        transfer_limit::set_limit(root.path(), &stored.id, 1).unwrap();
        assert_eq!((regular.transfer_concurrency(), source.transfer_concurrency()), (1, 1));
        provider.seed("download", ObjectRole::Pack, b"test".to_vec());
        let intent = ObjectIntent { repository_id: handle.repository_id.clone(), job_id: "job".into(), object_id: "upload".into(), role: ObjectRole::Pack, byte_length: 4, sha256: risunest_sync_wire::hash(b"test") };
        let upload_source = HeldSource;
        let mut upload = regular.create_object(&handle, &intent, &upload_source, None, &cancel);
        assert!(matches!(futures::poll!(upload.as_mut()), Poll::Pending));
        let locator = RemoteLocator { connection_identity: source_handle.connection_identity.clone(), collection: None, object: "download".into() };
        let mut sink = HeldSink;
        let mut download = source.read_object(&source_handle, &locator, None, &mut sink, &cancel);
        assert!(matches!(futures::poll!(download.as_mut()), Poll::Pending));
        assert_eq!(provider.read_attempts("download"), 0);
        drop(upload);
        assert!(matches!(futures::poll!(download.as_mut()), Poll::Pending));
        assert_eq!(provider.read_attempts("download"), 1);
        drop(download);
    });
}
