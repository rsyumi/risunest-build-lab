#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

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

mod win32 {
    use std::ffi::c_void;

    pub type Handle = *mut c_void;
    pub type Procedure = unsafe extern "system" fn(Handle, u32, usize, isize) -> isize;

    #[repr(C)]
    pub struct WindowClass {
        pub style: u32,
        pub procedure: Option<Procedure>,
        pub class_extra: i32,
        pub window_extra: i32,
        pub instance: Handle,
        pub icon: Handle,
        pub cursor: Handle,
        pub background: Handle,
        pub menu_name: *const u16,
        pub class_name: *const u16,
    }

    #[repr(C)]
    pub struct Message {
        pub window: Handle,
        pub message: u32,
        pub wparam: usize,
        pub lparam: isize,
        pub time: u32,
        pub point: [i32; 2],
    }

    #[link(name = "user32")]
    extern "system" {
        pub fn RegisterWindowMessageW(name: *const u16) -> u32;
        pub fn RegisterClassW(class: *const WindowClass) -> u16;
        pub fn CreateWindowExW(
            extended_style: u32,
            class_name: *const u16,
            title: *const u16,
            style: u32,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            parent: Handle,
            menu: Handle,
            instance: Handle,
            parameter: *mut c_void,
        ) -> Handle;
        pub fn DefWindowProcW(window: Handle, message: u32, wparam: usize, lparam: isize) -> isize;
        pub fn GetMessageW(message: *mut Message, window: Handle, first: u32, last: u32) -> i32;
        pub fn DispatchMessageW(message: *const Message) -> isize;
        pub fn PostQuitMessage(code: i32);
        pub fn CharLowerBuffW(text: *mut u16, length: u32) -> u32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn CreateMutexW(attributes: *const c_void, initial_owner: i32, name: *const u16) -> Handle;
        pub fn GetModuleHandleW(name: *const u16) -> Handle;
    }
}

static EXIT_REQUEST: AtomicU32 = AtomicU32::new(0);

unsafe extern "system" fn procedure(window: win32::Handle, message: u32, wparam: usize, lparam: isize) -> isize {
    if message != 0 && message == EXIT_REQUEST.load(Ordering::Relaxed) {
        unsafe { win32::PostQuitMessage(0) };
        return 0;
    }
    unsafe { win32::DefWindowProcW(window, message, wparam, lparam) }
}

fn record(root: &Path, event: &str) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("events"))?
        .write_all(format!("{event}\n").as_bytes())
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Stands in for a running app: registers the exit request names the app
/// builds in windows_session.rs, then takes a moment to close once asked.
fn serve(identifier: &str) -> std::io::Result<()> {
    let executable = std::env::current_exe()?;
    let root = executable
        .parent()
        .ok_or_else(|| std::io::Error::other("missing fixture directory"))?;
    let path = executable
        .to_str()
        .ok_or_else(|| std::io::Error::other("executable path is not text"))?;
    let mut lowercase: Vec<u16> = path.encode_utf16().collect();
    unsafe { win32::CharLowerBuffW(lowercase.as_mut_ptr(), lowercase.len() as u32) };
    let lowercase = String::from_utf16(&lowercase)
        .map_err(std::io::Error::other)?
        .replace('\\', "/");
    unsafe {
        let message = win32::RegisterWindowMessageW(wide(&format!("{identifier}.exit-request|{path}")).as_ptr());
        let mutex = win32::CreateMutexW(
            std::ptr::null(),
            0,
            wide(&format!("Local\\{identifier}.exit-request|{lowercase}")).as_ptr(),
        );
        if message == 0 || mutex.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        EXIT_REQUEST.store(message, Ordering::Relaxed);
        let class_name = wide("SyntheticRisuNest");
        let class = win32::WindowClass {
            style: 0,
            procedure: Some(procedure),
            class_extra: 0,
            window_extra: 0,
            instance: win32::GetModuleHandleW(std::ptr::null()),
            icon: std::ptr::null_mut(),
            cursor: std::ptr::null_mut(),
            background: std::ptr::null_mut(),
            menu_name: std::ptr::null(),
            class_name: class_name.as_ptr(),
        };
        if win32::RegisterClassW(&class) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // A hidden top-level window, which a broadcast reaches.
        let window = win32::CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            class.instance,
            std::ptr::null_mut(),
        );
        if window.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        std::fs::write(root.join("ready"), b"ready")?;
        let mut message = std::mem::zeroed::<win32::Message>();
        while win32::GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            win32::DispatchMessageW(&message);
        }
    }
    record(root, "exit-requested")?;
    // Saving and sync take a moment; the installer must wait for the exit.
    std::thread::sleep(std::time::Duration::from_secs(1));
    record(root, "exited")
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = match args.as_slice() {
        [option, identifier] if option == "--serve-exit-request" => serve(identifier),
        _ => cleanup(),
    };
    std::process::exit(if result.is_ok() { 0 } else { 7 });
}
