use std::{
    fs,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Command, Stdio},
    time::Instant,
};

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let root = PathBuf::from(&args[1]);
    let flags: u32 = args[2].parse().unwrap();
    let start = Instant::now();
    fs::write(root.join("native-entered"), b"entered").unwrap();
    let output = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(root.join("child.ps1"))
        .arg("-OutputPath")
        .arg(root.join("child.log"))
        .creation_flags(flags)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    fs::write(
        root.join("native-result"),
        format!(
            "elapsed_ms={} status={}\nstdout={}\nstderr={}",
            start.elapsed().as_millis(),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
    .unwrap();
    assert!(output.status.success());
}
