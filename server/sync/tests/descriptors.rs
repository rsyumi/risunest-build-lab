mod common;
use common::*;
use risunest_sync_server::store::{Device, Store};
use risunest_sync_wire::Domain;
use risunest_sync_wire::{
    descriptor::{build_reference_tree, RecordDescriptor},
    hash, ChangeSet, RecordChange, RecordVersion, TerminalStatus,
};

fn related(store: &Store, device: &Device, key: &str, target: &str) -> ChangeSet {
    let (root, pages) = build_reference_tree(&[target.to_owned()], true).unwrap();
    for (hash, bytes) in pages {
        store.put_object(device, &hash, &bytes).unwrap();
    }
    let body = b"synthetic content";
    store.put_object(device, &hash(body), body).unwrap();
    let descriptor = RecordDescriptor {
        relation_root: root,
        ..RecordDescriptor::content(hash(body))
    };
    let bytes = descriptor.bytes().unwrap();
    let digest = hash(&bytes);
    store.put_object(device, &digest, &bytes).unwrap();
    ChangeSet {
        changes: vec![RecordChange {
            domain: Domain::Library,
            key: key.into(),
            before: RecordVersion::Absent,
            after: RecordVersion::Live {
                object_hash: hash(body),
                descriptor_hash: Some(digest),
            },
        }],
        read_fences: vec![],
        scope_fences: vec![],
    }
}
#[test]
fn parent_delete_cannot_orphan_child_and_joint_delete_is_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let a = device(&store);
    let head = store.head().unwrap();
    let mut c = related(&store, &a, "child", "parent");
    c.changes
        .push(changes("parent", b"synthetic content").changes.remove(0));
    let intent = stage(&store, &a, &head, 1, &c);
    let head = store.commit(&a, &intent, &head.etag()).unwrap().head;
    let delete = |key: &str| RecordChange {
        domain: Domain::Library,
        key: key.into(),
        before: store.record(Domain::Library, key).unwrap(),
        after: RecordVersion::Tombstone {
            deletion_id: format!("delete-{key}"),
        },
    };
    let c = ChangeSet {
        changes: vec![delete("parent")],
        read_fences: vec![],
        scope_fences: vec![],
    };
    let intent = stage(&store, &a, &head, 2, &c);
    let failure = store.commit(&a, &intent, &head.etag()).unwrap();
    assert_eq!(failure.error.as_deref(), Some("referenced-record-deleted"));
    assert_eq!(store.head().unwrap(), head);
    let c = ChangeSet {
        changes: vec![delete("child"), delete("parent")],
        read_fences: vec![],
        scope_fences: vec![],
    };
    let intent = stage(&store, &a, &head, 3, &c);
    assert_eq!(
        store.commit(&a, &intent, &head.etag()).unwrap().status,
        TerminalStatus::Committed
    );
}
#[test]
fn missing_relation_fails_with_terminal_receipt_and_no_partial_record() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let a = device(&store);
    let head = store.head().unwrap();
    let c = related(&store, &a, "child", "missing");
    let intent = stage(&store, &a, &head, 1, &c);
    let failed = store.commit(&a, &intent, &head.etag()).unwrap();
    assert_eq!(failed.error.as_deref(), Some("missing-related-record"));
    assert_eq!(
        store.record(Domain::Library, "child").unwrap(),
        RecordVersion::Absent
    );
    assert_eq!(store.head().unwrap(), head);
    assert_eq!(store.commit(&a, &intent, &head.etag()).unwrap(), failed);
}
#[test]
fn descriptor_content_and_missing_dependency_cannot_be_forged() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let a = device(&store);
    let mut c = related(&store, &a, "child", "parent");
    if let RecordVersion::Live { object_hash, .. } = &mut c.changes[0].after {
        *object_hash = hash(b"wrong");
    }
    let rejection = store.stage_changes(&a, &c).err().unwrap();
    assert_eq!(rejection.code, "descriptor-object-mismatch");
    // The reply names the record the page failed on, not only the reason.
    assert_eq!(rejection.key.as_deref(), Some("child"));
    let (root, pages) = build_reference_tree(&[hash(b"missing dependency")], false).unwrap();
    for (digest, bytes) in pages {
        store.put_object(&a, &digest, &bytes).unwrap();
    }
    let descriptor = RecordDescriptor {
        dependency_root: root,
        ..RecordDescriptor::content(hash(b"synthetic content"))
    };
    let bytes = descriptor.bytes().unwrap();
    let digest = hash(&bytes);
    store.put_object(&a, &digest, &bytes).unwrap();
    c.changes[0].after = RecordVersion::Live {
        object_hash: descriptor.object_hash,
        descriptor_hash: Some(digest),
    };
    let rejection = store.stage_changes(&a, &c).err().unwrap();
    assert_eq!(rejection.code, "missing-dependency");
    assert_eq!(rejection.key.as_deref(), Some("child"));
}

#[test]
fn inline_dependencies_survive_lease_expiry_and_inline_relations_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let device = device(&store);
    let body = b"synthetic inline body";
    let asset = b"synthetic inline asset";
    store.put_object(&device, &hash(body), body).unwrap();
    let descriptor = RecordDescriptor {
        dependencies: vec![hash(asset)],
        relations: vec!["parent".into()],
        ..RecordDescriptor::content(hash(body))
    };
    let bytes = descriptor.bytes().unwrap();
    store.put_object(&device, &hash(&bytes), &bytes).unwrap();
    let mut change = ChangeSet {
        changes: vec![RecordChange {
            domain: Domain::Library,
            key: "child".into(),
            before: RecordVersion::Absent,
            after: RecordVersion::Live {
                object_hash: hash(body),
                descriptor_hash: Some(hash(&bytes)),
            },
        }],
        read_fences: vec![],
        scope_fences: vec![],
    };
    assert_eq!(
        store.stage_changes(&device, &change).err().unwrap().code,
        "missing-dependency"
    );
    store.put_object(&device, &hash(asset), asset).unwrap();
    let head = store.head().unwrap();
    let intent = stage(&store, &device, &head, 1, &change);
    assert_eq!(
        store
            .commit(&device, &intent, &head.etag())
            .unwrap()
            .error
            .as_deref(),
        Some("missing-related-record")
    );
    change
        .changes
        .push(changes("parent", body).changes.remove(0));
    let intent = stage(&store, &device, &head, 2, &change);
    assert_eq!(
        store.commit(&device, &intent, &head.etag()).unwrap().status,
        TerminalStatus::Committed
    );
    let db = rusqlite::Connection::open(dir.path().join("metadata.sqlite")).unwrap();
    db.execute("DELETE FROM object_leases", []).unwrap();
    store.maintain().unwrap();
    drop(store);
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(store.get_object(&hash(asset)).unwrap(), asset);
}
