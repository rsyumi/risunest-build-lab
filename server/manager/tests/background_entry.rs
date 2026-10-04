use std::process::Command;

const BACKGROUND: &str = env!("CARGO_BIN_EXE_risunest-sync-manager-background");

#[test]
fn the_windowless_build_refuses_the_menu_and_user_commands() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("data");
    for command in [&[][..], &["status"], &["update", "check"]] {
        let output = Command::new(BACKGROUND)
            .arg("--data-dir")
            .arg(&root)
            .args(command)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{command:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid-command"));
    }
    assert!(!root.exists());
}

#[cfg(windows)]
fn subsystem(path: &std::path::Path) -> u16 {
    let bytes = std::fs::read(path).unwrap();
    let header = u32::from_le_bytes(bytes[0x3c..0x40].try_into().unwrap()) as usize;
    assert_eq!(&bytes[header..header + 4], b"PE\0\0");
    // The optional header follows the signature and the 20-byte file header.
    let subsystem = header + 4 + 20 + 68;
    u16::from_le_bytes(bytes[subsystem..subsystem + 2].try_into().unwrap())
}

#[cfg(windows)]
#[test]
fn scheduled_runs_use_a_windowless_build_while_the_manager_keeps_its_console() {
    const WINDOWS_GUI: u16 = 2;
    const WINDOWS_CUI: u16 = 3;
    assert_eq!(subsystem(BACKGROUND.as_ref()), WINDOWS_GUI);
    assert_eq!(
        subsystem(env!("CARGO_BIN_EXE_risunest-sync-manager").as_ref()),
        WINDOWS_CUI
    );
}
