use super::*;
use crate::store::{Device, Store};

static FIXTURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Set on the child test process that runs the observer tests.
const OBSERVER_CHILD: &str = "RISUNEST_SOURCE_OBSERVER_CHILD";

pub(crate) struct Reset {
    _fixture: std::sync::MutexGuard<'static, ()>,
}
impl Reset {
    pub(crate) fn new() -> Self {
        assert!(
            std::env::var_os(OBSERVER_CHILD).is_some(),
            "observer tests run through observer_tests_run_alone_in_a_child_process"
        );
        let fixture = FIXTURE.lock_unpoisoned();
        install();
        assert!(registry().lock_unpoisoned().active.is_none());
        Self { _fixture: fixture }
    }
}
impl Drop for Reset {
    fn drop(&mut self) {
        // The scope leaves the registry before any check, so a failed check
        // cannot leak it into the next test.
        let active = registry().lock_unpoisoned().active.take();
        if std::thread::panicking() {
            return;
        }
        if let Some(context) = active {
            let scope = context.scope.lock_unpoisoned();
            assert_eq!(scope.workers, 0);
            assert_eq!(scope.requests, 0);
            assert!(scope
                .rows
                .values()
                .all(|row| row.work.outstanding_readers == 0));
            assert!(scope
                .read_barrier
                .as_ref()
                .is_none_or(|b| b.observation().waiters == 0));
        }
    }
}
/// The registry is process-wide, and work a test does without a scope of its
/// own is charged to whichever scope is active. The observer tests therefore
/// run alone in a child test process, one at a time, and the rest of the
/// suite stays parallel.
#[test]
fn observer_tests_run_alone_in_a_child_process() {
    let run = |listing: bool| {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--test-threads=1"])
            .args(listing.then_some("--list"))
            .args([
                "source_observer::tests::",
                "source_observer::read_barrier::tests::",
                "source_observer_harness::tests::",
            ])
            .env(OBSERVER_CHILD, "1")
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "{text}");
        text
    };
    let listed = run(true)
        .lines()
        .filter(|line| line.ends_with(": test"))
        .count();
    assert!(listed > 0);
    let report = run(false);
    assert!(
        report.contains(&format!("test result: ok. {listed} passed; 0 failed;")),
        "{report}"
    );
}

fn role(hash: &str, purposes: &[&str]) -> Role {
    Role {
        hash: hash.into(),
        purposes: purposes.iter().map(|v| (*v).into()).collect(),
    }
}
fn seeded() -> (tempfile::TempDir, Store, Device, Vec<u8>, Vec<u8>) {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let credential = store.add_device().unwrap();
    let device = Device {
        id: credential.device_id,
    };
    let inline = b"synthetic inline control".to_vec();
    let file = vec![17; 65_537];
    store
        .put_object(&device, &risunest_sync_wire::hash(&inline), &inline)
        .unwrap();
    store
        .put_object(&device, &risunest_sync_wire::hash(&file), &file)
        .unwrap();
    (root, store, device, inline, file)
}
fn conserved(snapshot: &Snapshot) {
    for axis in [&snapshot.placements, &snapshot.purposes, &snapshot.flows] {
        let mut total = Work::default();
        for row in axis.values() {
            assert!(!total.add(row));
        }
        assert_eq!(total, snapshot.total);
    }
    for row in &snapshot.objects {
        assert_eq!(
            row.read_ranges.iter().map(|r| r.bytes).sum::<u64>(),
            if row.placement == "inline" {
                0
            } else {
                row.work.read_bytes
            }
        );
    }
    assert_eq!(
        snapshot
            .hash_domains
            .values()
            .map(|w| w.hash_calls)
            .sum::<u64>(),
        snapshot.total.hash_calls
    );
    assert_eq!(
        snapshot
            .hash_domains
            .values()
            .map(|w| w.hash_bytes)
            .sum::<u64>(),
        snapshot.total.hash_bytes
    );
    assert_eq!(
        snapshot
            .hash_domains
            .values()
            .map(|w| w.hash_finalizations)
            .sum::<u64>(),
        snapshot.total.hash_finalizations
    );
}

#[test]
#[ignore = "runs in the observer child process"]
fn actual_file_inline_and_cross_thread_source_work_conserves() {
    let _reset = Reset::new();
    let (root, store, _, inline, file) = seeded();
    let inline_hash = risunest_sync_wire::hash(&inline);
    let file_hash = risunest_sync_wire::hash(&file);
    begin(
        root.path(),
        "positive".into(),
        "source-control".into(),
        vec![
            role(&inline_hash, &["Control"]),
            role(&file_hash, &["Control", "Asset"]),
        ],
    )
    .unwrap();
    assert_eq!(store.get_object(&inline_hash).unwrap(), inline);
    let worker = worker();
    std::thread::spawn(move || {
        let _enter = worker.enter();
        assert_eq!(store.get_object(&file_hash).unwrap(), file);
    })
    .join()
    .unwrap();
    let measured = snapshot(true).unwrap();
    assert!(measured.complete);
    conserved(&measured);
    assert_eq!(measured.total.opens, 2);
    assert_eq!(measured.total.read_bytes, 65_537 + inline.len() as u64);
    assert_eq!(measured.total.hash_calls, 2);
    assert_eq!(measured.total.hash_bytes, measured.total.read_bytes);
    assert_eq!(measured.purposes["Asset"].read_bytes, 65_537);
    assert_eq!(
        measured.hash_domains["inline-source-sha256"].hash_bytes,
        inline.len() as u64
    );
    assert_eq!(measured.pending_workers, 0);
}

#[test]
#[ignore = "runs in the observer child process"]
fn failed_prefix_hash_and_live_reader_cannot_be_reset() {
    let _reset = Reset::new();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("prefix");
    let bytes = vec![7; 97];
    std::fs::write(&path, &bytes).unwrap();
    let hash = risunest_sync_wire::hash(&bytes);
    begin(
        root.path(),
        "prefix".into(),
        "error".into(),
        vec![role(&hash, &["Asset"])],
    )
    .unwrap();
    let _operation = operation(root.path());
    let mut file = TrackedFile::open(root.path(), &hash, &path, "file").unwrap();
    let expected = shared_wire::delta::Base {
        hash: hash.clone(),
        size: 100,
    };
    assert!(shared_wire::stream_delta::verify(&mut file, &expected, &mut || Ok(())).is_err());
    let live = snapshot(true).unwrap();
    assert!(!live.complete);
    assert_eq!(live.total.read_bytes, 97);
    assert_eq!(live.total.hash_bytes, 97);
    assert_eq!(live.total.failed_hashes, 1);
    assert_eq!(
        begin(root.path(), "reset".into(), "test".into(), vec![]),
        Err("scope-not-settled")
    );
    drop(file);
    assert_eq!(
        begin(root.path(), "reset".into(), "test".into(), vec![]),
        Err("prior-scope-incomplete")
    );
    let ended = snapshot(false).unwrap();
    conserved(&ended);
    assert_eq!(ended.total.read_bytes, 97);
}

#[test]
#[ignore = "runs in the observer child process"]
fn unknown_object_failed_open_and_pending_drop_are_incomplete() {
    let _reset = Reset::new();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("body");
    std::fs::write(&path, b"fixture").unwrap();
    let hash = risunest_sync_wire::hash(b"fixture");
    begin(root.path(), "unknown".into(), "error".into(), vec![]).unwrap();
    {
        let mut file = TrackedFile::open(root.path(), &hash, &path, "file").unwrap();
        let mut bytes = [0; 3];
        assert_eq!(file.read(&mut bytes).unwrap(), 3);
        file.reader.pending_drop();
    }
    assert!(TrackedFile::open(root.path(), &hash, &root.path().join("absent"), "file").is_err());
    let measured = snapshot(true).unwrap();
    assert!(!measured.complete);
    assert_eq!(measured.unknown_work.read_bytes, 3);
    assert_eq!(measured.total.failed_opens, 1);
    assert_eq!(measured.total.outstanding_readers, 0);
    assert_eq!(measured.total.unknown_read_results, 1);
    conserved(&measured);
}

#[test]
#[ignore = "runs in the observer child process"]
fn worker_lifetime_late_work_and_overflow_fail_closed() {
    let _reset = Reset::new();
    let root = tempfile::tempdir().unwrap();
    let hash = risunest_sync_wire::hash(b"scope");
    begin(
        root.path(),
        "worker".into(),
        "error".into(),
        vec![role(&hash, &["Asset"])],
    )
    .unwrap();
    let worker = worker();
    assert_eq!(snapshot(true).unwrap().pending_workers, 1);
    assert_eq!(
        begin(root.path(), "reset".into(), "test".into(), vec![]),
        Err("scope-not-settled")
    );
    {
        let _enter = worker.enter();
        hashed(&hash, "test-actual-boundary", 5);
    }
    drop(worker);
    let measured = snapshot(false).unwrap();
    assert!(!measured.complete);
    assert_eq!(measured.total.hash_bytes, 5);
    assert!(measured.violations.contains_key("late-work"));
    let context = active().unwrap();
    context.update(
        &hash,
        "memory",
        Work {
            hash_bytes: u64::MAX,
            ..Default::default()
        },
        Some("test-actual-boundary"),
        None,
    );
    assert!(snapshot(false)
        .unwrap()
        .violations
        .contains_key("counter-overflow"));
}

#[test]
#[ignore = "runs in the observer child process"]
fn ingress_inline_comparison_is_separate_and_corruption_preserves_body_work() {
    let _reset = Reset::new();
    let (root, store, device, inline, _) = seeded();
    let hash = risunest_sync_wire::hash(&inline);
    begin(
        root.path(),
        "ingress".into(),
        "upload".into(),
        vec![role(&hash, &["Control"])],
    )
    .unwrap();
    store.put_object(&device, &hash, &inline).unwrap();
    let measured = snapshot(true).unwrap();
    assert!(measured.complete);
    assert_eq!(measured.flows.len(), 1);
    assert_eq!(measured.flows["ingress"].read_bytes, inline.len() as u64);
    conserved(&measured);
    begin(
        root.path(),
        "corrupt".into(),
        "error".into(),
        vec![role(&hash, &["Control"])],
    )
    .unwrap();
    let db = rusqlite::Connection::open(root.path().join("metadata.sqlite")).unwrap();
    db.execute(
        "UPDATE small_objects SET body=?1 WHERE hash=?2",
        rusqlite::params![vec![0u8; inline.len()], hash],
    )
    .unwrap();
    assert!(store.get_object(&hash).is_err());
    let measured = snapshot(true).unwrap();
    assert!(!measured.complete);
    assert_eq!(measured.total.read_bytes, inline.len() as u64);
    assert_eq!(measured.total.hash_bytes, inline.len() as u64);
    assert_eq!(measured.total.failed_hashes, 1);
    conserved(&measured);
}

#[test]
#[ignore = "runs in the observer child process"]
fn structural_recipe_adapter_preserves_actual_encode_and_negative_results() {
    let _reset = Reset::new();
    use risunest_sync_wire::{
        delta,
        transfer::{self, Frame},
    };
    let base = b"0123456789";
    let recipe = delta::Recipe {
        bases: vec![delta::Base {
            hash: risunest_sync_wire::hash(base),
            size: 10,
        }],
        target_hash: risunest_sync_wire::hash(b"34tail"),
        target_size: 6,
        ops: vec![
            delta::Op::Copy {
                base: 0,
                offset: 3,
                length: 2,
            },
            delta::Op::Insert(b"tail".to_vec()),
        ],
    };
    let linked = shared_wire::from_wire(recipe.clone());
    assert_eq!(linked.encode().unwrap(), recipe.encode().unwrap());
    assert_eq!(
        linked.apply(&[base]).unwrap(),
        recipe.apply(&[base]).unwrap()
    );
    assert_eq!(
        transfer::encode(&[Frame::Delta(shared_wire::to_wire(linked))]).unwrap(),
        transfer::encode(&[Frame::Delta(recipe.clone())]).unwrap()
    );
    let normal = transfer::encode(&[
        Frame::Full(base.to_vec()),
        Frame::Delta(recipe.clone()),
        Frame::FullRequired {
            hash: risunest_sync_wire::hash(b"required"),
            size: 777,
        },
    ])
    .unwrap();
    let linked = shared_wire::transfer::encode(&[
        shared_wire::transfer::Frame::Full(base.to_vec()),
        shared_wire::transfer::Frame::Delta(shared_wire::from_wire(recipe.clone())),
        shared_wire::transfer::Frame::FullRequired {
            hash: risunest_sync_wire::hash(b"required"),
            size: 777,
        },
    ])
    .unwrap();
    assert_eq!(normal, linked);
    assert_eq!(
        shared_wire::transfer::encode(&shared_wire::transfer::decode(&normal).unwrap()).unwrap(),
        normal
    );
    for bytes in [
        b"invalid".to_vec(),
        b"RNSB\0\0\x04\x01".to_vec(),
        normal[..normal.len() - 1].to_vec(),
        {
            let mut bad = normal.clone();
            bad[45] ^= 1;
            bad
        },
    ] {
        assert_eq!(
            shared_wire::transfer::decode(&bytes).unwrap_err().0,
            transfer::decode(&bytes).unwrap_err().0
        );
    }
    let mut negatives = Vec::new();
    let mut bad = recipe.clone();
    bad.target_size += 1;
    negatives.push(bad);
    let mut bad = recipe.clone();
    bad.ops[0] = delta::Op::Copy {
        base: 1,
        offset: 3,
        length: 2,
    };
    negatives.push(bad);
    let mut bad = recipe.clone();
    bad.bases[0].size = 1;
    negatives.push(bad);
    let mut bad = recipe.clone();
    bad.target_hash = "x".repeat(64);
    negatives.push(bad);
    let mut bad = recipe;
    bad.ops[1] = delta::Op::Insert(vec![]);
    negatives.push(bad);
    for bad in negatives {
        let linked = shared_wire::from_wire(bad.clone());
        assert_eq!(linked.encode().unwrap_err().0, bad.encode().unwrap_err().0);
        assert_eq!(
            transfer::encode(&[Frame::Delta(shared_wire::to_wire(linked))])
                .unwrap_err()
                .0,
            transfer::encode(&[Frame::Delta(bad)]).unwrap_err().0
        );
    }
}

#[test]
#[ignore = "runs in the observer child process"]
fn actual_fitting_full_and_delta_transfer_sha_domains_are_observed() {
    use crate::store::TransferRequest;
    let _reset = Reset::new();
    let (root, store, device, inline, file) = seeded();
    let file_hash = risunest_sync_wire::hash(&file);
    let inline_hash = risunest_sync_wire::hash(&inline);
    begin(
        root.path(),
        "full-transfer".into(),
        "source".into(),
        vec![role(&file_hash, &["Asset"])],
    )
    .unwrap();
    let encoded = store
        .transfer_objects(
            &device,
            &[TransferRequest {
                target: file_hash.clone(),
                bases: vec![],
            }],
        )
        .unwrap();
    assert!(
        matches!(risunest_sync_wire::transfer::decode(&encoded).unwrap().as_slice(),[risunest_sync_wire::transfer::Frame::Full(bytes)] if bytes==&file)
    );
    let full = snapshot(true).unwrap();
    assert!(full.complete);
    assert_eq!(full.total.opens, 1);
    assert_eq!(full.total.read_bytes, file.len() as u64);
    assert_eq!(
        full.hash_domains["object-source-sha256"].hash_bytes,
        file.len() as u64
    );
    assert_eq!(
        full.hash_domains["transfer-full-encode-sha256"].hash_bytes,
        2 * file.len() as u64
    );
    assert_eq!(
        full.hash_domains["transfer-full-encode-sha256"].hash_calls,
        2
    );
    conserved(&full);
    begin(
        root.path(),
        "frame-ingress".into(),
        "ingress".into(),
        vec![role(&inline_hash, &["Control"])],
    )
    .unwrap();
    let bytes = risunest_sync_wire::transfer::encode(&[risunest_sync_wire::transfer::Frame::Full(
        inline.clone(),
    )])
    .unwrap();
    store.receive_frames(&device, &bytes).unwrap();
    let ingress = snapshot(true).unwrap();
    assert!(ingress.complete);
    assert_eq!(ingress.flows.len(), 1);
    assert!(ingress.flows.contains_key("ingress"));
    assert_eq!(
        ingress.hash_domains["transfer-full-decode-sha256"].hash_bytes,
        inline.len() as u64
    );
    conserved(&ingress);
}

#[test]
#[ignore = "runs in the observer child process"]
fn actual_streamed_download_and_chunk_upload_hash_domains_conserve() {
    use crate::store::{DeltaProgress, TransferRequest, UploadManifest};
    let _reset = Reset::new();
    let (root, store, device, _, base) = seeded();
    let mut target = base.clone();
    target.extend_from_slice(b"synthetic appended target");
    let base_hash = risunest_sync_wire::hash(&base);
    let target_hash = risunest_sync_wire::hash(&target);
    store.put_object(&device, &target_hash, &target).unwrap();
    let request = TransferRequest {
        target: target_hash.clone(),
        bases: vec![base_hash.clone()],
    };
    let download = store.begin_download_delta(&device, &request).unwrap();
    begin(
        root.path(),
        "stream-source".into(),
        "source".into(),
        vec![role(&base_hash, &["Asset"]), role(&target_hash, &["Asset"])],
    )
    .unwrap();
    let worker = worker();
    std::thread::spawn(move || {
        let _entered = worker.enter();
        assert!(store.run_pending_download_delta().unwrap());
        (store, device)
    })
    .join()
    .map(|(store, device)| {
        assert!(matches!(
            store.download_delta_progress(&device, &download).unwrap(),
            DeltaProgress::Ready(_)
        ));
        let stream = snapshot(true).unwrap();
        assert!(stream.complete, "{:?}", stream.violations);
        assert_eq!(stream.total.opens, 2);
        assert!(stream.total.read_bytes >= base.len() as u64 + target.len() as u64);
        assert_eq!(
            stream.hash_domains["stream-create-base-sha256"].hash_bytes,
            base.len() as u64
        );
        assert_eq!(
            stream.hash_domains["stream-verify-sha256"].hash_bytes,
            target.len() as u64
        );
        assert_eq!(stream.total.hash_calls, 2);
        assert_eq!(stream.total.hash_finalizations, 2);
        assert_eq!(
            stream.total.hash_bytes,
            base.len() as u64 + target.len() as u64
        );
        conserved(&stream);
        let upload_body = vec![31; 65_541];
        let upload_hash = risunest_sync_wire::hash(&upload_body);
        let upload = store
            .begin_upload(
                &device,
                &UploadManifest {
                    hash: upload_hash.clone(),
                    size: (upload_body.len() as u64).into(),
                },
            )
            .unwrap();
        begin(
            root.path(),
            "chunk-ingress".into(),
            "ingress".into(),
            vec![role(&upload_hash, &["Asset"])],
        )
        .unwrap();
        store
            .put_upload_chunk(&device, &upload, 0, &upload_hash, &upload_body)
            .unwrap();
        assert_eq!(store.finish_upload(&device, &upload).unwrap(), upload_hash);
        let ingress = snapshot(true).unwrap();
        assert!(ingress.complete, "{:?}", ingress.violations);
        assert_eq!(ingress.flows.len(), 1);
        assert!(ingress.flows.contains_key("ingress"));
        assert_eq!(
            ingress.placements["upload-chunk"].read_bytes,
            upload_body.len() as u64
        );
        for domain in [
            "upload-chunk-ingress-sha256",
            "upload-chunk-sha256",
            "upload-full-sha256",
        ] {
            assert_eq!(
                ingress.hash_domains[domain].hash_bytes,
                upload_body.len() as u64
            );
            assert_eq!(ingress.hash_domains[domain].hash_calls, 1);
        }
        conserved(&ingress);
    })
    .unwrap();
}

#[test]
#[ignore = "runs in the observer child process"]
fn actual_read_error_preserves_prefix_and_escaped_path_is_incomplete() {
    let _reset = Reset::new();
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let path = root.path().join("body");
    std::fs::write(&path, b"synthetic prefix").unwrap();
    let hash = risunest_sync_wire::hash(b"synthetic prefix");
    begin(
        root.path(),
        "read-error".into(),
        "error".into(),
        vec![role(&hash, &["Asset"])],
    )
    .unwrap();
    let mut file = TrackedFile::open(root.path(), &hash, &path, "file").unwrap();
    let mut prefix = [0; 3];
    assert_eq!(file.read(&mut prefix).unwrap(), 3);
    file.file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    assert!(file.read(&mut prefix).is_err());
    drop(file);
    let escaped = outside.path().join("body");
    std::fs::write(&escaped, b"synthetic prefix").unwrap();
    {
        let mut file = TrackedFile::open(root.path(), &hash, &escaped, "file").unwrap();
        assert_eq!(file.read(&mut prefix).unwrap(), 3);
    }
    let measured = snapshot(true).unwrap();
    assert!(!measured.complete);
    assert_eq!(measured.total.read_bytes, 6);
    assert_eq!(measured.total.failed_reads, 1);
    assert_eq!(measured.total.unknown_read_results, 1);
    assert!(measured.violations.contains_key("source-path-escape"));
    conserved(&measured);
}

#[test]
#[ignore = "runs in the observer child process"]
fn dropped_actual_pending_async_read_preserves_known_prefix() {
    use std::sync::mpsc;
    let _reset = Reset::new();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("body");
    std::fs::write(&path, b"synthetic pending bytes").unwrap();
    let hash = risunest_sync_wire::hash(b"synthetic pending bytes");
    begin(
        root.path(),
        "pending-read".into(),
        "error".into(),
        vec![role(&hash, &["Asset"])],
    )
    .unwrap();
    let mut file = TrackedFile::open(root.path(), &hash, &path, "file").unwrap();
    let mut prefix = [0; 3];
    assert_eq!(file.read(&mut prefix).unwrap(), 3);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let (ready_send, ready_receive) = mpsc::channel();
    let (release_send, release_receive) = mpsc::channel();
    let blocking = runtime.spawn_blocking(move || {
        ready_send.send(()).unwrap();
        release_receive.recv().unwrap();
    });
    ready_receive.recv().unwrap();
    runtime.block_on(async {
        let mut file = file.into_async();
        let waker = futures_util::task::noop_waker();
        let mut cx = TaskContext::from_waker(&waker);
        let mut bytes = [0; 8];
        let mut buffer = ReadBuf::new(&mut bytes);
        assert!(Pin::new(&mut file)
            .poll_read(&mut cx, &mut buffer)
            .is_pending());
        drop(file);
    });
    release_send.send(()).unwrap();
    runtime.block_on(blocking).unwrap();
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    let measured = snapshot(true).unwrap();
    assert!(!measured.complete);
    assert_eq!(measured.total.read_bytes, 3);
    assert_eq!(measured.total.unknown_read_results, 1);
    assert_eq!(measured.total.outstanding_readers, 0);
    conserved(&measured);
}

#[test]
#[ignore = "runs in the observer child process"]
fn noncontiguous_actual_reads_and_selections_keep_every_range() {
    let _reset = Reset::new();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("body");
    let bytes = b"synthetic repeated range";
    std::fs::write(&path, bytes).unwrap();
    let hash = risunest_sync_wire::hash(bytes);
    begin(
        root.path(),
        "every-range".into(),
        "source".into(),
        vec![role(&hash, &["Asset"])],
    )
    .unwrap();
    let mut file = TrackedFile::open(root.path(), &hash, &path, "file").unwrap();
    for _ in 0..4097 {
        file.seek(SeekFrom::Start(0)).unwrap();
        file.reader.selected(0, 1);
        assert_eq!(file.read(&mut [0; 1]).unwrap(), 1);
    }
    drop(file);
    let measured = snapshot(true).unwrap();
    assert!(measured.complete);
    assert_eq!(measured.objects[0].read_ranges.len(), 4097);
    assert_eq!(measured.objects[0].selected_ranges.len(), 4097);
    assert_eq!(measured.total.read_calls, 4097);
    assert_eq!(measured.total.read_bytes, 4097);
    conserved(&measured);
}

#[test]
#[ignore = "runs in the observer child process"]
fn actual_failed_seek_preserves_prefix_and_is_incomplete() {
    let _reset = Reset::new();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("body");
    let bytes = b"synthetic seek prefix";
    std::fs::write(&path, bytes).unwrap();
    let hash = risunest_sync_wire::hash(bytes);
    begin(
        root.path(),
        "seek-error".into(),
        "error".into(),
        vec![role(&hash, &["Asset"])],
    )
    .unwrap();
    let mut file = TrackedFile::open(root.path(), &hash, &path, "file").unwrap();
    assert_eq!(file.read(&mut [0; 3]).unwrap(), 3);
    assert!(file.seek(SeekFrom::Start(u64::MAX)).is_err());
    drop(file);
    let measured = snapshot(true).unwrap();
    assert!(!measured.complete);
    assert_eq!(measured.total.read_bytes, 3);
    assert!(measured.violations.contains_key("source-seek-error"));
    conserved(&measured);
}

#[test]
#[ignore = "runs in the observer child process"]
fn actual_unresolved_and_mismatched_source_roots_cannot_report_complete_zero() {
    let _reset = Reset::new();
    let (root, store, _, inline, _) = seeded();
    let foreign = tempfile::tempdir().unwrap();
    let inline_hash = risunest_sync_wire::hash(&inline);
    let bytes = b"synthetic rooted body";
    let path = root.path().join("body");
    std::fs::write(&path, bytes).unwrap();
    begin(
        root.path(),
        "root-errors".into(),
        "error".into(),
        vec![role(&inline_hash, &["Control"])],
    )
    .unwrap();
    {
        let mut file = TrackedFile::open(
            &root.path().join("absent-parent/absent-root"),
            &inline_hash,
            &path,
            "file",
        )
        .unwrap();
        assert_eq!(file.read(&mut [0; 3]).unwrap(), 3);
    }
    {
        let mut file = TrackedFile::open(foreign.path(), &inline_hash, &path, "file").unwrap();
        assert_eq!(file.read(&mut [0; 3]).unwrap(), 3);
    }
    assert_eq!(store.get_object(&inline_hash).unwrap(), inline);
    let measured = snapshot(true).unwrap();
    assert!(!measured.complete);
    assert!(measured.violations.contains_key("source-root-unresolved"));
    assert!(measured.violations.contains_key("source-root-mismatch"));
    assert_eq!(measured.total.read_bytes, inline.len() as u64);
    assert!(begin(root.path(), "reset".into(), "error".into(), vec![]).is_err());
    conserved(&measured);
}

#[test]
#[ignore = "runs in the observer child process"]
fn actual_inline_sql_with_no_root_and_unattributed_upload_sha_fail_closed() {
    use crate::store::UploadManifest;
    let _reset = Reset::new();
    let root = tempfile::tempdir().unwrap();
    let foreign = tempfile::tempdir().unwrap();
    let store = Store::init(foreign.path()).unwrap();
    let credential = store.add_device().unwrap();
    let device = Device {
        id: credential.device_id,
    };
    let body = vec![59; 65_541];
    let hash = risunest_sync_wire::hash(&body);
    let upload = store
        .begin_upload(
            &device,
            &UploadManifest {
                hash: hash.clone(),
                size: (body.len() as u64).into(),
            },
        )
        .unwrap();
    store
        .put_upload_chunk(&device, &upload, 0, &hash, &body)
        .unwrap();
    let mut memory = rusqlite::Connection::open_in_memory().unwrap();
    small_object_store::initialize(&memory).unwrap();
    let inline = b"synthetic rootless inline";
    let inline_hash = risunest_sync_wire::hash(inline);
    {
        let tx = memory.transaction().unwrap();
        small_object_store::insert_batch(&tx, &[(&inline_hash, inline)]).unwrap();
        tx.commit().unwrap();
    }
    begin(
        root.path(),
        "missing-context".into(),
        "error".into(),
        vec![role(&hash, &["Asset"]), role(&inline_hash, &["Control"])],
    )
    .unwrap();
    assert_eq!(
        small_object_store::read(&memory, &inline_hash, 1024)
            .unwrap()
            .unwrap(),
        inline
    );
    assert_eq!(store.finish_upload(&device, &upload).unwrap(), hash);
    let measured = snapshot(true).unwrap();
    assert!(!measured.complete);
    assert!(
        measured.violations.contains_key("inline-origin-missing")
            || measured.violations.contains_key("inline-root-missing")
            || measured.violations.contains_key("source-root-unresolved")
    );
    assert!(measured.violations.contains_key("source-root-mismatch"));
    assert!(measured.violations.contains_key("unattributed-hash"));
    for domain in ["upload-full-sha256", "upload-chunk-sha256"] {
        assert_eq!(measured.hash_domains[domain].hash_calls, 1);
        assert_eq!(measured.hash_domains[domain].hash_bytes, body.len() as u64);
    }
    conserved(&measured);
}

#[test]
#[ignore = "runs in the observer child process"]
fn actual_reader_opened_before_scope_cannot_hide_reads_inside_scope() {
    let _reset = Reset::new();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("body");
    let bytes = b"synthetic unscoped reader";
    std::fs::write(&path, bytes).unwrap();
    let hash = risunest_sync_wire::hash(bytes);
    let mut file = TrackedFile::open(root.path(), &hash, &path, "file").unwrap();
    assert_eq!(file.read(&mut [0; 3]).unwrap(), 3);
    assert!(active().is_none());
    begin(
        root.path(),
        "unscoped-reader".into(),
        "error".into(),
        vec![role(&hash, &["Asset"])],
    )
    .unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    assert_eq!(file.read(&mut [0; 3]).unwrap(), 3);
    drop(file);
    let measured = snapshot(true).unwrap();
    assert!(!measured.complete);
    assert_eq!(measured.total, Work::default());
    assert!(measured.violations.contains_key("unattributed-source-read"));
    assert!(measured.violations.contains_key("unattributed-source-seek"));
    assert!(begin(root.path(), "reset".into(), "error".into(), vec![]).is_err());
}

#[test]
#[ignore = "runs in the observer child process"]
fn actual_unscoped_pending_async_seek_cannot_report_complete_zero() {
    let _reset = Reset::new();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("body");
    let bytes = b"synthetic unscoped pending seek";
    std::fs::write(&path, bytes).unwrap();
    let hash = risunest_sync_wire::hash(bytes);
    let file = TrackedFile::open(root.path(), &hash, &path, "file").unwrap();
    begin(
        root.path(),
        "unscoped-seek".into(),
        "error".into(),
        vec![role(&hash, &["Asset"])],
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut file = file.into_async();
        Pin::new(&mut file).start_seek(SeekFrom::Start(0)).unwrap();
        drop(file);
    });
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    let measured = snapshot(true).unwrap();
    assert!(!measured.complete);
    assert_eq!(measured.total, Work::default());
    assert!(measured.violations.contains_key("unattributed-source-seek"));
    assert!(begin(root.path(), "reset".into(), "error".into(), vec![]).is_err());
}

#[test]
#[ignore = "runs in the observer child process"]
fn actual_sync_first_read_barrier_is_one_shot_and_scoped() {
    let _reset = Reset::new();
    let (root, store, _, _, body) = seeded();
    let hash = risunest_sync_wire::hash(&body);
    let generation = begin(
        root.path(),
        "sync-held".into(),
        "asset-transfer".into(),
        vec![role(&hash, &["Asset"])],
    )
    .unwrap();
    let intent = read_barrier::Intent {
        scope_id: "sync-held".into(),
        generation,
        barrier_id: "sync-first-read".into(),
        hash: hash.clone(),
        flow: "source".into(),
    };
    let (send, receive) = std::sync::mpsc::channel();
    read_barrier::install_hook(Some(Arc::new(move |event| {
        send.send(serde_json::to_value(event).unwrap()).unwrap()
    })));
    assert_eq!(read_barrier::arm(intent.clone()).unwrap().status, "armed");
    assert!(read_barrier::release(&intent).is_err());
    let worker = worker();
    let read_hash = hash.clone();
    let task = std::thread::spawn(move || {
        let _entered = worker.enter();
        store.get_object(&read_hash)
    });
    let event = receive
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    assert_eq!(event["type"], "readBarrierReached");
    assert_eq!(event["readKind"], "stdRead");
    assert_eq!(event["hash"], hash);
    assert_eq!(event["offset"], 0);
    let held = snapshot(false).unwrap();
    assert!(!held.complete);
    assert_eq!(held.total.read_bytes, 0);
    assert_eq!(held.total.outstanding_readers, 1);
    assert_eq!(held.pending_workers, 1);
    assert_eq!(held.read_barrier.unwrap().waiters, 1);
    let mut wrong = intent.clone();
    wrong.barrier_id = "wrong-id".into();
    assert!(read_barrier::release(&wrong).is_err());
    wrong = intent.clone();
    wrong.hash = "0".repeat(64);
    assert!(read_barrier::release(&wrong).is_err());
    wrong = intent.clone();
    wrong.generation += 1;
    assert!(read_barrier::release(&wrong).is_err());
    assert_eq!(read_barrier::release(&intent).unwrap().status, "released");
    assert_eq!(task.join().unwrap().unwrap(), body);
    assert!(read_barrier::release(&intent).is_err());
    let done = snapshot(true).unwrap();
    assert!(done.complete);
    assert_eq!(done.total.read_bytes, body.len() as u64);
    assert_eq!(done.read_barrier.unwrap().waiters, 0);
    conserved(&snapshot(false).unwrap());
    let next = begin(
        root.path(),
        "sync-next".into(),
        "source".into(),
        vec![role(&hash, &["Asset"])],
    )
    .unwrap();
    let reused = read_barrier::Intent {
        scope_id: "sync-next".into(),
        generation: next,
        ..intent
    };
    assert!(read_barrier::arm(reused).is_err());
    assert!(snapshot(true).unwrap().complete);
}

#[test]
#[ignore = "runs in the observer child process"]
fn actual_held_sync_read_cancel_unblocks_and_reset_cannot_erase_failure() {
    let _reset = Reset::new();
    let (root, store, _, _, body) = seeded();
    let hash = risunest_sync_wire::hash(&body);
    let generation = begin(
        root.path(),
        "sync-cancel".into(),
        "asset-transfer".into(),
        vec![role(&hash, &["Asset"])],
    )
    .unwrap();
    let intent = read_barrier::Intent {
        scope_id: "sync-cancel".into(),
        generation,
        barrier_id: "sync-cancel-read".into(),
        hash: hash.clone(),
        flow: "source".into(),
    };
    let (send, receive) = std::sync::mpsc::channel();
    read_barrier::install_hook(Some(Arc::new(move |event| send.send(event).unwrap())));
    read_barrier::arm(intent.clone()).unwrap();
    let worker = worker();
    let task = std::thread::spawn(move || {
        let _entered = worker.enter();
        store.get_object(&hash)
    });
    receive
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    assert!(begin(
        root.path(),
        "premature-reset".into(),
        "source".into(),
        vec![]
    )
    .is_err());
    assert_eq!(read_barrier::cancel(&intent).unwrap().status, "cancelled");
    assert!(task.join().unwrap().is_err());
    let done = snapshot(true).unwrap();
    assert!(!done.complete);
    assert_eq!(done.total.read_bytes, 0);
    assert_eq!(done.read_barrier.unwrap().waiters, 0);
    assert!(begin(root.path(), "erase".into(), "source".into(), vec![]).is_err());
}

#[test]
#[ignore = "runs in the observer child process"]
fn actual_source_read_before_arm_and_nonasset_scope_are_rejected() {
    let _reset = Reset::new();
    let (root, store, _, _, body) = seeded();
    let hash = risunest_sync_wire::hash(&body);
    let generation = begin(
        root.path(),
        "arm-after-read".into(),
        "source".into(),
        vec![role(&hash, &["Asset"])],
    )
    .unwrap();
    assert_eq!(store.get_object(&hash).unwrap(), body);
    let intent = read_barrier::Intent {
        scope_id: "arm-after-read".into(),
        generation,
        barrier_id: "too-late-first-read".into(),
        hash: hash.clone(),
        flow: "source".into(),
    };
    assert!(read_barrier::arm(intent).is_err());
    assert!(snapshot(true).unwrap().complete);
    let next = begin(
        root.path(),
        "control-not-asset".into(),
        "source".into(),
        vec![role(&hash, &["Control"])],
    )
    .unwrap();
    let intent = read_barrier::Intent {
        scope_id: "control-not-asset".into(),
        generation: next,
        barrier_id: "control-first-read".into(),
        hash,
        flow: "source".into(),
    };
    assert!(read_barrier::arm(intent).is_err());
    assert!(snapshot(true).unwrap().complete);
}

#[test]
#[ignore = "runs in the observer child process"]
fn actual_empty_read_buffer_does_not_trigger_source_first_body_read() {
    let _reset = Reset::new();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("body");
    let bytes = b"synthetic nonempty physical read";
    std::fs::write(&path, bytes).unwrap();
    let hash = risunest_sync_wire::hash(bytes);
    let generation = begin(
        root.path(),
        "nonempty-read".into(),
        "source".into(),
        vec![role(&hash, &["Asset"])],
    )
    .unwrap();
    let intent = read_barrier::Intent {
        scope_id: "nonempty-read".into(),
        generation,
        barrier_id: "nonempty-body-first-read".into(),
        hash: hash.clone(),
        flow: "source".into(),
    };
    let (send, receive) = std::sync::mpsc::channel();
    read_barrier::install_hook(Some(Arc::new(move |event| send.send(event).unwrap())));
    read_barrier::arm(intent.clone()).unwrap();
    let mut file = TrackedFile::open(root.path(), &hash, &path, "file").unwrap();
    assert_eq!(file.read(&mut []).unwrap(), 0);
    assert!(receive.try_recv().is_err());
    assert_eq!(
        snapshot(false).unwrap().read_barrier.unwrap().status,
        "armed"
    );
    let worker = worker();
    let task = std::thread::spawn(move || {
        let _entered = worker.enter();
        file.read(&mut [0; 1]).unwrap()
    });
    receive
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    read_barrier::release(&intent).unwrap();
    assert_eq!(task.join().unwrap(), 1);
    assert!(snapshot(true).unwrap().complete);
}

#[test]
#[ignore = "runs in the observer child process"]
fn a_failed_scope_check_leaves_the_next_test_a_clean_observer() {
    let reset = Reset::new();
    let root = tempfile::tempdir().unwrap();
    begin(root.path(), "unsettled".into(), "error".into(), vec![]).unwrap();
    let pending = worker();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(reset)));
    assert!(failed.is_err());
    drop(pending);
    let _reset = Reset::new();
    assert!(active().is_none());
}

#[test]
#[ignore = "runs in the observer child process"]
fn a_panicking_observer_test_does_not_poison_the_next_one() {
    let failed = std::thread::spawn(|| {
        let _reset = Reset::new();
        let root = tempfile::tempdir().unwrap();
        begin(root.path(), "panicked".into(), "error".into(), vec![]).unwrap();
        panic!("synthetic observer test failure");
    })
    .join();
    assert!(failed.is_err());
    let _reset = Reset::new();
    assert!(active().is_none());
}
