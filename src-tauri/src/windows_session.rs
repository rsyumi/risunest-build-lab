use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::Manager;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::Shutdown::{ShutdownBlockReasonCreate, ShutdownBlockReasonDestroy},
    UI::{Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass}, WindowsAndMessaging::*},
};

const SETTLE_LIMIT: Duration = Duration::from_secs(2);
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();

#[derive(Default)]
struct PendingFlush { token: Option<String>, deadline: Option<Instant> }

impl PendingFlush {
    fn request(&mut self, now: Instant) -> String {
        self.token.get_or_insert_with(|| {
            self.deadline = Some(now + SETTLE_LIMIT);
            uuid::Uuid::new_v4().to_string()
        }).clone()
    }
    fn acknowledge(&mut self, token: &str) {
        if self.token.as_deref() == Some(token) { self.clear(); }
    }
    fn waiting(&self, now: Instant) -> bool {
        self.token.is_some() && self.deadline.is_some_and(|deadline| now < deadline)
    }
    fn clear(&mut self) { self.token = None; self.deadline = None; }
}

#[derive(Default)]
pub(crate) struct SessionState(Mutex<PendingFlush>);

#[tauri::command]
pub(crate) fn desktop_flush_complete(window: tauri::WebviewWindow, token: String) -> Result<(), String> {
    if window.label() != "main" { return Err("Lifecycle belongs to the main window".into()); }
    window.state::<SessionState>().0.lock().map_err(|_| "Session state unavailable")?.acknowledge(&token);
    Ok(())
}

pub(crate) fn install(app: &tauri::AppHandle) -> Result<(), String> {
    let window = app.get_webview_window("main").ok_or("Main window unavailable")?;
    let hwnd = window.hwnd().map_err(|error| error.to_string())?;
    APP.set(app.clone()).map_err(|_| "Session handler already installed")?;
    if unsafe { SetWindowSubclass(hwnd.0, Some(session_proc), 1, 0) } == 0 {
        return Err("Unable to install Windows session handler".into());
    }
    Ok(())
}

unsafe extern "system" fn session_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, id: usize, _data: usize) -> LRESULT {
    let Some(app) = APP.get() else { return unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }; };
    let state = app.state::<SessionState>();
    match message {
        WM_QUERYENDSESSION => {
            let token = state.0.lock().unwrap_or_else(|error| error.into_inner()).request(Instant::now());
            let reason: Vec<u16> = "RisuNest".encode_utf16().chain(Some(0)).collect();
            unsafe { ShutdownBlockReasonCreate(hwnd, reason.as_ptr()); }
            if let Some(window) = app.get_webview_window("main") {
                let detail = serde_json::json!({ "reason": "stop", "ackToken": token });
                let _ = window.eval(format!("window.dispatchEvent(new CustomEvent('risu-native-lifecycle',{{detail:{detail}}}))"));
            }
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
    #[test]
    fn shutdown_wait_is_bounded_and_only_matching_acknowledgement_settles_it() {
        let now = Instant::now();
        let mut state = PendingFlush::default();
        let token = state.request(now);
        assert_eq!(state.request(now + Duration::from_secs(1)), token);
        state.acknowledge("stale");
        assert!(state.waiting(now + Duration::from_secs(1)));
        assert!(!state.waiting(now + SETTLE_LIMIT));
        state.acknowledge(&token);
        assert!(!state.waiting(now));
        assert_ne!(state.request(now), token);
        state.clear();
        assert!(!state.waiting(now));
    }
}
