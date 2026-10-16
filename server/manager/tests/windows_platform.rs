#![cfg(windows)]

use risunest_sync_manager::platform;
use std::{
    fs,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::HANDLE,
        System::{JobObjects::*, Registry::*},
    },
};

fn wait_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "synthetic marker missing: {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "requires RISUNEST_TEST_SERVER and creates only isolated current-user Scheduler fixtures"]
fn existing_task_ownership_and_update_schedule_remain_compatible() {
    use risunest_sync_manager::update::UpdatePolicy;
    use windows::core::{Interface, BSTR};
    use windows::Win32::System::{Com::*, TaskScheduler::*, Variant::VARIANT};
    let root = tempfile::tempdir().unwrap();
    let executable = PathBuf::from(
        std::env::var_os("RISUNEST_TEST_SERVER").expect("explicit synthetic server required"),
    );
    assert!(executable.is_absolute() && executable.is_file());
    let name = BSTR::from(platform::instance_name(root.path()));
    let arguments = format!("serve --data-dir \"{}\"", root.path().display());
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap();
        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe {
                    CoUninitialize();
                }
            }
        }
        let _apartment = Apartment;
        let service: ITaskService =
            CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER).unwrap();
        let empty = VARIANT::default();
        service.Connect(&empty, &empty, &empty, &empty).unwrap();
        let folder = service.GetFolder(&BSTR::from("\\")).unwrap();
        struct Cleanup(ITaskFolder, BSTR, BSTR);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                unsafe {
                    let _ = self.0.DeleteTask(&self.1, 0);
                    let _ = self.0.DeleteTask(&self.2, 0);
                }
            }
        }
        let _cleanup = Cleanup(
            folder.clone(),
            name.clone(),
            BSTR::from(format!("{}-update", name)),
        );
        // Independently recreate the released task's account-name principal and action.
        let definition = service.NewTask(0).unwrap();
        let description = BSTR::from("RisuNest user sync server");
        definition
            .RegistrationInfo()
            .unwrap()
            .SetDescription(&description)
            .unwrap();
        let principal = definition.Principal().unwrap();
        let account = BSTR::from(format!(
            "{}\\{}",
            service.ConnectedDomain().unwrap(),
            service.ConnectedUser().unwrap()
        ));
        principal.SetUserId(&account).unwrap();
        principal
            .SetLogonType(TASK_LOGON_INTERACTIVE_TOKEN)
            .unwrap();
        principal.SetRunLevel(TASK_RUNLEVEL_LUA).unwrap();
        definition
            .Settings()
            .unwrap()
            .SetEnabled(true.into())
            .unwrap();
        let trigger: ILogonTrigger = definition
            .Triggers()
            .unwrap()
            .Create(TASK_TRIGGER_LOGON)
            .unwrap()
            .cast()
            .unwrap();
        trigger.SetUserId(&account).unwrap();
        trigger.SetEnabled(true.into()).unwrap();
        let action: IExecAction = definition
            .Actions()
            .unwrap()
            .Create(TASK_ACTION_EXEC)
            .unwrap()
            .cast()
            .unwrap();
        action
            .SetPath(&BSTR::from(executable.to_string_lossy().as_ref()))
            .unwrap();
        action
            .SetArguments(&BSTR::from(arguments.as_str()))
            .unwrap();
        let register = || {
            folder
                .RegisterTaskDefinition(
                    &name,
                    &definition,
                    TASK_CREATE_OR_UPDATE.0,
                    &empty,
                    &empty,
                    TASK_LOGON_INTERACTIVE_TOKEN,
                    &empty,
                )
                .unwrap()
        };
        register();
        let state = platform::startup(root.path(), &executable, "status").unwrap();
        assert!(state.registered && state.enabled && state.action_matches);
        action
            .SetArguments(&BSTR::from(arguments.to_uppercase()))
            .unwrap();
        definition
            .RegistrationInfo()
            .unwrap()
            .SetDescription(&BSTR::from("RISUNEST USER SYNC SERVER"))
            .unwrap();
        register();
        assert!(
            platform::startup(root.path(), &executable, "status")
                .unwrap()
                .action_matches
        );
        action
            .SetArguments(&BSTR::from("serve --data-dir wrong-root"))
            .unwrap();
        register();
        assert!(
            !platform::startup(root.path(), &executable, "status")
                .unwrap()
                .action_matches
        );
        assert!(
            platform::startup(root.path(), &executable, "install")
                .unwrap()
                .action_matches
        );
        definition
            .RegistrationInfo()
            .unwrap()
            .SetDescription(&BSTR::from("Foreign fixture"))
            .unwrap();
        register();
        assert!(platform::startup(root.path(), &executable, "remove")
            .unwrap_err()
            .contains(":ownership:"));
        assert!(folder.GetTask(&name).is_ok());
        definition
            .RegistrationInfo()
            .unwrap()
            .SetDescription(&description)
            .unwrap();
        let extra: IExecAction = definition
            .Actions()
            .unwrap()
            .Create(TASK_ACTION_EXEC)
            .unwrap()
            .cast()
            .unwrap();
        extra
            .SetPath(&BSTR::from(executable.to_string_lossy().as_ref()))
            .unwrap();
        register();
        assert!(platform::startup(root.path(), &executable, "install")
            .unwrap_err()
            .contains(":ownership:"));
        folder.DeleteTask(&name, 0).unwrap();
        assert!(
            !platform::startup(root.path(), &executable, "remove")
                .unwrap()
                .registered
        );
        let manager = root.path().join("manager.exe");
        // Registration is tested without an executable that could perform an update.
        fs::write(
            platform::background_manager_executable(&manager),
            b"synthetic fixture",
        )
        .unwrap();
        let state = platform::update_schedule(
            root.path(),
            &manager,
            &executable,
            UpdatePolicy::Automatic,
            "install",
        )
        .unwrap();
        assert!(state.registered && state.enabled && state.action_matches);
        let state = platform::update_schedule(
            root.path(),
            &manager,
            &executable,
            UpdatePolicy::Off,
            "install",
        )
        .unwrap();
        assert!(!state.registered && !state.enabled);
    }
}

#[test]
#[ignore = "writes and removes only an isolated current-user Run value"]
fn gui_autostart_preserves_the_command_and_existing_value_semantics() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("space 한글");
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = platform::gui::setting(&self.0, Some(false));
        }
    }
    let _cleanup = Cleanup(root.clone());
    assert!(!platform::gui::setting(&root, None).unwrap());
    assert!(platform::gui::setting(&root, Some(true)).unwrap());
    let name: Vec<_> = format!("{}-gui", platform::instance_name(&root))
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe {
        let mut length = 0;
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut length),
        )
        .ok()
        .unwrap();
        let mut buffer = vec![0u16; length as usize / 2];
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut length),
        )
        .ok()
        .unwrap();
        buffer.truncate(length as usize / 2);
        assert_eq!(buffer.pop(), Some(0));
        assert_eq!(
            String::from_utf16(&buffer).unwrap(),
            format!(
                "\"{}\" --tray --data-dir \"{}\"",
                std::env::current_exe().unwrap().display(),
                root.display()
            )
        );
    }
    assert!(platform::gui::setting(&root, None).unwrap());
    assert!(!platform::gui::setting(&root, Some(false)).unwrap());
    assert!(!platform::gui::setting(&root, Some(false)).unwrap());
}

#[test]
#[ignore = "internal subprocess for the isolated GUI lifetime test"]
fn gui_relaunch_worker() {
    let Some(root) = std::env::var_os("RISUNEST_GUI_PROBE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let probe = PathBuf::from(std::env::var_os("RISUNEST_TEST_GUI_PROBE").unwrap());
    fs::write(root.join("worker-ready"), b"ready").unwrap();
    wait_file(&root.join("worker-go"));
    platform::gui::relaunch(&root, &probe, true).unwrap();
    fs::write(root.join("worker-launched"), b"launched").unwrap();
    wait_file(&root.join("worker-stop"));
}

#[test]
#[ignore = "requires the interactive desktop and RISUNEST_TEST_GUI_PROBE, compiled from tests/fixtures/windows-gui-probe.rs"]
fn gui_relaunch_survives_the_launchers_job_without_powershell() {
    let probe = PathBuf::from(
        std::env::var_os("RISUNEST_TEST_GUI_PROBE").expect("explicit synthetic GUI probe required"),
    );
    assert!(probe.is_absolute() && probe.is_file());
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("gui space 한글");
    fs::create_dir(&root).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::write(self.0.join("gui-stop"), b"stop");
        }
    }
    let _cleanup = Cleanup(root.clone());
    unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null()).unwrap();
        let _job = OwnedHandle::from_raw_handle(job.0);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as _,
            size_of_val(&limits) as u32,
        )
        .unwrap();
        let mut child = platform::process(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "gui_relaunch_worker", "--nocapture"])
            .env("PATH", "")
            .env("RISUNEST_GUI_PROBE_ROOT", &root)
            .spawn()
            .unwrap();
        AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())).unwrap();
        wait_file(&root.join("worker-ready"));
        fs::write(root.join("worker-go"), b"go").unwrap();
        wait_file(&root.join("worker-launched"));
        wait_file(&root.join("gui-ready"));
        assert_eq!(fs::read(root.join("gui-ready")).unwrap(), b"tray");
        TerminateJobObject(job, 73).unwrap();
        assert!(!child.wait().unwrap().success());
        fs::write(root.join("gui-ping"), b"ping").unwrap();
        wait_file(&root.join("gui-pong"));
        fs::write(root.join("gui-stop"), b"stop").unwrap();
        wait_file(&root.join("gui-done"));
    }
}
