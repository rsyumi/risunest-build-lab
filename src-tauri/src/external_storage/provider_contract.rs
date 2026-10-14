//! Behavior every provider's read owes its caller, checked once here and run
//! against the fake and each wire-backed provider. Not compiled into the
//! product.
use super::{
    contract::{
        Cancellation, ErrorKind, Provider, ProviderFuture, RemoteLocator, RepositoryHandle,
        TransferSink,
    },
    transfer::SpoolSink,
    wire_fixture::Reply,
};
use std::{pin::Pin, sync::Arc, time::Duration};
use tokio::{io::AsyncWrite, sync::Notify};

/// A reply that sends the headers of a 100-byte body and then holds the body
/// until the returned sender drops, so nothing but cancellation ends a read
/// of it.
pub(crate) fn held_body() -> (Reply, std::sync::mpsc::Sender<()>) {
    let (ready, _) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    (Reply::Gated { send_headers: true, ready, release: released }, release)
}

/// A spool sink that says when a provider opens it. A provider opens its sink
/// once the response headers are in and before any body byte.
struct ObservedSink {
    inner: SpoolSink,
    opened: Arc<Notify>,
}

impl TransferSink for ObservedSink {
    fn open<'a>(
        &'a mut self,
        offset: u64,
        max_length: u64,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, Pin<Box<dyn AsyncWrite + Send>>> {
        Box::pin(async move {
            let writer = self.inner.open(offset, max_length, cancel).await?;
            self.opened.notify_one();
            Ok(writer)
        })
    }
    fn finish<'a>(&'a mut self, length: u64, sha256: &'a str) -> ProviderFuture<'a, ()> {
        self.inner.finish(length, sha256)
    }
}

/// A read cancelled once its body has begun ends `Cancelled` without waiting
/// for the rest of the body, and leaves its sink unverified.
pub(crate) async fn a_read_cancelled_during_its_body_stops(
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    locator: &RemoteLocator,
    cancel: &Cancellation,
) {
    let directory = tempfile::tempdir().unwrap();
    let opened = Arc::new(Notify::new());
    let mut sink = ObservedSink {
        inner: SpoolSink::create(&directory.path().join("body"), 4096).unwrap(),
        opened: opened.clone(),
    };
    let read = provider.read_object(repository, locator, None, &mut sink, cancel);
    let trigger = async {
        opened.notified().await;
        cancel.cancel();
    };
    // A guard against a read that never stops, not a latency budget.
    let (result, ()) =
        tokio::time::timeout(Duration::from_secs(10), futures::future::join(read, trigger))
            .await
            .expect("the cancelled read did not stop");
    assert_eq!(result.err().unwrap().kind, ErrorKind::Cancelled);
    assert!(!sink.inner.is_verified());
}

#[test]
fn the_fake_provider_stops_a_read_cancelled_during_its_body() {
    use super::{contract::ObjectRole, fake};
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let provider = fake::FakeProvider::new(false);
        let locator = RemoteLocator { object: "pack-held".into(), ..fake::locator() };
        provider.seed(&locator.object, ObjectRole::Pack, vec![7; 100]);
        let _barrier = provider.arm_read_barrier(&locator.object).unwrap();
        a_read_cancelled_during_its_body_stops(
            &provider,
            &fake::repository(),
            &locator,
            &Cancellation::default(),
        )
        .await;
    });
}
