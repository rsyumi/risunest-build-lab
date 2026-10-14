use super::*;
use crate::external_storage::lww_tests::CycleFixture;
use crate::persistent_store::{lww::{ApplyReceive, UnitMutation}, WorkingSetCommit};
use risunest_sync_wire::unit::UnitKey;

fn run(future: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(future)
}
fn set(store: &mut PersistentStore, value: &str) {
    store.commit(&WorkingSetCommit {
        expected_revision: store.revision().unwrap(),
        unit_mutations: Some(vec![UnitMutation::Set {
            key: UnitKey::new(&["root", "language"]).unwrap(), value: serde_json::json!(value),
        }]),
        ..Default::default()
    }).unwrap();
}
fn apply(store: &mut PersistentStore, ids: &[String]) {
    for id in ids {
        let page = store.external_lww_unfinished_receive(id).unwrap().unwrap();
        store.lww_stage_receive(&page).unwrap();
        store.lww_apply_receive(&ApplyReceive { header: page.header.clone(), generating: vec![] }).unwrap();
        store.lww_finish_receive(&page.header).unwrap();
    }
}
async fn publish(f: &mut CycleFixture, count: usize) {
    for index in 0..count {
        set(&mut f.a, &format!("synthetic-{index}"));
        f.publish_a().await;
    }
}

#[test]
fn limited_publish_settles_exact_uncertain_predecessor_and_retains_newer_edit() {
    run(async {
        let mut f = CycleFixture::new();
        let cancel = Cancellation::default();
        set(&mut f.a, "old");
        f.provider.state.lock().unwrap().lose_response = true;
        assert!(f.sender.publish_limited(&mut f.a, 0.into(), &[], Some(1), &cancel).await.is_err());
        let writer = f.a.lww_clock_state().unwrap().writer_id;
        let (pending, bytes) = f.a.external_lww_pending(&f.sender.target_scope(), &writer).unwrap().unwrap();
        assert!(pending.dispatched);
        set(&mut f.a, "new");
        f.restart_a();
        let result = f.sender.publish_limited(&mut f.a, 0.into(), &[], Some(1), &cancel).await.unwrap();
        assert_eq!(result.segments.0, 1);
        assert!(result.more);
        assert_eq!(f.provider.contents(&pending.object_id).unwrap(), bytes);
        assert_eq!(f.provider.upload_attempts(&pending.object_id), 1);
        assert_eq!(f.a.external_lww_next_sequence(&f.sender.target_scope(), &writer).unwrap(), 2);
        assert_eq!(f.a.lww_read_outbox(0.into(), 1).unwrap().entries.len(), 1);
        let drained = f.sender.publish(&mut f.a, 0.into(), &[], &cancel).await.unwrap();
        assert_eq!(drained.segments.0, 1);
        assert!(!drained.more);
        f.receive_b().await;
        assert_eq!(f.b.read_root(None).unwrap().value["language"], "new");
    });
}

#[test]
fn bounded_discovery_keeps_unoffered_pages_and_restarts_from_durable_progress() {
    run(async {
        let mut f = CycleFixture::new();
        publish(&mut f, 6).await;
        let cancel = Cancellation::default();
        let cache = Default::default();
        let all = f.receiver.receive_requests_limited(&mut f.b, 0.into(), &cache, 10, &cancel).await.unwrap();
        assert_eq!(all.len(), 6);
        let first = f.receiver.receive_requests_cached(&mut f.b, 0.into(), &cache, &cancel).await.unwrap();
        assert_eq!(first, all[..4]);
        assert!(f.b.external_lww_unfinished_receive(&all[5]).unwrap().is_some());
        assert!(f.b.lww_receive_progress(0.into()).unwrap().is_empty());
        apply(&mut f.b, &first);
        f.b = PersistentStore::open(f.directory_b.path()).unwrap();
        let next = f.receiver.receive_requests_cached(&mut f.b, 0.into(), &cache, &cancel).await.unwrap();
        assert_eq!(next, all[4..]);
        apply(&mut f.b, &next);
        assert!(f.receiver.receive_requests_cached(&mut f.b, 0.into(), &cache, &cancel).await.unwrap().is_empty());
        assert_eq!(f.b.read_root(None).unwrap().value["language"], "synthetic-5");
        assert_eq!(f.b.lww_receive_progress(0.into()).unwrap()[0].cursor.0, 6);
    });
}

#[test]
fn partial_discovery_does_not_fallback_to_checkpoint_or_prune_but_complete_gap_does() {
    run(async {
        let mut f = CycleFixture::new();
        publish(&mut f, 6).await;
        let cancel = Cancellation::default();
        let writer = f.a.lww_clock_state().unwrap().writer_id;
        let directory = tempfile::tempdir().unwrap();
        f.sender.compact_published(&mut f.a, directory.path(), "00000000-0000-4000-8000-0000000000e1",
            &writer, &f.sender.capabilities, &cancel, None).await.unwrap();
        let cache = Default::default();
        let first = f.receiver.receive_requests_limited(&mut f.b, 0.into(), &cache, 2, &cancel).await.unwrap();
        assert_eq!(first.len(), 2);
        assert!(first.iter().all(|id| id.starts_with("external-receive-")));
        let extra = StageReceive {
            header: Header { binding_authority: 0.into(), request_id: "synthetic-unoffered".into() },
            changes: vec![], progress: Progress { kind: "external".into(), writer_id: Some(writer), cursor: 0.into() },
            admitted_time_upper_ms: DecimalU64(crate::external_storage::runtime::now_ms() + 300_000),
        };
        f.b.external_lww_stable_receive(&f.receiver.target_scope(), &extra).unwrap();
        f.receiver.receive_requests_limited(&mut f.b, 0.into(), &cache, 2, &cancel).await.unwrap();
        assert!(f.b.external_lww_unfinished_receive(&extra.header.request_id).unwrap().is_some());
        for receipt in f.sender.listing(&cancel).await.unwrap() { f.provider.forget(&receipt.locator.object); }
        let fallback = f.receiver.receive_requests_cached(&mut f.b, 0.into(), &cache, &cancel).await.unwrap();
        assert!(fallback.iter().all(|id| id.starts_with("external-snapshot-")));
        assert!(!fallback.is_empty());
        assert!(f.b.external_lww_unfinished_receive(&extra.header.request_id).unwrap().is_none());
        apply(&mut f.b, &fallback);
        assert_eq!(f.b.read_root(None).unwrap().value["language"], "synthetic-5");
    });
}

#[test]
fn only_held_segments_still_fail_clock_admission_without_progress() {
    run(async {
        let mut f = CycleFixture::new();
        publish(&mut f, 1).await;
        let id = f.provider.uploaded_ids()[0].clone();
        let bytes = f.provider.contents(&id).unwrap();
        f.provider.forget(&id);
        let (writer, seq, _) = parse_segment_object_id(&id).unwrap();
        let mut payload = segment::open(&bytes, &f.sender.library, writer, seq, &[7; 32]).unwrap();
        payload.changes[0].stamp.physical_ms = DecimalU64(crate::external_storage::runtime::now_ms() + 600_000);
        let (sealed, _) = segment::seal(&payload, &[7; 32]).unwrap();
        f.provider.seed(&segment_object_id(writer, seq, &segment::digest(&sealed)).unwrap(), ObjectRole::Segment, sealed);
        let error = f.receiver.receive_requests_limited(&mut f.b, 0.into(), &Default::default(), 1, &Cancellation::default()).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::ClockSkew);
        assert!(f.b.lww_receive_progress(0.into()).unwrap().is_empty());
    });
}

#[test]
fn a_bounded_segment_keeps_page_group_progress_until_its_last_page_is_durable() {
    run(async {
        let mut f = CycleFixture::new();
        set(&mut f.a, "synthetic");
        f.a.commit(&WorkingSetCommit {
            expected_revision: f.a.revision().unwrap(),
            unit_mutations: Some(vec![UnitMutation::Set {
                key: UnitKey::new(&["root", "loreBookDepth"]).unwrap(), value: serde_json::json!(7),
            }]), ..Default::default()
        }).unwrap();
        f.publish_a().await;
        set_receive_page_bytes_for_test(1);
        let cancel = Cancellation::default();
        let cache = Default::default();
        let pages = f.receiver.receive_requests_limited(&mut f.b, 0.into(), &cache, 1, &cancel).await.unwrap();
        assert_eq!(pages.len(), 2);
        apply(&mut f.b, &pages[..1]);
        assert!(f.b.lww_receive_progress(0.into()).unwrap().is_empty());
        f.b = PersistentStore::open(f.directory_b.path()).unwrap();
        let resumed = f.receiver.receive_requests_limited(&mut f.b, 0.into(), &cache, 1, &cancel).await.unwrap();
        assert_eq!(resumed, pages[1..]);
        apply(&mut f.b, &resumed);
        assert_eq!(f.b.lww_receive_progress(0.into()).unwrap()[0].cursor.0, 1);
        assert_eq!(f.b.read_root(None).unwrap().value["loreBookDepth"], 7);
    });
}

#[test]
fn finished_pages_do_not_make_limited_discovery_report_an_incomplete_empty_check() {
    run(async {
        let mut f = CycleFixture::new();
        publish(&mut f, 3).await;
        let cancel = Cancellation::default();
        let cache = Default::default();
        let first = f.receiver.receive_requests_limited(&mut f.b, 0.into(), &cache, 1, &cancel).await.unwrap();
        apply(&mut f.b, &first);
        assert_eq!(first.len(), 1);
        assert!(f.b.external_lww_receive_finished(&first[0]).unwrap());
        // Retain completion proofs while independently injecting a lagging traversal cursor.
        f.b.device_store().unwrap().connection().execute("DELETE FROM lww_progress WHERE kind='external'", []).unwrap();
        let next = f.receiver.receive_requests_limited(&mut f.b, 0.into(), &cache, 1, &cancel).await.unwrap();
        assert_eq!(next.len(), 1);
        assert!(!first.contains(&next[0]));
        apply(&mut f.b, &next);
        assert_eq!(f.b.lww_receive_progress(0.into()).unwrap()[0].cursor.0, 2);
    });
}
