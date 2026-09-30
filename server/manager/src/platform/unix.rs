use super::*;

fn checked(command: &mut Command) -> Result<()> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|_| "user-service-unavailable")?;
    if !output.status.success() {
        return Err("user-service-operation-failed".into());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn systemd_arg(path: &Path) -> Result<String> {
    let value = path.to_str().ok_or("invalid-user-service-path")?;
    if value.chars().any(char::is_control) {
        return Err("invalid-user-service-path".into());
    }
    Ok(format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    ))
}
#[cfg(not(target_os = "macos"))]
pub(super) fn startup(root: &Path, executable: &Path, action: &str) -> Result<StartupStatus> {
    let directory = user_service_directory()?;
    let name = format!("{}.service", instance_name(root));
    let path = directory.join(&name);
    match action {
        "install" => {
            std::fs::create_dir_all(&directory).map_err(|_| "user-service-write-failed")?;
            crate::update::write_bytes_atomic(&path, unit(root, executable)?.as_bytes(), 0o644, "user-service-write-failed")?;
            checked(process("systemctl").args(["--user", "daemon-reload"]))?;
            checked(process("systemctl").args(["--user", "enable", &name]))?;
        }
        "remove" if path.exists() => {
            checked(process("systemctl").args(["--user", "disable", &name]))?;
            std::fs::remove_file(&path).map_err(|_| "user-service-remove-failed")?;
            checked(process("systemctl").args(["--user", "daemon-reload"]))?;
        }
        "start" => checked(process("systemctl").args(["--user", "start", &name]))?,
        _ => (),
    }
    let enabled = if path.exists() {
        let output = process("systemctl")
            .args(["--user", "is-enabled", &name])
            .output()
            .map_err(|_| "user-service-unavailable")?;
        match String::from_utf8_lossy(&output.stdout).trim() {
            "enabled" => true,
            "disabled" | "masked" | "static" => false,
            _ => return Err("user-service-status-unavailable".into()),
        }
    } else {
        false
    };
    Ok(StartupStatus {
        registered: path.exists(),
        enabled,
        action_matches: std::fs::read_to_string(&path).is_ok_and(|body| unit(root, executable).is_ok_and(|expected| body == expected)),
    })
}
#[cfg(not(target_os = "macos"))]
fn unit(root: &Path, executable: &Path) -> Result<String> {
    Ok(format!("[Unit]\nDescription=RisuNest sync server\nStartLimitIntervalSec=300\nStartLimitBurst=3\n[Service]\nExecStart={} serve --data-dir {}\nRestart=on-failure\nRestartSec=10\n[Install]\nWantedBy=default.target\n",systemd_arg(executable)?,systemd_arg(root)?))
}

#[cfg(not(target_os = "macos"))]
fn updater_unit(root: &Path, manager: &Path, server: &Path) -> Result<String> {
    Ok(format!(
        "[Unit]\nDescription=Check for a verified RisuNest Sync update\nAfter={}.service\n[Service]\nType=oneshot\nExecStart={} --data-dir {} --server {} update scheduled\n",
        instance_name(root),
        systemd_arg(manager)?,
        systemd_arg(root)?,
        systemd_arg(server)?
    ))
}

#[cfg(not(target_os = "macos"))]
fn updater_timer(update_service: &str, jitter_seconds: u64) -> String {
    format!(
        "[Unit]\nDescription=Schedule verified RisuNest Sync updates\n[Timer]\nOnBootSec={}s\nOnUnitActiveSec=1h\nRandomizedDelaySec=5m\nPersistent=true\nUnit={}\n[Install]\nWantedBy=timers.target\n",
        60 + jitter_seconds,
        update_service
    )
}

#[cfg(not(target_os = "macos"))]
pub(super) fn update_schedule(
    root: &Path,
    manager: &Path,
    server: &Path,
    policy: UpdatePolicy,
    action: &str,
) -> Result<UpdateScheduleStatus> {
    let directory = user_service_directory()?;
    let update_service = format!("{}-update.service", instance_name(root));
    let timer = format!("{}-update.timer", instance_name(root));
    let service_path = directory.join(&update_service);
    let timer_path = directory.join(&timer);
    let should_install = matches!(action, "install" | "install-recovery") && policy != UpdatePolicy::Off;
    if should_install {
        std::fs::create_dir_all(&directory).map_err(|_| "user-service-write-failed")?;
        crate::update::write_bytes_atomic(&service_path, updater_unit(root, manager, server)?.as_bytes(), 0o644, "user-service-write-failed")?;
        let jitter = instance_name(root).bytes().fold(0u64, |value, byte| {
            value.wrapping_mul(31).wrapping_add(byte as u64)
        }) % 300;
        crate::update::write_bytes_atomic(&timer_path, updater_timer(&update_service, jitter).as_bytes(), 0o644, "user-service-write-failed")?;
        checked(process("systemctl").args(["--user", "daemon-reload"]))?;
        checked(process("systemctl").args(["--user", "enable", "--now", &timer]))?;
    } else if action == "remove" || (action == "install" && policy == UpdatePolicy::Off) {
        remove_update_registration(&[&service_path, &timer_path], || {
            if timer_path.exists() {
                checked(process("systemctl").args(["--user", "disable", &timer]))?;
            }
            Ok(())
        }, || {
            checked(process("systemctl").args(["--user", "daemon-reload"]))?;
            stop_systemd(&timer)?;
            stop_systemd(&update_service)
        })?;
    }
    let enabled = if timer_path.exists() {
        process("systemctl")
            .args(["--user", "is-enabled", &timer])
            .output()
            .map_err(|_| "user-service-unavailable")?
            .status
            .success()
    } else {
        false
    };
    Ok(UpdateScheduleStatus {
        registered: service_path.exists() && timer_path.exists(),
        enabled,
        action_matches: std::fs::read_to_string(&service_path).is_ok_and(|body| updater_unit(root, manager, server).is_ok_and(|expected| body == expected))
            && std::fs::read_to_string(&timer_path).is_ok_and(|body| {
                let jitter = instance_name(root).bytes().fold(0u64, |value, byte| value.wrapping_mul(31).wrapping_add(byte as u64)) % 300;
                body == updater_timer(&update_service, jitter)
            }),
    })
}

#[cfg(all(test, not(target_os = "macos")))]
mod update_tests {
    use super::*;

    #[test]
    fn timer_runs_after_boot_and_every_hour() {
        let body = updater_timer("risunest-sync-test-update.service", 42);
        assert!(body.contains("OnBootSec=102s"));
        assert!(body.contains("OnUnitActiveSec=1h"));
        assert!(body.contains("RandomizedDelaySec=5m"));
        assert!(body.contains("Persistent=true"));
    }

    #[test]
    fn update_service_calls_the_noninteractive_manager_command() {
        let body = updater_unit(
            Path::new("/home/test/.local/share/risunest-sync"),
            Path::new("/home/test/.local/lib/risunest-sync/risunest-sync-manager"),
            Path::new("/home/test/.local/lib/risunest-sync/risunest-sync-server"),
        )
        .unwrap();
        assert!(body.contains("update scheduled"));
        assert!(body.contains("Type=oneshot"));
        assert!(!body.contains("Restart="));
    }

    #[test]
    fn mac_startup_registration_body_tracks_the_current_bundle_path() {
        let root = Path::new("/Users/test/Library/Application Support/RisuNest Sync");
        let old = startup_agent_body(
            "io.github.rsyumi.test",
            root,
            Path::new("/Applications/RisuNest Sync.app/Contents/MacOS/server"),
        );
        let moved = startup_agent_body(
            "io.github.rsyumi.test",
            root,
            Path::new("/Users/test/Applications/RisuNest Sync.app/Contents/MacOS/server"),
        );
        assert_ne!(old, moved);
        assert!(moved.contains("/Users/test/Applications/RisuNest Sync.app"));
    }
}

#[cfg(any(target_os = "macos", test))]
fn startup_agent_body(name: &str, root: &Path, executable: &Path) -> String {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>Label</key><string>{name}</string><key>ProgramArguments</key><array><string>{}</string><string>serve</string><string>--data-dir</string><string>{}</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>ThrottleInterval</key><integer>30</integer></dict></plist>",xml(&executable.to_string_lossy()),xml(&root.to_string_lossy()))
}

#[cfg(target_os = "macos")]
pub(super) fn startup(root: &Path, executable: &Path, action: &str) -> Result<StartupStatus> {
    let directory = user_service_directory()?;
    let name = format!("io.github.rsyumi.{}", instance_name(root));
    let path = directory.join(format!("{name}.plist"));
    let uid = process("id")
        .arg("-u")
        .output()
        .map_err(|_| "user-id-unavailable")?;
    let uid = String::from_utf8(uid.stdout).map_err(|_| "user-id-unavailable")?;
    let domain = format!("gui/{}", uid.trim());
    let service = format!("{domain}/{name}");
    let expected_body = startup_agent_body(&name, root, executable);
    match action {
        "install" => {
            std::fs::create_dir_all(&directory).map_err(|_| "user-service-write-failed")?;
            crate::update::write_bytes_atomic(&path, expected_body.as_bytes(), 0o644, "user-service-write-failed")?;
        }
        "remove" if path.exists() => {
            // Removing future login registration must not stop the current daemon.
            // The loaded job leaves the user domain at logout.
            std::fs::remove_file(&path).map_err(|_| "user-service-remove-failed")?;
        }
        "start" => {
            let loaded = process("launchctl")
                .args(["print", &service])
                .output()
                .map_err(|_| "user-service-unavailable")?
                .status
                .success();
            if loaded {
                checked(process("launchctl").args(["bootout", &service]))?;
            }
            checked(process("launchctl").args(["bootstrap", &domain]).arg(&path))?;
        }
        _ => (),
    }
    let enabled = path.exists();
    let action_matches =
        path.exists() && std::fs::read_to_string(&path).is_ok_and(|body| body == expected_body);
    Ok(StartupStatus {
        registered: path.exists(),
        enabled,
        action_matches,
    })
}

#[cfg(target_os = "macos")]
pub(super) fn update_schedule(
    root: &Path,
    manager: &Path,
    server: &Path,
    policy: UpdatePolicy,
    action: &str,
) -> Result<UpdateScheduleStatus> {
    let directory = user_service_directory()?;
    let name = format!("io.github.rsyumi.{}-update", instance_name(root));
    let path = directory.join(format!("{name}.plist"));
    let uid = process("id")
        .arg("-u")
        .output()
        .map_err(|_| "user-id-unavailable")?;
    let uid = String::from_utf8(uid.stdout).map_err(|_| "user-id-unavailable")?;
    let domain = format!("gui/{}", uid.trim());
    let service = format!("{domain}/{name}");
    let should_install = matches!(action, "install" | "install-recovery") && policy != UpdatePolicy::Off;
    let expected_body = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>Label</key><string>{name}</string><key>ProgramArguments</key><array><string>{}</string><string>--data-dir</string><string>{}</string><string>--server</string><string>{}</string><string>update</string><string>scheduled</string></array><key>RunAtLoad</key><true/><key>StartInterval</key><integer>3600</integer><key>ProcessType</key><string>Background</string><key>AbandonProcessGroup</key><true/></dict></plist>", xml(&manager.to_string_lossy()), xml(&root.to_string_lossy()), xml(&server.to_string_lossy()));
    if should_install {
        let loaded = process("launchctl")
            .args(["print", &service])
            .output()
            .map_err(|_| "user-service-unavailable")?
            .status
            .success();
        let matches = std::fs::read_to_string(&path).is_ok_and(|body| body == expected_body);
        if replace_update_agent(action, loaded, matches)? {
            std::fs::create_dir_all(&directory).map_err(|_| "user-service-write-failed")?;
            crate::update::write_bytes_atomic(&path, expected_body.as_bytes(), 0o644, "user-service-write-failed")?;
            if loaded {
                checked(process("launchctl").args(["bootout", &service]))?;
            }
            checked(process("launchctl").args(["bootstrap", &domain]).arg(&path))?;
        }
    } else if action == "remove" || (action == "install" && policy == UpdatePolicy::Off) {
        remove_update_registration(&[&path], || Ok(()), || unload_launchd(&service))?;
    }
    let enabled = path.exists()
        && process("launchctl")
            .args(["print", &service])
            .output()
            .map_err(|_| "user-service-unavailable")?
            .status
            .success();
    Ok(UpdateScheduleStatus {
        registered: path.exists(),
        enabled,
        action_matches: path.exists()
            && std::fs::read_to_string(&path).is_ok_and(|body| body == expected_body),
    })
}

fn remove_update_registration(paths: &[&Path], disable: impl FnOnce() -> Result<()>, stop: impl FnOnce() -> Result<()>) -> Result<()> {
    disable()?;
    for path in paths {
        match std::fs::remove_file(path) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err("user-service-remove-failed".into()),
        }
    }
    stop()
}

#[cfg(any(target_os = "macos", test))]
fn replace_update_agent(action: &str, loaded: bool, matches: bool) -> Result<bool> {
    if action == "install-recovery" && loaded {
        if !matches { return Err("update-recovery-schedule-mismatch".into()); }
        return Ok(false);
    }
    Ok(true)
}

#[cfg(test)]
mod recovery_schedule_tests {
    use super::*;

    #[test]
    fn loaded_recovery_agent_is_reused_or_rejected_without_replacement() {
        assert!(!replace_update_agent("install-recovery", true, true).unwrap());
        assert_eq!(replace_update_agent("install-recovery", true, false).unwrap_err(), "update-recovery-schedule-mismatch");
        assert!(replace_update_agent("install-recovery", false, false).unwrap());
        assert!(replace_update_agent("install", true, false).unwrap());
    }

    #[test]
    fn persistent_definitions_are_removed_before_a_stop_that_can_terminate_the_caller() {
        let temp = tempfile::tempdir().unwrap();
        let service = temp.path().join("update.service");
        let timer = temp.path().join("update.timer");
        std::fs::write(&service, b"service").unwrap();
        std::fs::write(&timer, b"timer").unwrap();
        let disabled = std::cell::Cell::new(false);
        let result = remove_update_registration(&[&service, &timer], || {
            assert!(service.exists() && timer.exists());
            disabled.set(true);
            Ok(())
        }, || {
            assert!(disabled.get());
            assert!(!service.exists() && !timer.exists());
            Err("synthetic-stop-failure".into())
        });
        assert_eq!(result.unwrap_err(), "synthetic-stop-failure");
        assert!(!service.exists() && !timer.exists());
    }

    #[test]
    fn failed_definition_removal_does_not_stop_the_running_updater() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(remove_update_registration(&[temp.path()], || Ok(()), || panic!("must preserve the running process")).unwrap_err(), "user-service-remove-failed");
    }
}

#[cfg(not(target_os = "macos"))]
fn stop_systemd(name: &str) -> Result<()> {
    let state = process("systemctl")
        .args([
            "--user",
            "show",
            "--property=LoadState",
            "--property=ActiveState",
            name,
        ])
        .output()
        .map_err(|_| "user-service-unavailable")?;
    if !state.status.success() {
        return Err("user-service-status-unavailable".into());
    }
    let state = String::from_utf8_lossy(&state.stdout);
    if !state.lines().any(|line| line == "LoadState=not-found")
        || !state.lines().any(|line| line == "ActiveState=inactive")
    {
        checked(process("systemctl").args(["--user", "stop", name]))?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) fn unload_launchd(service: &str) -> Result<()> {
    let domain = service.rsplit_once('/').ok_or("user-service-invalid")?.0;
    checked(process("launchctl").args(["print", domain]))?;
    let loaded = process("launchctl")
        .args(["print", service])
        .output()
        .map_err(|_| "user-service-unavailable")?
        .status
        .success();
    if loaded {
        checked(process("launchctl").args(["bootout", service]))?;
    }
    Ok(())
}

pub(super) fn remove_startup(root: &Path, executable: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let service = format!(
            "{}/io.github.rsyumi.{}",
            launchd_user_domain()?,
            instance_name(root)
        );
        unload_launchd(&service)?;
    }
    #[cfg(not(target_os = "macos"))]
    stop_systemd(&format!("{}.service", instance_name(root)))?;
    startup(root, executable, "remove")?;
    Ok(())
}
