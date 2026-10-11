#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn cleanup() -> std::io::Result<()> {
    if !std::env::args()
        .skip(1)
        .eq(["--remove-local-data", "--yes"])
    {
        return Err(std::io::Error::other("invalid cleanup arguments"));
    }
    let executable = std::env::current_exe()?;
    let root = executable
        .parent()
        .ok_or_else(|| std::io::Error::other("missing fixture directory"))?;
    if !root.join("process-checked").is_file() {
        return Err(std::io::Error::other("process check did not run"));
    }
    std::fs::write(root.join("cleanup-called"), b"called")?;
    if root.join("fail-cleanup").exists() {
        return Err(std::io::Error::other("synthetic cleanup failure"));
    }
    std::fs::remove_file(root.join("synthetic-data"))
}

fn main() {
    std::process::exit(if cleanup().is_ok() { 0 } else { 7 });
}
