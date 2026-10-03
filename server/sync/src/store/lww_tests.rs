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
    assert_eq!(
        store
            .push_at(&actor, &request, || Ok(1_000_000))
            .unwrap()
            .seq
            .0,
        1
    );
    request.operation_id = "beyond".into();
    request.changes[0].stamp.physical_ms = 1_300_001.into();
    assert_eq!(
        store
            .push_at(&actor, &request, || Ok(1_000_000))
            .unwrap_err()
            .code,
        "clock-skew"
    );
    assert_eq!(store.head().unwrap().seq.as_str(), "1");
}

#[test]
fn admission_reads_the_clock_after_waiting_for_the_object_gate() {
    use risunest_sync_wire::lww::PushRequest;
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let actor = Device {
        id: store.add_device().unwrap().device_id,
    };
    let writer = "00000000-0000-4000-8000-000000000001";
    let request = PushRequest {
        library_id: store.head().unwrap().library_id,
        writer_id: writer.into(),
        operation_id: "gated".into(),
        changes: vec![UnitChange {
            key: UnitKey::new(&["root", "time"]).unwrap(),
            stamp: Stamp {
                physical_ms: 1.into(),
                logical: 0,
                writer_id: writer.into(),
            },
            value: UnitValue::inline(br#"{}"#).unwrap(),
        }],
    };
    let receipt = store
        .push_at(&actor, &request, || {
            assert!(store.objects_gate.try_lock().is_err());
            Ok(1_000_000)
        })
        .unwrap();
    assert_eq!(receipt.server_time_ms.0, 1_000_000);
}

#[test]
fn replacing_a_winner_finds_its_parent_rows_through_an_index() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let db = store.reader().unwrap();
    let plan = db
        .prepare("EXPLAIN QUERY PLAN DELETE FROM unit_parents WHERE child=?1")
        .unwrap()
        .query_map(["[]"], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        plan.iter()
            .all(|detail| !detail.starts_with("SCAN") && detail.contains("INDEX")),
        "{plan:?}"
    );
}

#[test]
fn operation_receipts_and_writer_versions_expire_with_the_journal_tail() {
    use risunest_sync_wire::lww::PushRequest;
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let actor = Device {
        id: store.add_device().unwrap().device_id,
    };
    let writer = "00000000-0000-4000-8000-000000000001";
    let push = |operation: &str, physical: u64, value: &[u8]| PushRequest {
        library_id: store.head().unwrap().library_id,
        writer_id: writer.into(),
        operation_id: operation.into(),
        changes: vec![UnitChange {
            key: UnitKey::new(&["root", "key"]).unwrap(),
            stamp: Stamp {
                physical_ms: physical.into(),
                logical: 0,
                writer_id: writer.into(),
            },
            value: UnitValue::inline(value).unwrap(),
        }],
    };
    let first = push("first", 1, br#""a""#);
    let first_receipt = store.push(&actor, &first).unwrap();
    store.push(&actor, &push("second", 2, br#""b""#)).unwrap();
    let db = rusqlite::Connection::open(root.path().join("metadata.sqlite")).unwrap();
    let count = |table: &str| {
        db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap()
    };
    let current = super::uploads::now().unwrap();
    store.maintain_at(current + 604800 - 1).unwrap();
    assert_eq!((count("operations"), count("writer_versions")), (2, 2));
    store.maintain_at(current + 604800 + 2).unwrap();
    assert_eq!((count("operations"), count("writer_versions")), (0, 1));
    assert_eq!(
        db.query_row("SELECT physical FROM writer_versions", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "2"
    );
    let replayed = store.push(&actor, &first).unwrap();
    assert_ne!(replayed, first_receipt);
    assert!(replayed.accepted_keys.is_empty());
    assert_eq!(
        store
            .push(&actor, &push("clone", 2, br#""other""#))
            .unwrap_err()
            .code,
        "equal-stamp-integrity"
    );
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
