//! Structural throughput baselines. These tests assert the request shape a
//! transfer produces, not wall time, so they stay deterministic on any machine.
//! The recorded numbers are the comparison point for the batching work; each
//! assertion states the shape that holds today and is tightened when the
//! corresponding change lands.
use super::{observed::Observed, *};

/// Small enough that thousands fit in one reply, large enough that a page of
/// them exceeds the server's current inline reply target.
const SMALL_BODY_BYTES: usize = 2 * 1024;

fn body(index: usize) -> Vec<u8> {
    let mut bytes = format!("synthetic small object {index:08} ").into_bytes();
    bytes.resize(SMALL_BODY_BYTES, b'.');
    bytes
}

fn publish(server: &Observed, count: usize) -> Vec<String> {
    let device = server.device();
    (0..count)
        .map(|index| {
            let bytes = body(index);
            let digest = risunest_sync_wire::hash(&bytes);
            server.store.put_object(&device, &digest, &bytes).unwrap();
            digest
        })
        .collect()
}

/// A full page of small objects must not degrade into one request per object
/// once the first reply is full. Every body here fits the server's inline
/// target, so every body must arrive inside a batched reply.
#[test]
fn small_object_page_does_not_fall_back_to_one_request_per_object() {
    let server = Observed::start();
    let targets = publish(&server, risunest_sync_wire::transfer::MAX_BATCH_OBJECTS);
    let client = server.client();
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::open(directory.path()).unwrap();
    server.reset();
    Transfer::new(&client, &cache)
        .unwrap()
        .download(&targets, &[])
        .unwrap();
    let by_path = server.by_path();
    let total = server.total();
    eprintln!(
        "page of {} objects of {SMALL_BODY_BYTES} bytes: {total} requests {by_path:?}",
        targets.len()
    );
    for (digest, index) in targets.iter().zip(0..) {
        assert_eq!(cache.read(digest, SMALL_BODY_BYTES).unwrap(), body(index));
    }
    let batched = by_path
        .get("POST objects/transfer")
        .copied()
        .unwrap_or_default();
    let per_object = by_path
        .get("GET objects/{hash}")
        .copied()
        .unwrap_or_default();
    assert_eq!(
        per_object, 0,
        "bodies that fit the reply target must arrive in a batched reply"
    );
    // Each reply carries at most the shared target, so a page this size needs
    // the first request plus one regrouped request per further reply.
    let framed = targets.len() * (SMALL_BODY_BYTES + 45) + 8;
    let replies = framed.div_ceil(risunest_sync_wire::transfer::PREFERRED_BATCH_BYTES) as u64;
    assert!(
        batched <= replies + 1,
        "expected at most {} batched requests, saw {batched}",
        replies + 1
    );
    assert_eq!(total, batched);
}

/// Cached identities cost nothing. This is the A01 shape at transfer level: a
/// repeated receive of unchanged content sends no request at all.
#[test]
fn repeated_receive_of_cached_objects_sends_no_request() {
    let server = Observed::start();
    let targets = publish(&server, 64);
    let client = server.client();
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::open(directory.path()).unwrap();
    Transfer::new(&client, &cache)
        .unwrap()
        .download(&targets, &[])
        .unwrap();
    server.reset();
    Transfer::new(&client, &cache)
        .unwrap()
        .download(&targets, &[])
        .unwrap();
    assert_eq!(server.total(), 0, "{:?}", server.by_path());
}
