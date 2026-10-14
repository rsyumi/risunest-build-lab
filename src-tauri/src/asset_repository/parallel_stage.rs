use super::StagedPayload;
use std::{
    io,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

use super::workers::{self, WORKERS};
pub(crate) const MAX_BATCH: usize = 64;
const MAX_BATCH_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) fn batch_len(sizes: impl IntoIterator<Item = u64>) -> usize {
    let mut bytes = 0u64;
    let mut count = 0;
    for size in sizes.into_iter().take(MAX_BATCH) {
        if count > 0 && bytes.saturating_add(size) > MAX_BATCH_BYTES {
            break;
        }
        bytes = bytes.saturating_add(size);
        count += 1;
    }
    count
}

/// Prepares a bounded hydration page while its coordinator retains the locks,
/// cancellation checks, and publication. No worker writes catalog state.
pub(crate) fn stage_batch<T: Sync, E: From<io::Error> + Send>(
    inputs: &[T],
    check: &dyn Fn() -> Result<(), E>,
    stage: &(impl Fn(&T, &AtomicBool) -> Result<StagedPayload, E> + Sync),
) -> Result<Vec<StagedPayload>, E> {
    check()?;
    if inputs.len() > MAX_BATCH {
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "hydration batch is too large").into(),
        );
    }
    let next = AtomicUsize::new(0);
    let stopped = AtomicBool::new(false);
    let mut output = inputs.iter().map(|_| None).collect::<Vec<_>>();
    let mut failure = None;
    std::thread::scope(|scope| {
        let (sender, receiver) = mpsc::sync_channel(WORKERS);
        let mut workers = Vec::new();
        #[cfg(test)]
        let owner = std::thread::current().id();
        for _ in 0..WORKERS.min(inputs.len()) {
            let sender = sender.clone();
            let (next, stopped) = (&next, &stopped);
            let worker = workers::spawn(scope, move || {
                while !stopped.load(Ordering::Acquire) {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(input) = inputs.get(index) else {
                        break;
                    };
                    let result = stage(input, stopped);
                    #[cfg(test)]
                    let result = result.map(|mut payload| {
                        payload.transfer_observation_to(owner);
                        payload
                    });
                    let failed = result.is_err();
                    if sender.send((index, result)).is_err() {
                        break;
                    }
                    if failed {
                        stopped.store(true, Ordering::Release);
                        break;
                    }
                }
            });
            match worker {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    if workers.is_empty() {
                        failure = Some(error.into());
                    }
                    break;
                }
            }
        }
        drop(sender);
        loop {
            if failure.is_none() {
                if let Err(error) = check() {
                    stopped.store(true, Ordering::Release);
                    failure = Some(error);
                }
            }
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok((index, Ok(payload))) if failure.is_none() => output[index] = Some(payload),
                Ok((_, Ok(_))) => {}
                Ok((_, Err(error))) => {
                    stopped.store(true, Ordering::Release);
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        for worker in workers {
            if worker.join().is_err() && failure.is_none() {
                failure = Some(io::Error::other("hydration worker failed").into());
            }
        }
    });
    if let Some(error) = failure {
        return Err(error);
    }
    check()?;
    output
        .into_iter()
        .map(|payload| {
            payload.ok_or_else(|| io::Error::other("hydration preparation incomplete").into())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_repository::{body_io, PayloadCas};
    use sha2::{Digest, Sha256};
    use std::sync::Barrier;

    #[test]
    fn worker_refusal_keeps_preparation_bounded_and_observations_settled() {
        for limit in [0, 1, 2] {
            let root = tempfile::tempdir().unwrap();
            let cas = PayloadCas::new(root.path()).unwrap();
            let bytes = b"synthetic hydration";
            let hash = hex::encode(Sha256::digest(bytes));
            body_io::reset_body_io();
            body_io::register_object_purpose(&hash, body_io::BodyPurpose::Asset);
            crate::external_storage::worker_observation::begin();
            let result = workers::with_limit(limit, || {
                stage_batch(&[0; 8], &|| Ok::<_, io::Error>(()), &|_, _| {
                    cas.stage_reader_expected(&mut bytes.as_slice(), &hash, bytes.len() as u64)
                })
            });
            if limit == 0 {
                assert_eq!(result.err().unwrap().kind(), io::ErrorKind::WouldBlock);
            } else {
                assert_eq!(result.unwrap().len(), 8);
            }
            let receipts = crate::external_storage::worker_observation::take();
            assert_eq!(receipts.len(), limit);
            assert!(receipts.iter().all(|receipt| receipt.completed));
            let observed = body_io::take_body_io();
            assert!(observed.complete(), "{observed:?}");
            assert_eq!(observed.worker_scopes_started, limit as u64);
            assert!(cas.object_lengths().unwrap().is_empty());
        }
    }

    #[test]
    fn staging_pages_bound_file_count_and_bytes_and_admit_one_oversized_body() {
        assert_eq!(batch_len([]), 0);
        assert_eq!(batch_len([1; 1000]), MAX_BATCH);
        assert_eq!(batch_len([20 * 1024 * 1024; 8]), 3);
        assert_eq!(batch_len([MAX_BATCH_BYTES, 1]), 1);
        assert_eq!(batch_len([u64::MAX, 1]), 1);
    }

    #[test]
    #[ignore = "explicit filesystem performance measurement"]
    fn measure_downloaded_payload_preparation() {
        let source = tempfile::tempdir().unwrap();
        let inputs = (0_u64..1000)
            .map(|index| {
                let mut bytes = vec![0x73; 80 * 1024];
                bytes[..8].copy_from_slice(&index.to_le_bytes());
                let path = source.path().join(index.to_string());
                std::fs::write(&path, &bytes).unwrap();
                (
                    path,
                    hex::encode(Sha256::digest(&bytes)),
                    bytes.len() as u64,
                )
            })
            .collect::<Vec<_>>();
        for repetition in 0..2 {
            for mode in [
                "sync-serial",
                "sync-parallel",
                "external-serial",
                "external-parallel",
            ] {
                let root = tempfile::tempdir().unwrap();
                let cas = PayloadCas::new(root.path()).unwrap();
                let started = std::time::Instant::now();
                for page in inputs.chunks(MAX_BATCH) {
                    let stage = |(path, hash, size): &(std::path::PathBuf, String, u64),
                                 _: &AtomicBool| {
                        cas.stage_reader_expected(&mut std::fs::File::open(path)?, hash, *size)
                    };
                    if mode == "sync-serial" {
                        for input in page {
                            cas.publish_staged(stage(input, &AtomicBool::new(false)).unwrap())
                                .unwrap();
                        }
                        continue;
                    }
                    let staged = if mode.ends_with("parallel") {
                        stage_batch(page, &|| Ok::<_, io::Error>(()), &stage).unwrap()
                    } else {
                        page.iter()
                            .map(|input| stage(input, &AtomicBool::new(false)).unwrap())
                            .collect()
                    };
                    if mode.starts_with("sync") {
                        for payload in staged {
                            cas.publish_staged(payload).unwrap();
                        }
                    } else {
                        cas.publish_staged_batch(staged).unwrap();
                    }
                }
                let elapsed = started.elapsed();
                for (_, hash, size) in &inputs {
                    let bytes = cas.read_object(hash).unwrap().unwrap();
                    assert_eq!(hex::encode(Sha256::digest(&bytes)), *hash);
                    assert_eq!(bytes.len() as u64, *size);
                }
                println!(
                    "{}",
                    serde_json::json!({"mode":mode,"repetition":repetition,"objects":inputs.len(),
                    "bytesEach":80*1024,"elapsedMs":elapsed.as_secs_f64()*1000.0})
                );
            }
        }
    }

    #[test]
    fn four_workers_stage_exact_bodies_in_input_order_without_publishing() {
        let root = tempfile::tempdir().unwrap();
        let cas = PayloadCas::new(root.path()).unwrap();
        let inputs = (0..64)
            .map(|index| format!("hydration {index}").into_bytes())
            .collect::<Vec<_>>();
        let hashes = inputs
            .iter()
            .map(|bytes| hex::encode(Sha256::digest(bytes)))
            .collect::<Vec<_>>();
        body_io::reset_body_io();
        for hash in &hashes {
            body_io::register_object_purpose(hash, body_io::BodyPurpose::Asset);
        }
        let started = AtomicUsize::new(0);
        let active = AtomicUsize::new(0);
        let maximum = AtomicUsize::new(0);
        let barrier = Barrier::new(WORKERS);
        crate::external_storage::worker_observation::begin();
        let staged = stage_batch(&inputs, &|| Ok::<_, io::Error>(()), &|bytes, _| {
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            maximum.fetch_max(current, Ordering::SeqCst);
            if started.fetch_add(1, Ordering::SeqCst) < WORKERS {
                barrier.wait();
            }
            let hash = hex::encode(Sha256::digest(bytes));
            let result =
                cas.stage_reader_expected(&mut bytes.as_slice(), &hash, bytes.len() as u64);
            active.fetch_sub(1, Ordering::SeqCst);
            result
        })
        .unwrap();
        let receipts = crate::external_storage::worker_observation::take();
        assert_eq!(receipts.len(), WORKERS);
        assert!(receipts.iter().all(|receipt| receipt.completed));
        assert_eq!(
            receipts
                .iter()
                .map(|receipt| receipt.hashes.domains["cas_stage"].calls)
                .sum::<u64>(),
            inputs.len() as u64
        );
        assert_eq!(
            receipts
                .iter()
                .map(|receipt| receipt.hashes.domains["cas_stage"].bytes)
                .sum::<u64>(),
            inputs.iter().map(|bytes| bytes.len() as u64).sum::<u64>()
        );
        assert_eq!(maximum.load(Ordering::SeqCst), 4);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert!(hashes
            .iter()
            .all(|hash| cas.stat_object(hash).unwrap().is_none()));
        let published = cas.publish_staged_batch(staged).unwrap();
        assert_eq!(
            published
                .iter()
                .map(|payload| &payload.content_hash)
                .collect::<Vec<_>>(),
            hashes.iter().collect::<Vec<_>>()
        );
        for (hash, expected) in hashes.iter().zip(inputs) {
            assert_eq!(cas.read_object(hash).unwrap(), Some(expected));
        }
        let observed = body_io::take_body_io();
        assert!(observed.complete(), "{observed:?}");
        assert_eq!(observed.worker_scopes_started, 4);
    }

    #[test]
    fn cancellation_joins_all_workers_and_discards_unpublished_files() {
        let root = tempfile::tempdir().unwrap();
        let cas = PayloadCas::new(root.path()).unwrap();
        let active = AtomicUsize::new(0);
        let started = AtomicUsize::new(0);
        let error = stage_batch(
            &[0; 8],
            &|| {
                if started.load(Ordering::Acquire) > 0 {
                    Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "synthetic cancellation",
                    ))
                } else {
                    Ok(())
                }
            },
            &|_, stopped| {
                active.fetch_add(1, Ordering::SeqCst);
                started.fetch_add(1, Ordering::Release);
                let bytes = b"cancelled hydration";
                let staged = cas.stage_reader_expected(
                    &mut bytes.as_slice(),
                    &hex::encode(Sha256::digest(bytes)),
                    bytes.len() as u64,
                );
                while !stopped.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                active.fetch_sub(1, Ordering::SeqCst);
                staged
            },
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert!(cas.object_lengths().unwrap().is_empty());
        assert_eq!(
            std::fs::read_dir(root.path().join("assets/staging"))
                .unwrap()
                .count(),
            0
        );
    }
}
