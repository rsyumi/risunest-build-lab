//! Synthetic receive accounting through the real loopback client and native preparation.
use crate::server_sync::{cache::Cache, client::TestTraffic, lww_tests::{local, save, drain_publications, receive_cycle, LocalServerFixture}};
use risunest_sync_wire::{hash, payload_value, unit::{UnitKey, UnitValue, MAX_INLINE_UNIT_BYTES}};

fn own_publication(missing_body: bool) -> TestTraffic {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let client = server.client(&store);
    let cached = serde_json::json!("c".repeat(MAX_INLINE_UNIT_BYTES));
    let recoverable = serde_json::json!("r".repeat(MAX_INLINE_UNIT_BYTES));
    save(&mut store, &["root", "language"], serde_json::json!("en"));
    save(&mut store, &["root", "customCSS"], cached.clone());
    save(&mut store, &["root", "customGUI"], recoverable.clone());
    let cached_body = payload_value::encode(&cached).unwrap();
    let recovery_body = payload_value::encode(&recoverable).unwrap();
    assert!(cached_body.len() > MAX_INLINE_UNIT_BYTES);
    assert!(recovery_body.len() > MAX_INLINE_UNIT_BYTES);
    let cached_hash = hash(&cached_body);
    let recovery_hash = hash(&recovery_body);
    assert!(store.lww_verified_object_present(&cached_hash).unwrap());
    assert!(store.lww_verified_object_present(&recovery_hash).unwrap());
    let pending = store.lww_read_outbox(store.lww_binding_authority().unwrap(), 256).unwrap().entries;
    assert_eq!(pending.len(), 3, "the fixture must publish one inline and two object units");
    let value = |field: &str| &pending.iter().find(|entry| entry.key == UnitKey::new(&["root", field]).unwrap())
        .expect("every fixture field must be a published root unit").value;
    assert!(matches!(value("language"), UnitValue::Inline { .. }));
    for (field, expected_hash) in [("customCSS", &cached_hash), ("customGUI", &recovery_hash)] {
        assert!(matches!(value(field), UnitValue::Object { descriptor, .. } if &descriptor.object_hash == expected_hash));
    }
    drain_publications(&client, &mut store, &[]).unwrap();
    assert!(store.lww_receive_progress(store.lww_binding_authority().unwrap()).unwrap().is_empty(), "a push receipt never advances receive");
    if missing_body {
        // Remove only this synthetic published body, retaining its authoritative unit.
        store.connection.execute("DELETE FROM message_page_objects WHERE hash=?1", [&recovery_hash]).unwrap();
        Cache::open(&store.repository_root().join("server-sync/lww-cache")).unwrap().remove_derived(&recovery_hash).unwrap();
        assert!(!store.lww_verified_object_present(&recovery_hash).unwrap());
    }
    let counters = client.client.test_io.as_ref().unwrap();
    counters.reset();
    let received = receive_cycle(&client, &mut store, &[]).unwrap();
    let traffic = counters.traffic.lock().unwrap().clone();
    assert_eq!(received.received_units, 3, "every published fixture unit must reconcile");
    assert_eq!(traffic.prepared_units, received.received_units as u64);
    assert_eq!(traffic.journal_requests, 1);
    assert!(traffic.journal_response_bytes > traffic.journal_inline_decoded_bytes);
    assert!(traffic.journal_inline_decoded_bytes > 0);
    assert!(store.lww_verified_object_present(&cached_hash).unwrap());
    assert!(store.lww_verified_object_present(&recovery_hash).unwrap());
    assert_eq!(store.read_root(None).unwrap().value["customGUI"], recoverable);
    assert_eq!(store.read_root(None).unwrap().value["customCSS"], cached);
    assert!(!store.lww_receive_progress(store.lww_binding_authority().unwrap()).unwrap().is_empty());
    if missing_body {
        assert!(traffic.object_transfer_requests + traffic.object_get_requests > 0, "missing body is fetched through normal recovery");
        assert!(traffic.object_transfer_response_bytes + traffic.object_get_response_bytes > 0);
    } else {
        assert_eq!((traffic.object_get_requests, traffic.object_transfer_requests), (0, 0), "verified cached bodies need neither GETs nor framed transfers");
        assert_eq!((traffic.object_get_response_bytes, traffic.object_transfer_response_bytes), (0, 0));
    }
    traffic
}

#[test]
fn own_publication_attributes_journal_inline_cached_and_missing_bodies() {
    let cached = own_publication(false);
    let missing = own_publication(true);
    println!("{}", serde_json::json!({
        "schema": "risunest.receive-traffic/v1",
        "wireBytes": "response bodies only; HTTP headers and TLS excluded",
        "inlineBytes": "decoded subset of journal values; never add to wire bytes",
        "preparedUnits": "local validated units, not object downloads",
        "cached": cached,
        "missingBodyControl": missing,
    }));
}
