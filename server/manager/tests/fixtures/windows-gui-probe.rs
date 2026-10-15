#![windows_subsystem = "windows"]

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    let index = args
        .iter()
        .position(|arg| arg == "--data-dir")
        .expect("synthetic root");
    let root = PathBuf::from(&args[index + 1]);
    std::fs::write(
        root.join("gui-ready"),
        if args.iter().any(|arg| arg == "--tray") {
            "tray"
        } else {
            "normal"
        },
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline && !root.join("gui-stop").exists() {
        if root.join("gui-ping").exists() {
            std::fs::remove_file(root.join("gui-ping")).unwrap();
            std::fs::write(root.join("gui-pong"), b"alive").unwrap();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::write(root.join("gui-done"), b"done").unwrap();
}
