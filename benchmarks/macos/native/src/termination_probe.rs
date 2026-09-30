use std::sync::{Mutex, OnceLock};
use tauri::Manager;

static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static STATE: Mutex<Probe> = Mutex::new(Probe { attempt: 0, pending: false, user_event_modal: false });
struct Probe { attempt: u32, pending: bool, user_event_modal: bool }

unsafe extern "C" {
    fn risunest_probe_install(callback: extern "C" fn(), diagnostic: extern "C" fn(i32)) -> i32;
    fn risunest_probe_begin();
    fn risunest_probe_modal_mode() -> i32;
    fn risunest_probe_reply_count() -> u32;
    fn risunest_probe_begin_depth() -> u32;
    fn risunest_probe_delegate_depth() -> u32;
    fn risunest_probe_reply(approve: i32) -> i32;
}

fn failed(app: &tauri::AppHandle, reason: &str) {
    let _ = super::macos_bench_report("failure".into(), serde_json::json!({ "probe": "termination", "reason": reason }));
    app.exit(1);
}

fn diagnostic(stage: &str, detail: serde_json::Value) {
    let state = STATE.lock().unwrap();
    let result = serde_json::json!({
        "diagnostic": true, "attempt": state.attempt, "pending": state.pending,
        "userEventModal": state.user_event_modal,
        "beginDepth": unsafe { risunest_probe_begin_depth() },
        "delegateDepth": unsafe { risunest_probe_delegate_depth() },
        "replyCount": unsafe { risunest_probe_reply_count() }, "detail": detail,
    });
    drop(state);
    super::macos_bench_report(stage.into(), result).unwrap();
}

extern "C" fn native_diagnostic(event: i32) {
    let stage = match event {
        1 => "termination-probe-native-terminate-enter",
        2 => "termination-probe-native-terminate-return",
        3 => "termination-probe-native-delegate-enter",
        4 => "termination-probe-native-delegate-later",
        _ => return,
    };
    diagnostic(stage, serde_json::json!({}));
}

pub(crate) fn record_run_event(event: &tauri::RunEvent) {
    if std::env::var("RISUNEST_MACOS_PHASE").ok().as_deref() != Some("termination-probe") {
        return;
    }
    match event {
        tauri::RunEvent::ExitRequested { code, .. } => diagnostic(
            "termination-probe-exit-requested", serde_json::json!({ "code": code }),
        ),
        tauri::RunEvent::Exit => {
            let frames: Vec<String> = std::backtrace::Backtrace::force_capture().to_string().lines()
                .filter_map(|line| {
                    let (index, function) = line.trim().split_once(':')?;
                    index.parse::<usize>().ok()?;
                    Some(format!("{index}: {}", function.split(" at ").next()?.trim()))
                }).collect();
            diagnostic("termination-probe-exit", serde_json::json!({ "backtraceFrames": frames }));
        }
        _ => {}
    }
}

extern "C" fn requested() {
    let app = APP.get().unwrap().clone();
    let attempt = {
        let mut state = STATE.lock().unwrap();
        state.attempt += 1;
        state.pending = true;
        state.user_event_modal = false;
        state.attempt
    };
    let watchdog = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(15));
        let state = STATE.lock().unwrap();
        if state.pending && state.attempt == attempt {
            drop(state);
            failed(&watchdog, "NSTerminateLater did not service the complete renderer round trip within 15 seconds");
            std::process::exit(1);
        }
    });
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let handle = app.clone();
        app.run_on_main_thread(move || {
            STATE.lock().unwrap().user_event_modal = unsafe { risunest_probe_modal_mode() == 1 };
            diagnostic("termination-probe-native-dispatch-enter", serde_json::json!({ "dispatchAttempt": attempt }));
            let script = format!("window.dispatchEvent(new CustomEvent('termination-probe', {{detail:{attempt}}}))");
            if handle.get_webview_window("main").unwrap().eval(script).is_err() {
                failed(&handle, "WKWebView probe dispatch failed");
            }
        }).unwrap();
    });
}

#[tauri::command]
pub(crate) fn macos_bench_modal_begin(app: tauri::AppHandle) -> Result<(), String> {
    let handle = app.clone();
    app.run_on_main_thread(move || {
        if APP.get().is_none() {
            APP.set(handle.clone()).unwrap();
            if unsafe { risunest_probe_install(requested, native_diagnostic) } != 1 {
                failed(&handle, "Unable to install isolated termination probe");
                return;
            }
        }
        unsafe { risunest_probe_begin(); }
    }).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn macos_bench_modal_status() -> serde_json::Value {
    let state = STATE.lock().unwrap();
    serde_json::json!({ "attempt": state.attempt, "pending": state.pending })
}

#[tauri::command]
pub(crate) fn macos_bench_modal_ack(app: tauri::AppHandle, attempt: u32, approve: bool) -> Result<(), String> {
    let ipc_modal = unsafe { risunest_probe_modal_mode() == 1 };
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let mut state = STATE.lock().unwrap();
        if !state.pending || state.attempt != attempt || approve != (attempt == 3)
            || !state.user_event_modal || !ipc_modal {
            drop(state);
            failed(&handle, "Tao/WKWebView callbacks must run in NSModalPanelRunLoopMode for the matching attempt");
            return;
        }
        let replies = unsafe { risunest_probe_reply_count() };
        if replies != attempt - 1 {
            drop(state);
            failed(&handle, "Native termination reply count mismatch");
            return;
        }
        let stage = match attempt { 1 => "termination-probe-cancel", 2 => "termination-probe-reload", _ => "termination-probe-approved" };
        super::macos_bench_report(stage.into(), serde_json::json!({
            "passed": true, "attempt": attempt, "userEventModal": state.user_event_modal,
            "ipcModal": ipc_modal, "previousReplies": replies,
        })).unwrap();
        state.pending = false;
        drop(state);
        diagnostic("termination-probe-native-reply-enter", serde_json::json!({ "approve": approve }));
        let replied = unsafe { risunest_probe_reply(i32::from(approve)) };
        diagnostic("termination-probe-native-reply-return", serde_json::json!({ "approve": approve, "result": replied }));
        if replied != 1 {
            failed(&handle, "Native termination reply was rejected");
        }
    }).map_err(|error| error.to_string())
}
