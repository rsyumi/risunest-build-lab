#[cfg(any(windows, target_os = "macos"))]
use crate::platform;
use crate::Result;
use std::path::Path;

#[cfg(windows)]
fn setting_at(root: &Path, enabled: Option<bool>, current: &Path) -> Result<bool> {
    const SCRIPT: &str = r#"
    $ErrorActionPreference='Stop'
    try {
      if($env:RISUNEST_GUI_ACTION -eq 'enable') {
        $key=[Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Software\Microsoft\Windows\CurrentVersion\Run')
      } else {
        $key=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\Microsoft\Windows\CurrentVersion\Run',($env:RISUNEST_GUI_ACTION -eq 'disable'))
      }
      if($null -eq $key){'false';exit 0}
      if($env:RISUNEST_GUI_ACTION -eq 'enable'){$key.SetValue($env:RISUNEST_GUI_NAME,$env:RISUNEST_GUI_COMMAND)}
      if($env:RISUNEST_GUI_ACTION -eq 'disable'){$key.DeleteValue($env:RISUNEST_GUI_NAME,$false)}
      $value=$key.GetValue($env:RISUNEST_GUI_NAME); $key.Close()
      if($null -eq $value){'false'}else{'true'}
    } catch { exit 1 }
    "#;
    let value = format!(
        "\"{}\" --tray --data-dir \"{}\"",
        current.to_string_lossy(),
        root.to_string_lossy().trim_end_matches(['\\', '/'])
    );
    let output = platform::process("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            SCRIPT,
        ])
        .env(
            "RISUNEST_GUI_ACTION",
            match enabled {
                Some(true) => "enable",
                Some(false) => "disable",
                None => "status",
            },
        )
        .env(
            "RISUNEST_GUI_NAME",
            format!("{}-gui", platform::instance_name(root)),
        )
        .env("RISUNEST_GUI_COMMAND", value)
        .output()
        .map_err(|_| "gui-startup-unavailable")?;
    if !output.status.success() {
        return Err("gui-startup-operation-failed".into());
    }
    match String::from_utf8_lossy(&output.stdout).trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err("gui-startup-status-unavailable".into()),
    }
}

#[cfg(target_os = "macos")]
fn setting_at(root: &Path, enabled: Option<bool>, current: &Path) -> Result<bool> {
    let home = std::env::var_os("HOME").ok_or("home-unavailable")?;
    let directory = std::path::PathBuf::from(home).join("Library/LaunchAgents");
    let name = format!("io.github.rsyumi.{}-gui", platform::instance_name(root));
    let path = directory.join(format!("{name}.plist"));
    if let Some(enabled) = enabled {
        if enabled {
            std::fs::create_dir_all(&directory).map_err(|_| "gui-startup-write-failed")?;
            let xml=format!("<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>Label</key><string>{name}</string><key>ProgramArguments</key><array><string>{}</string><string>--tray</string><string>--data-dir</string><string>{}</string></array><key>RunAtLoad</key><true/></dict></plist>",platform::xml(&current.to_string_lossy()),platform::xml(&root.to_string_lossy()));
            crate::update::write_bytes_atomic(
                &path,
                xml.as_bytes(),
                0o644,
                "gui-startup-write-failed",
            )?;
        } else if path.exists() {
            std::fs::remove_file(&path).map_err(|_| "gui-startup-remove-failed")?;
        }
    }
    Ok(path.exists())
}
#[cfg(any(windows, target_os = "macos"))]
pub fn setting(root: &Path, enabled: Option<bool>) -> Result<bool> {
    setting_at(
        root,
        enabled,
        &std::env::current_exe().map_err(|_| "executable-unavailable")?,
    )
}

#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn reconcile(root: &Path, gui: &Path) -> Result<()> {
    if setting_at(root, None, gui)? {
        if !gui.is_file() {
            return Err("gui-startup-unavailable".into());
        }
        setting_at(root, Some(true), gui)?;
    }
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn setting(_: &Path, _: Option<bool>) -> Result<bool> {
    Err("gui-platform-not-supported".into())
}

pub fn relaunch(root: &Path, gui: &Path, tray: bool) -> Result<()> {
    #[cfg(windows)]
    {
        let arguments = format!(
            "{}--data-dir \"{}\"",
            if tray { "--tray " } else { "" },
            root.to_string_lossy().trim_end_matches(['\\', '/'])
        );
        let status = platform::process("powershell.exe")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
                "$ErrorActionPreference='Stop';$shell=New-Object -ComObject Shell.Application;$shell.ShellExecute($env:RISUNEST_GUI_PATH,$env:RISUNEST_GUI_ARGS,'','open',1)"])
            .env("RISUNEST_GUI_PATH", gui).env("RISUNEST_GUI_ARGS", arguments)
            .status().map_err(|_| "update-relaunch-unavailable")?;
        if !status.success() {
            return Err("update-relaunch-unavailable".into());
        }
    }
    #[cfg(target_os = "macos")]
    {
        let bundle = gui
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or("update-relaunch-path-invalid")?;
        let mut command = platform::process("open");
        if tray {
            command.arg("-g");
        }
        command.args(["-a"]).arg(bundle).arg("--args");
        if tray {
            command.arg("--tray");
        }
        let status = command
            .arg("--data-dir")
            .arg(root)
            .status()
            .map_err(|_| "update-relaunch-unavailable")?;
        if !status.success() {
            return Err("update-relaunch-unavailable".into());
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (root, gui, tray);
        return Err("gui-platform-not-supported".into());
    }
    #[cfg(any(windows, target_os = "macos"))]
    Ok(())
}

pub fn remove(root: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let service = format!(
            "{}/io.github.rsyumi.{}-gui",
            platform::launchd_user_domain()?,
            platform::instance_name(root)
        );
        platform::unix::unload_launchd(&service)?;
    }
    setting(root, Some(false)).map(|_| ())
}
