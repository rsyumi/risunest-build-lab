#[cfg(mobile)]
use super::{CleanupState, Maintenance};
#[cfg(mobile)]
use std::time::{Duration, Instant};
#[cfg(mobile)]
use tauri::{AppHandle, Manager};

#[cfg(mobile)]
pub(super) async fn prepare(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<CleanupState>();
    if app
        .try_state::<crate::native_file_jobs::NativeFileJobState>()
        .is_none()
    {
        app.manage(crate::native_file_jobs::NativeFileJobState::initialize(
            state.paths.data.join("native-file-jobs"),
        ));
    }
    if app
        .try_state::<crate::device_backup::DeviceBackupState>()
        .is_none()
    {
        app.manage(crate::device_backup::DeviceBackupState::initialize(
            state.paths.data.join("device-backup"),
        ));
    }
    if app
        .try_state::<crate::native_file_jobs::screenshot_output::ScreenshotOutputState>()
        .is_none()
    {
        app.manage(
            crate::native_file_jobs::screenshot_output::ScreenshotOutputState::initialize(
                state.paths.data.join("native-file-jobs/screenshot-output"),
            ),
        );
    }
    if let Some(media) = app.try_state::<crate::native_media::streaming::MediaServerState>() {
        media.begin_cleanup()?;
    }
    crate::server_sync::events::server_sync_events_stop(app.clone());
    let connections =
        app.state::<crate::external_storage::connection_commands::ConnectionCommandState>();
    let jobs = app.state::<crate::external_storage::job_store::JobCommandState>();
    let server = app.state::<crate::server_sync::commands::ServerSyncCommandState>();
    let native = app.state::<crate::native_file_jobs::NativeFileJobState>();
    connections
        .begin_cleanup()
        .map_err(|_| "cleanup-connections-busy")?;
    jobs.begin_cleanup()
        .map_err(|_| "cleanup-background-work-busy")?;
    server
        .begin_cleanup()
        .map_err(|_| "cleanup-server-sync-busy")?;
    native.begin_cleanup()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let connections_done = connections
            .cleanup_drained()
            .map_err(|_| "cleanup-connections-busy")?;
        let jobs_done = jobs
            .cleanup_drained()
            .map_err(|_| "cleanup-background-work-busy")?;
        let server_done = server
            .cleanup_drained()
            .map_err(|_| "cleanup-server-sync-busy")?;
        let media_done = app
            .try_state::<crate::native_media::streaming::MediaServerState>()
            .map(|media| media.cleanup_drained())
            .transpose()?
            .unwrap_or(true);
        if connections_done && jobs_done && server_done && media_done && native.cleanup_drained() {
            break;
        }
        if Instant::now() >= deadline {
            return Err("cleanup-background-work-busy".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if state
        .maintenance
        .lock()
        .map_err(|_| "cleanup-state-unavailable")?
        .is_none()
    {
        app.state::<crate::NativeStartupState>()
            .release_cleanup_gates()?;
        app.state::<crate::device_backup::DeviceBackupState>()
            .release_cleanup_gates()
            .map_err(|_| "cleanup-device-backup-busy")?;
        let native_permit = native.admission.file(true).map_err(str::to_owned)?;
        let handle = app.clone();
        let renderer = tauri::async_runtime::spawn_blocking(move || {
            handle
                .state::<crate::persistent_store::PersistentStoreState>()
                .acquire_cleanup_maintenance(Duration::from_secs(30))
                .map_err(|_| "cleanup-storage-busy".to_owned())
        })
        .await
        .map_err(|_| "cleanup-worker-failed")??;
        *state
            .maintenance
            .lock()
            .map_err(|_| "cleanup-state-unavailable")? = Some(Maintenance {
            _native: native_permit,
            _renderer: renderer,
        });
    }
    // Keep both maintenance permits across failures until the clean document starts.
    app.state::<crate::device_backup::DeviceBackupState>()
        .close_for_cleanup()
        .map_err(|_| "cleanup-device-backup-busy")?;
    app.state::<crate::asset_repository::commands::DurableCasJobState>()
        .close_for_cleanup()?;
    native.close_for_cleanup()?;
    app.state::<crate::native_file_jobs::screenshot_output::ScreenshotOutputState>()
        .close_for_cleanup()?;
    app.state::<crate::native_media::ipc::NativeMediaIpcState>()
        .reset_renderer_session()?;
    #[cfg(target_os = "android")]
    app.state::<crate::android_commit_transport::AndroidCommitState>()
        .reset();
    crate::native_log::global_state()
        .set_file_enabled(false)
        .map_err(|_| "cleanup-log-busy")?;
    Ok(())
}

#[cfg(mobile)]
pub(super) fn rebuild(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<CleanupState>();
    app.state::<crate::device_backup::DeviceBackupState>()
        .reopen_after_cleanup()
        .map_err(|_| "cleanup-device-backup-unavailable")?;
    app.state::<crate::native_file_jobs::NativeFileJobState>()
        .reopen_after_cleanup()?;
    app.state::<crate::native_file_jobs::screenshot_output::ScreenshotOutputState>()
        .reopen_after_cleanup()?;
    app.state::<crate::native_media::ipc::NativeMediaIpcState>()
        .configure(state.paths.data.join("native-media-ipc"))?;
    if let Some(media) = app.try_state::<crate::native_media::streaming::MediaServerState>() {
        media.reopen_after_cleanup(state.paths.data.clone())?;
    } else {
        app.manage(
            crate::native_media::streaming::MediaServerState::initialize_after_cleanup(state.paths.data.clone())?,
        );
    }
    let jobs = app.state::<crate::external_storage::job_store::JobCommandState>();
    if jobs.root.get().is_none() {
        jobs.root
            .set(state.paths.data.clone())
            .map_err(|_| "cleanup-state-unavailable")?;
    }
    if app
        .try_state::<crate::regex_shadow::RegexCancellationRegistry>()
        .is_none()
    {
        app.manage(crate::regex_shadow::RegexCancellationRegistry::default());
    }
    app.state::<crate::NativeStartupState>().finish_cleanup()?;
    crate::native_log::global_state().configure_file_path(&state.paths.data);
    Ok(())
}

#[cfg(mobile)]
pub(super) fn finish(app: &AppHandle) -> Result<(), String> {
    app.state::<crate::native_file_jobs::NativeFileJobState>().finish_cleanup()?;
    app.state::<crate::external_storage::connection_commands::ConnectionCommandState>()
        .finish_cleanup()
        .map_err(|_| "cleanup-connections-busy")?;
    app.state::<crate::external_storage::job_store::JobCommandState>()
        .finish_cleanup()
        .map_err(|_| "cleanup-background-work-busy")?;
    app.state::<crate::server_sync::commands::ServerSyncCommandState>()
        .finish_cleanup()
        .map_err(|_| "cleanup-server-sync-busy")?;
    app.state::<CleanupState>()
        .maintenance
        .lock()
        .map_err(|_| "cleanup-state-unavailable")?
        .take();
    Ok(())
}
