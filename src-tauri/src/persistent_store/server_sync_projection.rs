use super::{StoreError, StoreResult};
use crate::{
    asset_repository::owner_manifest_codec::decode_owner_manifest,
    logical_records::LogicalRecordEnvelope,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// Snapshot payloads preserve exact conversation values independently of message IDs.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ServerPayload {
    pub record: LogicalRecordEnvelope,
    pub messages: Option<Vec<Value>>,
    #[serde(skip)]
    pub derived_objects: std::collections::BTreeMap<String, Vec<u8>>,
}


pub(crate) fn dependencies_with(
    payload: &ServerPayload,
    mut read: impl FnMut(&str) -> StoreResult<Vec<u8>>,
) -> StoreResult<Vec<String>> {
    let mut hashes = BTreeSet::new();
    match &payload.record {
        LogicalRecordEnvelope::Root { owner_heads, .. }
        | LogicalRecordEnvelope::Character { owner_heads, .. } => {
            for head in owner_heads {
                if let Some(hash) = &head.manifest_hash {
                    hashes.insert(hash.clone());
                    let bytes = if let Some(bytes) = payload.derived_objects.get(hash) {
                        bytes.clone()
                    } else {
                        read(hash)?
                    };
                    for entry in
                        decode_owner_manifest(&bytes).map_err(|_| StoreError::Validation {
                            message: "Server source owner object is invalid".into(),
                        })?
                    {
                        if let Some(hash) = entry.payload_hash {
                            hashes.insert(hex::encode(hash));
                        }
                    }
                }
            }
        }
        LogicalRecordEnvelope::ArchivedCharacter {
            archive_object_hash,
            shared_archive_object_hash,
            asset_hashes,
            shared_asset_hashes,
            owner_heads,
            ..
        } => {
            hashes.insert(archive_object_hash.clone());
            hashes.insert(shared_archive_object_hash.clone());
            hashes.extend(asset_hashes.iter().cloned());
            hashes.extend(shared_asset_hashes.iter().cloned());
            for head in owner_heads {
                if let Some(hash) = &head.manifest_hash {
                    hashes.insert(hash.clone());
                    let bytes = read(hash)?;
                    for entry in decode_owner_manifest(&bytes).map_err(|_| {
                        StoreError::Validation {
                            message: "Server source owner object is invalid".into(),
                        }
                    })? {
                        if let Some(hash) = entry.payload_hash {
                            hashes.insert(hex::encode(hash));
                        }
                    }
                }
            }
        }
        LogicalRecordEnvelope::Asset { object_hash, .. }
        | LogicalRecordEnvelope::Inlay { object_hash, .. } => {
            if let Some(hash) = object_hash {
                hashes.insert(hash.clone());
            }
        }
        _ => (),
    }
    Ok(hashes.into_iter().collect())
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
