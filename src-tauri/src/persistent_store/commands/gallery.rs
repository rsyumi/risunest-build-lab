use super::{archive_operation, with_store_mutex_mut, with_store_mutex_mut_admitted};
use crate::persistent_store::{gallery::*, RevisionResult, StoreError};
use tauri::AppHandle;

#[tauri::command(async)]
pub(crate) async fn pds_gallery_page(app: AppHandle, query: GalleryQuery) -> Result<GalleryPage, StoreError> {
    archive_operation(app, "pds_gallery_page", move |state| with_store_mutex_mut(state, |store| store.gallery_page(&query))).await
}
#[tauri::command(async)]
pub(crate) async fn pds_gallery_keys(app: AppHandle, query: GalleryQuery) -> Result<GalleryKeys, StoreError> {
    archive_operation(app, "pds_gallery_keys", move |state| with_store_mutex_mut(state, |store| store.gallery_keys(&query))).await
}
#[tauri::command(async)]
pub(crate) async fn pds_gallery_index(app: AppHandle, query: GalleryQuery, key: String) -> Result<Option<usize>, StoreError> {
    archive_operation(app, "pds_gallery_index", move |state| with_store_mutex_mut(state, |store| store.gallery_index(&query, &key))).await
}
#[tauri::command(async)]
pub(crate) async fn pds_gallery_missing_keys(app: AppHandle, keys: Vec<String>) -> Result<Vec<String>, StoreError> {
    archive_operation(app, "pds_gallery_missing_keys", move |state| with_store_mutex_mut(state, |store| store.gallery_missing_keys(&keys))).await
}
#[tauri::command(async)]
pub(crate) async fn pds_gallery_delete(app: AppHandle, keys: Vec<String>, expected_revision: i64, scan_id: Option<String>) -> Result<RevisionResult, StoreError> {
    archive_operation(app, "pds_gallery_delete", move |state| with_store_mutex_mut(state, |store| store.gallery_delete(&keys, expected_revision, scan_id.as_deref()))).await
}
#[tauri::command(async)]
pub(crate) async fn pds_gallery_scan_start(app: AppHandle, expected_revision: i64) -> Result<ScanStatus, StoreError> {
    archive_operation(app, "pds_gallery_scan_start", move |state| {
        let guard = state.admit_renderer_operation()?;
        let (status, scan) = with_store_mutex_mut_admitted(state, &guard, |store| store.prepare_gallery_scan(expected_revision))?;
        state.cancel_on_renderer_maintenance(&scan.cancellation())?;
        tauri::async_runtime::spawn_blocking(move || { let _guard = guard; scan.run(); });
        Ok(status)
    }).await
}
#[tauri::command(async)]
pub(crate) async fn pds_gallery_scan_status(app: AppHandle) -> Result<Option<ScanStatus>, StoreError> {
    archive_operation(app, "pds_gallery_scan_status", |state| with_store_mutex_mut(state, |store| store.gallery_scan_status())).await
}
#[tauri::command(async)]
pub(crate) async fn pds_gallery_scan_cancel(app: AppHandle) -> Result<bool, StoreError> {
    archive_operation(app, "pds_gallery_scan_cancel", |state| with_store_mutex_mut(state, |store| store.gallery_scan_cancel())).await
}
#[tauri::command(async)]
pub(crate) async fn pds_gallery_locations(app: AppHandle, key: String) -> Result<GalleryLocations, StoreError> {
    archive_operation(app, "pds_gallery_locations", move |state| with_store_mutex_mut(state, |store| store.gallery_locations(&key))).await
}
#[tauri::command(async)]
pub(crate) async fn pds_gallery_usage_count(app: AppHandle, keys: Vec<String>) -> Result<usize, StoreError> {
    archive_operation(app, "pds_gallery_usage_count", move |state| with_store_mutex_mut(state, |store| store.gallery_usage_count(&keys))).await
}
#[tauri::command(async)]
pub(crate) async fn pds_gallery_scan_result(app: AppHandle) -> Result<Option<ScanStatus>, StoreError> {
    archive_operation(app, "pds_gallery_scan_result", |state| with_store_mutex_mut(state, |store| store.gallery_scan_result())).await
}
