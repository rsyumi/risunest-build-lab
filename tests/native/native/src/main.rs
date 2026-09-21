use std::{fs, path::PathBuf};
use tauri::Manager;

fn value(name: &str) -> String { std::env::var(name).expect(name) }

#[tauri::command]
fn boundary_phase(app: tauri::AppHandle) -> serde_json::Value {
    let root = app.path().app_data_dir().unwrap();
    assert_eq!(root, PathBuf::from(value("RISUNEST_BOUNDARY_ROOT")));
    assert_eq!(fs::read_to_string(root.join("boundary-owner")).unwrap(), value("RISUNEST_BOUNDARY_ID"));
    serde_json::json!({"runId": value("RISUNEST_BOUNDARY_ID"), "phase": value("RISUNEST_BOUNDARY_PHASE")})
}

#[tauri::command]
fn boundary_finish(app: tauri::AppHandle, report: serde_json::Value) -> Result<(), String> {
    if report["runId"] != value("RISUNEST_BOUNDARY_ID") || report["phase"] != value("RISUNEST_BOUNDARY_PHASE") {
        return Err("incorrect report identity".into());
    }
    let path = PathBuf::from(value("RISUNEST_BOUNDARY_REPORT"));
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| e.to_string())?;
    use std::io::Write;
    file.write_all(report.to_string().as_bytes()).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    app.exit(if report["success"] == true { 0 } else { 1 });
    Ok(())
}

fn boundary_handler() -> impl Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![boundary_phase, boundary_finish]
}

fn main() {
    let id = value("RISUNEST_BOUNDARY_ID");
    assert!(id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()));
    let identifier = format!("io.github.rsyumi.risunest.boundary.{id}");
    let root = PathBuf::from(value("RISUNEST_BOUNDARY_ROOT"));
    assert!(root.is_absolute());
    assert_eq!(root.file_name().unwrap(), identifier.as_str());
    assert_eq!(fs::read_to_string(root.join("boundary-owner")).unwrap(), id);
    let mut context = tauri::generate_context!();
    assert_eq!(context.config().identifier, "io.github.rsyumi.risunest.boundary");
    assert!(matches!(context.config().build.frontend_dist, Some(tauri::utils::config::FrontendDist::Files(_))));
    context.config_mut().identifier = identifier;
    context.config_mut().app.windows[0].data_directory = Some(root.join("webview"));
    #[cfg(target_os = "macos")]
    { context.config_mut().app.windows[0].incognito = true; }
    let paths = risunest_lib::AppPaths::isolated(root.clone()).expect("isolated boundary roots");
    assert_eq!(paths.data, root);
    let product = risunest_lib::invoke_handler();
    let boundary = boundary_handler();
    let bootstrap = format!(r#"
        window.addEventListener('error', event => {{
            window.__TAURI_INTERNALS__.invoke('boundary_finish', {{ report: {{runId: {}, phase: {}, success: false, cases: [], error: String(event.message)}} }});
        }});
        window.addEventListener('unhandledrejection', event => {{
            window.__TAURI_INTERNALS__.invoke('boundary_finish', {{ report: {{runId: {}, phase: {}, success: false, cases: [], error: String(event.reason)}} }});
        }});
    "#, serde_json::json!(id), serde_json::json!(value("RISUNEST_BOUNDARY_PHASE")), serde_json::json!(id), serde_json::json!(value("RISUNEST_BOUNDARY_PHASE")));
    let app = risunest_lib::builder().manage(paths).append_invoke_initialization_script(&bootstrap).invoke_handler(move |invoke| {
        if invoke.message.command().starts_with("boundary_") { boundary(invoke) } else { product(invoke) }
    }).build(context).expect("build isolated persistence boundary");
    assert_eq!(app.path().app_data_dir().unwrap(), root);
    app.run(risunest_lib::handle_run_event);
}
