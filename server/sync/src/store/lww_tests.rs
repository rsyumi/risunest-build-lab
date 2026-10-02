use super::{descriptors_index::commits, Device, Store};
use risunest_sync_wire::{
    canonical,
    descriptor::{build_reference_tree, RecordDescriptor},
    hash,
    lww::UnitChange,
    stamp::Stamp,
    unit::{UnitKey, UnitValue},
};

#[test]
fn descriptor_indexes_flush_bounded_groups_and_reuse_pending_reference_nodes() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let actor = Device {
        id: store.add_device().unwrap().device_id,
    };
    let payload = b"synthetic payload";
    store.put_object(&actor, &hash(payload), payload).unwrap();
    let mut hashes: Vec<String> = (0..400)
        .map(|index| {
            let body = format!("synthetic dependency {index}");
            let digest = hash(body.as_bytes());
            store.put_object(&actor, &digest, body.as_bytes()).unwrap();
            digest
        })
        .collect();
    hashes.sort();
    let (tree, pages) = build_reference_tree(&hashes, false).unwrap();
    for (digest, bytes) in pages {
        store.put_object(&actor, &digest, &bytes).unwrap();
    }
    let mut changes = Vec::new();
    for index in 0..300 {
        let descriptor = RecordDescriptor {
            object_hash: hash(payload),
            dependencies: vec![],
            dependency_root: tree.clone(),
            relations: vec![],
            relation_root: None,
            scopes: vec![],
        };
        store
            .put_object(
                &actor,
                &descriptor.hash().unwrap(),
                &canonical::encode(&descriptor).unwrap(),
            )
            .unwrap();
        changes.push(UnitChange {
            key: UnitKey::new(&["root", &format!("key-{index}")]).unwrap(),
            stamp: Stamp {
                physical_ms: 1.into(),
                logical: 0,
                writer_id: "00000000-0000-4000-8000-000000000001".into(),
            },
            value: UnitValue::object(descriptor).unwrap(),
        });
    }
    commits::take();
    store.prepare_descriptors(&changes).unwrap();
    let count = commits::take();
    assert!(
        count > 0 && count < 10,
        "300 units cost {count} index transactions"
    );
    let db = store.reader().unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM reference_objects", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        400
    );
    drop(db);
    store.prepare_descriptors(&changes).unwrap();
    assert_eq!(commits::take(), 0);
}

#[test]
fn admission_accepts_the_exact_future_boundary_and_rejects_the_next_millisecond() {
    use risunest_sync_wire::lww::PushRequest;
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let actor = Device {
        id: store.add_device().unwrap().device_id,
    };
    let writer = "00000000-0000-4000-8000-000000000001";
    let mut request = PushRequest {
        library_id: store.head().unwrap().library_id,
        writer_id: writer.into(),
        operation_id: "boundary".into(),
        changes: vec![UnitChange {
            key: UnitKey::new(&["root", "time"]).unwrap(),
            stamp: Stamp {
                physical_ms: 1_300_000.into(),
                logical: 0,
                writer_id: writer.into(),
            },
            value: UnitValue::inline(br#"{}"#).unwrap(),
        }],
    };
    assert_eq!(store.push_at(&actor, &request, 1_000_000).unwrap().seq.0, 1);
    request.operation_id = "beyond".into();
    request.changes[0].stamp.physical_ms = 1_300_001.into();
    assert_eq!(
        store.push_at(&actor, &request, 1_000_000).unwrap_err().code,
        "clock-skew"
    );
    assert_eq!(store.head().unwrap().seq.as_str(), "1");
}

#[test]
fn journal_and_active_ack_expire_at_the_exact_seven_day_boundary() {
    use risunest_sync_wire::lww::{AckRequest, PushRequest};
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let actor = Device {
        id: store.add_device().unwrap().device_id,
    };
    let writer = "00000000-0000-4000-8000-000000000001";
    let request = PushRequest {
        library_id: store.head().unwrap().library_id,
        writer_id: writer.into(),
        operation_id: "write".into(),
        changes: vec![UnitChange {
            key: UnitKey::new(&["root", "key"]).unwrap(),
            stamp: Stamp {
                physical_ms: 1.into(),
                logical: 0,
                writer_id: writer.into(),
            },
            value: UnitValue::inline(br#"{}"#).unwrap(),
        }],
    };
    store.push(&actor, &request).unwrap();
    store
        .acknowledge(&actor, &AckRequest { seq: 0.into() })
        .unwrap();
    let current = super::uploads::now().unwrap();
    let db = rusqlite::Connection::open(root.path().join("metadata.sqlite")).unwrap();
    db.execute("UPDATE journal SET created=?1", [current - 604800 + 1])
        .unwrap();
    db.execute("UPDATE devices SET last_ack=?1", [current - 604800])
        .unwrap();
    store.maintain_at(current).unwrap();
    assert_eq!(store.head().unwrap().min_retained_seq.as_str(), "0");
    db.execute("UPDATE journal SET created=?1", [current - 604800])
        .unwrap();
    db.execute("UPDATE devices SET last_ack=?1", [current - 604800 + 1])
        .unwrap();
    store.maintain_at(current).unwrap();
    assert_eq!(store.head().unwrap().min_retained_seq.as_str(), "0");
    db.execute("UPDATE devices SET last_ack=?1", [current - 604800])
        .unwrap();
    store.maintain_at(current).unwrap();
    assert_eq!(store.head().unwrap().min_retained_seq.as_str(), "1");
}
