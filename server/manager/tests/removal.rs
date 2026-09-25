#![cfg(target_os = "linux")]

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
