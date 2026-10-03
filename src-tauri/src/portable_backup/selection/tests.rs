use super::*;
use crate::data_health::Finding;
use crate::persistent_store::portable::create_raw_tables;

/// A raw archive projection with a group whose member is another character, two presets and one
/// plugin storage record.
fn fixture() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    create_raw_tables(&db).unwrap();
    db.execute("INSERT INTO root (value) VALUES ('{\"botPresetsId\":1}')", [])
        .unwrap();
    for (index, (id, detail)) in [
        (
            "char-a",
            "{\"chaId\":\"char-a\",\"name\":\"a\",\"type\":\"group\",\"characters\":[\"char-b\"],\"chatPage\":0}",
        ),
        (
            "char-b",
            "{\"chaId\":\"char-b\",\"name\":\"b\",\"type\":\"character\",\"chatPage\":0}",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        db.execute(
            "INSERT INTO characters (character_id,configured_index,recent_at,trashed,name,image,conversation_count,type,creator_notes,trash_time,detail)
             VALUES (?1,?2,0,0,?3,NULL,0,'character',NULL,NULL,?4)",
            rusqlite::params![id, index as i64, &id[5..], detail],
        )
        .unwrap();
    }
    for (index, name) in ["default", "studio"].into_iter().enumerate() {
        db.execute(
            "INSERT INTO bot_presets (preset_id,configured_index,name,image,value) VALUES (?1,?2,?3,NULL,?4)",
            rusqlite::params![
                index.to_string(),
                index as i64,
                name,
                format!("{{\"name\":\"{name}\"}}")
            ],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO plugin_storage (owner,storage_key,byte_size,ordinal,value) VALUES ('synthetic-plugin','p-1',2,0,'{}')",
        [],
    )
    .unwrap();
    db
}

#[test]
fn the_inventory_lists_what_an_archive_holds_and_marks_what_is_damaged() {
    let db = fixture();
    let findings = vec![
        Finding::new("record-invalid", "character", "char-a", "record JSON is invalid"),
        Finding::new(
            "reference-missing",
            "message",
            "char-a/chat-1/0",
            "reference has no target in this library",
        ),
        Finding::new("record-invalid", "preset", "1", "portable preset identity mismatch"),
    ];
    let inventory = inventory(&db, &findings).unwrap();
    assert_eq!(
        inventory
            .characters
            .iter()
            .map(|entry| (entry.id.as_str(), entry.damaged))
            .collect::<Vec<_>>(),
        [("char-a", 2), ("char-b", 0)],
        "a conversation's damage is counted against the character the reader chooses"
    );
    assert_eq!(inventory.presets.len(), 2);
    assert_eq!(inventory.presets[1].damaged, 1);
    assert_eq!(inventory.plugins[0].id, PluginKey { owner: "synthetic-plugin".into(), key: "p-1".into() }.identity());
    assert_eq!(inventory.characters[0].name, "a");
}
