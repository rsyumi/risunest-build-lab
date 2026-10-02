mod common;
use common::*;
use risunest_sync_server::store::{Device, Store};
use risunest_sync_wire::{
    canonical,
    descriptor::{build_reference_tree, RecordDescriptor},
    hash,
    unit::UnitValue,
};
use rusqlite::Connection;
fn publish_descriptor(store: &Store, actor: &Device, descriptor: RecordDescriptor) -> UnitValue {
    store
        .put_object(
            actor,
            &descriptor.hash().unwrap(),
            &canonical::encode(&descriptor).unwrap(),
        )
        .unwrap();
    UnitValue::object(descriptor).unwrap()
}
fn expire_objects(root: &std::path::Path) {
    let db = Connection::open(root.join("metadata.sqlite")).unwrap();
    db.execute("UPDATE object_leases SET expires=0", [])
        .unwrap();
    db.execute("UPDATE journal SET created=0", []).unwrap();
}
#[test]
fn forged_descriptor_hash_rejects_whole_batch_before_any_state_change() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let actor = device(&store);
    let descriptor = RecordDescriptor::content(hash(b"synthetic"));
    let mut value = UnitValue::object(descriptor).unwrap();
    if let UnitValue::Object {
        descriptor_hash, ..
    } = &mut value
    {
        *descriptor_hash = "0".repeat(64);
    }
    let req = request(
        &store,
        WRITER_A,
        "forged",
        vec![
            inline("valid", WRITER_A, 1, "valid"),
            unit(&["root", "forged"], WRITER_A, 1, value),
        ],
    );
    assert_eq!(
        store.push(&actor, &req).unwrap_err().code,
        "descriptor-hash-mismatch"
    );
    assert_eq!(store.head().unwrap().seq.as_str(), "0");
    assert!(store
        .changes(&actor, 0.into(), 10)
        .unwrap()
        .items
        .is_empty());
}
#[test]
fn missing_tree_dependency_names_key_and_rejection_is_terminal_after_upload() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let actor = device(&store);
    let payload = b"synthetic payload";
    let missing = b"synthetic dependency";
    store.put_object(&actor, &hash(payload), payload).unwrap();
    let (tree, pages) = build_reference_tree(&[hash(missing)], false).unwrap();
    for (digest, bytes) in pages {
        store.put_object(&actor, &digest, &bytes).unwrap();
    }
    let value = publish_descriptor(
        &store,
        &actor,
        RecordDescriptor {
            dependency_root: tree,
            ..RecordDescriptor::content(hash(payload))
        },
    );
    let req = request(
        &store,
        WRITER_A,
        "missing",
        vec![
            inline("valid", WRITER_A, 1, "valid"),
            unit(&["root", "child"], WRITER_A, 1, value),
        ],
    );
    let rejection = store.push(&actor, &req).unwrap_err();
    assert_eq!(rejection.code, "missing-dependency");
    assert_eq!(rejection.key.as_deref(), Some("[\"root\",\"child\"]"));
    store.put_object(&actor, &hash(missing), missing).unwrap();
    assert_eq!(
        store.push(&actor, &req).unwrap_err().code,
        "missing-dependency"
    );
    let mut retry = req;
    retry.operation_id = "corrected".into();
    assert_eq!(store.push(&actor, &retry).unwrap().seq.0, 2);
}
#[test]
fn current_units_and_pin_snapshots_keep_descriptor_dependencies_after_lease_expiry() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let actor = device(&store);
    let body = b"synthetic payload";
    let asset = b"synthetic asset";
    for bytes in [body.as_slice(), asset.as_slice()] {
        store.put_object(&actor, &hash(bytes), bytes).unwrap();
    }
    let descriptor = RecordDescriptor {
        dependencies: vec![hash(asset)],
        ..RecordDescriptor::content(hash(body))
    };
    let descriptor_hash = descriptor.hash().unwrap();
    let value = publish_descriptor(&store, &actor, descriptor);
    store
        .push(
            &actor,
            &request(
                &store,
                WRITER_A,
                "object",
                vec![unit(&["root", "key"], WRITER_A, 1, value)],
            ),
        )
        .unwrap();
    expire_objects(root.path());
    store.maintain().unwrap();
    assert!(store.object_presence(&hash(asset)).unwrap());
    let pin = store.create_state_pin(&actor).unwrap();
    store
        .push(
            &actor,
            &request(
                &store,
                WRITER_A,
                "replace",
                vec![inline("key", WRITER_A, 2, "new")],
            ),
        )
        .unwrap();
    expire_objects(root.path());
    store.maintain().unwrap();
    assert!(store.object_presence(&hash(body)).unwrap());
    assert!(store.object_presence(&hash(asset)).unwrap());
    assert!(store.object_presence(&descriptor_hash).unwrap());
    assert_eq!(
        store
            .state_page(&actor, &pin.pin_id, None, 1)
            .unwrap()
            .items
            .len(),
        1
    );
    store.release_state_pin(&actor, &pin.pin_id).unwrap();
    store.maintain().unwrap();
    assert!(!store.object_presence(&hash(body)).unwrap());
    assert!(!store.object_presence(&hash(asset)).unwrap());
    assert!(!store.object_presence(&descriptor_hash).unwrap());
}
#[test]
fn dependency_tree_pages_and_leaves_remain_reachable_without_record_relations() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let actor = device(&store);
    let mut hashes = Vec::new();
    for index in 0..400 {
        let bytes = format!("synthetic {index}");
        let digest = hash(bytes.as_bytes());
        store.put_object(&actor, &digest, bytes.as_bytes()).unwrap();
        hashes.push(digest);
    }
    hashes.sort();
    let (tree, pages) = build_reference_tree(&hashes, false).unwrap();
    let page_hashes: Vec<_> = pages.iter().map(|(digest, _)| digest.clone()).collect();
    for (digest, bytes) in pages {
        store.put_object(&actor, &digest, &bytes).unwrap();
    }
    let body = b"synthetic payload";
    store.put_object(&actor, &hash(body), body).unwrap();
    let value = publish_descriptor(
        &store,
        &actor,
        RecordDescriptor {
            dependency_root: tree,
            ..RecordDescriptor::content(hash(body))
        },
    );
    store
        .push(
            &actor,
            &request(
                &store,
                WRITER_A,
                "tree",
                vec![unit(&["root", "tree"], WRITER_A, 1, value)],
            ),
        )
        .unwrap();
    expire_objects(root.path());
    store.maintain().unwrap();
    for digest in hashes.into_iter().chain(page_hashes) {
        assert!(store.object_presence(&digest).unwrap());
    }
}

#[test]
fn cached_descriptor_validation_rejects_a_lost_file_dependency() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let actor = device(&store);
    let body = b"synthetic payload";
    let asset = vec![b'a'; 128 * 1024];
    store.put_object(&actor, &hash(body), body).unwrap();
    store.put_object(&actor, &hash(&asset), &asset).unwrap();
    let value = publish_descriptor(
        &store,
        &actor,
        RecordDescriptor {
            dependencies: vec![hash(&asset)],
            ..RecordDescriptor::content(hash(body))
        },
    );
    let first = request(
        &store,
        WRITER_A,
        "first",
        vec![unit(&["root", "first"], WRITER_A, 1, value.clone())],
    );
    store.push(&actor, &first).unwrap();
    let digest = hash(&asset);
    std::fs::remove_file(root.path().join("objects").join(&digest[..2]).join(&digest)).unwrap();
    let second = request(
        &store,
        WRITER_A,
        "second",
        vec![unit(&["root", "second"], WRITER_A, 2, value)],
    );
    assert_eq!(
        store.push(&actor, &second).unwrap_err().code,
        "missing-dependency"
    );
    assert_eq!(store.head().unwrap().seq.as_str(), "1");
    assert_eq!(store.push(&actor, &first).unwrap().seq.0, 1);
}
