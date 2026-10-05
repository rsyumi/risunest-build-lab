use super::*;
use crate::persistent_store::{message_pages, MessageObjectStore, MESSAGE_PAGE_SWEEP_LIMIT};
use risunest_external_storage_format::{
    logical_records::LOGICAL_MESSAGE_PAGE_SIZE,
    message_pages::{repage, MessageHash, MessageManifest, PageIndex, MAX_PAGE_BYTES},
};

fn stage(store: &mut PersistentStore, count: usize) -> String {
    let stage = stage_root(store, "Synthetic page staging");
    store.replace_put_presets(&stage, &[]).unwrap();
    store.replace_put_character_detail(
        &stage, &json!({"chaId":"paged","type":"character","name":"Paged"}), 1,
    ).unwrap();
    store.replace_put_conversation_row(
        &stage, "paged", 0, &json!({"id":"long","name":"Long"}), 0, count as i64,
    ).unwrap();
    stage
}

fn messages(count: usize) -> Vec<Value> {
    (0..count).map(|index| json!({
        "chatId": format!("synthetic-{index}"), "role":"user",
        "data": format!("{index}: {}", "text".repeat(index % 41)),
    })).collect()
}

#[test]
fn staged_batches_match_the_format_codec_and_bound_closing_reads() {
    for batch in [17, 257] {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let mut messages = messages(2061);
        messages[512]["data"] = json!("x".repeat(MAX_PAGE_BYTES + 1));
        let encoded = messages.iter().map(|message| risunest_sync_wire::payload_value::encode(message).unwrap()).collect::<Vec<_>>();
        let expected = repage::<crate::persistent_store::StoreError>(
            &PageIndex { messages: vec![], pages: vec![] },
            encoded.iter().map(|body| MessageHash::from_bytes(body)).collect(),
            0..0, encoded.len(), |index| Ok(encoded[index].clone()),
        ).unwrap();
        let stage = stage(&mut store, messages.len());
        for (index, chunk) in messages.chunks(batch).enumerate() {
            message_pages::reset_capture_work();
            store.replace_add_conversation_messages(&stage, "paged", "long", (index * batch) as i64, chunk).unwrap();
            let work = message_pages::take_capture_work().work;
            assert!(work.messages_read <= chunk.len() + LOGICAL_MESSAGE_PAGE_SIZE, "batch {batch}: {work:?}");
            assert!(work.peak_page_messages <= LOGICAL_MESSAGE_PAGE_SIZE, "{work:?}");
            assert!(work.peak_page_bytes <= encoded[512].len() + 128, "{work:?}");
            if (index + 1) * batch < messages.len() {
                let manifests: i64 = store.connection.query_row(
                    "SELECT count(*) FROM message_page_manifests WHERE generation=?1", [&stage], |row| row.get(0),
                ).unwrap();
                assert_eq!(manifests, 0, "an incomplete conversation published a manifest");
            }
        }
        assert_eq!(message_pages::load_pages(&store.connection, &stage, "paged", "long").unwrap(), expected.index.pages);
        let body: Vec<u8> = store.connection.query_row(
            "SELECT body FROM message_page_manifests WHERE generation=?1", [&stage], |row| row.get(0),
        ).unwrap();
        assert_eq!(MessageManifest::decode(&body).unwrap(), expected.index.manifest());
        let tx = store.connection.transaction().unwrap();
        let current = message_pages::current_manifest(&tx, &stage, "paged", "long").unwrap();
        assert_eq!(current, message_pages::capture_manifest(&tx, &stage, "paged", "long", None).unwrap());
        tx.rollback().unwrap();
        message_pages::reset_capture_work();
        store.replace_commit(&stage, Some(0)).unwrap();
        assert_eq!(message_pages::take_capture_work().capture_calls, 0);
        assert_eq!(store.materialize(None).unwrap()["characters"][0]["chats"][0]["message"], json!(messages));
    }
}

#[test]
fn cancellation_rolls_back_partial_page_construction_and_abort_releases_objects() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let stage = stage(&mut store, 513);
    store.replace_add_conversation_messages(&stage, "paged", "long", 0, &messages(257)).unwrap();
    let pages = message_pages::load_pages(&store.connection, &stage, "paged", "long").unwrap();
    let tx = store.connection.transaction().unwrap();
    message_pages::forget_pages(&tx, &stage, "paged", "long").unwrap();
    let checks = std::cell::Cell::new(0);
    let error = message_pages::stage_conversation_pages(&tx, &stage, "paged", "long", &|| {
        checks.set(checks.get() + 1);
        if checks.get() == 150 {
            Err(StoreError::Validation { message: "synthetic cancellation".into() })
        } else { Ok(()) }
    }).unwrap_err();
    assert!(matches!(error, StoreError::Validation { message } if message == "synthetic cancellation"));
    tx.rollback().unwrap();
    assert_eq!(message_pages::load_pages(&store.connection, &stage, "paged", "long").unwrap(), pages);
    assert_eq!(store.revision().unwrap(), 0);
    store.replace_abort(&stage).unwrap();
    for _ in 0..20 {
        if !store.purge_retired_batch(128).unwrap() { break; }
    }
    for step in 0..3 {
        store.sweep_message_page_objects(MessageObjectStore::Library,
            1_900_000_000_000 + step * ASSET_GC_PRODUCT_MINIMUM_GRACE_MS, MESSAGE_PAGE_SWEEP_LIMIT).unwrap();
    }
    for page in pages {
        assert!(message_pages::object_body(&store.connection, &page.page.hash).unwrap().is_none());
    }
}

#[test]
fn reopening_retires_partial_staging_pages_without_activating_them() {
    let (directory, mut store, original) = open_fixture();
    let revision = store.revision().unwrap();
    let stage = stage(&mut store, 513);
    store.replace_add_conversation_messages(&stage, "paged", "long", 0, &messages(257)).unwrap();
    let pages = message_pages::load_pages(&store.connection, &stage, "paged", "long").unwrap();
    assert!(!pages.is_empty());
    drop(store);
    let mut store = PersistentStore::open(directory.path()).unwrap();
    assert_eq!(store.revision().unwrap(), revision);
    assert_eq!(store.materialize(None).unwrap(), original);
    assert!(store.replace_commit(&stage, Some(revision)).is_err());
    let state: String = store.connection.query_row("SELECT state FROM generations WHERE id=?1", [&stage], |row| row.get(0)).unwrap();
    assert_eq!(state, "retired");
    store.sweep_message_page_objects(MessageObjectStore::Library, 1_900_000_000_000, MESSAGE_PAGE_SWEEP_LIMIT).unwrap();
    assert!(pages.iter().all(|page| message_pages::retained_object_present(&store.connection, &page.page.hash).unwrap()));
    for _ in 0..30 {
        if !store.purge_retired_batch(128).unwrap() { break; }
    }
    for step in 1..4 {
        store.sweep_message_page_objects(MessageObjectStore::Library,
            1_900_000_000_000 + step * ASSET_GC_PRODUCT_MINIMUM_GRACE_MS, MESSAGE_PAGE_SWEEP_LIMIT).unwrap();
    }
    assert!(message_pages::load_pages(&store.connection, &stage, "paged", "long").unwrap().is_empty());
    assert!(pages.iter().all(|page| message_pages::object_body(&store.connection, &page.page.hash).unwrap().is_none()));
    assert_eq!(store.materialize(None).unwrap(), original);
}
