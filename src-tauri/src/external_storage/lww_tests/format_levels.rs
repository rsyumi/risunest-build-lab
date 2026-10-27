use super::*;
use crate::server_sync::library_format::LEVEL;
use risunest_sync_wire::library_format;
use serde_json::json;

/// The published segments in sequence order, opened.
async fn segments(f: &CycleFixture, engine: &ExternalLwwEngine) -> Vec<(String, lww_segment::Segment)> {
    let mut listed = engine.listing(&Cancellation::default()).await.unwrap();
    listed.sort_by_key(|receipt| parse_segment_object_id(&receipt.locator.object).unwrap().1);
    listed.into_iter().map(|receipt| {
        let object = receipt.locator.object;
        let (writer, seq, _) = parse_segment_object_id(&object).unwrap();
        let bytes = f.provider.contents(&object).unwrap();
        let payload = lww_segment::open(&bytes, &engine.library, writer, seq, &engine.root_key).unwrap();
        (object, payload)
    }).collect()
}
fn levels(payload: &lww_segment::Segment) -> Vec<Option<u64>> {
    payload.changes.iter().filter(|change| library_format::is_key(&change.key)).map(|change| library_format::level(&change.value)).collect()
}

#[test]
fn every_segment_declares_this_level_once_and_a_level_only_segment_carries_nothing_else() {
    run(async {
        use crate::persistent_store::sync_selection::{SwitchBindingRequest, SyncTarget};
        let mut f = CycleFixture::new();
        let cancel = Cancellation::default();
        set(&mut f.a, &["root", "language"], json!("ja"));
        f.publish_a().await;
        f.sender.publish_format_level(&mut f.a, DecimalU64(0), &cancel).await.unwrap();
        let published = segments(&f, &f.sender).await;
        assert_eq!(published.len(), 2);
        assert_eq!(levels(&published[0].1), [Some(LEVEL)]);
        assert_eq!(published[0].1.changes.len(), 2);
        assert_eq!(levels(&published[1].1), [Some(LEVEL)]);
        assert_eq!(published[1].1.changes.len(), 1);
        assert!(f.a.lww_read_outbox(DecimalU64(0), 100).unwrap().entries.is_empty());
        assert_eq!(f.receive_b().await, 2);
        assert_eq!(f.b.read_root(None).unwrap().value["language"], "ja");
        // The level unit is not content of the device's own.
        assert_eq!(f.b.lww_binding_content().unwrap().opaque_shared_unit_count.0, 0);

        // A device that received the level unit queues it with the rest of
        // its state when it first publishes to a repository, and its
        // segment still declares the level once.
        let before = f.b.lww_binding_state().unwrap();
        let target = SyncTarget::External("repository".into());
        let inspection_id = f.b.register_lww_binding_inspection(before.target_authority, &target, &f.receiver.repository.connection_identity, &f.receiver.library).unwrap();
        let authority = f.b.switch_lww_binding(&SwitchBindingRequest {
            header: crate::persistent_store::lww::Header { binding_authority: before.target_authority, request_id: uuid::Uuid::new_v4().to_string() },
            expected_selection_epoch: before.selection_epoch, target, inspection_id: Some(inspection_id), initial_publication: true,
        }).unwrap().target_authority;
        let header = crate::persistent_store::lww::Header { binding_authority: authority, request_id: "synthetic-initial".into() };
        assert!(f.b.lww_finish_initial_publication(&header).unwrap());
        assert!(f.b.lww_read_outbox(authority, 100).unwrap().entries.iter().any(|entry| library_format::is_key(&entry.key)));
        f.receiver.publish(&mut f.b, authority, &[], &cancel).await.unwrap();
        assert!(f.b.lww_read_outbox(authority, 100).unwrap().entries.is_empty());
        let writer = f.b.lww_clock_state().unwrap().writer_id;
        let republished = segments(&f, &f.receiver).await.into_iter()
            .filter(|(object, _)| parse_segment_object_id(object).unwrap().0 == writer).collect::<Vec<_>>();
        assert_eq!(republished.len(), 1);
        assert_eq!(levels(&republished[0].1), [Some(LEVEL)]);
    })
}

#[test]
fn a_segment_that_declares_a_later_level_stops_receive_and_compaction_before_anything_is_used() {
    run(async {
        let mut f = CycleFixture::new();
        let cancel = Cancellation::default();
        small_asset(&mut f.a, "synthetic-asset", b"synthetic asset body");
        set(&mut f.a, &["root", "language"], json!("ja"));
        f.publish_a().await;
        let (object, mut payload) = segments(&f, &f.sender).await.remove(0);
        let (writer, seq, _) = parse_segment_object_id(&object).unwrap();
        for change in &mut payload.changes {
            if library_format::is_key(&change.key) {
                change.value = library_format::value(LEVEL + 1).unwrap();
            }
        }
        let (bytes, _) = lww_segment::seal(&payload, &f.sender.root_key).unwrap();
        f.provider.forget(&object);
        f.provider.seed(&segment_object_id(writer, seq, &lww_segment::digest(&bytes)).unwrap(), ObjectRole::Segment, bytes);
        let dependencies = [ObjectRole::Catalog, ObjectRole::Pack].into_iter()
            .flat_map(|role| f.provider.objects_with_role(role)).collect::<Vec<_>>();
        assert!(!dependencies.is_empty());
        let reads = |f: &CycleFixture| dependencies.iter().map(|id| f.provider.read_attempts(id)).collect::<Vec<_>>();
        let before = reads(&f);

        let progress = serde_json::to_value(f.b.lww_receive_progress(0.into()).unwrap()).unwrap();
        let received = f.receiver.receive_and_apply(&mut f.b, DecimalU64(0), &[], &cancel).await.unwrap_err();
        assert_eq!(received.kind, ErrorKind::AppUpdateRequired);
        assert_ne!(f.b.read_root(None).unwrap().value["language"], "ja");
        assert_eq!(serde_json::to_value(f.b.lww_receive_progress(0.into()).unwrap()).unwrap(), progress);
        assert_eq!(reads(&f), before, "a segment dependency was read");
        let recorded = |directory: &tempfile::TempDir, id: &str| {
            super::super::connection_store::ConnectionStore::open(directory.path()).unwrap().format_level(id).unwrap()
        };
        assert_eq!(recorded(&f.directory_b, "receiver"), LEVEL + 1);

        let job = tempfile::tempdir().unwrap();
        let writer = f.a.lww_clock_state().unwrap().writer_id;
        let compacted = f.sender.compact_published(&mut f.a, job.path(), "00000000-0000-4000-8000-0000000000f1",
            &writer, &f.sender.capabilities, &cancel, None).await.err().unwrap();
        assert_eq!(compacted.kind, ErrorKind::AppUpdateRequired);
        assert!(f.provider.objects_with_role(ObjectRole::Snapshot).is_empty());
        assert_eq!(recorded(&f.directory_a, "sender"), LEVEL + 1);
    })
}

/// Writes a structured plugin value whose binary contents are `parts`.
fn structured(store: &mut PersistentStore, key: &str, parts: &[Vec<u8>]) -> String {
    use crate::persistent_store::device_store::plugin_values::{structure, PluginDeviceMutation};
    let hashes = parts.iter()
        .map(|part| structure::put_part(store.device_store().unwrap().connection(), part).unwrap())
        .collect::<Vec<_>>();
    let length = parts.iter().map(Vec::len).sum::<usize>();
    let text = format!(
        "#risunest-structured:1
{{\"parts\":[{}],\"bytes\":{length},\"nodes\":[{{\"type\":\"ArrayBuffer\",\"offset\":0,\"byteLength\":{length}}}],\"root\":{{\"ref\":0}}}}",
        hashes.iter().map(|hash| format!("\"p:{hash}\"")).collect::<Vec<_>>().join(","),
    );
    store.device_store_mut().unwrap().write_plugin_device_values("synthetic-owner", &[PluginDeviceMutation::Set {
        space: "json".into(), key: key.into(), value: text.clone(),
    }]).unwrap();
    text
}
fn plugin_value(store: &PersistentStore, key: &str) -> Option<String> {
    store.device_store().unwrap().connection().query_row(
        "SELECT value FROM plugin_device_storage WHERE owner='synthetic-owner' AND space='json' AND key=?1 AND tombstone=0",
        [key], |row| row.get(0),
    ).ok()
}

#[test]
fn structured_plugin_value_parts_travel_raw_through_segments_and_checkpoints() {
    run(async {
        use crate::persistent_store::device_store::Section;
        let mut f = CycleFixture::new();
        let cancel = Cancellation::default();
        let (_third_directory, mut third, third_engine) = third_device(&f);
        for store in [&mut f.a, &mut f.b, &mut third] {
            store.device_store_mut().unwrap().set_section_participating(Section::LocalPlugins, true).unwrap();
        }
        let full = crate::persistent_store::device_store::plugin_values::structure::PART_BYTES as usize;
        // A small value, a value of one full part, and a value whose last
        // part is a remainder.
        let values = [
            ("small", vec![vec![0u8, 255, 128]]),
            ("full", vec![vec![1u8; full]]),
            ("remainder", vec![vec![2u8; full], vec![3u8; 5]]),
        ];
        let texts = values.iter().map(|(key, parts)| structured(&mut f.a, key, parts)).collect::<Vec<_>>();
        let parts = values.iter().flat_map(|(_, parts)| parts.iter().map(|part| risunest_sync_wire::hash(part))).collect::<Vec<_>>();
        f.publish_a().await;
        for (_, payload) in segments(&f, &f.sender).await {
            assert!(parts.iter().all(|part| !payload.message_pages.contains_key(part)), "a part was carried as Base64 text in the segment");
        }
        let check = |store: &PersistentStore| {
            for ((key, bytes), text) in values.iter().zip(&texts) {
                assert_eq!(plugin_value(store, key).as_ref(), Some(text), "{key}");
                for part in bytes {
                    assert_eq!(store.lww_object_body(&risunest_sync_wire::hash(part)).unwrap().as_ref(), Some(part));
                }
            }
        };
        f.receive_b().await;
        check(&f.b);

        // A device that starts from a checkpoint after its segments are gone
        // receives the parts with it.
        let job = tempfile::tempdir().unwrap();
        let writer = f.a.lww_clock_state().unwrap().writer_id;
        f.sender.compact_published(&mut f.a, job.path(), "00000000-0000-4000-8000-0000000000f2",
            &writer, &f.sender.capabilities, &cancel, None).await.unwrap();
        for object in f.sender.listing(&cancel).await.unwrap() {
            f.provider.delete_object(&f.sender.repository, &object.locator, &cancel).await.unwrap();
        }
        third_engine.receive_and_apply(&mut third, DecimalU64(0), &[], &cancel).await.unwrap();
        check(&third);
    })
}

#[test]
fn a_checkpoint_without_the_parts_of_a_value_leaves_out_only_that_value() {
    run(async {
        use crate::external_storage::lww_compaction::set_released_part_carriage_for_test;
        use crate::persistent_store::device_store::{plugin_values::PluginDeviceMutation, Section};
        let mut f = CycleFixture::new();
        let cancel = Cancellation::default();
        let (_third_directory, mut third, third_engine) = third_device(&f);
        for store in [&mut f.a, &mut third] {
            store.device_store_mut().unwrap().set_section_participating(Section::LocalPlugins, true).unwrap();
        }
        structured(&mut f.a, "structured", &[vec![0u8, 255, 128]]);
        f.a.device_store_mut().unwrap().write_plugin_device_values("synthetic-owner", &[PluginDeviceMutation::Set {
            space: "json".into(), key: "plain".into(), value: "1".into(),
        }]).unwrap();
        f.publish_a().await;
        // A released compactor writes its checkpoint without the parts.
        let job = tempfile::tempdir().unwrap();
        let writer = f.a.lww_clock_state().unwrap().writer_id;
        set_released_part_carriage_for_test(true);
        let compacted = f.sender.compact_published(&mut f.a, job.path(), "00000000-0000-4000-8000-0000000000f3",
            &writer, &f.sender.capabilities, &cancel, None).await;
        set_released_part_carriage_for_test(false);
        compacted.unwrap();
        for object in f.sender.listing(&cancel).await.unwrap() {
            f.provider.delete_object(&f.sender.repository, &object.locator, &cancel).await.unwrap();
        }
        third_engine.receive_and_apply(&mut third, DecimalU64(0), &[], &cancel).await.unwrap();
        assert_eq!(plugin_value(&third, "structured"), None);
        assert_eq!(plugin_value(&third, "plain").as_deref(), Some("1"));

        // A later compaction goes on past the missing part.
        set(&mut f.a, &["root", "language"], json!("ja"));
        f.publish_a().await;
        let job = tempfile::tempdir().unwrap();
        f.sender.compact_published(&mut f.a, job.path(), "00000000-0000-4000-8000-0000000000f4",
            &writer, &f.sender.capabilities, &cancel, None).await.unwrap();
    })
}
