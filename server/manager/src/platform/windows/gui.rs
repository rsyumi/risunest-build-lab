use super::native::{self, At, Failure, NativeResult};
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::path::Path;
use windows::core::{w, Interface, BSTR, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, E_ACCESSDENIED, HWND};
use windows::Win32::System::Com::{
    CoCreateInstance, IDispatch, IServiceProvider, CLSCTX_LOCAL_SERVER,
};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{
    IShellBrowser, IShellDispatch2, IShellFolderViewDual, IShellWindows, SID_STopLevelBrowser,
    ShellWindows, SVGIO_BACKGROUND, SWC_DESKTOP, SWFO_NEEDDISPATCH,
};
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

pub(crate) fn setting(root: &Path, enabled: Option<bool>, current: &Path) -> crate::Result<bool> {
    registry_setting(root, enabled, current)
        .map_err(|error| native::diagnostic("gui-startup-operation-failed", error))
}
fn registry_setting(root: &Path, enabled: Option<bool>, current: &Path) -> NativeResult<bool> {
    let name = native::wide(&format!("{}-gui", super::instance_name(root)));
    let path = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    let mut key = HKEY::default();
    unsafe {
        let result = if enabled == Some(true) {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path,
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_QUERY_VALUE | KEY_SET_VALUE,
                None,
                &mut key,
                None,
            )
        } else {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                path,
                None,
                if enabled == Some(false) {
                    KEY_QUERY_VALUE | KEY_SET_VALUE
                } else {
                    KEY_QUERY_VALUE
                },
                &mut key,
            )
        };
        if result == ERROR_FILE_NOT_FOUND {
            return Ok(false);
        }
        result.ok().at("registry-open")?;
        let key = Key(key);
        let name = PCWSTR(name.as_ptr());
        if enabled == Some(true) {
            let command = native::wide(&format!(
                "\"{}\" --tray --data-dir \"{}\"",
                current.to_string_lossy(),
                root.to_string_lossy().trim_end_matches(['\\', '/'])
            ));
            let bytes: Vec<u8> = command
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect();
            RegSetValueExW(key.0, name, None, REG_SZ, Some(&bytes))
                .ok()
                .at("registry-write")?;
        } else if enabled == Some(false) {
            let deleted = RegDeleteValueW(key.0, name);
            if deleted != ERROR_FILE_NOT_FOUND {
                deleted.ok().at("registry-delete")?;
            }
        }
        let result = RegQueryValueExW(key.0, name, None, None, None, None);
        if result == ERROR_FILE_NOT_FOUND {
            return Ok(false);
        }
        result.ok().at("registry-read")?;
        Ok(true)
    }
}

pub(crate) fn relaunch(root: &Path, gui: &Path, tray: bool) -> crate::Result<()> {
    let arguments = format!(
        "{}--data-dir \"{}\"",
        if tray { "--tray " } else { "" },
        root.to_string_lossy().trim_end_matches(['\\', '/'])
    );
    native::on_com("update-relaunch-unavailable", || unsafe {
        // Launch through the desktop shell so updater job cleanup cannot kill the GUI.
        let shell_windows: IShellWindows =
            CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER).at("shell-connect")?;
        let mut window = 0;
        let desktop = shell_windows
            .FindWindowSW(
                &VARIANT::from(0i32),
                &VARIANT::default(),
                SWC_DESKTOP,
                &mut window,
                SWFO_NEEDDISPATCH,
            )
            .at("shell-desktop")?;
        let mut pid = 0;
        GetWindowThreadProcessId(HWND(window as isize as *mut _), Some(&mut pid));
        let process =
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).at("shell-user")?;
        let _process = OwnedHandle::from_raw_handle(process.0);
        if !native::equal(
            &native::process_sid(process)?,
            &native::current_sid()?,
            true,
        )? {
            return Err(Failure {
                stage: "shell-user",
                code: E_ACCESSDENIED,
            });
        }
        let provider: IServiceProvider = desktop.cast().at("shell-interface")?;
        let browser: IShellBrowser = provider
            .QueryService(&SID_STopLevelBrowser)
            .at("shell-browser")?;
        let view = browser.QueryActiveShellView().at("shell-view")?;
        let dispatch: IDispatch = view.GetItemObject(SVGIO_BACKGROUND).at("shell-view")?;
        let folder: IShellFolderViewDual = dispatch.cast().at("shell-interface")?;
        let shell: IShellDispatch2 = folder
            .Application()
            .at("shell-application")?
            .cast()
            .at("shell-interface")?;
        shell
            .ShellExecute(
                &BSTR::from(gui.to_string_lossy().as_ref()),
                &VARIANT::from(arguments.as_str()),
                &VARIANT::from(""),
                &VARIANT::from("open"),
                &VARIANT::from(1i32),
            )
            .at("shell-launch")
    })
}
