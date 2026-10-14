use crate::persistent_store::{lww::{Change, Header}, sync_selection::SyncTarget, PersistentStore};
use risunest_sync_wire::{descriptor::RecordDescriptor, stamp::Stamp, unit::{UnitKey, UnitValue}};
use serde_json::{json, Value};
use std::time::Instant;

fn count(name: &str, default: usize) -> usize {
    std::env::var(name).ok().map(|value| value.parse().unwrap()).unwrap_or(default)
}

fn change(key: &[&str], value: Value) -> Change {
    Change {
        key: UnitKey::new(key).unwrap(),
        stamp: Stamp { physical_ms: 7.into(), logical: 0, writer_id: "00000000-0000-4000-8000-000000000001".into() },
        value: UnitValue::inline(&serde_json::to_vec(&value).unwrap()).unwrap(),
    }
}

fn messages(character: &str, conversation: &str, count: usize, bytes: usize, objects: &mut Vec<(String, Vec<u8>)>) -> UnitValue {
    use risunest_external_storage_format::message_pages::{repage, MessageHash};
    let bodies = (0..count).map(|index| {
        risunest_sync_wire::payload_value::encode(&json!({"role":"user","chatId":format!("{character}-{conversation}-{index}"),"data":"m".repeat(bytes)})).unwrap()
    }).collect::<Vec<_>>();
    let repaged = repage::<crate::persistent_store::StoreError>(&Default::default(), bodies.iter().map(|body| MessageHash::from_bytes(body)).collect(), 0..0, bodies.len(), |index| Ok(bodies[index].clone())).unwrap();
    objects.extend(repaged.objects.into_iter().map(|object| (object.hash, object.bytes)));
    let manifest = repaged.index.manifest().encode().unwrap();
    let mut descriptor = RecordDescriptor::content(manifest.hash.clone());
    objects.push((manifest.hash, manifest.bytes));
    let mut hashes = repaged.index.pages.iter().map(|page| page.page.hash.clone()).collect::<Vec<_>>();
    hashes.sort();
    hashes.dedup();
    if risunest_sync_wire::descriptor::inline_references(&hashes).unwrap() {
        descriptor.dependencies = hashes;
    } else {
        let (root, references) = risunest_sync_wire::descriptor::build_reference_tree(&hashes, false).unwrap();
        descriptor.dependency_root = root;
        objects.extend(references);
    }
    UnitValue::object(descriptor).unwrap()
}

#[test]
#[ignore = "synthetic staging measurement"]
fn measure_staging() {
    let assets = count("RISUNEST_STAGING_ASSETS", 1_000);
    let characters = count("RISUNEST_STAGING_CHARACTERS", 100);
    let conversations = count("RISUNEST_STAGING_CONVERSATIONS", 4);
    let description_bytes = count("RISUNEST_STAGING_RECORD_BYTES", 256);
    let text_fields = count("RISUNEST_STAGING_TEXT_FIELDS", 0);
    let fields = ["creatorNotes", "exampleMessage", "firstMessage", "personality", "postHistoryInstructions", "scenario", "systemPrompt", "translatorNote", "notes", "nickname", "backgroundCSS", "backgroundHTML"];
    assert!(text_fields <= fields.len());
    let repetitions = count("RISUNEST_STAGING_REPETITIONS", 3);
    let messages_per_chat = count("RISUNEST_STAGING_MESSAGES_PER_CHAT", 0);
    let message_bytes = count("RISUNEST_STAGING_MESSAGE_BYTES", 256);
    let mut incoming = Vec::new();
    let mut bodies = Vec::new();
    for index in 0..assets {
        let key = format!("synthetic-asset-{index:08}");
        let hash = risunest_sync_wire::hash(key.as_bytes());
        let alias = crate::persistent_store::AssetAlias { key: key.clone(), object_hash: Some(hash), kind: "asset".into(), size: key.len() as i64, mime: "application/octet-stream".into(), name: key.clone(), ext: String::new(), inlay_type: None, width: None, height: None, metadata: json!({}) };
        incoming.push(change(&["asset", &key], serde_json::to_value(alias).unwrap()));
    }
    for index in 0..characters {
        let character = format!("synthetic-character-{index:08}");
        incoming.push(change(&["exists", "character", &character], json!({"type":"character"})));
        incoming.push(change(&["character", &character, "name"], json!(character)));
        let description = json!(format!("{index}:{}", "x".repeat(description_bytes)));
        let encoded = serde_json::to_vec(&description).unwrap();
        if encoded.len() > risunest_sync_wire::unit::MAX_INLINE_UNIT_BYTES {
            let hash = risunest_sync_wire::hash(&encoded);
            let mut entry = change(&["character", &character, "desc"], json!(""));
            entry.value = UnitValue::object(RecordDescriptor::content(hash.clone())).unwrap();
            incoming.push(entry);
            bodies.push((hash, encoded));
        } else {
            incoming.push(change(&["character", &character, "desc"], description));
        }
        for field in fields.iter().take(text_fields) {
            incoming.push(change(&["character", &character, field], json!("synthetic field")));
        }
        for chat in 0..conversations {
            let conversation = format!("synthetic-conversation-{chat:08}");
            incoming.push(change(&["exists", "conversation", &character, &conversation], json!(true)));
            incoming.push(change(&["conversation", &character, &conversation, "name"], json!(conversation)));
            if messages_per_chat > 0 {
                let mut entry = change(&["messages", &character, &conversation], Value::Null);
                entry.value = messages(&character, &conversation, messages_per_chat, message_bytes, &mut bodies);
                incoming.push(entry);
            }
        }
        incoming.push(change(&["order", "conversations", &character], json!({"ids":(0..conversations).rev().map(|chat| format!("synthetic-conversation-{chat:08}")).collect::<Vec<_>>(),"folders":[]})));
    }
    incoming.push(change(&["order", "characters"], json!((0..characters).rev().map(|index| format!("synthetic-character-{index:08}")).collect::<Vec<_>>())));
    for run in 0..repetitions {
        let root = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(root.path()).unwrap();
        for (hash, body) in &bodies { store.lww_put_object(hash, body).unwrap(); }
        let header = Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: uuid::Uuid::new_v4().to_string() };
        let inspection = store.register_lww_binding_inspection(header.binding_authority, &SyncTarget::Server("synthetic".into()), "target", "library").unwrap();
        let started = Instant::now();
        let mut observations = Vec::new();
        let staged = store.lww_stage_binding_units_observed(&header, &inspection, &incoming, 7.into(), |progress| {
            observations.push(json!({"atMs":started.elapsed().as_secs_f64()*1000.0,"phase":progress.phase,"done":progress.done,"total":progress.total}));
            Ok(())
        }).unwrap();
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        let rows = |table: &str| store.connection.query_row(&format!("SELECT count(*) FROM {table} WHERE generation=?1"), [&staged.staging_id], |row| row.get::<_,i64>(0)).unwrap();
        assert_eq!(rows("asset_aliases"), assets as i64);
        assert_eq!(rows("characters"), characters as i64);
        assert_eq!(rows("conversations"), (characters * conversations) as i64);
        assert_eq!(rows("messages"), (characters * conversations * messages_per_chat) as i64);
        if characters > 0 {
            let detail: String = store.connection.query_row("SELECT detail FROM characters WHERE generation=?1 AND character_id='synthetic-character-00000000'", [&staged.staging_id], |row| row.get(0)).unwrap();
            assert_eq!(serde_json::from_str::<Value>(&detail).unwrap()["desc"], format!("0:{}", "x".repeat(description_bytes)));
        }
        assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_binding_source_units WHERE staging_id=?1", [&staged.staging_id], |row| row.get::<_,i64>(0)).unwrap(), incoming.len() as i64);
        println!("staging_measurement {}", json!({"run":run,"assets":assets,"characters":characters,"conversationsPerCharacter":conversations,"recordBytesPerCharacter":description_bytes,"textFieldsPerCharacter":text_fields,"messagesPerConversation":messages_per_chat,"messageBytes":message_bytes,"units":incoming.len(),"elapsedMs":elapsed,"observations":observations}));
        assert_eq!(store.revision().unwrap(), 0);
    }
}
