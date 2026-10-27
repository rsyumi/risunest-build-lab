use serde_json::Value;
use std::collections::HashMap;

/// Length and FNV-1a over UTF-16 code units, equal to `authorNoteFingerprint` in TypeScript.
pub(crate) fn fingerprint(text: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    let mut length = 0usize;
    for unit in text.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(0x0100_0193);
        length += 1;
    }
    format!("{length}:{hash:08x}")
}

/// The preset a conversation reads its author's note from, while its note still matches the link.
pub(crate) fn matching_preset_id(conversation: &Value) -> Option<&str> {
    let link = conversation.get("authorNotePreset")?;
    let id = link.get("id")?.as_str()?;
    let expected = link.get("fingerprint")?.as_str()?;
    let note = conversation.get("note").and_then(Value::as_str).unwrap_or("");
    (fingerprint(note) == expected).then_some(id)
}

/// Preset texts by id, skipping entries the app does not read.
pub(crate) fn preset_texts(root: &Value) -> HashMap<String, String> {
    root.get("authorNotePresets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|preset| preset.get("name").is_some_and(Value::is_string))
        .filter_map(|preset| {
            Some((
                preset.get("id")?.as_str()?.to_owned(),
                preset.get("text")?.as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// Stores the author's note a linked conversation reads, empty when its preset was deleted.
pub(crate) fn materialize(conversation: &mut Value, presets: &HashMap<String, String>) {
    let Some(id) = matching_preset_id(conversation).map(str::to_owned) else {
        return;
    };
    let text = presets.get(&id).cloned().unwrap_or_default();
    if let Some(object) = conversation.as_object_mut() {
        object.insert("note".into(), Value::String(text));
        object.remove("authorNotePreset");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fingerprint_matches_the_typescript_vectors() {
        for (text, expected) in [
            ("", "0:811c9dc5"),
            ("a", "1:e40c292c"),
            ("Hello, 세계", "9:97908cb3"),
            ("\u{1F600}", "2:cb31c4b8"),
            ("a\r\nb", "4:c40663db"),
        ] {
            assert_eq!(fingerprint(text), expected, "{text:?}");
        }
    }

    #[test]
    fn materialize_reads_the_preset_until_the_note_is_edited_elsewhere() {
        let presets = preset_texts(&json!({"authorNotePresets": [
            {"id": "p", "name": "Preset", "text": "current"},
            {"id": "broken", "text": "no name"},
        ]}));
        let link = |note: &str| json!({"id": "p", "fingerprint": fingerprint(note)});

        let mut linked = json!({"note": "old copy", "authorNotePreset": link("old copy")});
        materialize(&mut linked, &presets);
        assert_eq!(linked, json!({"note": "current"}));

        let mut edited = json!({"note": "edited", "authorNotePreset": link("old copy")});
        materialize(&mut edited, &presets);
        assert_eq!(edited["note"], "edited");

        let mut deleted = json!({"note": "copy", "authorNotePreset": {"id": "broken", "fingerprint": fingerprint("copy")}});
        materialize(&mut deleted, &presets);
        assert_eq!(deleted, json!({"note": ""}));
    }
}
