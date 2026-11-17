use super::{AuthOutcome, Capabilities, CoverPolicy, LifecycleSnapshot};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, WebviewWindow};
use tauri_plugin_ios_native::IosNativeExt;

pub(super) fn setup(_app: &AppHandle) -> Result<(), String> {
    // The Swift plugin installs its synchronous cover while loading the WebView.
    Ok(())
}

pub(super) async fn check(app: &AppHandle) -> Result<Capabilities, String> {
    app.ios_native().app_lock_request("appLockCheck", ()).await
}

pub(super) async fn authenticate(window: &WebviewWindow, reason: String) -> Result<AuthOutcome, String> {
    #[derive(Serialize)]
    struct Request { reason: String }
    #[derive(Deserialize)]
    struct Response { status: AuthOutcome }
    let response: Response = window.ios_native().app_lock_request("appLockAuthenticate", Request { reason }).await?;
    Ok(response.status)
}

pub(super) async fn set_policy(app: &AppHandle, policy: &CoverPolicy) -> Result<LifecycleSnapshot, String> {
    #[derive(Serialize)]
    struct Request<'a> { policy: &'a CoverPolicy }
    app.ios_native().app_lock_request("appLockSetPolicy", Request { policy }).await
}

pub(super) async fn ready(app: &AppHandle, epoch: u64, locked: bool) -> Result<bool, String> {
    #[derive(Serialize)]
    struct Request { epoch: u64, locked: bool }
    #[derive(Deserialize)]
    struct Response { accepted: bool }
    let response: Response = app.ios_native().app_lock_request("appLockReady", Request { epoch, locked }).await?;
    Ok(response.accepted)
}
