use risunest_external_storage_format::snapshot::{
    envelope_length, CatalogDocument, CatalogEntryFragment, CatalogEntryKind, CatalogKind,
    LibrarySnapshotRef, ObjectRole, PublicObjectHeader, StoredChunk, StoredObject, SyncStateDocument,
};
use risunest_external_storage_format::{
    control::{BackupBundleDocument, BackupPointDocument, BackupPointKind, BundleSource, InventoryEntry, LeaseDocument, LeaseKind},
    section::{ObjectReference, SectionValue, HypaValue, InlineOrObject, LocalPluginValue, PluginSpace, ValueOrObject},
    catalog::{ChunkReference, Entry},
};
use risunest_sync_wire::Sequence;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn object(role: ObjectRole, id: &str) -> StoredObject {
    let header = PublicObjectHeader::new("repository".into(), id.into(), role, 10).unwrap();
    StoredObject {
        ciphertext_length: envelope_length(&header).unwrap(),
        ciphertext_sha256: [2; 32],
        plaintext_length: 10,
        plaintext_sha256: [1; 32],
        locator: risunest_external_storage_format::snapshot::WireLocator {
            connection_identity: "account/root".into(), collection: None, object: id.into(),
        },
        header,
    }
}
fn catalog() -> CatalogDocument {
    CatalogDocument::leaf(CatalogKind::Records, vec![CatalogEntryFragment {
        kind: CatalogEntryKind::Record, key: "record/a".into(), content_sha256: [3; 32],
        byte_length: 4, fragment_index: 0, fragment_count: 1,
        chunks: vec![StoredChunk {
            pack_id: "pack".into(), offset: 0, stored_length: 10, plaintext_length: 4,
            plaintext_sha256: [3; 32],
        }],
    }], vec![object(ObjectRole::Pack, "pack")]).unwrap()
}
fn state() -> SyncStateDocument {
    SyncStateDocument::new("state".into(), "repository".into(), "library".into(), "epoch".into(),
        Sequence::from(1u64), None, "writer".into(), u64::MAX, LibrarySnapshotRef {
            record_catalog: object(ObjectRole::Catalog, "records"),
            asset_catalog: object(ObjectRole::Catalog, "assets"), content_fingerprint: [3; 32],
        }, BTreeMap::new()).unwrap()
}
fn rejects<T: DeserializeOwned>(value: &Value, field: &str, overflow: &str) {
    for invalid in [json!(0), json!(0.5), json!(true), Value::Null, json!(""), json!("00"),
        json!("+1"), json!("-1"), json!(" 1"), json!("1 "), json!(overflow)] {
        let mut changed = value.clone();
        changed[field] = invalid;
        assert!(serde_json::from_value::<T>(changed.clone()).is_err(), "accepted {changed}");
    }
}

#[test]
fn snapshot_metadata_rejects_numeric_noncanonical_and_overflow_counters() {
    let pack = serde_json::to_value(object(ObjectRole::Pack, "pack")).unwrap();
    rejects::<PublicObjectHeader>(&pack["header"], "plaintextLength", "18446744073709551616");
    for field in ["ciphertextLength", "plaintextLength"] {
        rejects::<StoredObject>(&pack, field, "18446744073709551616");
    }
    let value = serde_json::to_value(catalog()).unwrap();
    rejects::<CatalogDocument>(&value, "level", "65536");
    rejects::<CatalogEntryFragment>(&value["entries"][0], "byteLength", "18446744073709551616");
    for field in ["fragmentIndex", "fragmentCount"] {
        rejects::<CatalogEntryFragment>(&value["entries"][0], field, "4294967296");
    }
    for field in ["offset", "storedLength", "plaintextLength"] {
        rejects::<StoredChunk>(&value["entries"][0]["chunks"][0], field, "18446744073709551616");
    }
    rejects::<SyncStateDocument>(&serde_json::to_value(state()).unwrap(), "createdAtMs", "18446744073709551616");
}

#[test]
fn snapshot_metadata_decimal_wire_roundtrips_with_hash_bytes_unchanged() {
    let doc = catalog();
    let bytes = doc.encode(4096).unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["level"], "0");
    assert_eq!(value["entries"][0]["byteLength"], "4");
    assert_eq!(value["entries"][0]["fragmentIndex"], "0");
    assert_eq!(value["entries"][0]["fragmentCount"], "1");
    assert_eq!(value["entries"][0]["chunks"][0]["offset"], "0");
    assert_eq!(value["entries"][0]["chunks"][0]["storedLength"], "10");
    assert_eq!(value["entries"][0]["chunks"][0]["plaintextLength"], "4");
    assert_eq!(value["packs"][0]["plaintextLength"], "10");
    assert_eq!(value["packs"][0]["header"]["plaintextLength"], "10");
    assert!(value["packs"][0]["ciphertextLength"].is_string());
    assert_eq!(value["packs"][0]["ciphertextSha256"][0], 2);
    assert_eq!(value["entries"][0]["contentSha256"][0], 3);
    assert_eq!(CatalogDocument::decode(&bytes, 4096).unwrap(), doc);
    let doc = state();
    let bytes = doc.encode(4096).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap()["createdAtMs"], "18446744073709551615");
    assert_eq!(SyncStateDocument::decode(&bytes, 4096).unwrap(), doc);
    let header = PublicObjectHeader::new("repository".into(), "pack".into(), ObjectRole::Pack, u64::MAX).unwrap();
    assert_eq!(serde_json::to_string(&header).unwrap(), r#"{"schema":"risunest.external-object/v1","repositoryId":"repository","objectId":"pack","role":"pack","plaintextLength":"18446744073709551615"}"#);
    assert_eq!(serde_json::from_value::<PublicObjectHeader>(serde_json::to_value(&header).unwrap()).unwrap(), header);
}

#[test]
fn backup_lease_inventory_and_catalog_counters_use_strict_decimal_strings() {
    let bundle = BackupBundleDocument::new("repository".into(), "bundle".into(),
        BundleSource::SyncState { commit_id: "commit".into() }, 17,
        Some(Sequence::from(1u64)), Some(Sequence::from(1u64)), None,
        state().library, BTreeMap::new(), None).unwrap();
    let point = BackupPointDocument::single("repository".into(), "point".into(),
        BackupPointKind::Backup, 19, object(ObjectRole::BackupBundle, "bundle")).unwrap();
    let lease = LeaseDocument::new("writer".into(), "job".into(), LeaseKind::Work, 2, 23).unwrap();
    let inventory = InventoryEntry {
        object_id: "pack".into(), role: ObjectRole::Pack, ciphertext_length: 11,
        ciphertext_sha256: [3; 32], plaintext_length: 13, plaintext_sha256: [4; 32],
    };
    let chunk = ChunkReference { pack_id: "pack".into(), offset: 29, length: 31, hash: [5; 32] };
    let entry = Entry { key: "asset".into(), content_hash: [6; 32], byte_length: 37, chunks: vec![chunk.clone()] };
    for (value, fields) in [
        (serde_json::to_value(&bundle).unwrap(), vec!["capturedAtMs"]),
        (serde_json::to_value(&point).unwrap(), vec!["createdAtMs"]),
        (serde_json::to_value(&lease).unwrap(), vec!["seq", "createdAtMs", "expiresAtMs"]),
        (serde_json::to_value(&inventory).unwrap(), vec!["ciphertextLength", "plaintextLength"]),
        (serde_json::to_value(&chunk).unwrap(), vec!["offset", "length"]),
        (serde_json::to_value(&entry).unwrap(), vec!["byteLength"]),
    ] {
        for field in fields { assert!(value[field].is_string(), "{value}"); }
    }
    rejects::<BackupBundleDocument>(&serde_json::to_value(&bundle).unwrap(), "capturedAtMs", "18446744073709551616");
    rejects::<BackupPointDocument>(&serde_json::to_value(&point).unwrap(), "createdAtMs", "18446744073709551616");
    for field in ["seq", "createdAtMs", "expiresAtMs"] {
        rejects::<LeaseDocument>(&serde_json::to_value(&lease).unwrap(), field, "18446744073709551616");
    }
    for field in ["ciphertextLength", "plaintextLength"] {
        rejects::<InventoryEntry>(&serde_json::to_value(&inventory).unwrap(), field, "18446744073709551616");
    }
    for field in ["offset", "length"] {
        rejects::<ChunkReference>(&serde_json::to_value(&chunk).unwrap(), field, "18446744073709551616");
    }
    rejects::<Entry>(&serde_json::to_value(&entry).unwrap(), "byteLength", "18446744073709551616");
    assert_eq!(BackupBundleDocument::decode(&bundle.encode(8192).unwrap(), 8192).unwrap(), bundle);
    assert_eq!(BackupPointDocument::decode(&point.encode(8192).unwrap(), 8192).unwrap(), point);
    assert_eq!(LeaseDocument::decode(&lease.encode(8192).unwrap(), 8192).unwrap(), lease);
}

#[test]
fn section_control_metadata_changes_without_normalizing_user_payload_numbers() {
    let object = ObjectReference { content_sha256: [7; 32], byte_length: u64::MAX };
    let encoded = serde_json::to_value(&object).unwrap();
    assert_eq!(encoded["byteLength"], "18446744073709551615");
    assert_eq!(encoded["contentSha256"][0], 7);
    rejects::<ObjectReference>(&encoded, "byteLength", "18446744073709551616");
    let marker = SectionValue::tombstone(Sequence::from(2u64), 41);
    let mut value = serde_json::to_value(&marker).unwrap();
    assert_eq!(value["tombstone"]["firstPublishedAtMs"], "41");
    assert_eq!(serde_json::from_value::<SectionValue>(value.clone()).unwrap(), marker);
    for invalid in [json!(41), json!("041"), json!("18446744073709551616")] {
        value["tombstone"]["firstPublishedAtMs"] = invalid;
        assert!(serde_json::from_value::<SectionValue>(value.clone()).is_err());
    }
    let hypa = HypaValue {
        producer: "synthetic".into(), model: "model".into(), endpoint: None,
        preprocess_version: 2, dimensions: 1, vector: InlineOrObject::inline(&[0; 4]).unwrap(),
        metadata: Some(json!({"n": 1.25, "nested": [3]})),
    };
    let value = serde_json::to_value(&hypa).unwrap();
    assert_eq!(value["preprocessVersion"], 2);
    assert_eq!(value["dimensions"], 1);
    assert_eq!(value["metadata"], json!({"n": 1.25, "nested": [3]}));
    assert_eq!(serde_json::from_value::<HypaValue>(value).unwrap(), hypa);
    let payload = json!({"fraction": 0.5, "count": 9});
    let plugin = LocalPluginValue { space: PluginSpace::Json, value: ValueOrObject::Inline(payload.clone()) };
    assert_eq!(serde_json::to_value(&plugin).unwrap()["value"], payload);
}


#[test]
fn logical_owner_head_control_counters_are_decimal_without_changing_payload_numbers() {
    use risunest_external_storage_format::logical_records::{LogicalOwnerHead, LogicalOwnerLocator};
    let head = LogicalOwnerHead {
        owner: LogicalOwnerLocator::RootModule { module_id: "module".into() },
        present: true, manifest_hash: Some("a".repeat(64)),
        entry_count: u64::MAX, property_index: Some(u64::MAX),
    };
    let value = serde_json::to_value(&head).unwrap();
    assert_eq!(value["entryCount"], "18446744073709551615");
    assert_eq!(value["propertyIndex"], "18446744073709551615");
    assert_eq!(serde_json::from_value::<LogicalOwnerHead>(value.clone()).unwrap(), head);
    rejects::<LogicalOwnerHead>(&value, "entryCount", "18446744073709551616");
    // propertyIndex remains optional, but a supplied value must be canonical decimal.
    for invalid in [json!(0), json!(0.5), json!(true), json!(""), json!("00"),
        json!("+1"), json!("-1"), json!(" 1"), json!("1 "), json!("18446744073709551616")] {
        let mut changed = value.clone();
        changed["propertyIndex"] = invalid;
        assert!(serde_json::from_value::<LogicalOwnerHead>(changed.clone()).is_err(), "accepted {changed}");
    }
    let absent = LogicalOwnerHead { entry_count: 0, property_index: None, ..head };
    let value = serde_json::to_value(&absent).unwrap();
    assert!(value.get("propertyIndex").is_none());
    assert_eq!(serde_json::from_value::<LogicalOwnerHead>(value).unwrap(), absent);
}
