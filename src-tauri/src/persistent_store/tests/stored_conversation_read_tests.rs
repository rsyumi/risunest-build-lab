use super::*;

fn conversation_json(store: &PersistentStore, character_id: &str, conversation_id: &str) -> (String, String) {
    let parsed = store
        .read_conversation(character_id, conversation_id, None)
        .expect("read parsed conversation");
    let stored = store
        .read_stored_conversation(character_id, conversation_id, None)
        .expect("read stored conversation");
    (
        serde_json::to_string(&parsed).expect("serialize parsed conversation"),
        serde_json::to_string(&stored).expect("serialize stored conversation"),
    )
}

fn window_json(store: &PersistentStore, query: &ConversationWindowQuery) -> (String, String) {
    let parsed = store
        .read_conversation_window(query, None)
        .expect("read parsed window");
    let stored = store
        .read_stored_conversation_window(query, None)
        .expect("read stored window");
    (
        serde_json::to_string(&parsed).expect("serialize parsed window"),
        serde_json::to_string(&stored).expect("serialize stored window"),
    )
}

fn window_query(conversation_id: &str) -> ConversationWindowQuery {
    ConversationWindowQuery {
        character_id: "char-a".to_owned(),
        conversation_id: conversation_id.to_owned(),
        start_index: None,
        limit: None,
        anchor_message_id: None,
        anchor_occurrence: None,
        before: None,
        after: None,
        skip_parser_work: None,
    }
}

#[test]
fn stored_conversation_reads_send_the_same_json_as_parsed_reads() {
    let (_directory, store, _) = open_fixture();
    // A stored message with text and numbers that a reformatting writer would change.
    let message = json!({"role": "char", "data": "é \u{1F600} \"quoted\"\n", "chatId": "msg-001", "time": 1.5e-7, "z": {"b": 1, "a": [0.1, -2]}});
    store
        .connection
        .execute(
            "UPDATE messages SET value = ?1 WHERE character_id = 'char-a' AND conversation_id = 'conv-long' AND message_index = 1",
            params![serde_json::to_string(&message).unwrap()],
        )
        .expect("store synthetic message");

    for (character_id, conversation_id) in [("char-a", "conv-long"), ("char-a", "conv-short"), ("char-b", "conv-beta"), ("char-a", "absent")] {
        let (parsed, stored) = conversation_json(&store, character_id, conversation_id);
        assert_eq!(stored, parsed, "{character_id}/{conversation_id}");
    }

    let mut queries = vec![window_query("conv-long"), window_query("conv-short"), window_query("absent")];
    let mut range = window_query("conv-long");
    range.start_index = Some(1);
    range.limit = Some(20);
    queries.push(range);
    let mut anchored = window_query("conv-long");
    anchored.anchor_message_id = Some("msg-050".to_owned());
    anchored.anchor_occurrence = Some(AnchorOccurrence::Last);
    anchored.before = Some(3);
    anchored.after = Some(4);
    queries.push(anchored);
    for query in &queries {
        let (parsed, stored) = window_json(&store, query);
        assert_eq!(stored, parsed, "{query:?}");
    }
}

#[test]
fn stored_conversation_reads_keep_the_detail_shape_without_messages() {
    let (_directory, store, _) = open_fixture();
    // A detail out of key order that still carries a `message` key from an older writer.
    store
        .connection
        .execute(
            "UPDATE conversations SET detail = ?1, message_count = 0 WHERE character_id = 'char-a' AND conversation_id = 'conv-short'",
            params![r#"{"zeta":1,"id":"conv-short","message":[{"stale":true}],"alpha":{"b":2,"a":1},"name":"Short"}"#],
        )
        .expect("store synthetic detail");
    store
        .connection
        .execute("DELETE FROM messages WHERE character_id = 'char-a' AND conversation_id = 'conv-short'", [])
        .expect("empty the conversation");

    let (parsed, stored) = conversation_json(&store, "char-a", "conv-short");
    assert_eq!(stored, parsed);
    assert!(stored.contains(r#"{"zeta":1,"id":"conv-short","message":[],"alpha":{"b":2,"a":1},"name":"Short"}"#), "{stored}");
    let (parsed, stored) = window_json(&store, &window_query("conv-short"));
    assert_eq!(stored, parsed);
    assert!(stored.contains(r#""messages":[]"#), "{stored}");

    // Without a `message` key the messages go last, as `read_conversation` inserts them.
    store
        .connection
        .execute(
            "UPDATE conversations SET detail = ?1 WHERE character_id = 'char-a' AND conversation_id = 'conv-short'",
            params![r#"{"zeta":1,"id":"conv-short"}"#],
        )
        .expect("store detail without messages");
    let (parsed, stored) = conversation_json(&store, "char-a", "conv-short");
    assert_eq!(stored, parsed);
    assert!(stored.contains(r#"{"zeta":1,"id":"conv-short","message":[]}"#), "{stored}");
}
