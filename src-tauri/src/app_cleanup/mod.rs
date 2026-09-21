mod files;
mod paths;
pub(crate) mod mobile;

use files::Result;
use paths::Paths;
use serde::{Deserialize, Serialize};
use std::{fs, sync::atomic::{AtomicBool, Ordering}};
#[cfg(desktop)]
use std::time::{Duration, Instant};
#[cfg(test)]
use std::path::Path;
#[cfg(mobile)]
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Mode { Reset, PrepareRemoval }

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u32,
    token: String,
    mode: Mode,
    webview_cleared: bool,
    error: Option<String>,
    appimage: Option<std::path::PathBuf>,
}

fn read_request(paths: &Paths) -> Result<Option<Request>> {
    files::validate(&paths.request())?;
    let bytes = match fs::read(paths.request()) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("cleanup-journal-unavailable".into()),
    };
    let request: Request = serde_json::from_slice(&bytes).map_err(|_| "cleanup-journal-corrupt")?;
    if request.version != 1 || !uuid::Uuid::parse_str(&request.token)
        .is_ok_and(|value| value.get_version_num() == 4 && value.to_string() == request.token) {
        return Err("cleanup-journal-corrupt".into());
    }
    Ok(Some(request))
}

pub(crate) struct CleanupState {
    paths: Paths,
    pending: AtomicBool,
    running: AtomicBool,
    closing: AtomicBool,
    #[cfg(desktop)]
    _owner: fs::File,
    #[cfg(mobile)]
    maintenance: Mutex<Option<Maintenance>>,
    #[cfg(mobile)]
    ready: AtomicBool,
}

#[cfg(mobile)]
struct Maintenance {
    _native: crate::native_file_jobs::admission::Permit,
    _renderer: crate::persistent_store::commands::DeviceMaintenanceGuard,
}

impl CleanupState {
    pub(crate) fn initialize(app: &AppHandle) -> Result<Self> {
        let paths = Paths::from_app(app)?;
        #[cfg(desktop)]
        let owner = lock(&paths, Duration::ZERO)?;
        let pending = !matches!(read_request(&paths), Ok(None));
        Ok(Self { paths, pending: AtomicBool::new(pending), running: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            #[cfg(desktop)] _owner: owner,
            #[cfg(mobile)] maintenance: Mutex::new(None),
            #[cfg(mobile)] ready: AtomicBool::new(false),
        })
    }
}

pub(crate) fn pending(app: &AppHandle) -> bool {
    app.try_state::<CleanupState>().is_some_and(|state| state.pending.load(Ordering::Acquire))
}

pub(crate) fn closing(app: &AppHandle) -> bool {
    app.try_state::<CleanupState>().is_some_and(|state| state.closing.load(Ordering::Acquire))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Status { pending: bool, mode: Option<Mode>, error: Option<String> }

#[tauri::command]
pub(crate) fn app_cleanup_status(app: AppHandle) -> Result<Status> {
    let state = app.try_state::<CleanupState>().ok_or("cleanup-state-unavailable")?;
    #[cfg(mobile)]
    if state.ready.load(Ordering::Acquire) {
        // The fresh document hands storage back to normal bootstrap.
        mobile::finish(&app)?;
        fs::remove_file(state.paths.request()).map_err(|_| "cleanup-journal-unavailable")?;
        let _ = fs::remove_dir(&state.paths.control);
        state.pending.store(false, Ordering::Release);
        state.ready.store(false, Ordering::Release);
    }
    match read_request(&state.paths) {
        Ok(Some(request)) => Ok(Status { pending: true, mode: Some(request.mode), error: request.error }),
        Ok(None) => Ok(Status { pending: false, mode: None, error: None }),
        Err(error) => Ok(Status { pending: true, mode: None, error: Some(error) }),
    }
}

fn navigate(app: &AppHandle) -> Result<()> {
    let window = app.get_webview_window("main").ok_or("cleanup-window-unavailable")?;
    let mut url = window.url().map_err(|_| "cleanup-window-unavailable")?;
    url.set_query(None);
    url.set_fragment(None);
    window.navigate(url).map_err(|_| "cleanup-navigation-failed".into())
}

#[tauri::command]
pub(crate) async fn app_cleanup_request(app: AppHandle, mode: Mode) -> Result<()> {
    #[cfg(mobile)]
    if mode == Mode::PrepareRemoval { return Err("cleanup-mode-unavailable".into()); }
    let state = app.state::<CleanupState>();
    state.paths.preflight()?;
    crate::cleanup_webview::preflight(&app).await?;
    if state.pending.swap(true, Ordering::AcqRel) { return Err("cleanup-already-pending".into()); }
    let request = Request {
        version: 1, token: uuid::Uuid::new_v4().to_string(), mode,
        webview_cleared: false, error: None,
        #[cfg(target_os = "linux")]
        appimage: app.env().appimage.map(std::path::PathBuf::from),
        #[cfg(not(target_os = "linux"))]
        appimage: None,
    };
    if let Err(error) = files::write_json(&state.paths.request(), &request) {
        state.pending.store(false, Ordering::Release);
        return Err(error);
    }
    navigate(&app)
}

struct Running<'a>(&'a AtomicBool);
impl Drop for Running<'_> { fn drop(&mut self) { self.0.store(false, Ordering::Release); } }

#[tauri::command]
pub(crate) async fn app_cleanup_resume(app: AppHandle) -> Result<()> {
    let state = app.state::<CleanupState>();
    if state.running.swap(true, Ordering::AcqRel) { return Err("cleanup-already-running".into()); }
    let _running = Running(&state.running);
    let mut request = read_request(&state.paths)?.ok_or("cleanup-not-pending")?;
    let result = resume(&app, &mut request).await;
    if let Err(error) = &result {
        request.error = Some(error.clone());
        let _ = files::write_json(&state.paths.request(), &request);
    }
    result
}

async fn resume(app: &AppHandle, request: &mut Request) -> Result<()> {
    crate::cleanup_webview::preflight(app).await?;
    #[cfg(mobile)]
    mobile::prepare(app).await?;
    crate::cleanup_webview::clear(app).await?;
    let state = app.state::<CleanupState>();
    request.webview_cleared = true;
    request.error = None;
    files::write_json(&state.paths.request(), request)?;
    #[cfg(desktop)]
    {
        let executable = std::env::current_exe().map_err(|_| "cleanup-executable-unavailable")?;
        let mut command = std::process::Command::new(executable);
        command.args(["--cleanup-worker", &request.token]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()).spawn().map_err(|_| "cleanup-worker-start-failed")?;
        state.closing.store(true, Ordering::Release);
        app.exit(0);
        Ok(())
    }
    #[cfg(mobile)]
    {
        let paths = state.paths.clone();
        tauri::async_runtime::spawn_blocking(move || erase(&paths)).await
            .map_err(|_| "cleanup-worker-failed")??;
        mobile::rebuild(app)?;
        state.ready.store(true, Ordering::Release);
        if let Err(error) = navigate(app) {
            state.ready.store(false, Ordering::Release);
            return Err(error);
        }
        Ok(())
    }
}

fn erase(paths: &Paths) -> Result<()> {
    paths.preflight()?;
    crate::cleanup_secrets::remove_all(&paths.data)?;
    for root in &paths.roots { files::remove(root)?; }
    Ok(())
}

#[cfg(desktop)]
fn lock(paths: &Paths, timeout: Duration) -> Result<fs::File> {
    files::validate(&paths.control)?;
    fs::create_dir_all(&paths.control).map_err(|_| "cleanup-journal-unavailable")?;
    let file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .open(paths.control.join("owner.lock")).map_err(|_| "cleanup-lock-unavailable")?;
    let deadline = Instant::now() + timeout;
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return Err("cleanup-app-still-running".into()),
        }
    }
}

#[cfg(desktop)]
pub(crate) fn run_cli(manifest: &crate::app_paths::AppPaths) -> Option<i32> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if !args.first().is_some_and(|arg| matches!(arg.as_str(), "--remove-local-data" | "--cleanup-worker")) { return None; }
    let result = (|| {
        let paths = Paths::from_manifest(manifest);
        match args.as_slice() {
            [command, yes] if command == "--remove-local-data" && yes == "--yes" => {
                let owner = lock(&paths, Duration::from_secs(30))?;
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    match erase(&paths) {
                        Ok(()) => break,
                        Err(error) if error == "cleanup-files-busy-or-denied" && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
                        Err(error) => return Err(error),
                    }
                }
                drop(owner);
                files::remove(&paths.control)
            },
            [command, token] if command == "--cleanup-worker" => {
                let owner = lock(&paths, Duration::from_secs(60))?;
                let mut request = read_request(&paths)?.ok_or("cleanup-not-pending")?;
                if request.token != *token || !request.webview_cleared { return Err("cleanup-request-mismatch".into()); }
                let result = (|| {
                    if request.mode == Mode::PrepareRemoval { remove_integration(manifest, &request)?; }
                    let deadline = Instant::now() + Duration::from_secs(10);
                    loop {
                        match erase(&paths) {
                            Ok(()) => break,
                            Err(error) if error == "cleanup-files-busy-or-denied" && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
                            Err(error) => return Err(error),
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    request.error = Some(error.clone());
                    let _ = files::write_json(&paths.request(), &request);
                    drop(owner);
                    let _ = relaunch(&request);
                    return Err(error);
                }
                drop(owner);
                files::remove(&paths.control)?;
                if request.mode == Mode::Reset {
                    relaunch(&request)?;
                }
                Ok(())
            },
            _ => Err("cleanup-confirmation-required".into()),
        }
    })();
    match result {
        Ok(()) => Some(0),
        Err(error) => { eprintln!("{error}"); Some(1) },
    }
}

#[cfg(desktop)]
fn relaunch(request: &Request) -> Result<()> {
    let executable = match &request.appimage {
        Some(path) => path.clone(),
        None => std::env::current_exe().map_err(|_| "cleanup-executable-unavailable")?,
    };
    std::process::Command::new(executable).spawn().map_err(|_| "cleanup-relaunch-failed")?;
    Ok(())
}

#[cfg(desktop)]
fn remove_integration(manifest: &crate::app_paths::AppPaths, request: &Request) -> Result<()> {
    #[cfg(target_os = "linux")]
    if let (Some(image), Some(integration)) = (&request.appimage, manifest.integration.as_ref()) {
        return crate::appimage_integration::remove_owned(image, integration);
    }
    let _ = (manifest, request);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn paths(root: &Path) -> Paths {
        Paths { data: root.join("data"), roots: vec![root.join("data"), root.join("webview")], control: root.join("control"), install_conflict: None }
    }
    #[test]
    fn cold_cleanup_removes_all_owned_roots_but_preserves_exports_and_other_product() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        for path in [&paths.data, &paths.roots[1], &root.path().join("sync")] {
            fs::create_dir_all(path).unwrap();
            fs::write(path.join("synthetic"), b"fixture").unwrap();
        }
        fs::write(root.path().join("backup.risunest"), b"fixture").unwrap();
        erase(&paths).unwrap();
        erase(&paths).unwrap();
        assert!(!paths.data.exists());
        assert!(!paths.roots[1].exists());
        assert!(root.path().join("backup.risunest").exists());
        assert!(root.path().join("sync/synthetic").exists());
    }
    #[test]
    fn a_root_overlapping_the_installation_directory_refuses_before_any_deletion() {
        let root = tempfile::tempdir().unwrap();
        let mut paths = paths(root.path());
        paths.install_conflict = Some(paths.data.clone());
        for path in [&paths.data, &paths.roots[1]] {
            fs::create_dir_all(path).unwrap();
            fs::write(path.join("synthetic"), b"fixture").unwrap();
        }
        // The refusal happens in the first statement of erase, so neither the
        // OS secret entries nor the files are reached.
        assert_eq!(erase(&paths), Err("cleanup-root-overlaps-install".to_owned()));
        assert!(paths.data.join("synthetic").exists());
        assert!(paths.roots[1].join("synthetic").exists());
    }

    #[test]
    fn corrupt_request_never_becomes_a_normal_start() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        fs::create_dir_all(&paths.control).unwrap();
        fs::write(paths.request(), b"{}").unwrap();
        assert!(read_request(&paths).is_err());
    }
    #[cfg(desktop)]
    #[test]
    fn cleanup_excludes_a_live_owner_before_deleting_any_data() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        let owner = lock(&paths, Duration::ZERO).unwrap();
        assert!(lock(&paths, Duration::ZERO).is_err());
        drop(owner);
        assert!(lock(&paths, Duration::ZERO).is_ok());
    }
}
