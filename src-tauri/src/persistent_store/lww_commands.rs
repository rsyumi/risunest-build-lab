use super::{
    commands::with_store_mut,
    lww::{self, Header},
    PersistentStoreState, StoreError,
};
use risunest_sync_wire::stamp::DecimalU64;
use serde::Deserialize;
use tauri::State;
use crate::native_log::logged;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReadOutbox {
    #[serde(flatten)]
    header: Header,
    limit: DecimalU64,
    #[serde(default)]
    generating: Vec<lww::MessageLocator>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Replacement {
    #[serde(flatten)]
    header: Header,
    staging_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NewDeviceReplacement {
    #[serde(flatten)]
    header: Header,
    staging_id: String,
    authorization_id: String,
}

#[tauri::command]
pub(crate) fn pds_lww_replace_target_as_new_device(
    state: State<'_, PersistentStoreState>,
    request: NewDeviceReplacement,
) -> Result<lww::NewDeviceResult, StoreError> {
    logged("pds_lww_replace_target_as_new_device", with_store_mut(state, |store| {
        store.lww_replace_target_as_new_device(
            &request.header,
            &request.staging_id,
            &request.authorization_id,
        )
    }))
}

#[tauri::command]
pub(crate) fn pds_lww_read_outbox(
    state: State<'_, PersistentStoreState>,
    request: ReadOutbox,
) -> Result<lww::OutboxPage, StoreError> {
    logged("pds_lww_read_outbox", with_store_mut(state, |store| {
        store.lww_read_outbox_generating(
            request.header.binding_authority,
            usize::try_from(request.limit.0).map_err(lww::error)?,
            &request.generating,
        )
    }))
}
#[tauri::command]
pub(crate) fn pds_lww_stage_receive(
    state: State<'_, PersistentStoreState>,
    request: lww::StageReceive,
) -> Result<(), StoreError> {
    logged("pds_lww_stage_receive", with_store_mut(state, |store| store.lww_stage_receive(&request)))
}
#[tauri::command]
pub(crate) fn pds_lww_apply_receive(
    state: State<'_, PersistentStoreState>,
    request: lww::ApplyReceive,
) -> Result<lww::ApplyResult, StoreError> {
    logged("pds_lww_apply_receive", with_store_mut(state, |store| store.lww_apply_receive(&request)))
}
#[tauri::command]
pub(crate) fn pds_lww_finish_receive(
    state: State<'_, PersistentStoreState>,
    request: Header,
) -> Result<(), StoreError> {
    logged("pds_lww_finish_receive", with_store_mut(state, |store| store.lww_finish_receive(&request)))
}
#[tauri::command]
pub(crate) fn pds_lww_drain_deferred(
    state: State<'_, PersistentStoreState>,
    request: lww::ApplyReceive,
) -> Result<lww::ApplyResult, StoreError> {
    logged("pds_lww_drain_deferred", with_store_mut(state, |store| store.lww_drain_deferred(&request)))
}
#[tauri::command]
pub(crate) fn pds_lww_commit_replacement(
    state: State<'_, PersistentStoreState>,
    request: Replacement,
) -> Result<super::RevisionResult, StoreError> {
    logged("pds_lww_commit_replacement", with_store_mut(state, |store| {
        store.lww_commit_replacement(&request.header, &request.staging_id)
    }))
}

#[tauri::command]
pub(crate) fn pds_lww_finish_initial_publication(
    state: State<'_, PersistentStoreState>,
    request: Header,
) -> Result<bool, StoreError> {
    logged("pds_lww_finish_initial_publication", with_store_mut(state, |store| {
        store.lww_finish_initial_publication(&request)
    }))
}
