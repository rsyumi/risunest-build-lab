mod common;
use common::*;
use risunest_sync_server::store::Store;
use risunest_sync_wire::{
    lww::{AckRequest, CancelOperationRequest, OperationReceipt},
    unit::{UnitKey, UnitValue},
};
use rusqlite::Connection;

fn state(
    store: &Store,
    device: &risunest_sync_server::store::Device,
) -> Vec<risunest_sync_wire::lww::UnitChange> {
    let pin = store.create_state_pin(device).unwrap();
    let page = store.state_page(device, &pin.pin_id, None, 1024).unwrap();
    assert!(page.next_key.is_none());
    store.release_state_pin(device, &pin.pin_id).unwrap();
    page.items
}
fn expire_journal(root: &std::path::Path) {
    Connection::open(root.join("metadata.sqlite"))
        .unwrap()
        .execute("UPDATE journal SET created=0", [])
        .unwrap();
}

#[test]
fn mixed_origin_initialization_preserves_stamps_and_binds_only_publishers() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    let b = device(&store);
    let origin = "00000000-0000-4000-8000-000000000003";
    let parent = unit(
        &["exists", "character", "char"],
        WRITER_B,
        10,
        UnitValue::inline(b"true").unwrap(),
    );
    let chat = unit(
        &["exists", "conversation", "char", "chat"],
        WRITER_B,
        11,
        UnitValue::inline(b"true").unwrap(),
    );
    let messages = unit(
        &["messages", "char", "chat"],
        WRITER_B,
        12,
        UnitValue::inline(b"[]").unwrap(),
    );
    let initial = request(
        &store,
        WRITER_A,
        "forward-initial",
        vec![
            parent.clone(),
            chat.clone(),
            messages,
            inline("other-origin", origin, 13, "existing"),
            inline("own", WRITER_A, 14, "existing"),
        ],
    );
    let receipt = store.push(&a, &initial).unwrap();
    assert_eq!(receipt.seq.0, 5);
    let persisted = state(&store, &a);
    assert_eq!(persisted.len(), initial.changes.len());
    for change in &initial.changes {
        assert!(persisted.contains(change));
    }
    let db = Connection::open(root.path().join("metadata.sqlite")).unwrap();
    let publishers: Vec<String> = db
        .prepare("SELECT writer FROM writers ORDER BY writer")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(publishers, [WRITER_A]);
    for (issuer, count) in [(WRITER_A, 1), (WRITER_B, 3), (origin, 1)] {
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM writer_versions WHERE writer=?1",
                [issuer],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            count
        );
    }
    let changed = unit(
        &["messages", "char", "chat"],
        WRITER_B,
        20,
        UnitValue::inline(br#"["changed"]"#).unwrap(),
    );
    let next = request(
        &store,
        WRITER_B,
        "forward-parents",
        vec![
            parent.clone(),
            chat.clone(),
            changed.clone(),
            initial.changes.last().unwrap().clone(),
        ],
    );
    let next_receipt = store.push(&b, &next).unwrap();
    assert_eq!(next_receipt.seq.0, 6);
    assert_eq!(next_receipt.accepted_keys, vec![changed.key.clone()]);
    let persisted = state(&store, &b);
    assert!(persisted.contains(&parent));
    assert!(persisted.contains(&chat));
    assert!(persisted.contains(&changed));
    assert_eq!(store.push(&a, &initial).unwrap(), receipt);
    assert_eq!(store.push(&b, &next).unwrap(), next_receipt);
}

#[test]
fn forwarded_issuer_collision_after_supersession_rejects_other_publishers_atomically() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    let b = device(&store);
    let origin = "00000000-0000-4000-8000-000000000003";
    let first = request(
        &store,
        WRITER_A,
        "origin-old",
        vec![inline("key", origin, 10, "old")],
    );
    let first_receipt = store.push(&a, &first).unwrap();
    store
        .push(
            &a,
            &request(
                &store,
                WRITER_A,
                "origin-new",
                vec![inline("key", origin, 20, "new")],
            ),
        )
        .unwrap();
    let bad = request(
        &store,
        WRITER_B,
        "origin-bad",
        vec![
            inline("unrelated", WRITER_B, 21, "unrelated"),
            inline("key", origin, 10, "altered"),
        ],
    );
    assert_eq!(
        store.push(&b, &bad).unwrap_err().code,
        "equal-stamp-integrity"
    );
    assert_eq!(store.head().unwrap().seq.as_str(), "2");
    assert_eq!(state(&store, &a).len(), 1);
    assert!(
        matches!(store.operation(&b, "origin-bad").unwrap(), OperationReceipt::Rejected { body_digest, error, .. } if body_digest == bad.digest().unwrap() && error == "equal-stamp-integrity")
    );
    let db = Connection::open(root.path().join("metadata.sqlite")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM writers", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM writer_versions WHERE writer=?1",
            [origin],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    let identical = request(&store, WRITER_B, "origin-identical", first.changes.clone());
    assert!(store.push(&b, &identical).unwrap().accepted_keys.is_empty());
    let different_issuer = request(
        &store,
        WRITER_B,
        "issuer-independent",
        vec![inline("key", WRITER_B, 10, "different issuer")],
    );
    assert!(store
        .push(&b, &different_issuer)
        .unwrap()
        .accepted_keys
        .is_empty());
    assert_eq!(store.push(&a, &first).unwrap(), first_receipt);
    drop(store);
    let reopened = Store::open(root.path()).unwrap();
    assert_eq!(
        reopened.push(&b, &bad).unwrap_err().code,
        "equal-stamp-integrity"
    );
    let mut fresh_bad = bad.clone();
    fresh_bad.operation_id = "origin-bad-reopened".into();
    assert_eq!(
        reopened.push(&b, &fresh_bad).unwrap_err().code,
        "equal-stamp-integrity"
    );
    let mut altered = bad;
    altered.changes[1].value = first.changes[0].value.clone();
    assert_eq!(
        reopened.push(&b, &altered).unwrap_err().code,
        "operation-integrity"
    );
    assert_eq!(reopened.head().unwrap().seq.as_str(), "2");
}

#[test]
fn independent_units_converge_in_both_orders() {
    let mut results = Vec::new();
    for reverse in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let store = Store::init(root.path()).unwrap();
        let a = device(&store);
        let b = device(&store);
        let ra = request(&store, WRITER_A, "a", vec![inline("a", WRITER_A, 10, "a")]);
        let rb = request(&store, WRITER_B, "b", vec![inline("b", WRITER_B, 10, "b")]);
        for (actor, req) in if reverse {
            [(&b, &rb), (&a, &ra)]
        } else {
            [(&a, &ra), (&b, &rb)]
        } {
            store.push(actor, req).unwrap();
        }
        results.push(state(&store, &a));
    }
    assert_eq!(results[0], results[1]);
    assert_eq!(results[0].len(), 2);
}

#[test]
fn same_conversation_keeps_larger_manifest_and_independent_metadata() {
    for reverse in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let store = Store::init(root.path()).unwrap();
        let a = device(&store);
        let b = device(&store);
        let left = unit(
            &["messages", "character", "chat"],
            WRITER_A,
            10,
            UnitValue::inline(br#"["first"]"#).unwrap(),
        );
        let right = unit(
            &["messages", "character", "chat"],
            WRITER_B,
            11,
            UnitValue::inline(br#"["second"]"#).unwrap(),
        );
        let meta = unit(
            &["conversation", "character", "chat", "note"],
            WRITER_A,
            10,
            UnitValue::inline(br#""note""#).unwrap(),
        );
        let ra = request(&store, WRITER_A, "a", vec![left, meta.clone()]);
        let rb = request(&store, WRITER_B, "b", vec![right.clone()]);
        for (actor, req) in if reverse {
            [(&b, &rb), (&a, &ra)]
        } else {
            [(&a, &ra), (&b, &rb)]
        } {
            store.push(actor, req).unwrap();
        }
        let values = state(&store, &a);
        assert!(values.contains(&right));
        assert!(values.contains(&meta));
        assert_eq!(values.len(), 2);
    }
}

#[test]
fn stale_writes_and_identical_versions_do_not_emit_and_retries_are_exact() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    let initial = request(
        &store,
        WRITER_A,
        "first",
        vec![inline("key", WRITER_A, 20, "new")],
    );
    let receipt = store.push(&a, &initial).unwrap();
    let identical = request(&store, WRITER_A, "repeat", initial.changes.clone());
    assert!(store.push(&a, &identical).unwrap().accepted_keys.is_empty());
    let stale = request(
        &store,
        WRITER_A,
        "stale",
        vec![inline("key", WRITER_A, 10, "old")],
    );
    assert!(store.push(&a, &stale).unwrap().accepted_keys.is_empty());
    assert_eq!(store.push(&a, &initial).unwrap(), receipt);
    assert_eq!(store.changes(&a, 0.into(), 10).unwrap().items.len(), 1);
    let mut altered = initial;
    altered.changes[0].value = UnitValue::inline(br#""different""#).unwrap();
    assert_eq!(
        store.push(&a, &altered).unwrap_err().code,
        "operation-integrity"
    );
}

#[test]
fn equal_stamp_collision_is_detected_after_supersession_and_rejects_whole_batch() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    store
        .push(
            &a,
            &request(
                &store,
                WRITER_A,
                "old",
                vec![inline("key", WRITER_A, 10, "old")],
            ),
        )
        .unwrap();
    store
        .push(
            &a,
            &request(
                &store,
                WRITER_A,
                "new",
                vec![inline("key", WRITER_A, 20, "new")],
            ),
        )
        .unwrap();
    let bad = request(
        &store,
        WRITER_A,
        "bad",
        vec![
            inline("unrelated", WRITER_A, 21, "new"),
            inline("key", WRITER_A, 10, "altered"),
        ],
    );
    assert_eq!(
        store.push(&a, &bad).unwrap_err().code,
        "equal-stamp-integrity"
    );
    assert_eq!(store.head().unwrap().seq.as_str(), "2");
    assert_eq!(state(&store, &a).len(), 1);
    assert!(
        matches!(store.operation(&a, "bad").unwrap(), OperationReceipt::Rejected { error, .. } if error == "equal-stamp-integrity")
    );
}

#[test]
fn future_admission_rejects_every_change_and_old_changes_remain_valid() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    let now = store.time_sample().unwrap();
    assert_eq!(now.precision_ms.0, 1);
    let bad = request(
        &store,
        WRITER_A,
        "future",
        vec![
            inline("ordinary", WRITER_A, 1, "old"),
            inline("future", WRITER_A, u64::MAX, "future"),
        ],
    );
    assert_eq!(store.push(&a, &bad).unwrap_err().code, "clock-skew");
    assert!(state(&store, &a).is_empty());
    store
        .push(
            &a,
            &request(
                &store,
                WRITER_A,
                "old",
                vec![inline("ordinary", WRITER_A, 1, "old")],
            ),
        )
        .unwrap();
    assert_eq!(store.head().unwrap().seq.as_str(), "1");
}

#[test]
fn copied_writer_identity_cannot_bind_a_second_device() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    let b = device(&store);
    store
        .push(
            &a,
            &request(&store, WRITER_A, "a", vec![inline("a", WRITER_A, 1, "a")]),
        )
        .unwrap();
    assert_eq!(
        store
            .push(
                &b,
                &request(&store, WRITER_A, "b", vec![inline("b", WRITER_A, 2, "b")])
            )
            .unwrap_err()
            .code,
        "writer-collision"
    );
    assert_eq!(state(&store, &a).len(), 1);
}

#[test]
fn journal_keeps_latest_per_key_and_page_cursor_stops_at_durable_items() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    store
        .push(
            &a,
            &request(
                &store,
                WRITER_A,
                "batch",
                vec![
                    inline("a", WRITER_A, 1, "a"),
                    inline("b", WRITER_A, 1, "b"),
                    inline("c", WRITER_A, 1, "c"),
                ],
            ),
        )
        .unwrap();
    let first = store.changes(&a, 0.into(), 1).unwrap();
    assert_eq!(first.through_seq.0, 3);
    assert_eq!(first.next_after.0, 1);
    store
        .push(
            &a,
            &request(
                &store,
                WRITER_A,
                "update",
                vec![inline("a", WRITER_A, 2, "updated")],
            ),
        )
        .unwrap();
    let next = store.changes(&a, first.next_after, 1).unwrap();
    assert_eq!(next.items[0].seq.0, 2);
    assert_eq!(next.next_after.0, 2);
    let tail = store.changes(&a, next.next_after, 10).unwrap();
    assert_eq!(tail.items.len(), 2);
    assert_eq!(tail.next_after.0, 4);
    let all = store.changes(&a, 0.into(), 10).unwrap();
    assert_eq!(all.items.len(), 3);
    assert_eq!(all.items[2].seq.0, 4);
    let empty = store.changes(&a, 4.into(), 10).unwrap();
    assert!(empty.items.is_empty());
    assert_eq!(empty.next_after.0, 4);
}

#[test]
fn stable_state_pin_pages_in_key_byte_order_while_tail_continues() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    let b = device(&store);
    store
        .push(
            &a,
            &request(
                &store,
                WRITER_A,
                "initial",
                vec![inline("z", WRITER_A, 1, "z"), inline("a", WRITER_A, 1, "a")],
            ),
        )
        .unwrap();
    let pin = store.create_state_pin(&a).unwrap();
    assert_eq!((pin.start_seq.0, pin.unit_count.0), (2, 2));
    store
        .push(
            &a,
            &request(
                &store,
                WRITER_A,
                "changed",
                vec![inline("a", WRITER_A, 2, "new")],
            ),
        )
        .unwrap();
    let first = store.state_page(&a, &pin.pin_id, None, 1).unwrap();
    assert_eq!(first.items[0], inline("a", WRITER_A, 1, "a"));
    let next = store
        .state_page(&a, &pin.pin_id, first.next_key.as_ref(), 1)
        .unwrap();
    assert_eq!(next.items[0].key, UnitKey::new(&["root", "z"]).unwrap());
    assert!(next.next_key.is_none());
    assert_eq!(store.changes(&a, pin.start_seq, 10).unwrap().items.len(), 1);
    assert_eq!(
        store.state_page(&b, &pin.pin_id, None, 1).unwrap_err().code,
        "state-pin-expired"
    );
    store.release_state_pin(&b, &pin.pin_id).unwrap();
    assert!(store.state_page(&a, &pin.pin_id, None, 1).is_ok());
    Connection::open(root.path().join("metadata.sqlite"))
        .unwrap()
        .execute("UPDATE state_pins SET expires=0", [])
        .unwrap();
    assert_eq!(
        store.state_page(&a, &pin.pin_id, None, 1).unwrap_err().code,
        "state-pin-expired"
    );
}

#[test]
fn a_device_that_left_pins_behind_can_still_read_state() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    let b = device(&store);
    store
        .push(
            &a,
            &request(
                &store,
                WRITER_A,
                "initial",
                vec![inline("a", WRITER_A, 1, "a")],
            ),
        )
        .unwrap();
    let other = store.create_state_pin(&b).unwrap();
    let left: Vec<_> = (0..4)
        .map(|_| store.create_state_pin(&a).unwrap())
        .collect();
    let next = store.create_state_pin(&a).unwrap();
    assert_eq!(
        store
            .state_page(&a, &left[0].pin_id, None, 1)
            .unwrap_err()
            .code,
        "state-pin-expired"
    );
    for pin in left[1..].iter().chain([&next]) {
        assert!(store.state_page(&a, &pin.pin_id, None, 1).is_ok());
    }
    assert!(store.state_page(&b, &other.pin_id, None, 1).is_ok());
    let held: i64 = Connection::open(root.path().join("metadata.sqlite"))
        .unwrap()
        .query_row(
            "SELECT count(*) FROM state_pin_units WHERE pin=?1",
            [&left[0].pin_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(held, 0);
}

#[test]
fn pruning_respects_active_ack_and_pin_then_returns_structured_floor() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    let offline = device(&store);
    let _unused = device(&store);
    store
        .acknowledge(&offline, &AckRequest { seq: 0.into() })
        .unwrap();
    store
        .push(
            &a,
            &request(
                &store,
                WRITER_A,
                "first",
                vec![inline("a", WRITER_A, 1, "a")],
            ),
        )
        .unwrap();
    let pin = store.create_state_pin(&a).unwrap();
    store
        .push(
            &a,
            &request(
                &store,
                WRITER_A,
                "second",
                vec![inline("b", WRITER_A, 2, "b")],
            ),
        )
        .unwrap();
    store
        .acknowledge(&a, &AckRequest { seq: 2.into() })
        .unwrap();
    expire_journal(root.path());
    store.maintain().unwrap();
    assert_eq!(store.changes(&a, 0.into(), 10).unwrap().journal_floor.0, 0);
    store.revoke_device(&offline.id).unwrap();
    store.maintain().unwrap();
    assert_eq!(store.head().unwrap().min_retained_seq.as_str(), "1");
    store.release_state_pin(&a, &pin.pin_id).unwrap();
    store.maintain().unwrap();
    let error = store.changes(&a, 0.into(), 10).unwrap_err();
    assert_eq!(error.code, "journal-floor");
    assert_eq!(error.journal_floor, Some((2.into(), 2.into())));
    assert_eq!(state(&store, &a).len(), 2);
    assert!(matches!(
        store.operation(&a, "first").unwrap(),
        OperationReceipt::Accepted { .. }
    ));
}

#[test]
fn retention_period_and_old_offline_ack_do_not_pin_journal_forever() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    store
        .push(
            &a,
            &request(
                &store,
                WRITER_A,
                "first",
                vec![inline("a", WRITER_A, 1, "a")],
            ),
        )
        .unwrap();
    store.maintain().unwrap();
    assert_eq!(store.head().unwrap().min_retained_seq.as_str(), "0");
    store
        .acknowledge(&a, &AckRequest { seq: 0.into() })
        .unwrap();
    Connection::open(root.path().join("metadata.sqlite"))
        .unwrap()
        .execute("UPDATE devices SET last_ack=0", [])
        .unwrap();
    expire_journal(root.path());
    store.maintain().unwrap();
    assert_eq!(store.head().unwrap().min_retained_seq.as_str(), "1");
}

#[test]
fn retirement_is_permanent_suppresses_children_and_survives_pruning() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    let child = unit(
        &["conversation", "c", "chat", "note"],
        WRITER_A,
        10,
        UnitValue::inline(br#""child""#).unwrap(),
    );
    store
        .push(&a, &request(&store, WRITER_A, "child", vec![child.clone()]))
        .unwrap();
    assert!(state(&store, &a).contains(&child));
    let parent = unit(
        &["exists", "character", "c"],
        WRITER_A,
        100,
        UnitValue::inline(br#"{}"#).unwrap(),
    );
    store
        .push(&a, &request(&store, WRITER_A, "parent", vec![parent]))
        .unwrap();
    let deletion = unit(
        &["exists", "character", "c"],
        WRITER_A,
        20,
        UnitValue::Deleted,
    );
    store
        .push(
            &a,
            &request(&store, WRITER_A, "delete", vec![deletion.clone()]),
        )
        .unwrap();
    let revive = unit(
        &["exists", "character", "c"],
        WRITER_A,
        200,
        UnitValue::inline(br#"{}"#).unwrap(),
    );
    assert!(store
        .push(&a, &request(&store, WRITER_A, "revive", vec![revive]))
        .unwrap()
        .accepted_keys
        .is_empty());
    let stale_child = unit(
        &["messages", "c", "new-chat"],
        WRITER_A,
        201,
        UnitValue::inline(br#"[]"#).unwrap(),
    );
    assert!(store
        .push(
            &a,
            &request(&store, WRITER_A, "stale-child", vec![stale_child])
        )
        .unwrap()
        .accepted_keys
        .is_empty());
    expire_journal(root.path());
    store.maintain().unwrap();
    assert_eq!(state(&store, &a), vec![deletion]);
}

#[test]
fn parent_deletion_in_one_batch_suppresses_descendants_in_either_input_order() {
    for reverse in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let store = Store::init(root.path()).unwrap();
        let a = device(&store);
        let deletion = unit(
            &["exists", "character", "c"],
            WRITER_A,
            1,
            UnitValue::Deleted,
        );
        let child = unit(
            &["messages", "c", "chat"],
            WRITER_A,
            2,
            UnitValue::inline(br#"[]"#).unwrap(),
        );
        let changes = if reverse {
            vec![child, deletion.clone()]
        } else {
            vec![deletion.clone(), child]
        };
        assert_eq!(
            store
                .push(&a, &request(&store, WRITER_A, "joint", changes))
                .unwrap()
                .accepted_keys,
            vec![deletion.key.clone()]
        );
        assert_eq!(state(&store, &a), vec![deletion]);
    }
}

#[test]
fn ordinary_plugin_deletion_can_be_reinstalled() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    for (seq, value) in [
        (1, UnitValue::inline(br#"{}"#).unwrap()),
        (2, UnitValue::Deleted),
        (3, UnitValue::inline(br#"{"version":"new"}"#).unwrap()),
    ] {
        store
            .push(
                &a,
                &request(
                    &store,
                    WRITER_A,
                    &format!("plugin-{seq}"),
                    vec![unit(
                        &["record", "plugins", "synthetic"],
                        WRITER_A,
                        seq,
                        value.clone(),
                    )],
                ),
            )
            .unwrap();
        assert_eq!(state(&store, &a)[0].value, value);
    }
}

#[test]
fn absent_lookup_is_not_proof_but_cancellation_blocks_delayed_push_across_restart() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    let req = request(
        &store,
        WRITER_A,
        "delayed",
        vec![inline("a", WRITER_A, 1, "a")],
    );
    assert_eq!(
        store.operation(&a, "delayed").unwrap_err().code,
        "operation-not-found"
    );
    let cancel = CancelOperationRequest {
        body_digest: req.digest().unwrap(),
    };
    let receipt = store.cancel_operation(&a, "delayed", &cancel).unwrap();
    drop(store);
    let store = Store::open(root.path()).unwrap();
    assert_eq!(
        store.cancel_operation(&a, "delayed", &cancel).unwrap(),
        receipt
    );
    assert_eq!(
        store.push(&a, &req).unwrap_err().code,
        "operation-cancelled"
    );
    assert!(state(&store, &a).is_empty());
    assert_eq!(
        store
            .cancel_operation(
                &a,
                "delayed",
                &CancelOperationRequest {
                    body_digest: "0".repeat(64)
                }
            )
            .unwrap_err()
            .code,
        "operation-integrity"
    );
}

#[test]
fn cancellation_of_superseded_accepted_operation_returns_original_proof() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    let first = request(
        &store,
        WRITER_A,
        "first",
        vec![inline("a", WRITER_A, 1, "a")],
    );
    let accepted = store.push(&a, &first).unwrap();
    store
        .push(
            &a,
            &request(
                &store,
                WRITER_A,
                "later",
                vec![inline("a", WRITER_A, 2, "b")],
            ),
        )
        .unwrap();
    expire_journal(root.path());
    store.maintain().unwrap();
    let result = store
        .cancel_operation(
            &a,
            "first",
            &CancelOperationRequest {
                body_digest: first.digest().unwrap(),
            },
        )
        .unwrap();
    assert!(matches!(result, OperationReceipt::Accepted { receipt, .. } if receipt == accepted));
    drop(store);
    let store = Store::open(root.path()).unwrap();
    assert_eq!(store.push(&a, &first).unwrap(), accepted);
}

#[test]
fn sql_failure_rolls_back_state_journal_writer_identity_and_receipt_together() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    let req = request(
        &store,
        WRITER_A,
        "atomic",
        vec![inline("a", WRITER_A, 1, "a"), inline("b", WRITER_A, 1, "b")],
    );
    let db = Connection::open(root.path().join("metadata.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER synthetic_failure BEFORE INSERT ON units WHEN NEW.key='[\"root\",\"b\"]' BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
    assert_eq!(store.push(&a, &req).unwrap_err().code, "metadata-storage");
    assert_eq!(store.head().unwrap().seq.as_str(), "0");
    assert!(state(&store, &a).is_empty());
    for table in ["writers", "writer_versions", "journal", "operations"] {
        assert_eq!(
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    db.execute_batch("DROP TRIGGER synthetic_failure").unwrap();
    assert_eq!(store.push(&a, &req).unwrap().seq.0, 2);
}

#[test]
fn a_replayed_rejection_reports_the_original_error_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    let future = request(
        &store,
        WRITER_A,
        "future",
        vec![inline("time", WRITER_A, 4_000_000_000_000, "future")],
    );
    let noncanonical = request(
        &store,
        WRITER_A,
        "noncanonical",
        vec![unit(
            &["root", "shape"],
            WRITER_A,
            1,
            UnitValue::Inline {
                bytes: "eyB9".into(),
            },
        )],
    );
    let expected = [
        ("clock-skew", 409, Some(r#"["root","time"]"#.to_owned())),
        ("noncanonical-inline-value", 400, None),
    ];
    for (operation, expected) in [&future, &noncanonical].into_iter().zip(&expected) {
        let first = store.push(&a, operation).unwrap_err();
        assert_eq!(&(first.code, first.status, first.key), expected);
        let replay = store.push(&a, operation).unwrap_err();
        assert_eq!(&(replay.code, replay.status, replay.key), expected);
    }
    drop(store);
    let store = Store::open(root.path()).unwrap();
    for (operation, expected) in [&future, &noncanonical].into_iter().zip(&expected) {
        let replay = store.push(&a, operation).unwrap_err();
        assert_eq!(&(replay.code, replay.status, replay.key), expected);
    }
}

#[test]
fn acknowledgements_are_device_scoped_and_monotonic() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    let b = device(&store);
    store
        .push(
            &a,
            &request(
                &store,
                WRITER_A,
                "first",
                vec![inline("a", WRITER_A, 1, "a")],
            ),
        )
        .unwrap();
    store
        .acknowledge(&a, &AckRequest { seq: 1.into() })
        .unwrap();
    assert!(store
        .acknowledge(&a, &AckRequest { seq: 0.into() })
        .is_err());
    assert!(store
        .acknowledge(&b, &AckRequest { seq: 2.into() })
        .is_err());
    store
        .acknowledge(&b, &AckRequest { seq: 0.into() })
        .unwrap();
    assert!(store.operation(&b, "first").is_err());
    store.revoke_device(&a.id).unwrap();
    assert!(store.operation(&a, "first").is_err());
    assert!(store
        .cancel_operation(
            &a,
            "missing",
            &CancelOperationRequest {
                body_digest: "0".repeat(64)
            }
        )
        .is_err());
}

#[test]
fn admin_restore_revokes_old_posts_preserves_retirement_and_requires_fresh_writer() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let credential = store.add_device().unwrap();
    let a = store
        .authenticate(&credential.library_id, &credential.token)
        .unwrap();
    let deletion = unit(
        &["exists", "character", "retired"],
        WRITER_A,
        1,
        UnitValue::Deleted,
    );
    let first = request(&store, WRITER_A, "accepted", vec![deletion.clone()]);
    store.push(&a, &first).unwrap();
    let delayed = request(
        &store,
        WRITER_A,
        "delayed",
        vec![inline("later", WRITER_A, 2, "later")],
    );
    store.rotate_restored_epoch().unwrap();
    assert_eq!(store.push(&a, &delayed).unwrap_err().code, "unauthorized");
    assert!(store
        .authenticate(&credential.library_id, &credential.token)
        .is_err());
    let b = device(&store);
    assert_eq!(state(&store, &b), vec![deletion]);
    let error = store.changes(&b, 100.into(), 10).unwrap_err();
    assert_eq!(error.code, "journal-floor");
    assert_eq!(error.journal_floor, Some((1.into(), 1.into())));
    assert_eq!(
        store.push(&b, &delayed).unwrap_err().code,
        "writer-collision"
    );
    store
        .push(
            &b,
            &request(
                &store,
                WRITER_B,
                "fresh",
                vec![inline("fresh", WRITER_B, 3, "fresh")],
            ),
        )
        .unwrap();
    assert_eq!(store.changes(&b, 1.into(), 10).unwrap().items[0].seq.0, 2);
}
