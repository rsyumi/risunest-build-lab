//! This executable alone owns the verification commands. Product builds never import it.
use std::{io::Write, sync::Mutex};
use tauri::Manager;
#[cfg(target_os = "macos")]
mod termination_probe;

#[derive(Default)]
struct Events(Mutex<Vec<&'static str>>);

#[cfg(target_os = "macos")]
static SESSION_STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

#[cfg(target_os = "macos")]
static PRODUCT_APP: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

#[cfg(target_os = "macos")]
extern "C" fn observe_product_reply(approve: i32, main_thread: i32, modal: i32) {
    let result = std::panic::catch_unwind(|| {
        let app = PRODUCT_APP.get().ok_or("Product reply observer unavailable")?;
        let state = app.state::<Events>();
        let (reply_count, runtime_quits) = {
            let mut events = state.0.lock().map_err(|_| "Product events unavailable")?;
            events.push(if approve == 0 { "native-reply-no" } else { "native-reply-yes" });
            (events.iter().filter(|event| event.starts_with("native-reply-")).count(),
             events.iter().filter(|event| **event == "quit").count())
        };
        if matches!(macos_bench_phase().as_str(), "session-deadline" | "session-upgrade") {
            let elapsed = SESSION_STARTED.get().ok_or("Session timer not started")?.elapsed().as_secs_f64();
            let expected = if macos_bench_phase() == "session-upgrade" { 6.0 } else { 5.0 };
            let passed = approve == 1 && main_thread == 1 && modal == 1 && reply_count == 1
                && runtime_quits == 0 && (expected - 0.1..expected + 0.75).contains(&elapsed);
            macos_bench_report(if passed { "session-deadline-reply" } else { "failure" }.into(),
                serde_json::json!({ "passed": passed, "elapsedSeconds": elapsed,
                    "mainThread": main_thread == 1, "modal": modal == 1, "replyCount": reply_count }))?;
        }
        macos_bench_report("app-native-reply".into(), serde_json::json!({
            "approve": approve != 0, "mainThread": main_thread == 1, "modal": modal == 1,
            "replyCount": reply_count, "runtimeQuitRequests": runtime_quits,
        }))
    });
    if !matches!(result, Ok(Ok(()))) {
        let _ = macos_bench_report("failure".into(), serde_json::json!({
            "passed": false, "message": "Unable to record product native termination reply",
        }));
    }
}

#[cfg(target_os = "macos")]
extern "C" fn observe_product_decision(reply: i32) {
    // NSTerminateCancel, NSTerminateNow and NSTerminateLater.
    let event = match reply {
        0 => "product-decision-cancel",
        1 => "product-decision-now",
        2 => "product-decision-later",
        _ => "product-decision-other",
    };
    let _ = std::panic::catch_unwind(|| {
        if let Some(app) = PRODUCT_APP.get() {
            app.state::<Events>().0.lock().unwrap_or_else(|error| error.into_inner()).push(event);
        }
        if macos_bench_phase().starts_with("session-dispatch-") {
            let _ = macos_bench_report("session-dispatch-delegate".into(),
                serde_json::json!({ "reply": reply }));
        }
    });
}

#[tauri::command]
fn macos_bench_phase() -> String {
    std::env::var("RISUNEST_MACOS_PHASE").expect("controller phase")
}

#[tauri::command]
fn macos_bench_report(stage: String, result: serde_json::Value) -> Result<(), String> {
    let path = std::env::var("RISUNEST_MACOS_REPORT").map_err(|error| error.to_string())?;
    let record = serde_json::json!({ "stage": stage, "result": result });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(format!("{record}\n").as_bytes())
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

#[tauri::command]
fn macos_bench_expected() -> Option<String> {
    std::env::var("RISUNEST_MACOS_EXPECTED").ok()
}

#[tauri::command]
fn macos_bench_events(state: tauri::State<'_, Events>) -> Vec<&'static str> {
    state.0.lock().unwrap().clone()
}

/// Resolves once every closure queued for the main thread before it has run.
#[tauri::command]
async fn macos_bench_main_thread_settled(app: tauri::AppHandle) -> Result<(), String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let _ = sender.send(());
    })
    .map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || receiver.recv())
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn macos_bench_quit(app: tauri::AppHandle) {
    app.exit(0);
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn macos_bench_native_quit(app: tauri::AppHandle) -> Result<(), String> {
    if !matches!(macos_bench_phase().as_str(), "app" | "quit-escape")
        && !macos_bench_phase().starts_with("session-dispatch-") {
        return Err("Native product quit belongs to the app and quit-escape phases".into());
    }
    unsafe extern "C" {
        fn risunest_bench_queue_native_quit(
            observer: extern "C" fn(i32, i32, i32),
            decision: extern "C" fn(i32),
        ) -> i32;
    }
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let _ = PRODUCT_APP.set(handle.clone());
        if unsafe { risunest_bench_queue_native_quit(observe_product_reply, observe_product_decision) } != 1 {
            let _ = macos_bench_report("failure".into(), serde_json::json!({
                "passed": false, "message": "Unable to queue product native termination",
            }));
            handle.exit(1);
        }
    }).map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn macos_bench_dispatch_session_event(app: tauri::AppHandle) -> Result<(), String> {
    if !macos_bench_phase().starts_with("session-dispatch-") {
        return Err("Session dispatch phase required".into());
    }
    unsafe extern "C" {
        fn risunest_bench_dispatch_session_event(
            observer: extern "C" fn(i32, i32, i32), decision: extern "C" fn(i32),
        ) -> i32;
    }
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let _ = PRODUCT_APP.set(handle.clone());
        let result = unsafe { risunest_bench_dispatch_session_event(observe_product_reply, observe_product_decision) };
        let _ = macos_bench_report(if result == 1 { "session-dispatch-sent" } else { "failure" }.into(),
            serde_json::json!({ "passed": result == 1, "status": result, "route": "self-targeted-apple-event" }));
        if result != 1 { handle.exit(1); }
    }).map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn macos_bench_session_quit(app: tauri::AppHandle) -> Result<(), String> {
    if !matches!(macos_bench_phase().as_str(), "session-deadline" | "session-upgrade") { return Err("Session probe phase required".into()); }
    unsafe extern "C" {
        fn risunest_bench_session_quit(observer: extern "C" fn(i32, i32, i32), decision: extern "C" fn(i32), upgrade: i32) -> i32;
    }
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let _ = PRODUCT_APP.set(handle.clone());
        let _ = SESSION_STARTED.set(std::time::Instant::now());
        if unsafe { risunest_bench_session_quit(observe_product_reply, observe_product_decision, i32::from(macos_bench_phase() == "session-upgrade")) } != 1 {
            let _ = macos_bench_report("failure".into(), serde_json::json!({ "message": "Unable to start session-end probe" }));
            handle.exit(1);
        }
    }).map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn macos_bench_repeat_native_quit(app: tauri::AppHandle) -> Result<(), String> {
    let dispatch_cleanup = macos_bench_phase().starts_with("session-dispatch-");
    if macos_bench_phase() != "quit-escape" && !dispatch_cleanup {
        return Err("A repeated native quit belongs to the quit-escape or dispatch cleanup phase".into());
    }
    unsafe extern "C" {
        fn risunest_bench_queue_repeated_quit() -> i32;
    }
    let handle = app.clone();
    app.run_on_main_thread(move || {
        if dispatch_cleanup {
            let _ = macos_bench_report("session-dispatch-cleanup-repeated-terminate".into(),
                serde_json::json!({ "purpose": "cleanup-after-observation" }));
        }
        if unsafe { risunest_bench_queue_repeated_quit() } != 1 {
            let _ = macos_bench_report("failure".into(), serde_json::json!({
                "passed": false, "message": "Unable to queue a repeated native quit",
            }));
            handle.exit(1);
        }
    }).map_err(|error| error.to_string())
}

fn benchmark_handler() -> impl Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        macos_bench_phase,
        macos_bench_report,
        macos_bench_expected,
        macos_bench_main_thread_settled,
        macos_bench_events,
        macos_bench_quit,
        #[cfg(target_os = "macos")]
        macos_bench_native_quit,
        #[cfg(target_os = "macos")]
        macos_bench_repeat_native_quit,
        #[cfg(target_os = "macos")]
        macos_bench_session_quit,
        #[cfg(target_os = "macos")]
        macos_bench_dispatch_session_event,
        #[cfg(target_os = "macos")]
        termination_probe::macos_bench_modal_begin,
        #[cfg(target_os = "macos")]
        termination_probe::macos_bench_modal_ack,
        #[cfg(target_os = "macos")]
        termination_probe::macos_bench_modal_status,
    ]
}

fn main() {
    let product = risunest_lib::invoke_handler();
    let benchmark = benchmark_handler();
    let app = risunest_lib::builder()
        .manage(Events::default())
        .invoke_handler(move |invoke| {
            if invoke.message.command().starts_with("macos_bench_") {
                benchmark(invoke)
            } else {
                product(invoke)
            }
        })
        .build(tauri::generate_context!())
        .expect("build isolated Mac harness");
    assert_eq!(
        app.config().identifier,
        "io.github.rsyumi.risunest.macos.bench"
    );
    app.run(|app, event| {
        let product_exit = matches!(&event, tauri::RunEvent::Exit) && macos_bench_phase() == "app";
        let session_exit = matches!(&event, tauri::RunEvent::Exit) && matches!(macos_bench_phase().as_str(), "session-deadline" | "session-upgrade");
        let escape_exit = matches!(&event, tauri::RunEvent::Exit) && macos_bench_phase() == "quit-escape";
        let dispatch_exit = matches!(&event, tauri::RunEvent::Exit) && macos_bench_phase().starts_with("session-dispatch-");
        let name = match &event {
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Opened { .. } => Some("opened"),
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => Some("reopen"),
            tauri::RunEvent::ExitRequested { .. } => Some("quit"),
            tauri::RunEvent::Exit => Some("exit"),
            tauri::RunEvent::WindowEvent {
                event: tauri::WindowEvent::CloseRequested { .. },
                ..
            } => Some("close"),
            _ => None,
        };
        if let Some(name) = name {
            app.state::<Events>().0.lock().unwrap().push(name);
        }
        #[cfg(target_os = "macos")]
        termination_probe::record_run_event(&event);
        risunest_lib::handle_run_event(app, event);
        if dispatch_exit {
            let state = app.state::<Events>();
            let events = state.0.lock().unwrap();
            let replies = events.iter().filter(|event| event.starts_with("native-reply-")).count();
            let decisions = events.iter().filter(|event| event.starts_with("product-decision-")).copied().collect::<Vec<_>>();
            let _ = macos_bench_report("session-dispatch-exit".into(), serde_json::json!({
                "nativeReplies": replies, "productDecisions": decisions,
                "runtimeQuitRequests": events.iter().filter(|event| **event == "quit").count(),
                "exitCount": events.iter().filter(|event| **event == "exit").count(),
                "productHandlerReturned": true,
            }));
        }
        if product_exit {
            let state = app.state::<Events>();
            let (replies, runtime_quits, exits) = {
                let events = state.0.lock().unwrap();
                (events.iter().filter(|event| event.starts_with("native-reply-")).count(),
                 events.iter().filter(|event| **event == "quit").count(),
                 events.iter().filter(|event| **event == "exit").count())
            };
            let _ = macos_bench_report("app-native-exit".into(), serde_json::json!({
                "passed": replies == 2 && runtime_quits == 0 && exits == 1,
                "nativeReplies": replies, "runtimeQuitRequests": runtime_quits,
                "exitCount": exits, "productHandlerReturned": true,
            }));
        }
        if session_exit {
            let state = app.state::<Events>();
            let events = state.0.lock().unwrap();
            let decisions = if macos_bench_phase() == "session-upgrade" { 3 } else { 2 };
            let passed = events.iter().filter(|event| **event == "native-reply-yes").count() == 1
                && events.iter().filter(|event| **event == "product-decision-later").count() == decisions;
            let _ = macos_bench_report(if passed { "session-deadline-exit" } else { "failure" }.into(),
                serde_json::json!({ "passed": passed, "repeatedSessionDelivered": true }));
        }
        if escape_exit {
            let state = app.state::<Events>();
            let (replies, runtime_quits, exits, decisions) = {
                let events = state.0.lock().unwrap();
                (events.iter().filter(|event| event.starts_with("native-reply-")).count(),
                 events.iter().filter(|event| **event == "quit").count(),
                 events.iter().filter(|event| **event == "exit").count(),
                 events.iter().filter(|event| event.starts_with("product-decision-")).copied().collect::<Vec<_>>())
            };
            let passed = replies == 0 && runtime_quits == 0 && exits == 1
                && decisions == ["product-decision-later"];
            let detail = serde_json::json!({
                "passed": passed,
                "nativeReplies": replies, "runtimeQuitRequests": runtime_quits, "exitCount": exits,
                "productDecisions": decisions,
            });
            let stage = if passed { "quit-escape-exit" } else { "failure" };
            let _ = macos_bench_report(stage.into(), detail);
        }
    });
}
