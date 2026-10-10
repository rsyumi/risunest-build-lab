use super::*;
use crate::external_storage::lww_tests::CycleFixture;
use crate::persistent_store::{lww::UnitMutation, WorkingSetCommit};
use risunest_sync_wire::unit::UnitKey;

fn run(future: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(future)
}
async fn checkpoint(f: &mut CycleFixture) -> String {
    f.a.commit(&WorkingSetCommit {
        expected_revision: f.a.revision().unwrap(),
        unit_mutations: Some(vec![UnitMutation::Set {
            key: UnitKey::new(&["root", "language"]).unwrap(), value: serde_json::json!("synthetic"),
        }]), ..Default::default()
    }).unwrap();
    f.publish_a().await;
    let directory = tempfile::tempdir().unwrap();
    let writer = f.a.lww_clock_state().unwrap().writer_id;
    f.sender.compact_published(&mut f.a, directory.path(), "00000000-0000-4000-8000-0000000000e2",
        &writer, &f.sender.capabilities, &Cancellation::default(), None).await.unwrap();
    f.sender.snapshot_receipts(&Cancellation::default()).await.unwrap()[0].locator.object.clone()
}

#[test]
fn slow_checkpoint_read_releases_cache_lock_and_cannot_replace_newer_listing() {
    run(async {
        let mut f = CycleFixture::new();
        let id = checkpoint(&mut f).await;
        let barrier = f.provider.arm_read_barrier(&id).unwrap();
        let cache = tokio::sync::Mutex::new(CheckpointSummaries::default());
        let cancel = Cancellation::default();
        let slow = f.receiver.checkpoint_summaries(&cache, &cancel);
        tokio::pin!(slow);
        tokio::select! {
            _ = barrier.reached.notified() => (),
            result = &mut slow => panic!("checkpoint did not wait: {:?}", result.map(|v| v.len())),
        }
        f.provider.forget(&id);
        let fast = tokio::time::timeout(std::time::Duration::from_secs(2),
            f.receiver.checkpoint_summaries(&cache, &cancel)).await.expect("network read held cache lock").unwrap();
        assert!(fast.is_empty());
        let revision = cache.lock().await.revision;
        barrier.release();
        assert_eq!(slow.await.unwrap().len(), 1);
        let stored = cache.lock().await;
        assert_eq!(stored.revision, revision);
        assert!(stored.classified.is_empty(), "old listing overwrote the newer empty listing");
    });
}

#[test]
fn invalidated_or_replaced_scope_cannot_be_resurrected_by_a_slow_completion() {
    run(async {
        let mut f = CycleFixture::new();
        let id = checkpoint(&mut f).await;
        let cache = tokio::sync::Mutex::new(CheckpointSummaries::default());
        let cancel = Cancellation::default();
        for replacement in [false, true] {
            let barrier = f.provider.arm_read_barrier(&id).unwrap();
            let slow = f.receiver.checkpoint_summaries(&cache, &cancel);
            tokio::pin!(slow);
            tokio::select! {
                _ = barrier.reached.notified() => (),
                _ = &mut slow => panic!("checkpoint did not wait"),
            }
            cache.lock().await.invalidate();
            let expected = if replacement {
                let mut other = CycleFixture::new();
                other.receiver.descriptor.repository_id = "synthetic-replacement-repository".into();
                assert!(other.receiver.checkpoint_summaries(&cache, &cancel).await.unwrap().is_empty());
                cache.lock().await.scope.clone()
            } else { String::new() };
            barrier.release();
            assert_eq!(slow.await.unwrap().len(), 1);
            let stored = cache.lock().await;
            assert_eq!(stored.scope, expected);
            assert!(stored.classified.is_empty());
        }
    });
}
