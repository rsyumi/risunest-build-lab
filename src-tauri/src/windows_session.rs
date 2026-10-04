use std::sync::OnceLock;
use std::time::Instant;
use tauri::Manager;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::Shutdown::{ShutdownBlockReasonCreate, ShutdownBlockReasonDestroy},
    UI::{Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass}, WindowsAndMessaging::*},
};
use crate::desktop_session::SessionState;

static APP: OnceLock<tauri::AppHandle> = OnceLock::new();

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
