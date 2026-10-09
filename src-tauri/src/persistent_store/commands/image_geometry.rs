use tauri::State;
use crate::native_log::logged;
use crate::persistent_store::{image_geometry::{self, ImageGeometry}, StoreError};
use super::{PersistentStoreState, with_store_mutex_admitted};

async fn run<R: Send + 'static>(
    state: &PersistentStoreState,
    operation: impl FnOnce(&std::path::Path) -> Result<R, StoreError> + Send + 'static,
) -> Result<R, StoreError> {
    let permit = state.admit_renderer_operation()?;
    let root = with_store_mutex_admitted(state, &permit, |store| Ok(store.repository_root().to_path_buf()))?;
    tauri::async_runtime::spawn_blocking(move || {
        let _permit = permit;
        operation(&root)
    }).await.map_err(|_| StoreError::Store { message: "image geometry worker failed".into() })?
}

#[tauri::command]
pub(crate) async fn pds_read_image_geometry(state: State<'_, PersistentStoreState>, hashes: Vec<String>) -> Result<Vec<ImageGeometry>, StoreError> {
    logged("pds_read_image_geometry", run(&state, move |root| image_geometry::read(root, &hashes)).await)
}

#[tauri::command]
pub(crate) async fn pds_write_image_geometry(state: State<'_, PersistentStoreState>, values: Vec<ImageGeometry>) -> Result<(), StoreError> {
    logged("pds_write_image_geometry", run(&state, move |root| image_geometry::write(root, &values)).await)
}

#[tauri::command]
pub(crate) async fn pds_compute_image_geometry(state: State<'_, PersistentStoreState>, content_hash: String) -> Result<Option<ImageGeometry>, StoreError> {
    logged("pds_compute_image_geometry", run(&state, move |root| image_geometry::compute(root, &content_hash)).await)
}
