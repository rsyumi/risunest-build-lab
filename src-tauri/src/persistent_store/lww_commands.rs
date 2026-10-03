use super::{
    commands::with_store_mut,
    lww::{self, Header},
    PersistentStoreState, StoreError,
};
use risunest_sync_wire::stamp::DecimalU64;
use serde::Deserialize;
use tauri::State;

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
    with_store_mut(state, |store| {
        store.lww_replace_target_as_new_device(
            &request.header,
            &request.staging_id,
            &request.authorization_id,
        )
    })
}

#[tauri::command]
pub(crate) fn pds_lww_read_outbox(
    state: State<'_, PersistentStoreState>,
    request: ReadOutbox,
) -> Result<lww::OutboxPage, StoreError> {
    with_store_mut(state, |store| {
        store.lww_read_outbox_generating(
            request.header.binding_authority,
            usize::try_from(request.limit.0).map_err(lww::error)?,
            &request.generating,
        )
    })
}
#[tauri::command]
pub(crate) fn pds_lww_stage_receive(
    state: State<'_, PersistentStoreState>,
    request: lww::StageReceive,
) -> Result<(), StoreError> {
    with_store_mut(state, |store| store.lww_stage_receive(&request))
}
#[tauri::command]
pub(crate) fn pds_lww_apply_receive(
    state: State<'_, PersistentStoreState>,
    request: lww::ApplyReceive,
) -> Result<lww::ApplyResult, StoreError> {
    with_store_mut(state, |store| store.lww_apply_receive(&request))
}
#[tauri::command]
pub(crate) fn pds_lww_finish_receive(
    state: State<'_, PersistentStoreState>,
    request: Header,
) -> Result<(), StoreError> {
    with_store_mut(state, |store| store.lww_finish_receive(&request))
}
#[tauri::command]
pub(crate) fn pds_lww_drain_deferred(
    state: State<'_, PersistentStoreState>,
    request: lww::ApplyReceive,
) -> Result<lww::ApplyResult, StoreError> {
    with_store_mut(state, |store| store.lww_drain_deferred(&request))
}
#[tauri::command]
pub(crate) fn pds_lww_commit_replacement(
    state: State<'_, PersistentStoreState>,
    request: Replacement,
) -> Result<super::RevisionResult, StoreError> {
    with_store_mut(state, |store| {
        store.lww_commit_replacement(&request.header, &request.staging_id)
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UnitStateRequest {
    #[serde(flatten)]
    header: Header,
    after_key: Option<risunest_sync_wire::unit::UnitKey>,
    limit: DecimalU64,
}
#[tauri::command]
pub(crate) fn pds_lww_queue_unit_state_page(
    state: State<'_, PersistentStoreState>,
    request: UnitStateRequest,
) -> Result<lww::UnitStatePage, StoreError> {
    with_store_mut(state, |store| {
        store.lww_queue_unit_state_page(
            &request.header,
            request.after_key.as_ref(),
            usize::try_from(request.limit.0).map_err(lww::error)?,
        )
    })
}
