use std::{
    io,
    thread::{Scope, ScopedJoinHandle},
};

pub(super) const WORKERS: usize = 4;

pub(super) fn spawn<'scope, 'env, F>(
    scope: &'scope Scope<'scope, 'env>,
    run: F,
) -> io::Result<ScopedJoinHandle<'scope, ()>>
where
    F: FnOnce() + Send + 'scope,
{
    #[cfg(test)]
    if REFUSE_AFTER.with(|remaining| {
        let count = remaining.get();
        remaining.set(count.saturating_sub(1));
        count == 0
    }) {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "synthetic worker refusal",
        ));
    }
    #[cfg(test)]
    let (sender, receiver) = std::sync::mpsc::sync_channel::<(
        super::body_io::BodyIoScope,
        crate::server_sync::hash_metrics::Scope,
        crate::external_storage::worker_observation::WorkerScope,
    )>(1);
    let worker = std::thread::Builder::new()
        .name("asset-io".into())
        .spawn_scoped(scope, move || {
            #[cfg(test)]
            {
                let (body, hashes, receipts) = receiver.recv().expect("worker observation");
                super::body_io::with_body_io_scope(body, || {
                    let _hashes = crate::server_sync::hash_metrics::enter(hashes);
                    crate::external_storage::worker_observation::with_scope(receipts, run);
                });
            }
            #[cfg(not(test))]
            run();
        })?;
    #[cfg(test)]
    sender
        .send((
            super::body_io::capture_body_io_scope(),
            crate::server_sync::hash_metrics::capture(),
            crate::external_storage::worker_observation::capture_scope(),
        ))
        .unwrap_or_else(|_| panic!("worker observation receiver closed"));
    Ok(worker)
}

#[cfg(test)]
thread_local! { static REFUSE_AFTER: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) }; }

#[cfg(test)]
pub(super) fn with_limit<T>(limit: usize, run: impl FnOnce() -> T) -> T {
    struct Reset(usize);
    impl Drop for Reset {
        fn drop(&mut self) {
            REFUSE_AFTER.with(|remaining| remaining.set(self.0));
        }
    }
    let _reset = Reset(REFUSE_AFTER.with(|remaining| remaining.replace(limit)));
    run()
}
