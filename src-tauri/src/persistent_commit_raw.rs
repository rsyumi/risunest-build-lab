//! Raw commit payload shared by Windows and Linux. Store/revision semantics stay in PDS.
use crate::persistent_store::{
    commands::with_store_mut, AssetAlias, RevisionResult, StoreError, StoreResult, WorkingSetCommit,
};
use serde::Deserialize;
use crate::native_log::logged;
use tauri::{
    ipc::{InvokeBody, Request},
    AppHandle, Manager, WebviewWindow,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Envelope {
    pub(crate) commit: WorkingSetCommit,
    pub(crate) asset_aliases: Vec<AssetAlias>,
}

pub(crate) fn decode_envelope(bytes: &[u8]) -> StoreResult<Envelope> {
    serde_json::from_slice(bytes).map_err(|error| StoreError::CommitDecode {
        message: format!("commit envelope {}", crate::native_log::json_failure(&error)),
    })
}

pub(crate) fn commit_bytes(app: &AppHandle, bytes: &[u8]) -> StoreResult<RevisionResult> {
    let envelope = decode_envelope(bytes)?;
    with_store_mut(app.state(), |store| {
        store.commit_with_asset_aliases(&envelope.commit, &envelope.asset_aliases)
    })
}

#[tauri::command(async)]
pub(crate) fn pds_commit_raw(
    app: AppHandle,
    window: WebviewWindow,
    request: Request<'_>,
) -> StoreResult<RevisionResult> {
    if window.label() != "main" {
        return logged("pds_commit_raw", Err(StoreError::Validation {
            message: "commit transport requires the main webview".to_owned(),
        }));
    }
    logged("pds_commit_raw", match request.body() {
        InvokeBody::Raw(bytes) => commit_bytes(&app, bytes),
        _ => Err(StoreError::RawBodyUnavailable),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decoder_errors_are_classified_without_echoing_payload_strings() {
        for (bytes, category) in [
            (b"{".as_slice(), "incomplete"),
            (br#"{"commit":"private-value","assetAliases":[]}"#.as_slice(), "shape"),
            (b"{#".as_slice(), "syntax"),
        ] {
            let error = decode_envelope(bytes).err().expect("invalid envelope");
            assert!(matches!(error, StoreError::CommitDecode { .. }));
            let text = error.to_string();
            assert!(text.contains(category));
            assert!(text.contains("line"));
            assert!(!text.contains("private-value"));
        }
    }
}
