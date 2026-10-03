use super::lww_tests::{drain_publications, header, local, publish_cycle, receive_available, save, LocalServerFixture};
use crate::persistent_store::PersistentStore;
use risunest_sync_wire::{descriptor::RecordDescriptor, unit::{UnitValue, MAX_INLINE_UNIT_BYTES}, MAX_METADATA_BYTES};
use serde_json::{json, Value};

const FIELDS: [&str; 5] = ["additionalPrompt", "NAIImgUrl", "autofillRequestUrl", "language", "ImagenModel"];

fn pending(store: &PersistentStore) -> usize {
    store.lww_read_outbox(store.lww_binding_authority().unwrap(), 256).unwrap().entries.len()
}

#[test]
fn large_root_field_converges_through_a_chunked_body_without_asset_custody() {
    let server = LocalServerFixture::new();
    let (_a, mut a) = local();
    let (_b, mut b) = local();
    let ca = server.client(&a);
    let cb = server.client(&b);
    let large = Value::String("x".repeat(2 * MAX_METADATA_BYTES));
    save(&mut a, &["root", "additionalPrompt"], large.clone());
    let hash = risunest_sync_wire::hash(&risunest_sync_wire::payload_value::encode(&large).unwrap());
    let entry = a.lww_read_outbox(0.into(), 256).unwrap().entries.remove(0);
    assert_eq!(entry.value, UnitValue::object(RecordDescriptor::content(hash.clone())).unwrap());
    drain_publications(&ca, &mut a, &[]).unwrap();
    assert_eq!(pending(&a), 0);
    receive_available(&cb, &mut b, &[]).unwrap();
    assert_eq!(b.read_root(None).unwrap().value["additionalPrompt"], large);
    assert!(b.lww_verified_object_present(&hash).unwrap());
    assert!(crate::asset_repository::PayloadCas::new(b.repository_root()).unwrap().stat_object(&hash).unwrap().is_none());
    assert!(super::residency::Residency::open(b.repository_root()).unwrap().object(&hash, None).unwrap().is_none());
    assert_eq!(pending(&b), 0);
}

#[test]
fn pushes_split_outbox_pages_by_canonical_request_bytes() {
    let server = LocalServerFixture::new();
    let (_a, mut a) = local();
    let (_b, mut b) = local();
    let ca = server.client(&a);
    let cb = server.client(&b);
    let values = FIELDS.iter().enumerate()
        .map(|(i, field)| (*field, Value::String(format!("{i}{}", "v".repeat(MAX_INLINE_UNIT_BYTES - 3)))))
        .collect::<Vec<_>>();
    for (field, value) in &values {
        save(&mut a, &["root", field], value.clone());
    }
    assert!(a.lww_read_outbox(0.into(), 256).unwrap().entries.iter().all(|entry| matches!(entry.value, UnitValue::Inline { .. })));
    assert!(publish_cycle(&ca, &mut a, &[]).unwrap().is_some());
    let remaining = pending(&a);
    assert!(remaining > 0 && remaining < FIELDS.len(), "{remaining}");
    drain_publications(&ca, &mut a, &[]).unwrap();
    assert_eq!(pending(&a), 0);
    receive_available(&cb, &mut b, &[]).unwrap();
    let root = b.read_root(None).unwrap().value;
    for (field, value) in &values {
        assert!(root[field] == *value, "{field}");
    }
}

#[test]
fn a_rejected_single_entry_push_reports_unit_too_large_and_keeps_the_entry() {
    let server = LocalServerFixture::with_router(|router| {
        router.layer(axum::middleware::from_fn(
            |request: axum::extract::Request, next: axum::middleware::Next| async move {
                if request.uri().path() == "/push" {
                    return axum::response::Response::builder()
                        .status(413)
                        .body(axum::body::Body::from("{\"error\":\"body-too-large\"}"))
                        .unwrap();
                }
                next.run(request).await
            },
        ))
    });
    let (_a, mut a) = local();
    let ca = server.client(&a);
    save(&mut a, &["root", "language"], json!("synthetic"));
    let request = header(&a);
    let error = ca.push(&mut a, &request, &[]).unwrap_err();
    assert_eq!((error.code.as_str(), error.retryable), ("unit-too-large", false));
    assert_eq!(pending(&a), 1);
}
