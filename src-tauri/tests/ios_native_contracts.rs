#![cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]

#[allow(dead_code)]
#[path = "../../crates/tauri-plugin-ios-native/src/lib.rs"]
mod ios_native_source;
