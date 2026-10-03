//! Synthetic interoperability vector generator, excluded from normal builds.
use risunest_external_storage_format::{
    content_identity::hash,
    control::{
        BackupBundleDocument, BackupPointDocument, BackupPointKind, BundleSource, HeadDocument,
    },
    crypto, pack,
    section::{
        hypa_entry_key, local_plugin_entry_key, HypaValue, InlineOrObject, LocalPluginValue,
        LocalSettingValue, ObjectReference, PluginSpace, SectionEntry, SectionEntryVersion,
        SectionKind, SectionValue, ValueOrObject, SECTION_CODEC,
    },
    snapshot::{
        envelope_length, keyed_object_id, open_envelope, seal_envelope, CatalogDocument,
        CatalogKind, LibrarySnapshotRef, ObjectRole, PublicObjectHeader, SectionSnapshotRef,
        StoredObject, SyncStateDocument, WireLocator,
    },
};
use risunest_sync_wire::head::Sequence;
use std::collections::BTreeMap;

const REPOSITORY: &str = "synthetic-repository";
const LIBRARY: &str = "synthetic-library";
const OBJECT_NAMESPACE: &str = "synthetic-capture-job";
const MAX_DOCUMENT_BYTES: usize = 64 * 1024;

fn bytes(value: &serde_json::Value, key: &str) -> Vec<u8> {
    serde_json::from_value(value[key].clone()).unwrap()
}

fn stored(role: ObjectRole, object_id: &str, plaintext: &[u8]) -> StoredObject {
    let header = PublicObjectHeader::new(
        REPOSITORY.into(),
        object_id.into(),
        role,
        plaintext.len() as u64,
    )
    .unwrap();
    StoredObject {
        ciphertext_length: envelope_length(&header).unwrap(),
        ciphertext_sha256: [2; 32],
        plaintext_length: plaintext.len() as u64,
        plaintext_sha256: hash(plaintext),
        locator: WireLocator {
            connection_identity: "synthetic-account/root".into(),
            collection: Some("synthetic".into()),
            object: format!("opaque-{object_id}"),
        },
        header,
    }
}

fn original_catalog() -> StoredObject {
    let plaintext = CatalogDocument::leaf(CatalogKind::Records, vec![], vec![])
        .unwrap()
        .encode(MAX_DOCUMENT_BYTES)
        .unwrap();
    let mut root = stored(ObjectRole::Catalog, "original-units-empty", &plaintext);
    // A fixed synthetic envelope keeps document goldens reproducible.
    let sealed = hex::decode(concat!(
        "524e5831990000007b22736368656d61223a22726973756e6573742e65787465726e616c2d6f626a6563742f7631222c2272",
        "65706f7369746f72794964223a2273796e7468657469632d7265706f7369746f7279222c226f626a6563744964223a226f72",
        "6967696e616c2d756e6974732d656d707479222c22726f6c65223a22636174616c6f67222c22706c61696e746578744c656e",
        "677468223a22313335227d524e45318700000000000000a1d987d309fee22d2aaf68ba15c14f34645abb2a4f2e4146980000",
        "00e1b796584a9a9b7c2259cf30dec312c8d2b43ceb2eff2ed271aecef52f58d523fa0304438493731c32e27a46b4d275acc3",
        "2e1f248725e5fa38c039aca25646fe636b6efac78d6373ecb65cbcd4365b4a8d179e98bc01974a3dfeccff70d8189b49f948",
        "5dc7c8a5391670cac057fdb217a46471616c0279d725628eacb203e9b72b0fa623bb0e0e572b627ef7e24165a605720ee5ee",
        "274a8911000000c0e554fd295e14d511f3ae552d7515106c",
    )).unwrap();
    let mut opened = Vec::new();
    assert_eq!(
        open_envelope(
            &mut sealed.as_slice(),
            &mut opened,
            &[7; 32],
            MAX_DOCUMENT_BYTES as u64
        )
        .unwrap(),
        root.header
    );
    assert_eq!(opened, plaintext);
    assert_eq!(root.ciphertext_length, sealed.len() as u64);
    root.ciphertext_sha256 = hash(&sealed);
    root
}

const HYPA_KEY: &str = "3f2a1b0c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8";

fn section_reference(kind: SectionKind, generation: u64, max_write_clock: u64) -> SectionSnapshotRef {
    SectionSnapshotRef {
        kind,
        codec: SECTION_CODEC.into(),
        generation: Sequence::from(generation),
        gc_floor: Sequence::from(generation / 2),
        max_write_clock: Sequence::from(max_write_clock),
        entries_root: stored(
            ObjectRole::Catalog,
            &format!("catalog-section-{}", kind.id()),
            kind.id().as_bytes(),
        ),
        content_fingerprint: hash(kind.id().as_bytes()),
    }
}

/// Section identifiers, key shapes, counters, device values carried as objects
/// and the empty section. An empty published section carries a reference; a
/// section that was never published carries no map key at all.
fn section_entries() -> Vec<Vec<u8>> {
    let version = |clock: u64, writer: &str| {
        Some(SectionEntryVersion {
            write_clock: Sequence::from(clock),
            writer_id: writer.into(),
        })
    };
    [
        SectionEntry::new(
            SectionKind::Hypa,
            hypa_entry_key(HYPA_KEY).unwrap(),
            SectionValue::Hypa(HypaValue {
                producer: "hypa-v2".into(),
                model: "text-embedding-3-small".into(),
                endpoint: None,
                preprocess_version: 1,
                dimensions: 8,
                vector: InlineOrObject::inline(&(0..32).map(|n| n as u8).collect::<Vec<_>>())
                    .unwrap(),
                metadata: None,
            }),
            version(1, "writer-a"),
        )
        .unwrap(),
        SectionEntry::new(
            SectionKind::Hypa,
            hypa_entry_key(&"0".repeat(64)).unwrap(),
            SectionValue::tombstone(Sequence::from(9u64), 1_760_000_000_000),
            version(18_446_744_073_709_551_615, "writer-b"),
        )
        .unwrap(),
        SectionEntry::new(
            SectionKind::LocalPlugins,
            local_plugin_entry_key("provider-manager", "json", "settings").unwrap(),
            SectionValue::LocalPlugin(LocalPluginValue {
                space: PluginSpace::Json,
                value: ValueOrObject::Inline(serde_json::json!({ "zeta": [1, 2], "alpha": null })),
            }),
            version(4, "writer-a"),
        )
        .unwrap(),
        SectionEntry::new(
            SectionKind::LocalPlugins,
            local_plugin_entry_key("yumi-translator", "string", "cache:index").unwrap(),
            SectionValue::LocalPlugin(LocalPluginValue {
                space: PluginSpace::String,
                value: ValueOrObject::Inline(serde_json::Value::String("kept verbatim".into())),
            }),
            None,
        )
        .unwrap(),
        SectionEntry::new(
            SectionKind::LocalSettings,
            "risuNestDeviceSettings".into(),
            SectionValue::LocalSetting(LocalSettingValue {
                value: ValueOrObject::Inline(serde_json::json!({ "startup": "restore" })),
            }),
            None,
        )
        .unwrap(),
        SectionEntry::new(
            SectionKind::LocalPlugins,
            local_plugin_entry_key("provider-manager", "json", "history").unwrap(),
            SectionValue::LocalPlugin(LocalPluginValue {
                space: PluginSpace::Json,
                value: ValueOrObject::Object(ObjectReference {
                    content_sha256: [9; 32],
                    byte_length: 70_000,
                }),
            }),
            version(5, "writer-a"),
        )
        .unwrap(),
        SectionEntry::new(
            SectionKind::LocalSettings,
            "risuNestUpdateSettings".into(),
            SectionValue::LocalSetting(LocalSettingValue {
                value: ValueOrObject::Object(ObjectReference {
                    content_sha256: [10; 32],
                    byte_length: 4_097,
                }),
            }),
            None,
        )
        .unwrap(),
    ]
    .iter()
    .map(|entry| entry.encode().unwrap())
    .collect()
}

fn documents() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let library = LibrarySnapshotRef {
        record_catalog: stored(ObjectRole::Catalog, "catalog-records", b"records"),
        asset_catalog: stored(ObjectRole::Catalog, "catalog-assets", b"assets"),
        content_fingerprint: [5; 32],
    };
    let state = SyncStateDocument::new(
        "state-synthetic".into(),
        REPOSITORY.into(),
        LIBRARY.into(),
        "epoch-synthetic".into(),
        Sequence::from(17u64),
        Some("state-parent".into()),
        "writer-synthetic".into(),
        1_726_272_000_000,
        library.clone(),
        BTreeMap::from([
            (
                SectionKind::Hypa.id().into(),
                section_reference(SectionKind::Hypa, 17, 240),
            ),
            (
                SectionKind::LocalPlugins.id().into(),
                section_reference(SectionKind::LocalPlugins, 12, 31),
            ),
        ]),
    )
    .unwrap();
    let state_fingerprint = state.state_fingerprint;
    let state_bytes = state.encode(MAX_DOCUMENT_BYTES).unwrap();
    let state_object = stored(ObjectRole::SyncState, "state-synthetic", &state_bytes);
    let head = HeadDocument::new(
        REPOSITORY.into(),
        LIBRARY.into(),
        "commit-synthetic".into(),
        Some("commit-parent".into()),
        state_fingerprint,
        state_object,
    )
    .unwrap()
    .encode(MAX_DOCUMENT_BYTES)
    .unwrap();
    // A selected section with no entries is still included, which is what
    // separates an emptied section from one this capture left out.
    let bundle_document = BackupBundleDocument::new(
        REPOSITORY.into(),
        "bundle-synthetic".into(),
        BundleSource::Device {
            writer_id: "writer-synthetic".into(),
        },
        1_726_272_000_001,
        Some(Sequence::from(17u64)),
        Some(Sequence::from(4u64)),
        None,
        library,
        BTreeMap::from([
            (
                SectionKind::Hypa.id().into(),
                section_reference(SectionKind::Hypa, 0, 0),
            ),
            (
                SectionKind::LocalSettings.id().into(),
                section_reference(SectionKind::LocalSettings, 3, 3),
            ),
        ]),
        Some(original_catalog()),
    )
    .unwrap();
    let bundle = bundle_document.encode(MAX_DOCUMENT_BYTES).unwrap();
    let bundle_object = stored(ObjectRole::BackupBundle, "bundle-synthetic", &bundle);
    let point = BackupPointDocument::single(
        REPOSITORY.into(),
        "point-synthetic".into(),
        BackupPointKind::Manual,
        1_726_272_000_002,
        bundle_object,
    )
    .unwrap()
    .encode(MAX_DOCUMENT_BYTES)
    .unwrap();
    (state_bytes, head, bundle, point)
}

fn seal(bytes: &[u8], key: &[u8; 32], object_id: &str, role: ObjectRole) -> Vec<u8> {
    let header = PublicObjectHeader::new(
        REPOSITORY.into(),
        object_id.into(),
        role,
        bytes.len() as u64,
    )
    .unwrap();
    let mut envelope = Vec::new();
    seal_envelope(
        &mut std::io::Cursor::new(bytes),
        &mut envelope,
        key,
        &header,
    )
    .unwrap();
    envelope
}

fn open(envelope: &[u8], key: &[u8; 32], object_id: &str, role: ObjectRole) -> Vec<u8> {
    let mut plaintext = Vec::new();
    let header = open_envelope(
        &mut std::io::Cursor::new(envelope),
        &mut plaintext,
        key,
        MAX_DOCUMENT_BYTES as u64,
    )
    .unwrap();
    assert_eq!(header.repository_id, REPOSITORY);
    assert_eq!(header.object_id, object_id);
    assert_eq!(header.role, role);
    assert_eq!(header.plaintext_length, plaintext.len() as u64);
    plaintext
}

fn verify_wasm_vector(value: serde_json::Value) {
    let plaintext = bytes(&value, "plaintext");
    assert_eq!(
        pack::decompress(
            &bytes(&value, "compressed"),
            plaintext.len(),
            &hash(&plaintext),
        )
        .unwrap(),
        plaintext
    );
    let (state, head, bundle, point) = documents();
    let key = [7; 32];
    let opened_state = open(
        &bytes(&value, "stateEnvelope"),
        &key,
        "state-synthetic",
        ObjectRole::SyncState,
    );
    assert_eq!(opened_state, state);
    SyncStateDocument::decode(&opened_state, MAX_DOCUMENT_BYTES).unwrap();
    let opened_head = open(
        &bytes(&value, "headEnvelope"),
        &key,
        "head",
        ObjectRole::Head,
    );
    assert_eq!(opened_head, head);
    HeadDocument::decode(&opened_head, MAX_DOCUMENT_BYTES).unwrap();
    let opened_bundle = open(
        &bytes(&value, "bundleEnvelope"),
        &key,
        "bundle-synthetic",
        ObjectRole::BackupBundle,
    );
    assert_eq!(opened_bundle, bundle);
    BackupBundleDocument::decode(&opened_bundle, MAX_DOCUMENT_BYTES).unwrap();
    let opened_point = open(
        &bytes(&value, "pointEnvelope"),
        &key,
        "point-synthetic",
        ObjectRole::BackupPoint,
    );
    assert_eq!(opened_point, point);
    BackupPointDocument::decode(&opened_point, MAX_DOCUMENT_BYTES).unwrap();

    let expected_entries = section_entries();
    let published: Vec<Vec<u8>> = serde_json::from_value(value["sectionEntries"].clone()).unwrap();
    assert_eq!(published, expected_entries);
    for entry in &published {
        SectionEntry::decode(entry).unwrap();
    }

    let document_hash = hash(&state);
    assert_eq!(
        value["keyedPackId"].as_str().unwrap(),
        keyed_object_id(&key, OBJECT_NAMESPACE, ObjectRole::Pack, &document_hash).unwrap()
    );
    assert_eq!(
        value["keyedCatalogId"].as_str().unwrap(),
        keyed_object_id(&key, OBJECT_NAMESPACE, ObjectRole::Catalog, &document_hash).unwrap()
    );
    println!("WASM-to-native RNX1, control and naming vectors passed.");
}

fn main() {
    if let Some(path) = std::env::args().nth(1) {
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        verify_wasm_vector(value);
        return;
    }

    let plaintext = (0..100_005)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let key = [7; 32];
    let binding = "synthetic-repository/synthetic-object/data/v1";
    let mut ciphertext = Vec::new();
    let compressed = pack::compress(&plaintext).unwrap();
    crypto::encrypt(
        &mut std::io::Cursor::new(&plaintext),
        &mut ciphertext,
        &key,
        binding.as_bytes(),
        plaintext.len() as u64,
    )
    .unwrap();
    let (state, head, bundle, point) = documents();
    let document_hash = hash(&state);
    println!(
        "{}",
        serde_json::json!({
            "key": key,
            "binding": binding,
            "plaintext": plaintext,
            "ciphertext": ciphertext,
            "hash": hash(&plaintext),
            "compressed": compressed,
            "state": state,
            "head": head,
            "bundle": bundle,
            "point": point,
            "sectionEntries": section_entries(),
            "stateEnvelope": seal(&state, &key, "state-synthetic", ObjectRole::SyncState),
            "headEnvelope": seal(&head, &key, "head", ObjectRole::Head),
            "bundleEnvelope": seal(&bundle, &key, "bundle-synthetic", ObjectRole::BackupBundle),
            "pointEnvelope": seal(&point, &key, "point-synthetic", ObjectRole::BackupPoint),
            "objectNamespace": OBJECT_NAMESPACE,
            "keyedPackId": keyed_object_id(&key, OBJECT_NAMESPACE, ObjectRole::Pack, &document_hash).unwrap(),
            "keyedCatalogId": keyed_object_id(&key, OBJECT_NAMESPACE, ObjectRole::Catalog, &document_hash).unwrap(),
        })
    );
}
