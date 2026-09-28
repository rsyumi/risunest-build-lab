//! A check that stops and resumes while another device keeps publishing and
//! cleaning up the same repository. Device A checks; device B publishes and
//! runs the ordinary cleanup, which never sees A's local record of its root.
use super::tests::published;
use super::*;
use crate::external_storage::{
    cleanup::{self, CleanupLimits, CleanupOutcome, CleanupRequest, ConnectedDocuments},
    connection_store::StoredConnection,
    contract::{ConnectionConfig, RemoteLocator},
    control::{BackupPointDocument, BackupPointKind, HeadDocument, ObservedHead, PublicationResult},
    fake::{self, FakeLeaseClock, FakeProvider},
    journal::{JobIdentity, TransferJournal},
    leases::LeaseClock,
    packaging::CompletedSnapshot,
    publication::PublicationMode,
};
use crate::persistent_store::sync_selection::CaptureIdentity;
use risunest_external_storage_format::{
    control::BundleSource,
    format::{Descriptor, Strategy},
};
use std::{path::PathBuf, sync::Arc, time::Instant};

const NOW: u64 = 1000 * 24 * 60 * leases::MINUTE_MS;
const KEY: [u8; 32] = [9; 32];

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn descriptor() -> Descriptor {
    Descriptor::new("format-repository".into(), Some(Strategy::Cas)).unwrap()
}

fn connected(provider: Arc<FakeProvider>) -> ConnectedRepository {
    let test = fake::loopback_dependencies(fake::MemoryVault::default(), 1_000);
    let repository = fake::repository();
    ConnectedRepository {
        stored: StoredConnection {
            id: "connection".into(),
            config: ConnectionConfig {
                provider: "synthetic".into(),
                profile: None,
                endpoint: "https://synthetic.invalid".into(),
                account_id: "account".into(),
                location: BTreeMap::new(),
                oauth_profile: None,
            },
            descriptor: descriptor(),
            descriptor_locator: RemoteLocator {
                connection_identity: repository.connection_identity.clone(),
                collection: None,
                object: "descriptor".into(),
            },
            provider_repository_id: repository.repository_id.clone(),
            credential_ref: "credential".into(),
            root_key_ref: "key".into(),
            recovery_key_ref: "recovery-key".into(),
            capture_policy: None,
            retention_policy: None,
            capabilities: fake::capabilities(true),
            created_at_ms: 1_000,
            last_sync_at_ms: None,
            last_backup_at_ms: None,
        },
        provider,
        handle: repository,
        dependencies: test.dependencies,
        root_key: zeroize::Zeroizing::new(KEY),
    }
}

struct Device {
    root: tempfile::TempDir,
    writer: &'static str,
    connected: ConnectedRepository,
}

/// Two devices on one repository and one clock.
struct World {
    provider: Arc<FakeProvider>,
    clock: FakeLeaseClock,
    a: Device,
    b: Device,
}

impl World {
    fn new() -> Self {
        let provider = Arc::new(FakeProvider::new(true));
        let device = |writer| Device {
            root: tempfile::tempdir().unwrap(),
            writer,
            connected: connected(provider.clone()),
        };
        Self {
            a: device("device-a"),
            b: device("device-b"),
            clock: FakeLeaseClock::new(NOW),
            provider,
        }
    }

    fn context<'a>(&'a self, device: &'a Device) -> LeaseContext<'a> {
        LeaseContext {
            root: device.root.path(),
            connection_id: &device.connected.stored.id,
            writer_id: device.writer,
            descriptor: &device.connected.stored.descriptor,
            root_key: &device.connected.root_key,
            provider: device.connected.provider.as_ref(),
            repository: &device.connected.handle,
            clock: &self.clock,
            protection_supported: true,
        }
    }

    /// B publishes a state of its own and moves the head onto it. A later
    /// state is packed apart from the first one's cache, so it reuses none of
    /// its bodies and leaves them unreachable.
    async fn publish(
        &self,
        records: usize,
        revision: i64,
        expected: Option<&ObservedHead>,
    ) -> (CompletedSnapshot, ObservedHead) {
        let repository = &self.b.connected.handle;
        let root = match revision {
            1 => self.b.root.path().to_path_buf(),
            _ => self.b.root.path().join(format!("publication-{revision}")),
        };
        fs::create_dir_all(&root).unwrap();
        let completed = published(&root, &self.provider, repository, records, revision, None).await;
        let document = HeadDocument::new(
            &descriptor(),
            "epoch".into(),
            format!("commit-{revision}"),
            expected.map(|head| head.document.commit_id.clone()),
            completed.fingerprint.clone(),
            completed.reference.clone(),
        )
        .unwrap();
        let prepared = control::prepare_head(&descriptor(), &KEY, repository, document).unwrap();
        let head = match control::publish_head(
            self.provider.as_ref(),
            repository,
            &fake::capabilities(true),
            &descriptor(),
            &KEY,
            Strategy::Cas,
            expected,
            &prepared,
            PublicationMode::Foreground,
            &Cancellation::default(),
        )
        .await
        .unwrap()
        {
            PublicationResult::Confirmed(head) => head,
            other => panic!("the head did not move: {other:?}"),
        };
        (completed, head)
    }

    /// B keeps a manual backup of a published state's library.
    async fn keep(&self, state: &CompletedSnapshot, bundle_id: &str) -> RemoteObject {
        let connected = &self.b.connected;
        let cancel = Cancellation::default();
        let view = control::read_snapshot_document(connected, &state.reference, &cancel)
            .await
            .unwrap();
        let mut journal = TransferJournal::open(
            &self.b.root.path().join(format!("journal-{bundle_id}")),
            JobIdentity {
                job_id: format!("keep-{bundle_id}"),
                connection_id: "connection".into(),
                repository_id: connected.handle.repository_id.clone(),
                capture_id: bundle_id.into(),
                capture: CaptureIdentity {
                    store_id: "store".into(),
                    library_epoch: "epoch".into(),
                    generation: "generation".into(),
                    selection_epoch: "selection".into(),
                    revision: 1,
                },
            },
        )
        .unwrap();
        let bundle = control::upload_backup_bundle(
            &descriptor(),
            &KEY,
            bundle_id.into(),
            BundleSource::SyncState { commit_id: "commit-1".into() },
            NOW,
            view.library,
            view.sections,
            &mut journal,
            self.provider.as_ref(),
            &connected.handle,
            &cancel,
        )
        .await
        .unwrap();
        let point = BackupPointDocument::single(
            &descriptor(),
            format!("point-{bundle_id}"),
            BackupPointKind::Manual,
            NOW,
            bundle.clone(),
        )
        .unwrap();
        control::upload_backup_point(
            &descriptor(),
            &KEY,
            point,
            &mut journal,
            self.provider.as_ref(),
            &connected.handle,
            &cancel,
        )
        .await
        .unwrap()
    }

    /// Every pack a published state reads, by object id.
    async fn packs(&self, state: &CompletedSnapshot) -> BTreeMap<String, RemoteObject> {
        let staging = self.b.root.path().join("read").join(&state.snapshot_id);
        let mut packs = BTreeMap::new();
        for (catalog, kind) in [
            (&state.record_catalog, wire::CatalogKind::Records),
            (&state.asset_catalog, wire::CatalogKind::Assets),
        ] {
            let (_, referenced, _) = snapshot_restore::read_catalog(
                catalog,
                kind,
                &KEY,
                &staging,
                self.provider.as_ref(),
                &self.b.connected.handle,
                &Cancellation::default(),
            )
            .await
            .unwrap();
            packs.extend(referenced);
        }
        packs
    }

    /// One ordinary cleanup run by B at the current time.
    async fn cleanup(&self) -> CleanupOutcome {
        let b = &self.b;
        let cache = b.root.path().join("package-cache");
        let scratch = b.root.path().join("cleanup-probe");
        let cancel = Cancellation::default();
        let view = ConnectedRepositoryView {
            connected: &b.connected,
            writer_id: b.writer,
            policy: RetentionPolicy::DEFAULT,
            now_ms: self.clock.reading().wall_ms,
            unfinished: Vec::new(),
            cache_root: &cache,
        };
        let documents = ConnectedDocuments { connected: &b.connected, cancel: &cancel, scratch: &scratch };
        let available = |_: Instant| Ok(true);
        cleanup::run(
            &self.context(b),
            &CleanupRequest {
                job_id: "cleanup",
                cleanup_supported: true,
                connection_time: &available,
                limits: CleanupLimits::default(),
            },
            &view,
            &documents,
            &cancel,
        )
        .await
        .unwrap()
    }

    /// B's cleanup observes what became unreachable, the grace period
    /// passes, and the next run removes it.
    async fn collect(&self) -> u64 {
        assert_eq!(self.cleanup().await.deleted_objects, 0, "removed before the grace period");
        self.clock.advance(leases::UNREACHABLE_GRACE_MS);
        let outcome = self.cleanup().await;
        assert_eq!(outcome.stop_reason, cleanup::StopReason::Complete);
        outcome.deleted_objects
    }

    fn job(&self, job: &str) -> PathBuf {
        self.a.root.path().join("jobs").join(job)
    }

    /// One attempt of A's check job through the production orchestration.
    async fn attempt(&self, job: &str, snapshot_id: Option<&str>, cancel: &Cancellation) -> Result<Value> {
        let a = &self.a;
        let directory = self.job(job);
        let cache = a.root.path().join("package-cache");
        let mut evidence = ObjectEvidence::open(a.root.path(), &a.connected.stored.id).unwrap();
        run_job(
            &self.context(a),
            &a.connected,
            &CheckJob {
                job_id: job,
                snapshot_id,
                directory: &directory,
                cache_root: &cache,
                policy: RetentionPolicy::DEFAULT,
                now_ms: self.clock.reading().wall_ms,
            },
            &mut evidence,
            &|_: u64, _: u64, _: u64, _: u64| {},
            cancel,
        )
        .await
    }

    /// Stops A's check once it has read the second pack of `packs`.
    async fn stop_after_second(
        &self,
        job: &str,
        snapshot_id: Option<&str>,
        packs: &BTreeMap<String, RemoteObject>,
    ) {
        let ids = packs.keys().collect::<Vec<_>>();
        assert!(ids.len() > 2, "the stop has to fall between packs");
        let cancel = Cancellation::default();
        self.provider.cancel_after_read(ids[1], 1, &cancel);
        let error = self.attempt(job, snapshot_id, &cancel).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(self.proved(job), 1, "the pack before the stop was proved");
        assert!(
            leases::survey(&self.context(&self.a), &Cancellation::default())
                .await
                .unwrap()
                .leases
                .is_empty(),
            "a stopped check kept its work lease"
        );
    }

    fn proved(&self, job: &str) -> usize {
        fs::read_to_string(self.job(job).join(LEDGER_FILE))
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    fn reads(&self, packs: &BTreeMap<String, RemoteObject>) -> BTreeMap<String, usize> {
        packs.keys().map(|id| (id.clone(), self.provider.read_attempts(id))).collect()
    }

    fn evidence(&self, object: &RemoteObject) -> Option<UnusableReason> {
        ConnectionStore::open(self.a.root.path())
            .unwrap()
            .unusable_object(
                "connection",
                &reachability::object_identity(object, &self.a.connected.handle).unwrap(),
            )
            .unwrap()
    }
}

fn count(summary: &Value, field: &str) -> u64 {
    summary[field].as_str().expect(field).parse().unwrap()
}

#[test]
fn a_stopped_check_whose_head_another_device_collected_expires_instead_of_reporting_damage() {
    runtime().block_on(async {
        let world = World::new();
        let (first, head) = world.publish(60, 1, None).await;
        let packs = world.packs(&first).await;
        world.stop_after_second("check", None, &packs).await;

        let (second, _) = world.publish(4, 2, Some(&head)).await;
        let current = world.packs(&second).await;
        let retired = packs.keys().filter(|id| !current.contains_key(*id)).collect::<Vec<_>>();
        assert!(retired.len() > 2, "the newer state reuses almost everything");
        assert!(world.collect().await > 0);
        for id in &retired {
            assert!(!world.provider.holds(id), "B's cleanup kept {id}");
        }

        let before = world.reads(&packs);
        let result = world.attempt("check", None, &Cancellation::default()).await.unwrap();
        assert_eq!(result, json!({"stopReason":"expired"}), "a retired root was reported as {result}");
        assert_eq!(world.reads(&packs), before, "an expired root was read");
        for object in packs.values().chain(current.values()) {
            assert_eq!(world.evidence(object), None, "{} lost its reuse evidence", object.object_id);
        }
        let selected = pinned_root(&world.job("check"), &world.a.connected.handle).unwrap().unwrap();
        let handle = &world.a.connected.handle;
        assert_eq!(
            reachability::object_identity(&selected, handle).unwrap(),
            reachability::object_identity(&first.reference, handle).unwrap(),
            "the job forgot the root it selected"
        );

        let fresh = world.attempt("check-again", None, &Cancellation::default()).await.unwrap();
        assert_eq!(fresh["snapshotId"], "checked-2");
        assert_eq!(count(&fresh, "damagedObjects"), 0);
        assert_eq!(
            count(&fresh, "verifiedObjects") - count(&fresh, "metadataObjects"),
            current.len() as u64
        );
    });
}

#[test]
fn an_unchanged_head_resumes_and_proves_every_body_again() {
    runtime().block_on(async {
        let world = World::new();
        let (first, _) = world.publish(60, 1, None).await;
        let packs = world.packs(&first).await;
        world.stop_after_second("check", None, &packs).await;
        assert_eq!(world.collect().await, 0, "B removed part of its own head");

        let before = world.reads(&packs);
        let result = world.attempt("check", None, &Cancellation::default()).await.unwrap();
        assert_eq!(result["snapshotId"], "checked-1");
        assert_eq!(count(&result, "damagedObjects"), 0);
        assert_eq!(
            count(&result, "verifiedObjects") - count(&result, "metadataObjects"),
            packs.len() as u64
        );
        for (id, reads) in world.reads(&packs) {
            assert_eq!(reads, before[&id] + 1, "{id} was not proved again after the lease was released");
        }
        assert_eq!(world.proved("check"), packs.len());
    });
}

#[test]
fn a_kept_backup_resumes_after_the_head_moved_on_and_was_collected() {
    runtime().block_on(async {
        let world = World::new();
        let (first, head) = world.publish(60, 1, None).await;
        world.keep(&first, "kept").await;
        let packs = world.packs(&first).await;
        world.stop_after_second("check", Some("kept"), &packs).await;

        world.publish(4, 2, Some(&head)).await;
        world.collect().await;
        assert!(!world.provider.holds(&first.reference.object_id), "the old state stayed");
        assert!(packs.keys().all(|id| world.provider.holds(id)), "a kept backup lost a pack");

        let before = world.reads(&packs);
        let result = world.attempt("check", Some("kept"), &Cancellation::default()).await.unwrap();
        assert_eq!(result["snapshotId"], "kept");
        assert_eq!(count(&result, "damagedObjects"), 0);
        for (id, reads) in world.reads(&packs) {
            assert_eq!(reads, before[&id] + 1, "{id} was not proved again");
        }
    });
}

#[test]
fn a_backup_removed_while_its_check_was_stopped_expires_instead_of_reporting_damage() {
    runtime().block_on(async {
        let world = World::new();
        let (first, head) = world.publish(60, 1, None).await;
        let point = world.keep(&first, "removed").await;
        let packs = world.packs(&first).await;
        world.stop_after_second("check", Some("removed"), &packs).await;

        world.publish(4, 2, Some(&head)).await;
        world.provider.forget(&point.object_id);
        world.collect().await;
        assert!(
            packs.keys().any(|id| !world.provider.holds(id)),
            "nothing of the removed backup was collected"
        );

        let before = world.reads(&packs);
        let result = world.attempt("check", Some("removed"), &Cancellation::default()).await.unwrap();
        assert_eq!(result, json!({"stopReason":"expired"}), "a removed backup was reported as {result}");
        assert_eq!(world.reads(&packs), before);
        for object in packs.values() {
            assert_eq!(world.evidence(object), None, "{} lost its reuse evidence", object.object_id);
        }
    });
}

#[test]
fn damage_to_a_kept_root_found_after_a_resume_and_a_lost_cache_is_remembered() {
    runtime().block_on(async {
        let world = World::new();
        let (first, _) = world.publish(60, 1, None).await;
        let packs = world.packs(&first).await;
        world.stop_after_second("check", None, &packs).await;
        assert_eq!(world.collect().await, 0);
        let (damaged, object) = packs.iter().next_back().unwrap();
        let mut bytes = world.provider.contents(damaged).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        world.provider.seed(damaged, ObjectRole::Pack, bytes);
        let _ = fs::remove_dir_all(world.a.root.path().join("package-cache"));
        let _ = fs::remove_dir_all(world.job("check").join(STAGING_DIRECTORY));

        let result = world.attempt("check", None, &Cancellation::default()).await.unwrap();
        assert_eq!(count(&result, "damagedObjects"), 1);
        assert_eq!(result["damaged"][0]["objectId"], damaged.as_str());
        assert_eq!(result["damaged"][0]["reason"], "corrupt");
        assert_eq!(world.evidence(object), Some(UnusableReason::Damaged));
        for other in packs.values().filter(|other| other.object_id != *damaged) {
            assert_eq!(world.evidence(other), None);
        }
    });
}

#[test]
fn a_head_that_moves_right_before_the_resume_expires_the_check_with_every_body_present() {
    runtime().block_on(async {
        let world = World::new();
        let (first, head) = world.publish(60, 1, None).await;
        let packs = world.packs(&first).await;
        world.stop_after_second("check", None, &packs).await;
        world.publish(4, 2, Some(&head)).await;
        assert!(packs.keys().all(|id| world.provider.holds(id)));

        let before = world.reads(&packs);
        let result = world.attempt("check", None, &Cancellation::default()).await.unwrap();
        assert_eq!(result, json!({"stopReason":"expired"}));
        assert_eq!(world.reads(&packs), before, "a check resumed on a root that is no longer kept");
    });
}

#[test]
fn a_check_whose_selected_root_was_lost_does_not_move_to_a_newer_one() {
    runtime().block_on(async {
        let world = World::new();
        let (first, _) = world.publish(60, 1, None).await;
        let packs = world.packs(&first).await;
        world.stop_after_second("check", None, &packs).await;
        fs::remove_file(world.job("check").join(ROOT_FILE)).unwrap();
        let result = world.attempt("check", None, &Cancellation::default()).await.unwrap();
        assert_eq!(result, json!({"stopReason":"expired"}));
        assert!(pinned_root(&world.job("check"), &world.a.connected.handle).unwrap().is_none());
    });
}

#[test]
fn a_check_cancelled_before_admission_selects_nothing_and_leaves_no_lease() {
    runtime().block_on(async {
        let world = World::new();
        world.publish(4, 1, None).await;
        let cancel = Cancellation::default();
        cancel.cancel();
        let error = world.attempt("check", None, &cancel).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert!(pinned_root(&world.job("check"), &world.a.connected.handle).unwrap().is_none());
        assert!(leases::survey(&world.context(&world.a), &Cancellation::default())
            .await
            .unwrap()
            .leases
            .is_empty());
    });
}

#[test]
fn a_check_that_cannot_hold_a_lease_still_resolves_its_root_before_resuming() {
    runtime().block_on(async {
        let world = World::new();
        let (first, head) = world.publish(60, 1, None).await;
        let packs = world.packs(&first).await;
        world.stop_after_second("check", None, &packs).await;
        world.publish(4, 2, Some(&head)).await;
        let a = &world.a;
        let mut context = world.context(a);
        context.protection_supported = false;
        let directory = world.job("check");
        let cache = a.root.path().join("package-cache");
        let mut evidence = ObjectEvidence::open(a.root.path(), "connection").unwrap();
        let result = run_job(
            &context,
            &a.connected,
            &CheckJob {
                job_id: "check",
                snapshot_id: None,
                directory: &directory,
                cache_root: &cache,
                policy: RetentionPolicy::DEFAULT,
                now_ms: NOW,
            },
            &mut evidence,
            &|_: u64, _: u64, _: u64, _: u64| {},
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(result, json!({"stopReason":"expired"}));
    });
}
