use serde::Serialize;
use tauri::AppHandle;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IntegrationState {
    available: bool,
    registered: bool,
    replaces_existing: bool,
    token: String,
    error: Option<String>,
}

#[cfg(any(test, target_os = "linux"))]
fn permitted(debug: bool, agent: bool, bundle: Option<&str>) -> bool {
    !debug && !agent && bundle == Some("appimage")
}

#[cfg(any(test, target_os = "linux"))]
fn registration_status(handler: &str, name: &str, desktop: Option<&str>, expected_exec: &str) -> (bool, bool) {
    let same_path = desktop.is_some_and(|value| {
        let mut entry = false;
        let mut executable = None;
        for line in value.lines() {
            if line.starts_with('[') { entry = line == "[Desktop Entry]"; }
            if entry { if let Some(value) = line.strip_prefix("Exec=") { executable = Some(value); } }
        }
        executable == Some(expected_exec)
    });
    (handler == name && same_path,
        (!handler.is_empty() && handler != name) || (desktop.is_some() && !same_path))
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{fs, path::PathBuf, process::Command};
    use tauri::Manager;
    use tauri_plugin_deep_link::DeepLinkExt;
    const MIME: &str = "x-scheme-handler/risunestlocal";

    pub(super) fn available(app: &AppHandle) -> bool {
        let agent = app.config().plugins.0.get("risunest")
            .and_then(|value| value.get("agent")).and_then(|value| value.as_bool()).unwrap_or(false);
        permitted(cfg!(debug_assertions), agent,
            tauri::utils::platform::bundle_type().as_ref().map(|value| value.to_string()).as_deref())
            && app.env().appimage.is_some()
    }

    fn paths(app: &AppHandle) -> Result<(PathBuf, String, String), String> {
        let image = PathBuf::from(app.env().appimage.ok_or("AppImage path is unavailable")?);
        let image_text = image.to_str().ok_or("AppImage path is not valid UTF-8")?;
        if !image.is_absolute() || !image.is_file() || image_text.chars().any(|ch| "\"\\`$%\n\r".contains(ch)) {
            return Err("Move the AppImage to a regular local path without quotes or special characters, then try again.".into());
        }
        let binary = tauri::utils::platform::current_exe().map_err(|error| error.to_string())?;
        let name = format!("{}-handler.desktop", binary.file_name().ok_or("AppImage executable is unavailable")?.to_string_lossy());
        let manifest = crate::app_paths::manifest(app)?;
        let integration = manifest.integration.as_ref().ok_or("desktop integration roots are unavailable")?;
        let target = integration.data_home.join("applications").join(&name);
        Ok((target, name, format!("\"{image_text}\" %u")))
    }

    pub(super) fn state(app: &AppHandle) -> Result<IntegrationState, String> {
        let (target, name, expected_exec) = paths(app)?;
        let output = Command::new("xdg-mime").args(["query", "default", MIME]).output().map_err(|error| error.to_string())?;
        if !output.status.success() { return Err("The desktop URL handler could not be read.".into()); }
        let handler = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
        let handler = handler.trim();
        let desktop = match fs::read_to_string(&target) {
            Ok(value) => Some(value),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        let (registered, replaces_existing) = registration_status(handler, &name, desktop.as_deref(), &expected_exec);
        let token = hex::encode(Sha256::digest(format!("{handler}\0{}", desktop.as_deref().unwrap_or(""))));
        Ok(IntegrationState { available: true, registered, replaces_existing, token, error: None })
    }

    pub(super) fn register(app: &AppHandle, token: &str, replace_existing: bool) -> Result<(), String> {
        if !available(app) { return Err("AppImage integration is unavailable in this build.".into()); }
        let current = state(app)?;
        if current.token != token { return Err("The URL handler changed. Try again.".into()); }
        if current.replaces_existing && !replace_existing { return Err("Replacing the existing URL handler requires confirmation.".into()); }
        app.deep_link().register("risunestlocal").map_err(|error| error.to_string())?;
        if !state(app)?.registered { return Err("The desktop URL handler could not be registered.".into()); }
        Ok(())
    }
}

#[tauri::command]
pub(crate) async fn appimage_integration_state(_app: AppHandle) -> Result<IntegrationState, String> {
    let unavailable = IntegrationState { available: false, registered: false, replaces_existing: false, token: String::new(), error: None };
    #[cfg(target_os = "linux")]
    if linux::available(&_app) {
        return tauri::async_runtime::spawn_blocking(move || match linux::state(&_app) {
            Ok(state) => state,
            Err(error) => IntegrationState { available: true, error: Some(error), ..unavailable },
        }).await.map_err(|error| error.to_string());
    }
    Ok(unavailable)
}

#[cfg(any(test, target_os = "linux"))]
fn without_owned_mime_handler(contents: &str, name: &str) -> String {
    let mut relevant = false;
    let mut result = String::with_capacity(contents.len());
    for line in contents.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        let trimmed = body.trim();
        if trimmed.starts_with('[') {
            relevant = matches!(trimmed, "[Default Applications]" | "[Added Associations]" | "[Removed Associations]" | "[MIME Cache]");
        }
        if relevant {
            if let Some((key, value)) = body.split_once('=') {
                if key.trim() == "x-scheme-handler/risunestlocal" && value.split(';').any(|entry| entry.trim() == name) {
                    let remaining: Vec<_> = value.split(';').filter(|entry| !entry.trim().is_empty() && entry.trim() != name).collect();
                    if !remaining.is_empty() {
                        result.push_str(key);
                        result.push('=');
                        result.push_str(&remaining.join(";"));
                        if value.ends_with(';') { result.push(';'); }
                        result.push_str(&line[body.len()..]);
                    }
                    continue;
                }
            }
        }
        result.push_str(line);
    }
    result
}

#[cfg(any(test, target_os = "linux"))]
fn remove_owned_at(image: &std::path::Path, name: &str, applications: &std::path::Path, config: &std::path::Path) -> Result<(), String> {
    use std::{fs, io::Write};
    let failure = || "app-cleanup-integration-failed".to_owned();
    let desktop = applications.join(name);
    let read_regular = |path: &std::path::Path| -> Result<Option<String>, String> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(failure()),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() { return Err(failure()); }
        fs::read_to_string(path).map(Some).map_err(|_| failure())
    };
    let Some(original) = read_regular(&desktop)? else { return Ok(()); };
    let expected = format!("\"{}\" %u", image.to_str().ok_or_else(failure)?);
    if !registration_status(name, name, Some(&original), &expected).0 { return Ok(()); }
    let mut candidates = vec![config.join("mimeapps.list"), applications.join("mimeapps.list"), applications.join("mimeinfo.cache")];
    for directory in [config, applications] {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(failure()),
        };
        for entry in entries {
            let entry = entry.map_err(|_| failure())?;
            if entry.file_name().to_string_lossy().ends_with("-mimeapps.list") {
                candidates.push(entry.path());
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    for path in candidates {
        let Some(contents) = read_regular(&path)? else { continue; };
        let updated = without_owned_mime_handler(&contents, name);
        if updated == contents { continue; }
        if read_regular(&desktop)?.as_deref() != Some(original.as_str()) { return Err(failure()); }
        let permissions = fs::metadata(&path).map_err(|_| failure())?.permissions();
        let mut temporary = tempfile::NamedTempFile::new_in(path.parent().ok_or_else(failure)?).map_err(|_| failure())?;
        temporary.write_all(updated.as_bytes()).map_err(|_| failure())?;
        temporary.as_file().set_permissions(permissions).map_err(|_| failure())?;
        temporary.as_file().sync_all().map_err(|_| failure())?;
        if read_regular(&path)?.as_deref() != Some(contents.as_str()) { return Err(failure()); }
        temporary.persist(&path).map_err(|_| failure())?;
    }
    // Keep the ownership evidence until every association edit finishes so failures can retry.
    if read_regular(&desktop)?.as_deref() != Some(original.as_str()) { return Err(failure()); }
    fs::remove_file(desktop).map_err(|_| failure())
}

#[cfg(target_os = "linux")]
pub(crate) fn remove_owned(
    image: &std::path::Path,
    integration: &crate::app_paths::Integration,
) -> Result<(), String> {
    let failure = || "app-cleanup-integration-failed".to_owned();
    let binary = tauri::utils::platform::current_exe().map_err(|_| failure())?;
    let name = format!("{}-handler.desktop", binary.file_name().ok_or_else(failure)?.to_string_lossy());
    let applications = integration.data_home.join("applications");
    remove_owned_at(image, &name, &applications, &integration.config_home)
}

#[tauri::command]
pub(crate) async fn appimage_integration_register(_app: AppHandle, token: String, replace_existing: bool) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return tauri::async_runtime::spawn_blocking(move || linux::register(&_app, &token, replace_existing))
        .await.map_err(|error| error.to_string())?;
    #[cfg(not(target_os = "linux"))]
    { let _ = (token, replace_existing); Err("AppImage integration is unavailable on this platform.".into()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registration_compares_both_default_handler_and_launch_path() {
        let same = "[Desktop Entry]\nExec=\"/a/app.AppImage\" %u\n";
        let expected = "\"/a/app.AppImage\" %u";
        assert_eq!(registration_status("", "app-handler.desktop", None, expected), (false, false));
        assert_eq!(registration_status("other.desktop", "app-handler.desktop", Some(same), expected), (false, true));
        assert_eq!(registration_status("app-handler.desktop", "app-handler.desktop", Some(same), expected), (true, false));
        assert_eq!(registration_status("app-handler.desktop", "app-handler.desktop", Some(same), "\"/moved/app.AppImage\" %u"), (false, true));
        let duplicate = format!("{same}Exec=other %u\n");
        assert_eq!(registration_status("app-handler.desktop", "app-handler.desktop", Some(&duplicate), expected), (false, true));
    }
    #[test]
    fn integration_requires_a_production_appimage() {
        assert!(permitted(false, false, Some("appimage")));
        assert!(!permitted(true, false, Some("appimage")));
        assert!(!permitted(false, true, Some("appimage")));
        assert!(!permitted(false, false, Some("deb")));
        assert!(!permitted(false, false, None));
    }

    #[test]
    fn cleanup_preserves_other_schemes_sections_and_handlers() {
        let source = "# keep\r\n[Default Applications]\r\nx-scheme-handler/risunestlocal=other.desktop;app-handler.desktop;last.desktop;\r\nx-scheme-handler/https=app-handler.desktop;\r\n[Added Associations]\r\nx-scheme-handler/risunestlocal=app-handler.desktop;\r\n[Other]\r\nx-scheme-handler/risunestlocal=app-handler.desktop;\r\n";
        assert_eq!(without_owned_mime_handler(source, "app-handler.desktop"), "# keep\r\n[Default Applications]\r\nx-scheme-handler/risunestlocal=other.desktop;last.desktop;\r\nx-scheme-handler/https=app-handler.desktop;\r\n[Added Associations]\r\n[Other]\r\nx-scheme-handler/risunestlocal=app-handler.desktop;\r\n");
    }

    #[test]
    fn cleanup_removes_only_selected_appimage_and_is_repeatable() {
        let temp = tempfile::tempdir().unwrap();
        let apps = temp.path().join("applications");
        let config = temp.path().join("config");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::create_dir_all(&config).unwrap();
        let image = temp.path().join("chosen.AppImage");
        let name = "risunest-handler.desktop";
        let desktop = apps.join(name);
        std::fs::write(&desktop, format!("[Desktop Entry]\nExec=\"{}\" %u\n", image.to_str().unwrap())).unwrap();
        let mime = format!("[Default Applications]\nx-scheme-handler/risunestlocal={name};other.desktop;\n");
        for file in [config.join("mimeapps.list"), apps.join("mimeapps.list"), config.join("gnome-mimeapps.list")] {
            std::fs::write(file, &mime).unwrap();
        }
        std::fs::write(apps.join("mimeinfo.cache"), format!("[MIME Cache]\nx-scheme-handler/risunestlocal={name};\n")).unwrap();
        remove_owned_at(&temp.path().join("other.AppImage"), name, &apps, &config).unwrap();
        assert!(desktop.exists());
        assert_eq!(std::fs::read_to_string(config.join("mimeapps.list")).unwrap(), mime);
        remove_owned_at(&image, name, &apps, &config).unwrap();
        assert!(!desktop.exists());
        for file in [config.join("mimeapps.list"), apps.join("mimeapps.list"), config.join("gnome-mimeapps.list")] {
            assert_eq!(std::fs::read_to_string(file).unwrap(), "[Default Applications]\nx-scheme-handler/risunestlocal=other.desktop;\n");
        }
        assert_eq!(std::fs::read_to_string(apps.join("mimeinfo.cache")).unwrap(), "[MIME Cache]\n");
        std::fs::write(config.join("mimeapps.list"), &mime).unwrap();
        remove_owned_at(&image, name, &apps, &config).unwrap();
        assert_eq!(std::fs::read_to_string(config.join("mimeapps.list")).unwrap(), mime);
    }

    #[test]
    fn cleanup_retains_ownership_evidence_on_partial_failure_for_retry() {
        let temp = tempfile::tempdir().unwrap();
        let apps = temp.path().join("applications");
        let config = temp.path().join("config");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::create_dir_all(&config).unwrap();
        let image = temp.path().join("chosen.AppImage");
        let name = "risunest-handler.desktop";
        let desktop = apps.join(name);
        std::fs::write(&desktop, format!("[Desktop Entry]\nExec=\"{}\" %u\n", image.to_str().unwrap())).unwrap();
        std::fs::create_dir(config.join("mimeapps.list")).unwrap();
        assert!(remove_owned_at(&image, name, &apps, &config).is_err());
        assert!(desktop.is_file());
        std::fs::remove_dir(config.join("mimeapps.list")).unwrap();
        remove_owned_at(&image, name, &apps, &config).unwrap();
        assert!(!desktop.exists());
    }
}
