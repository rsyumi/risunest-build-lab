use crate::logical_records::LogicalRecordEnvelope;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Snapshot payloads preserve exact conversation values independently of message IDs.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ServerPayload {
    pub record: LogicalRecordEnvelope,
    pub messages: Option<Vec<Value>>,
    #[serde(skip)]
    pub derived_objects: std::collections::BTreeMap<String, Vec<u8>>,
}


pub(crate) fn preserve_local_view(
    incoming: &mut ServerPayload,
    raw_character: Option<&Value>,
    raw_root: Option<&Value>,
) {
    match &mut incoming.record {
        LogicalRecordEnvelope::Root { value, .. } => {
            if let Some(messages) = raw_root
                .and_then(|v| v.get("statics"))
                .and_then(|v| v.get("messages"))
            {
                if let Some(root) = value.as_object_mut() {
                    let statics = root
                        .entry("statics")
                        .or_insert_with(|| serde_json::json!({}));
                    if let Some(statics) = statics.as_object_mut() {
                        statics.insert("messages".into(), messages.clone());
                    }
                }
            }
        }
        LogicalRecordEnvelope::Character { detail, .. } => {
            if let Some(detail) = detail.as_object_mut() {
                for key in ["chatPage", "lastInteraction"] {
                    if let Some(value) = raw_character.and_then(|v| v.get(key)) {
                        detail.insert(key.into(), value.clone());
                    }
                }
            }
        }
        LogicalRecordEnvelope::ArchivedCharacter { .. } => (),
        _ => (),
    }
}
