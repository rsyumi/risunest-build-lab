//! This executable alone owns the verification commands. Product builds never import it.
use std::{io::Write, sync::Mutex};
use tauri::Manager;
#[cfg(target_os = "macos")]
mod termination_probe;

#[derive(Default)]
struct Events(Mutex<Vec<&'static str>>);

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

#[tauri::command]
fn macos_bench_quit(app: tauri::AppHandle) {
    app.exit(0);
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn macos_bench_native_quit(app: tauri::AppHandle) -> Result<(), String> {
    if !matches!(macos_bench_phase().as_str(), "app" | "quit-escape") {
        return Err("Native product quit belongs to the app and quit-escape phases".into());
    }
    unsafe extern "C" {
        fn risunest_bench_queue_native_quit(observer: extern "C" fn(i32, i32, i32)) -> i32;
    }
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let _ = PRODUCT_APP.set(handle.clone());
        if unsafe { risunest_bench_queue_native_quit(observe_product_reply) } != 1 {
            let _ = macos_bench_report("failure".into(), serde_json::json!({
                "passed": false, "message": "Unable to queue product native termination",
            }));
            handle.exit(1);
        }
    }).map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn macos_bench_repeat_native_quit(app: tauri::AppHandle) -> Result<(), String> {
    if macos_bench_phase() != "quit-escape" {
        return Err("A repeated native quit belongs to the quit-escape phase".into());
    }
    unsafe extern "C" {
        fn risunest_bench_queue_repeated_quit() -> i32;
    }
    let handle = app.clone();
    app.run_on_main_thread(move || {
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
        macos_bench_events,
        macos_bench_quit,
        #[cfg(target_os = "macos")]
        macos_bench_native_quit,
        #[cfg(target_os = "macos")]
        macos_bench_repeat_native_quit,
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
        let escape_exit = matches!(&event, tauri::RunEvent::Exit) && macos_bench_phase() == "quit-escape";
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
        if escape_exit {
            let state = app.state::<Events>();
            let (replies, runtime_quits, exits) = {
                let events = state.0.lock().unwrap();
                (events.iter().filter(|event| event.starts_with("native-reply-")).count(),
                 events.iter().filter(|event| **event == "quit").count(),
                 events.iter().filter(|event| **event == "exit").count())
            };
            let detail = serde_json::json!({
                "passed": runtime_quits == 0 && exits == 1,
                "nativeReplies": replies, "runtimeQuitRequests": runtime_quits, "exitCount": exits,
            });
            let stage = if runtime_quits == 0 && exits == 1 { "quit-escape-exit" } else { "failure" };
            let _ = macos_bench_report(stage.into(), detail);
        }
    });
}
