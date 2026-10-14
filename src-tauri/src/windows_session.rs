use std::sync::OnceLock;
use std::time::Instant;
use tauri::Manager;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::{Shutdown::{ShutdownBlockReasonCreate, ShutdownBlockReasonDestroy}, Threading::CreateMutexW},
    UI::{Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass}, WindowsAndMessaging::*},
};
use crate::desktop_session::SessionState;

static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static EXIT_REQUEST: OnceLock<u32> = OnceLock::new();
/// Registered message and kernel object names longer than this are refused.
const MAX_NAME_LENGTH: usize = 255;

/// The names the installer of `executable` uses to ask its running instances to
/// close, as null-terminated UTF-16. Window message names compare without case;
/// the mutex, which the installer polls until every instance has exited, is
/// lowercased the same way on both sides and carries no backslash. Agent and
/// development builds run from other paths, so an installer never reaches them.
fn exit_request_names(identifier: &str, executable: &str) -> Option<(Vec<u16>, Vec<u16>)> {
    let message: Vec<u16> = format!("{identifier}.exit-request|{executable}").encode_utf16().collect();
    let mut lowercase: Vec<u16> = executable.encode_utf16().collect();
    unsafe { CharLowerBuffW(lowercase.as_mut_ptr(), u32::try_from(lowercase.len()).ok()?); }
    let lowercase = String::from_utf16(&lowercase).ok()?.replace('\\', "/");
    let mutex: Vec<u16> = format!("Local\\{identifier}.exit-request|{lowercase}").encode_utf16().collect();
    (message.len() <= MAX_NAME_LENGTH && mutex.len() <= MAX_NAME_LENGTH)
        .then(|| (message.into_iter().chain(Some(0)).collect(), mutex.into_iter().chain(Some(0)).collect()))
}

/// Accepts the installer's exit request for the life of the process. The mutex
/// handle stays open until the process exits, which is how the installer sees
/// every instance gone.
fn register_exit_request(app: &tauri::AppHandle) {
    let Some((message, mutex)) = std::env::current_exe().ok()
        .and_then(|executable| exit_request_names(&app.config().identifier, executable.to_str()?)) else { return };
    let message = unsafe { RegisterWindowMessageW(message.as_ptr()) };
    if message == 0 || unsafe { CreateMutexW(std::ptr::null(), 0, mutex.as_ptr()) }.is_null() { return; }
    let _ = EXIT_REQUEST.set(message);
}

/// Closes the main window the way its close button does, so the renderer saves
/// and can wait for sync in front of the user.
fn request_exit(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if crate::app_cleanup::closing(&app) { return; }
        let Some(window) = app.get_webview_window("main") else { return };
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        let _ = window.close();
    });
}

pub(crate) fn install(app: &tauri::AppHandle) -> Result<(), String> {
    let window = app.get_webview_window("main").ok_or("Main window unavailable")?;
    let hwnd = window.hwnd().map_err(|error| error.to_string())?;
    APP.set(app.clone()).map_err(|_| "Session handler already installed")?;
    register_exit_request(app);
    if unsafe { SetWindowSubclass(hwnd.0, Some(session_proc), 1, 0) } == 0 {
        return Err("Unable to install Windows session handler".into());
    }
    Ok(())
}

unsafe extern "system" fn session_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, id: usize, _data: usize) -> LRESULT {
    let Some(app) = APP.get() else { return unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }; };
    if EXIT_REQUEST.get() == Some(&message) {
        request_exit(app);
        return 0;
    }
    let state = app.state::<SessionState>();
    match message {
        WM_QUERYENDSESSION => {
            let reason: Vec<u16> = "RisuNest".encode_utf16().chain(Some(0)).collect();
            unsafe { ShutdownBlockReasonCreate(hwnd, reason.as_ptr()); }
            crate::desktop_session::request_flush(app, false);
            return 1;
        }
        WM_ENDSESSION => {
            if wparam != 0 {
                // WebView2 replies on this thread. Pump a bounded number per iteration
                // so message floods cannot extend the shutdown deadline.
                'settle: while state.0.lock().unwrap_or_else(|error| error.into_inner()).waiting(Instant::now()) {
                    unsafe { MsgWaitForMultipleObjects(0, std::ptr::null(), 0, 10, QS_ALLINPUT); }
                    for _ in 0..64 {
                        if !state.0.lock().unwrap_or_else(|error| error.into_inner()).waiting(Instant::now()) { break 'settle; }
                        let mut message = std::mem::zeroed();
                        if unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } == 0 { break; }
                        if message.message == WM_QUIT {
                            unsafe { PostQuitMessage(message.wParam as i32); }
                            break 'settle;
                        }
                        unsafe { TranslateMessage(&message); DispatchMessageW(&message); }
                    }
                }
            }
            state.0.lock().unwrap_or_else(|error| error.into_inner()).clear();
            unsafe { ShutdownBlockReasonDestroy(hwnd); }
        }
        WM_NCDESTROY => { unsafe { RemoveWindowSubclass(hwnd, Some(session_proc), id); } }
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(name: Vec<u16>) -> String {
        assert_eq!(name.last(), Some(&0));
        String::from_utf16(&name[..name.len() - 1]).unwrap()
    }

    #[test]
    fn exit_request_names_match_the_installer() {
        // windows.nsh builds the same names from $INSTDIR.
        let (message, mutex) = exit_request_names("synthetic.risunest", r"C:\Users\Test\AppData\Local\RisuNest\RisuNest.exe").unwrap();
        assert_eq!(text(message), r"synthetic.risunest.exit-request|C:\Users\Test\AppData\Local\RisuNest\RisuNest.exe");
        assert_eq!(text(mutex), r"Local\synthetic.risunest.exit-request|c:/users/test/appdata/local/risunest/risunest.exe");
        let (_, mutex) = exit_request_names("synthetic.risunest", r"D:\Ünïcode\RisuNest.exe").unwrap();
        assert_eq!(text(mutex), r"Local\synthetic.risunest.exit-request|d:/ünïcode/risunest.exe");
        assert!(exit_request_names("synthetic.risunest", &format!(r"C:\{}\RisuNest.exe", "a".repeat(240))).is_none());
    }
}
