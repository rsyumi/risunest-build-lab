// Task Scheduler starts this build, so it must not allocate a console window.
#![cfg_attr(windows, windows_subsystem = "windows")]

use risunest_sync_manager::cli::{self, Entry};

#[tokio::main]
async fn main() {
    cli::main(Entry::Background).await;
}
