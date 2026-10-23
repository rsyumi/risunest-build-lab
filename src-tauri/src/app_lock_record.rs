use std::path::Path;
use serde_json::Value;
use tauri::{AppHandle, State};
use crate::persistent_store::{device_store::{DeviceStore, DEVICE_DATABASE_FILE}, PersistentStoreState, StoreError};

const KEY: &str = "risuNestAppLock";

fn read_record(root: &Path) -> Result<Option<String>, StoreError> {
    let directory = root.join("persistent");
    if !directory.join(DEVICE_DATABASE_FILE).try_exists()? { return Ok(None); }
    let store = DeviceStore::open(&directory)?;
    Ok(store.read_setting(KEY)?.map(|value| match value {
        Value::String(record) => record,
        other => other.to_string(),
    }))
}

fn write_record(root: &Path, record: &str) -> Result<(), StoreError> {
    let directory = root.join("persistent");
    std::fs::create_dir_all(&directory)?;
    DeviceStore::open(&directory)?.write_setting(KEY, &Value::String(record.to_owned()))
}

#[tauri::command]
pub(crate) fn app_lock_read_record(app: AppHandle, state: State<'_, PersistentStoreState>) -> Result<Option<String>, String> {
    let _guard = state.admit_renderer_operation().map_err(|error| error.to_string())?;
    read_record(&crate::app_paths::data_root(&app)?).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn app_lock_write_record(app: AppHandle, state: State<'_, PersistentStoreState>, record: String) -> Result<(), String> {
    let _guard = state.admit_renderer_operation().map_err(|error| error.to_string())?;
    write_record(&crate::app_paths::data_root(&app)?, &record).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_record_survives_reopen_without_opening_a_library() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(read_record(root.path()).unwrap(), None);
        assert!(!root.path().join("persistent").exists());
        let record = r#"{"schema":"risunest.app-lock/v1","enabled":false}"#;
        write_record(root.path(), record).unwrap();
        assert_eq!(read_record(root.path()).unwrap().as_deref(), Some(record));
        assert!(!root.path().join("persistent").join(crate::persistent_store::DATABASE_FILE).exists());
    }

    #[test]
    fn a_broken_device_store_is_an_error_and_is_not_replaced() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("persistent");
        std::fs::create_dir(&directory).unwrap();
        let file = directory.join(DEVICE_DATABASE_FILE);
        std::fs::write(&file, b"synthetic damaged database").unwrap();
        assert!(read_record(root.path()).is_err());
        assert!(write_record(root.path(), "{}").is_err());
        assert_eq!(std::fs::read(file).unwrap(), b"synthetic damaged database");
    }
}
