use super::*;
use crate::asset_repository::PayloadCas;
use crate::persistent_store::server_sync_engine::CycleItemCounter;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};

const ACTIVITY_UPLOADING: u8 = 4;
const REJECTED_KEY: &str = "synthetic-rejected-record";
const ACTIVITY_VERIFYING: u8 = 6;

#[derive(Default)]
struct Traffic {
    missing: AtomicU64,
    pins: AtomicU64,
    frames: AtomicU64,
    page_puts: AtomicU64,
    /// Remaining page writes to answer as if an object they depend on were gone.
    page_faults: AtomicU64,
    missing_while_verifying: AtomicU64,
    frames_while_uploading: AtomicU64,
    /// Answers the commit with a server fault, leaving the operation reserved
    /// here so the next cycle takes the resume path.
    reject_commit: AtomicBool,
    /// Answers one staged-changes progress read as if the stage were gone.
    forget_stage: AtomicBool,
}
impl Traffic {
    fn reset(&self) {
        for counter in [
            &self.missing,
            &self.pins,
            &self.frames,
            &self.page_puts,
            &self.missing_while_verifying,
            &self.frames_while_uploading,
        ] {
            counter.store(0, Ordering::Relaxed);
        }
    }
    fn object_requests(&self) -> u64 {
        self.missing.load(Ordering::Relaxed) + self.pins.load(Ordering::Relaxed)
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    server: Arc<Store>,
    runtime: tokio::runtime::Runtime,
    task: tokio::task::JoinHandle<()>,
    endpoint: String,
    traffic: Arc<Traffic>,
    activity: Arc<AtomicU8>,
}

fn fault(status: u16, code: &str) -> axum::response::Response {
    body(status, format!("{{\"error\":\"{code}\"}}"))
}
fn rejected_record(status: u16, code: &str, key: &str) -> axum::response::Response {
    body(status, format!("{{\"error\":\"{code}\",\"key\":\"{key}\"}}"))
}
fn body(status: u16, json: String) -> axum::response::Response {
    axum::response::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(json))
        .unwrap()
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let server = Arc::new(Store::init(root.path()).unwrap());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let traffic = Arc::new(Traffic::default());
        let activity = Arc::new(AtomicU8::new(0));
        let remote = server.clone();
        let observed = traffic.clone();
        let reported = activity.clone();
        let task = runtime.spawn(async move {
            let router = http::router(remote).layer(axum::middleware::from_fn(
                move |request: axum::extract::Request, next: axum::middleware::Next| {
                    let traffic = observed.clone();
                    let activity = reported.clone();
                    async move {
                        let path = request.uri().path().to_owned();
                        let method = request.method().clone();
                        let now = activity.load(Ordering::Relaxed);
                        match path.as_str() {
                            "/objects/missing" => {
                                traffic.missing.fetch_add(1, Ordering::Relaxed);
                                if now == ACTIVITY_VERIFYING {
                                    traffic.missing_while_verifying.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            "/objects/pins" => {
                                traffic.pins.fetch_add(1, Ordering::Relaxed);
                            }
                            "/uploads/frames" => {
                                traffic.frames.fetch_add(1, Ordering::Relaxed);
                                if now == ACTIVITY_UPLOADING {
                                    traffic.frames_while_uploading.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            _ => (),
                        }
                        if method == axum::http::Method::POST
                            && path == "/commits"
                            && traffic.reject_commit.load(Ordering::Relaxed)
                        {
                            return fault(500, "synthetic-commit-fault");
                        }
                        if method == axum::http::Method::GET
                            && path.starts_with("/staged-changes/")
                            && traffic.forget_stage.swap(false, Ordering::Relaxed)
                        {
                            return fault(404, "staging-not-found");
                        }
                        if method == axum::http::Method::PUT && path.contains("/pages/") {
                            traffic.page_puts.fetch_add(1, Ordering::Relaxed);
                            if traffic
                                .page_faults
                                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                                    left.checked_sub(1)
                                })
                                .is_ok()
                            {
                                return rejected_record(
                                    409,
                                    "missing-dependency",
                                    REJECTED_KEY,
                                );
                            }
                        }
                        next.run(request).await
                    }
                },
            ));
            axum::serve(listener, router).await.unwrap()
        });
        Self {
            _root: root,
            server,
            runtime,
            task,
            endpoint,
            traffic,
            activity,
        }
    }
}

impl Fixture {
    fn bind(&self, store: &mut PersistentStore) {
        let device = self.server.add_device().unwrap();
        store
            .server_bind(&ServerConfig {
                directory: None,
                endpoint: self.endpoint.clone(),
                library_id: device.library_id,
                device_id: device.device_id,
                token: device.token,
            })
            .unwrap();
    }
    /// Cycle options whose activity the loopback server reads while it answers,
    /// so a label can be asserted against the request it was shown for.
    fn options(&self) -> CycleOptions {
        CycleOptions {
            cycle_items: Some(Arc::new(CycleItemCounter {
                total: AtomicU64::new(0),
                done: AtomicU64::new(0),
                activity: self.activity.clone(),
                processed: AtomicU64::new(0),
                expected: AtomicU64::new(0),
            })),
            ..CycleOptions::default()
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn put(store: &mut PersistentStore, key: &str, bytes: &[u8]) {
    let object = PayloadCas::new(store.repository_root())
        .unwrap()
        .prepare_bytes(bytes)
        .unwrap();
    store
        .asset_object_catalog()
        .register(
            &[
                crate::persistent_store::asset_object_catalog::AssetObjectRegistration {
                    object_hash: object.content_hash.clone(),
                    byte_size: object.byte_size,
                },
            ],
            1,
        )
        .unwrap();
    let alias = AssetAlias {
        key: key.into(),
        object_hash: Some(object.content_hash),
        kind: "asset".into(),
        size: bytes.len() as i64,
        mime: "image/png".into(),
        name: "synthetic".into(),
        ext: "png".into(),
        inlay_type: None,
        width: None,
        height: None,
        metadata: json!({}),
    };
    let revision = store.revision().unwrap();
    store.commit_asset_alias(&alias, revision).unwrap();
}

/// Runs a cycle the loopback server is set to reject, so the operation stays
/// reserved for the next resume.
fn rejected(store: &mut PersistentStore, options: &CycleOptions) {
    assert!(
        store.server_cycle(options).is_err(),
        "the publication was expected to fail"
    );
}

fn verification(store: &PersistentStore) -> (Option<i64>, Option<String>) {
    let pending = store.server_pending().unwrap().expect("pending operation");
    (pending.verified_at, pending.verified_stage)
}

#[test]
fn a_recorded_object_pass_is_skipped_on_the_next_resume_and_runs_again_after_the_window() {
    let fixture = Fixture::new();
    let (_dir, mut store) = prepared();
    put(&mut store, "assets/synthetic-verification.png", &[7; 4096]);
    fixture.bind(&mut store);
    let options = fixture.options();
    fixture.traffic.reject_commit.store(true, Ordering::Relaxed);

    rejected(&mut store, &options);
    assert!(store.server_pending().unwrap().is_some());
    // The object goes out on the first attempt, and every frame leaves while the
    // label reads uploading rather than verifying.
    let frames = fixture.traffic.frames.load(Ordering::Relaxed);
    assert!(frames > 0);
    assert_eq!(
        fixture.traffic.frames_while_uploading.load(Ordering::Relaxed),
        frames
    );
    assert_eq!(verification(&store), (None, None));

    // The resume walks the records once and records the pass it completed.
    fixture.traffic.reset();
    rejected(&mut store, &options);
    assert!(fixture.traffic.missing.load(Ordering::Relaxed) > 0);
    assert_eq!(
        fixture.traffic.missing_while_verifying.load(Ordering::Relaxed),
        fixture.traffic.missing.load(Ordering::Relaxed)
    );
    // Nothing is missing any more, so the verifying pass sends no bytes.
    assert_eq!(fixture.traffic.frames.load(Ordering::Relaxed), 0);
    let (verified_at, verified_stage) = verification(&store);
    let stage = store
        .server_pending()
        .unwrap()
        .unwrap()
        .intent
        .staged_changes_id;
    assert_eq!(verified_stage.as_deref(), Some(stage.as_str()));
    assert!(verified_at.is_some());

    // The next resume trusts it and goes straight to the request that failed.
    fixture.traffic.reset();
    rejected(&mut store, &options);
    assert_eq!(fixture.traffic.object_requests(), 0);
    assert_eq!(verification(&store), (verified_at, verified_stage));

    // Past the reuse window the leases can no longer be trusted, so it runs.
    store
        .connection
        .execute(
            "UPDATE server_sync_operation SET verified_at=?1",
            [verified_at.unwrap() - 21 * 60 * 60],
        )
        .unwrap();
    fixture.traffic.reset();
    rejected(&mut store, &options);
    assert!(fixture.traffic.missing.load(Ordering::Relaxed) > 0);

    fixture.traffic.reject_commit.store(false, Ordering::Relaxed);
    assert_eq!(settle(&mut store).phase, "idle");
    assert!(store.server_pending().unwrap().is_none());
}

#[test]
fn a_skipped_pass_is_trusted_once_and_a_restage_clears_the_marker() {
    let fixture = Fixture::new();
    let (_dir, mut store) = prepared();
    put(&mut store, "assets/synthetic-recovery.png", &[11; 4096]);
    fixture.bind(&mut store);
    let options = fixture.options();
    fixture.traffic.reject_commit.store(true, Ordering::Relaxed);
    rejected(&mut store, &options);
    rejected(&mut store, &options);
    let recorded = verification(&store);
    assert!(recorded.0.is_some());

    // The server answering that an object it confirmed is gone buys exactly one
    // more pass. The second refusal is reported instead of looping.
    fixture.traffic.reset();
    fixture.traffic.page_faults.store(2, Ordering::Relaxed);
    rejected(&mut store, &options);
    assert_eq!(fixture.traffic.page_faults.load(Ordering::Relaxed), 0);
    assert_eq!(fixture.traffic.page_puts.load(Ordering::Relaxed), 2);
    assert_eq!(fixture.traffic.missing.load(Ordering::Relaxed), 1);
    // The refusal that stops the cycle names the record in this device's own log.
    assert!(crate::native_log::global_state()
        .tail(None)
        .iter()
        .any(|entry| entry.target == "server-sync"
            && entry.message.contains(REJECTED_KEY)
            && entry.message.contains("missing-dependency")
            && entry.message.contains("status=409")));

    // The recovery pass records itself, so the resume after it skips again.
    fixture.traffic.reset();
    rejected(&mut store, &options);
    assert_eq!(fixture.traffic.object_requests(), 0);
    assert_eq!(fixture.traffic.page_puts.load(Ordering::Relaxed), 1);

    // A stage the server no longer holds is restaged, and the pass runs against
    // the new one because nothing was confirmed for it.
    fixture.traffic.reset();
    fixture.traffic.forget_stage.store(true, Ordering::Relaxed);
    rejected(&mut store, &options);
    assert!(fixture.traffic.missing.load(Ordering::Relaxed) > 0);
    let restaged = verification(&store);
    assert_ne!(restaged.1, recorded.1);

    fixture.traffic.reject_commit.store(false, Ordering::Relaxed);
    assert_eq!(settle(&mut store).phase, "idle");
}

fn operation_records(store: &PersistentStore) -> i64 {
    store
        .connection
        .query_row(
            "SELECT count(*) FROM server_sync_operation_records",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

fn markers(store: &PersistentStore) -> i64 {
    store
        .connection
        .query_row(
            "SELECT count(*) FROM server_sync_dirty WHERE kind='full'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn the_first_whole_set_activation_ends_the_full_comparison() {
    let fixture = Fixture::new();
    let (_dir, mut store) = prepared();
    fixture.bind(&mut store);
    assert!(store.server_status().unwrap().full_scan);
    put(&mut store, "assets/synthetic-first-scan.png", &[23; 2048]);
    let changes = store.server_status().unwrap().dirty_records;
    assert!(changes > 0);
    // The marker asks for a comparison. It is not a change waiting to be sent,
    // so the value the user reads stays the same.
    store
        .connection
        .execute("INSERT INTO server_sync_dirty VALUES('full','','',1)", [])
        .unwrap();
    assert_eq!(markers(&store), 1);
    assert_eq!(store.server_status().unwrap().dirty_records, changes);

    // The first publication still carries the whole set.
    fixture.traffic.reject_commit.store(true, Ordering::Relaxed);
    rejected(&mut store, &fixture.options());
    let whole = operation_records(&store);
    assert!(whole > changes);
    fixture.traffic.reject_commit.store(false, Ordering::Relaxed);
    assert_eq!(settle(&mut store).phase, "idle");
    let status = store.server_status().unwrap();
    assert!(!status.full_scan);
    assert_eq!(status.dirty_records, 0);
    assert_eq!(markers(&store), 0);

    // From here a publication carries the outbox, not the whole set again, and
    // a rejected one leaves the comparison finished.
    put(&mut store, "assets/synthetic-after-scan.png", &[29; 2048]);
    let edited = store.server_status().unwrap().dirty_records;
    assert!(edited > 0 && edited < whole);
    fixture.traffic.reject_commit.store(true, Ordering::Relaxed);
    rejected(&mut store, &fixture.options());
    assert_eq!(operation_records(&store), edited);
    assert!(!store.server_status().unwrap().full_scan);
    assert_eq!(markers(&store), 0);

    fixture.traffic.reject_commit.store(false, Ordering::Relaxed);
    assert_eq!(settle(&mut store).phase, "idle");
    assert_eq!(store.server_status().unwrap().dirty_records, 0);
}

fn projections(store: &PersistentStore) -> std::collections::BTreeMap<String, i64> {
    let mut statement = store
        .connection
        .prepare("SELECT key,revision FROM server_sync_prepared")
        .unwrap();
    let rows = statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .unwrap();
    rows.collect::<std::result::Result<_, _>>().unwrap()
}

#[test]
fn projection_rows_outlive_publication_and_are_replaced_one_key_at_a_time() {
    const KEY: &str = "assets/synthetic-projection.png";
    let fixture = Fixture::new();
    let (_dir, mut store) = prepared();
    put(&mut store, KEY, &[31; 2048]);
    fixture.bind(&mut store);
    assert_eq!(settle(&mut store).phase, "idle");
    let published = projections(&store);
    assert!(published.len() > 1);
    let wire = published
        .keys()
        .find(|key| key.starts_with("r1:asset:"))
        .expect("the published asset keeps a projection row")
        .clone();

    // Reading the device's own commit back leaves every projection in place.
    assert_eq!(settle(&mut store).phase, "idle");
    assert_eq!(projections(&store), published);

    // A local edit replaces exactly the row for the key it changed.
    put(&mut store, KEY, &[41; 2048]);
    assert_eq!(settle(&mut store).phase, "idle");
    let edited = projections(&store);
    assert!(edited[&wire] > published[&wire]);
    for (key, revision) in &published {
        if key != &wire {
            assert_eq!(edited.get(key), Some(revision), "{key} was re-projected");
        }
    }

    // Restore and replacement change the generation, which invalidates all of it.
    store
        .connection
        .execute(
            "UPDATE server_sync_prepared SET generation='synthetic-other-generation'",
            [],
        )
        .unwrap();
    assert_eq!(settle(&mut store).phase, "idle");
    assert!(projections(&store).is_empty());
}

#[test]
fn an_applied_record_drops_the_projection_this_device_held_for_it() {
    const KEY: &str = "assets/synthetic-applied.png";
    let fixture = Fixture::new();
    let (_first_dir, mut first) = prepared();
    fixture.bind(&mut first);
    assert_eq!(settle(&mut first).phase, "idle");
    let second_dir = tempfile::tempdir().unwrap();
    let mut second = PersistentStore::open(second_dir.path()).unwrap();
    fixture.bind(&mut second);
    assert_eq!(settle(&mut second).phase, "idle");

    put(&mut second, KEY, &[53; 2048]);
    assert_eq!(settle(&mut second).phase, "idle");
    let held = projections(&second);
    let wire = held
        .keys()
        .find(|key| key.starts_with("r1:asset:"))
        .expect("the published asset keeps a projection row")
        .clone();

    assert_eq!(settle(&mut first).phase, "idle");
    put(&mut first, KEY, &[59; 2048]);
    assert_eq!(settle(&mut first).phase, "idle");

    // The record the server hands back replaces what this device projected, so
    // the superseded row cannot survive to be reused.
    assert_eq!(settle(&mut second).phase, "idle");
    let applied = projections(&second);
    assert!(!applied.contains_key(&wire));
    for (key, revision) in &held {
        if key != &wire {
            assert_eq!(applied.get(key), Some(revision), "{key} was re-projected");
        }
    }
}
