//! Shared body admission for every handle of a saved connection.
use super::{capabilities::Capabilities, connection_store::ConnectionStore, contract::*, phase_progress::PhaseProgress};
use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::{Arc, Mutex, OnceLock}};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) const DEFAULT: usize = 4;
pub(crate) fn validate(value: usize) -> Result<()> {
    if (1..=16).contains(&value) { Ok(()) } else { Err(ProviderError::new(ErrorKind::PreconditionFailed)) }
}

struct Counts { target: usize, total: usize }
struct Limiter { semaphore: Arc<Semaphore>, counts: Mutex<Counts> }
impl Limiter {
    fn new(target: usize) -> Self {
        Self { semaphore: Arc::new(Semaphore::new(target)), counts: Mutex::new(Counts { target, total: target }) }
    }
    fn target(&self) -> usize { self.counts.lock().unwrap_or_else(|e| e.into_inner()).target }
    fn resize(&self, target: usize) {
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        counts.target = target;
        if counts.total < target {
            self.semaphore.add_permits(target - counts.total);
            counts.total = target;
        } else {
            counts.total -= self.semaphore.forget_permits(counts.total - target);
        }
    }
    async fn acquire(self: &Arc<Self>, cancel: &Cancellation) -> Result<Permit> {
        loop {
            cancel.check()?;
            let permit = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(ProviderError::new(ErrorKind::Cancelled)),
                permit = self.semaphore.clone().acquire_owned() => permit.map_err(|_| ProviderError::new(ErrorKind::Cancelled))?,
            };
            // A waiter can already own a permit when a lower target is saved.
            // Retire that permit before allowing its body to start.
            let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
            if counts.total > counts.target {
                permit.forget();
                counts.total -= 1;
                continue;
            }
            drop(counts);
            let permit = Permit { limiter: self.clone(), permit: Some(permit) };
            cancel.check()?;
            return Ok(permit);
        }
    }
}
struct Permit { limiter: Arc<Limiter>, permit: Option<OwnedSemaphorePermit> }
impl Drop for Permit {
    fn drop(&mut self) {
        let mut counts = self.limiter.counts.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(permit) = self.permit.take() {
            if counts.total > counts.target { permit.forget(); counts.total -= 1; }
            else { drop(permit); }
        }
    }
}
type Registry = BTreeMap<(PathBuf, String), Arc<Limiter>>;
static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
fn registry() -> std::sync::MutexGuard<'static, Registry> {
    REGISTRY.get_or_init(Mutex::default).lock().unwrap_or_else(|e| e.into_inner())
}
fn key(root: &Path, id: &str) -> Result<(PathBuf, String)> {
    let root = std::fs::canonicalize(root).map_err(|_| ProviderError::new(ErrorKind::Transient))?;
    Ok((root, id.to_owned()))
}
fn limiter(root: &Path, id: &str) -> Result<Arc<Limiter>> {
    let key = key(root, id)?;
    let mut registry = registry();
    if let Some(limiter) = registry.get(&key) { return Ok(limiter.clone()); }
    let value = ConnectionStore::open(root)?.read(id)?.transfer_concurrency.unwrap_or(DEFAULT);
    validate(value)?;
    let limiter = Arc::new(Limiter::new(value));
    registry.insert(key, limiter.clone());
    Ok(limiter)
}
pub(crate) fn effective_limit(root: &Path, connection_id: &str) -> Result<usize> {
    Ok(limiter(root, connection_id)?.target())
}
pub(crate) fn set_limit(root: &Path, id: &str, value: usize) -> Result<()> {
    validate(value)?;
    let key = key(root, id)?;
    let registry = registry();
    ConnectionStore::open(root)?.set_transfer_concurrency(id, value)?;
    if let Some(limiter) = registry.get(&key) { limiter.resize(value); }
    Ok(())
}
pub(crate) fn remove_connection(root: &Path, id: &str) -> Result<()> {
    let key = key(root, id)?;
    let mut registry = registry();
    ConnectionStore::open(root)?.remove(id)?;
    registry.remove(&key);
    Ok(())
}
pub(crate) fn wrap(root: &Path, connection_id: &str, provider: Arc<dyn Provider>) -> Result<Arc<dyn Provider>> {
    Ok(Arc::new(LimitedProvider { provider, limiter: limiter(root, connection_id)? }))
}
struct LimitedProvider { provider: Arc<dyn Provider>, limiter: Arc<Limiter> }
impl Provider for LimitedProvider {
    fn transfer_concurrency(&self) -> usize { self.limiter.target() }
    fn progress_stage(&self, stage: &'static str) { self.provider.progress_stage(stage) }
    fn preparation_progress(&self) -> Arc<PhaseProgress> { self.provider.preparation_progress() }
    fn open_repository<'a>(&'a self, c: &'a ConnectionConfig, s: &'a SecretRef, m: OpenMode, x: &'a Cancellation) -> ProviderFuture<'a, (RepositoryHandle, Capabilities)> { self.provider.open_repository(c,s,m,x) }
    fn read_object<'a>(&'a self, r: &'a RepositoryHandle, l: &'a RemoteLocator, v: Option<&'a VersionToken>, s: &'a mut dyn TransferSink, x: &'a Cancellation) -> ProviderFuture<'a, ReadReceipt> {
        Box::pin(async move {
            let _permit = self.limiter.acquire(x).await?;
            self.provider.read_object(r,l,v,s,x).await
        })
    }
    fn begin_upload<'a>(&'a self, r: &'a RepositoryHandle, i: &'a ObjectIntent, x: &'a Cancellation) -> ProviderFuture<'a, Option<ResumeState>> { self.provider.begin_upload(r,i,x) }
    fn create_object<'a>(&'a self, r: &'a RepositoryHandle, i: &'a ObjectIntent, s: &'a dyn TransferSource, resume: Option<&'a ResumeState>, x: &'a Cancellation) -> ProviderFuture<'a, ObjectReceipt> {
        Box::pin(async move {
            let _permit = self.limiter.acquire(x).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::Poll;

    #[test]
    fn limits_lowering_raising_cancellation_and_drop_keep_the_ceiling() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            for width in [1, 4, 16] {
                let limiter = Arc::new(Limiter::new(width));
                let cancel = Cancellation::default();
                let mut active = Vec::new();
                for _ in 0..width { active.push(limiter.acquire(&cancel).await.unwrap()); }
                let mut waiting = Box::pin(limiter.acquire(&cancel));
                assert!(matches!(futures::poll!(waiting.as_mut()), Poll::Pending));
                drop(active.pop());
                let replacement = waiting.await.unwrap();
                drop(replacement);
                drop(active);
                assert_eq!(limiter.semaphore.available_permits(), width);
            }
            let limiter = Arc::new(Limiter::new(4));
            let cancel = Cancellation::default();
            let mut active = Vec::new();
            for _ in 0..4 { active.push(limiter.acquire(&cancel).await.unwrap()); }
            limiter.resize(2);
            let mut waiting = Box::pin(limiter.acquire(&cancel));
            for _ in 0..2 {
                drop(active.pop());
                assert!(matches!(futures::poll!(waiting.as_mut()), Poll::Pending));
            }
            drop(active.pop());
            let replacement = waiting.await.unwrap();
            drop(replacement);
            drop(active);
            assert_eq!(limiter.semaphore.available_permits(), 2);
            limiter.resize(1);
            assert_eq!(limiter.semaphore.available_permits(), 1);
            let held = limiter.acquire(&cancel).await.unwrap();
            let cancelled = Cancellation::default();
            let mut waiting = Box::pin(limiter.acquire(&cancelled));
            assert!(matches!(futures::poll!(waiting.as_mut()), Poll::Pending));
            cancelled.cancel();
            assert!(matches!(waiting.await, Err(error) if error.kind == ErrorKind::Cancelled));
            let mut waiting = Box::pin(limiter.acquire(&cancel));
            assert!(matches!(futures::poll!(waiting.as_mut()), Poll::Pending));
            limiter.resize(4);
            let raised = waiting.await.unwrap();
            drop((held, raised));
            assert_eq!(limiter.semaphore.available_permits(), 4);
        });
    }

    #[test]
    fn lowering_retires_a_permit_already_assigned_to_a_waiter() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let limiter = Arc::new(Limiter::new(4));
            let cancel = Cancellation::default();
            let mut active = Vec::new();
            for _ in 0..4 { active.push(limiter.acquire(&cancel).await.unwrap()); }
            let mut waiting = Box::pin(limiter.acquire(&cancel));
            assert!(matches!(futures::poll!(waiting.as_mut()), Poll::Pending));
            drop(active.pop());
            limiter.resize(1);
            assert!(matches!(futures::poll!(waiting.as_mut()), Poll::Pending));
            drop(active.pop());
            drop(active.pop());
            assert!(matches!(futures::poll!(waiting.as_mut()), Poll::Pending));
            drop(active.pop());
            drop(waiting.await.unwrap());
            assert_eq!(limiter.semaphore.available_permits(), 1);
        });
    }
    struct HeldSource;
    impl TransferSource for HeldSource {
        fn byte_length(&self) -> u64 { 1 }
        fn open<'a>(&'a self, _: u64, _: u64, _: &'a Cancellation) -> ProviderFuture<'a, std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>> {
            Box::pin(std::future::pending())
        }
    }
    struct HeldCompletionSink;
    impl TransferSink for HeldCompletionSink {
        fn open<'a>(&'a mut self, _: u64, _: u64, _: &'a Cancellation) -> ProviderFuture<'a, std::pin::Pin<Box<dyn tokio::io::AsyncWrite + Send>>> {
            Box::pin(async { Ok(Box::pin(tokio::io::sink()) as std::pin::Pin<Box<dyn tokio::io::AsyncWrite + Send>>) })
        }
        fn finish<'a>(&'a mut self, _: u64, _: &'a str) -> ProviderFuture<'a, ()> { Box::pin(std::future::pending()) }
    }

    #[test]
    fn mixed_body_handles_share_slots_through_completion_and_drop() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let limiter = Arc::new(Limiter::new(2));
            let provider = Arc::new(super::super::fake::FakeProvider::new(false));
            provider.seed("read", ObjectRole::Pack, vec![7]);
            let first = LimitedProvider { provider: provider.clone(), limiter: limiter.clone() };
            let second = LimitedProvider { provider: provider.clone(), limiter: limiter.clone() };
            let separate = LimitedProvider { provider: provider.clone(), limiter: Arc::new(Limiter::new(1)) };
            let repository = super::super::fake::repository();
            let locator = RemoteLocator { connection_identity: repository.connection_identity.clone(), collection: None, object: "read".into() };
            let intent = ObjectIntent { repository_id: repository.repository_id.clone(), job_id: "job".into(), object_id: "upload".into(), role: ObjectRole::Pack, byte_length: 1, sha256: risunest_sync_wire::hash(&[7]) };
            let cancel = Cancellation::default();
            let source = HeldSource;
            let mut sink = HeldCompletionSink;
            let mut upload = first.create_object(&repository, &intent, &source, None, &cancel);
            let mut read = second.read_object(&repository, &locator, None, &mut sink, &cancel);
            assert!(matches!(futures::poll!(upload.as_mut()), Poll::Pending));
            assert!(matches!(futures::poll!(read.as_mut()), Poll::Pending));
            assert_eq!(provider.upload_count(), 1);
            assert_eq!(provider.read_count(), 1);
            let mut waiting = second.create_object(&repository, &intent, &source, None, &cancel);
            assert!(matches!(futures::poll!(waiting.as_mut()), Poll::Pending));
            assert_eq!(provider.upload_count(), 1);
            let mut independent = separate.create_object(&repository, &intent, &source, None, &cancel);
            assert!(matches!(futures::poll!(independent.as_mut()), Poll::Pending));
            assert_eq!(provider.upload_count(), 2);
            drop(read);
            assert!(matches!(futures::poll!(waiting.as_mut()), Poll::Pending));
            assert_eq!(provider.upload_count(), 3);
            drop((upload, waiting, independent));
            assert_eq!(limiter.semaphore.available_permits(), 2);
        });
    }

    fn saved(root: &Path, id: &str) -> super::super::connection_store::StoredConnection {
        let stored = super::super::connection_store::StoredConnection {
            id: id.into(),
            config: ConnectionConfig { provider: "webdav".into(), profile: None, endpoint: "https://synthetic.invalid".into(), account_id: "account".into(), location: Default::default(), oauth_profile: None },
            descriptor: risunest_external_storage_format::format::Descriptor::new(format!("repository-{id}"), None).unwrap(),
            descriptor_locator: super::super::fake::locator(),
            provider_repository_id: super::super::fake::repository().repository_id,
            credential_ref: "credential".into(), root_key_ref: "root".into(), recovery_key_ref: "recovery".into(),
            retention_policy: None, transfer_concurrency: None, capabilities: Capabilities::default(),
            created_at_ms: 1, verified_at_ms: 1, last_sync_at_ms: None, last_backup_at_ms: None,
        };
        ConnectionStore::open(root).unwrap().insert(&stored).unwrap();
        stored
    }

    #[test]
    fn registry_shares_handles_isolates_roots_and_removes_without_reclaiming_active_work() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let root = tempfile::tempdir().unwrap();
            let other_root = tempfile::tempdir().unwrap();
            let stored = saved(root.path(), "first");
            saved(root.path(), "second");
            saved(other_root.path(), "first");
            let first = limiter(root.path(), "first").unwrap();
            assert!(Arc::ptr_eq(&first, &limiter(root.path(), "first").unwrap()));
            assert!(!Arc::ptr_eq(&first, &limiter(root.path(), "second").unwrap()));
            assert!(!Arc::ptr_eq(&first, &limiter(other_root.path(), "first").unwrap()));
            let provider = Arc::new(super::super::fake::FakeProvider::new(false));
            let wrapped = wrap(root.path(), "first", provider.clone()).unwrap();
            let reopened = wrap(root.path(), "first", provider.clone()).unwrap();
            set_limit(root.path(), "first", 1).unwrap();
            assert_eq!(wrapped.transfer_concurrency(), 1);
            assert_eq!(super::super::progress::Observer::default().wrap(wrapped.clone()).transfer_concurrency(), 1);
            assert_eq!(reopened.transfer_concurrency(), 1);
            assert_eq!(effective_limit(root.path(), "second").unwrap(), 4);
            let cancel = Cancellation::default();
            let active = first.acquire(&cancel).await.unwrap();
            remove_connection(root.path(), "first").unwrap();
            assert!(effective_limit(root.path(), "first").is_err());
            ConnectionStore::open(root.path()).unwrap().insert(&stored).unwrap();
            let replacement = limiter(root.path(), "first").unwrap();
            assert!(!Arc::ptr_eq(&first, &replacement));
            assert_eq!(replacement.target(), 4);
            assert_eq!(first.semaphore.available_permits(), 0);
            drop(active);
            assert_eq!(first.semaphore.available_permits(), 1);
        });
    }

}
