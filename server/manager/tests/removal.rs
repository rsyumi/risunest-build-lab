#[cfg(target_os = "linux")]
mod linux {

use risunest_sync_manager::platform;
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

#[test]
fn uninstall_stops_loaded_jobs_without_unit_files_and_preserves_data() {
    run_removal(false);
}

#[test]
fn failed_profile_cleanup_preserves_gui_server_and_manager_for_retry() {
    run_removal(true);
}

fn run_removal(fail_profile_cleanup: bool) {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("data-home");
    let root = base.join("risunest-sync");
    let install = temp.path().join("install");
    let registry = base.join(".risunest-sync-instances");
    let commands = temp.path().join("commands");
    for path in [&root, &install, &registry, &commands] {
        fs::create_dir_all(path).unwrap();
    }
    let manager = install.join("risunest-sync-manager");
    let server = install.join("risunest-sync-server");
    fs::copy(env!("CARGO_BIN_EXE_risunest-sync-manager"), &manager).unwrap();
    fs::write(&server, "synthetic never executed").unwrap();
    let gui = install.join("risunest-sync-gui");
    fs::write(&gui, "synthetic never executed").unwrap();
    if fail_profile_cleanup {
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), "synthetic").unwrap();
        std::os::unix::fs::symlink(&outside, base.join("risunest-sync-webview")).unwrap();
    }
    fs::write(root.join("metadata.sqlite"), "synthetic never opened").unwrap();
    fs::write(install.join("risunest-sync-bundle.json"), serde_json::json!({
        "schema":"risunest-sync-bundle/v1", "product":"sync", "variant":"managed", "version":"1.0.0",
        "protocolId":risunest_sync_server::PROTOCOL_ID, "storeFormatId":risunest_sync_server::STORE_FORMAT_ID,
        "files":["risunest-sync-manager", "risunest-sync-server", "risunest-sync-gui"], "vendor":[]
    }).to_string()).unwrap();
    let instance = platform::instance_name(&root);
    let record = serde_json::json!({"schema":1,"data":root,"install":install,"server":server,"instance":instance,"lingerChanged":false}).to_string();
    fs::write(root.join("risunest-sync-instance.json"), &record).unwrap();
    fs::write(registry.join(format!("{instance}.json")), &record).unwrap();
    let control = commands.join("systemctl");
    fs::write(&control, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$SYNTHETIC_LOG\"\nif test \"$2\" = show; then printf 'LoadState=not-found\\nActiveState=active\\n'; fi\nexit 0\n").unwrap();
    fs::set_permissions(&control, fs::Permissions::from_mode(0o700)).unwrap();
    let log = temp.path().join("commands.log");
    let invoke = |arguments: &[&str]| {
        Command::new(&manager)
            .arg("--data-dir")
            .arg(&root)
            .args(arguments)
            .env("PATH", &commands)
            .env("SYNTHETIC_LOG", &log)
            .env("HOME", temp.path())
            .env("XDG_DATA_HOME", &base)
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .output()
            .unwrap()
    };
    let preview = invoke(&["uninstall", "--dry-run"]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    assert!(!log.exists());
    assert!(!root.join("manager-update").exists());
    let cancelled = invoke(&["uninstall"]);
    assert!(!cancelled.status.success());
    assert!(!log.exists());
    assert!(manager.exists());
    let result = if fail_profile_cleanup {
        invoke(&["uninstall", "--yes", "--delete-data"])
    } else {
        invoke(&["uninstall", "--yes"])
    };
    if fail_profile_cleanup {
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("removal-linked-path"));
        for executable in [&manager, &server, &gui] {
            assert!(executable.exists());
        }
        assert!(install.join("risunest-sync-bundle.json").exists());
        assert!(root.join("risunest-sync-instance.json").exists());
        assert!(root.join("metadata.sqlite").exists());
        assert!(temp.path().join("outside/keep").exists());
        return;
    }
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        fs::read(root.join("metadata.sqlite")).unwrap(),
        b"synthetic never opened"
    );
    assert!(!install.exists());
    let commands = fs::read_to_string(log).unwrap();
    for suffix in ["-update.timer", "-update.service", ".service"] {
        assert!(
            commands.contains(&format!("--user stop {instance}{suffix}")),
            "{commands}"
        );
    }
    assert!(!PathBuf::from(temp.path())
        .join("config/systemd/user")
        .exists());
}
}

#[cfg(windows)]
mod windows {
    use std::{fs, path::PathBuf, process::Command};

    #[test]
    fn incomplete_installation_and_force_use_the_real_manager_without_deleting_unknown_data() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let local = base.join("local");
        let root = local.join("RisuNestSyncData");
        let install = base.join("installation with spaces");
        fs::create_dir_all(&install).unwrap();
        let manager = install.join("risunest-sync-manager.exe");
        fs::copy(env!("CARGO_BIN_EXE_risunest-sync-manager"), &manager).unwrap();
        for value in ["installer", "lock-held", "prepare-update", "--dry-run"] {
            let result = Command::new(&manager).args(["--data-dir", value, "status"]).output().unwrap();
            let error = String::from_utf8_lossy(&result.stderr);
            assert!(!result.status.success());
            assert!(error.contains("absolute-data-dir-required"));
            assert!(!error.starts_with("absolute-data-dir-required"), "An option value selected installer diagnostics: {error}");
        }
        let server = install.join("risunest-sync-server.exe");
        fs::write(&server, "synthetic never executed").unwrap();
        let marker = install.join("risunest-sync-bundle.json");
        fs::write(&marker, serde_json::json!({
            "schema":"risunest-sync-bundle/v1", "product":"sync", "variant":"managed", "version":"1.0.3",
            "protocolId":risunest_sync_server::PROTOCOL_ID, "storeFormatId":risunest_sync_server::STORE_FORMAT_ID,
            "files":["risunest-sync-manager.exe", "risunest-sync-server.exe"], "vendor":[]
        }).to_string()).unwrap();
        let invoke = |executable: &std::path::Path, args: &[&str]| {
            Command::new(executable).arg("--data-dir").arg(&root).arg("--server").arg(&server)
                .args(args).env("LOCALAPPDATA", &local).env("APPDATA", base.join("roaming"))
                .output().unwrap()
        };
        let registered = invoke(&manager, &["installer", "register"]);
        assert!(registered.status.success(), "{}", String::from_utf8_lossy(&registered.stderr));
        assert!(root.join("risunest-sync-instance.json").exists());
        assert!(!root.join("metadata.sqlite").exists());
        let normal = invoke(&manager, &["uninstall", "--dry-run"]);
        assert!(normal.status.success());
        assert!(!root.join("manager-update").exists());
        let extra = install.join("unowned.txt");
        fs::write(&extra, "synthetic").unwrap();
        let blocked_preview = invoke(&manager, &["uninstall", "--dry-run"]);
        assert!(!blocked_preview.status.success());
        let blocked_plan: serde_json::Value = serde_json::from_slice(&blocked_preview.stdout).unwrap();
        assert_eq!(blocked_plan["blockers"], serde_json::json!(["removal-unowned-install-files"]));
        assert!(String::from_utf8_lossy(&blocked_preview.stderr).contains("removal-unowned-install-files"));
        let installer_check = invoke(&manager, &["installer", "removal-check", "normal", "preserve"]);
        assert!(!installer_check.status.success());
        assert!(installer_check.stdout.is_empty());
        assert!(String::from_utf8_lossy(&installer_check.stderr).starts_with("removal-unowned-install-files\n"));
        let cleanup = invoke(&manager, &["uninstall", "lock-held"]);
        assert!(cleanup.status.success(), "{}", String::from_utf8_lossy(&cleanup.stderr));
        let instance = risunest_sync_manager::platform::instance_name(&root);
        fs::remove_file(local.join(".risunest-sync-instances").join(format!("{instance}.json"))).unwrap();
        fs::remove_file(root.join("risunest-sync-instance.json")).unwrap();
        fs::write(root.join("preserved-data"), "synthetic").unwrap();
        let forced = invoke(&manager, &["uninstall", "--force", "--dry-run", "--delete-data"]);
        assert!(forced.status.success());
        let plan: serde_json::Value = serde_json::from_slice(&forced.stdout).unwrap();
        assert_eq!(plan["deleteData"], false);
        assert_eq!(plan["dataKeptWithoutRegistration"], true);
        assert!(!root.join("manager-update").exists());
        let unconfirmed = invoke(&manager, &["uninstall", "--force"]);
        assert!(!unconfirmed.status.success());
        assert!(String::from_utf8_lossy(&unconfirmed.stderr).contains("removal-confirmation-required-use-yes"));
        assert!(server.exists() && manager.exists());
        let blocked = invoke(&manager, &["uninstall", "--dry-run"]);
        assert!(!blocked.status.success());
        assert!(blocked.stdout.is_empty());
        assert!(String::from_utf8_lossy(&blocked.stderr).contains("removal-registration-missing"));
        let copied = base.join("removal-helper.exe");
        fs::copy(&manager, &copied).unwrap();
        let removed = invoke(&copied, &["installer", "remove-files", "force"]);
        assert!(removed.status.success(), "{}", String::from_utf8_lossy(&removed.stderr));
        assert!(!server.exists() && !manager.exists() && !marker.exists());
        assert_eq!(fs::read(&extra).unwrap(), b"synthetic");
        assert_eq!(fs::read(root.join("preserved-data")).unwrap(), b"synthetic");
        assert!(PathBuf::from(&copied).exists());
    }
}
