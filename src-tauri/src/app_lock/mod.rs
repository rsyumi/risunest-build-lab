use serde::{Deserialize, Serialize};
use tauri::{AppHandle, WebviewWindow};

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;
#[cfg(desktop)]
mod desktop;

#[cfg(target_os = "android")]
use android as platform;
#[cfg(target_os = "ios")]
use ios as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(windows)]
use windows as platform;

pub(crate) const EVENT_NAME: &str = "risunest-app-lock";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum AuthStatus { Unsupported, NotEnrolled, Available }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum AuthMethod { WindowsHello, FaceId, TouchId, Device }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuthAvailability {
    pub status: AuthStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<AuthMethod>,
}

impl AuthAvailability {
    pub fn unsupported() -> Self { Self { status: AuthStatus::Unsupported, method: None } }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Capabilities {
    pub auth: AuthAvailability,
    pub preview_supported: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum AuthOutcome { Success, Cancelled, Unavailable, Failed }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum LockTiming {
    #[default]
    #[serde(rename = "launch")]
    Launch,
    #[serde(rename = "immediate")]
    Immediate,
    #[serde(rename = "after-1m")]
    After1m,
    #[serde(rename = "after-5m")]
    After5m,
    #[serde(rename = "after-15m")]
    After15m,
    #[serde(rename = "after-30m")]
    After30m,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CoverPolicy {
    pub enabled: bool,
    pub timing: LockTiming,
    pub gate_required: bool,
    pub hide_recents_preview: bool,
    pub background_color: String,
}

impl CoverPolicy {
    pub fn installs_return_cover(&self) -> bool {
        self.enabled && self.timing != LockTiming::Launch
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LifecycleSnapshot {
    pub active: bool,
    pub epoch: u64,
    pub at_ms: u64,
    pub departed_at_ms: Option<u64>,
}

pub(crate) fn setup(app: &AppHandle) -> Result<(), String> { platform::setup(app) }

#[tauri::command]
pub(crate) async fn app_lock_check(app: AppHandle) -> Result<Capabilities, String> {
    platform::check(&app).await
}

#[tauri::command]
pub(crate) async fn app_lock_authenticate(window: WebviewWindow, reason: String) -> Result<AuthOutcome, String> {
    platform::authenticate(&window, reason).await
}

#[tauri::command]
pub(crate) async fn app_lock_set_policy(app: AppHandle, policy: CoverPolicy) -> Result<LifecycleSnapshot, String> {
    platform::set_policy(&app, &policy).await
}

#[tauri::command]
pub(crate) async fn app_lock_ready(app: AppHandle, epoch: u64, locked: bool) -> Result<bool, String> {
    platform::ready(&app, epoch, locked).await
}
