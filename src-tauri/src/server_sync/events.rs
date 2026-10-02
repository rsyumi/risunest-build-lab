use tauri::{AppHandle, Emitter, Runtime};
pub(crate) const DEVICE_CHANGED_EVENT: &str = "risu-server-sync-device-changed";
pub(crate) fn notify_device_changed<R: Runtime>(app: &AppHandle<R>) { let _ = app.emit(DEVICE_CHANGED_EVENT, ()); }
