use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct DeviceInfo {
    model: Option<String>,
    arch: &'static str,
}

#[tauri::command]
pub(crate) async fn diagnostic_device_info() -> DeviceInfo {
    let model = tauri::async_runtime::spawn_blocking(device_model)
        .await
        .ok()
        .flatten();
    DeviceInfo {
        model,
        arch: std::env::consts::ARCH,
    }
}

fn model_label(manufacturer: Option<String>, model: Option<String>) -> Option<String> {
    let clean = |value: String| {
        let value = value.trim_matches(|c: char| c.is_whitespace() || c == '\0');
        if value.is_empty()
            || [
                "unknown",
                "none",
                "default string",
                "system product name",
                "to be filled by o.e.m.",
                "arm64",
                "aarch64",
                "x86_64",
                "i386",
            ]
            .iter()
            .any(|placeholder| value.eq_ignore_ascii_case(placeholder))
        {
            None
        } else {
            Some(value.to_owned())
        }
    };
    let model = clean(model?)?;
    match manufacturer.and_then(clean) {
        Some(manufacturer)
            if !model
                .to_lowercase()
                .starts_with(&manufacturer.to_lowercase()) =>
        {
            Some(format!("{manufacturer} {model}"))
        }
        _ => Some(model),
    }
}

#[cfg(windows)]
fn device_model() -> Option<String> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let read = |name: &str| {
        let key: Vec<u16> = "HARDWARE\\DESCRIPTION\\System\\BIOS\0"
            .encode_utf16()
            .collect();
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let mut buffer = [0u16; 256];
        let mut bytes = std::mem::size_of_val(&buffer) as u32;
        let result = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        (result == 0).then(|| {
            let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
            String::from_utf16_lossy(&buffer[..end])
        })
    };
    model_label(read("SystemManufacturer"), read("SystemProductName"))
}

#[cfg(target_os = "android")]
fn device_model() -> Option<String> {
    let read = |name: &std::ffi::CStr| {
        let mut buffer = [0 as libc::c_char; libc::PROP_VALUE_MAX as usize];
        let length = unsafe { libc::__system_property_get(name.as_ptr(), buffer.as_mut_ptr()) };
        (length > 0).then(|| unsafe {
            std::ffi::CStr::from_ptr(buffer.as_ptr())
                .to_string_lossy()
                .into_owned()
        })
    };
    model_label(read(c"ro.product.manufacturer"), read(c"ro.product.model"))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn device_model() -> Option<String> {
    #[cfg(target_os = "macos")]
    let key = c"hw.model";
    #[cfg(target_os = "ios")]
    let key = c"hw.machine";
    let mut buffer = [0u8; 256];
    let mut length = buffer.len();
    let result = unsafe {
        libc::sysctlbyname(
            key.as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return None;
    }
    model_label(
        Some("Apple".into()),
        Some(String::from_utf8_lossy(&buffer[..length]).into_owned()),
    )
}

#[cfg(target_os = "linux")]
fn device_model() -> Option<String> {
    use std::io::Read;
    let read = |path: &str| {
        let mut value = String::new();
        std::fs::File::open(path)
            .ok()?
            .take(1024)
            .read_to_string(&mut value)
            .ok()?;
        Some(value)
    };
    model_label(
        read("/sys/class/dmi/id/sys_vendor"),
        read("/sys/class/dmi/id/product_name"),
    )
    .or_else(|| model_label(None, read("/proc/device-tree/model")))
}

#[cfg(not(any(
    windows,
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "linux"
)))]
fn device_model() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_only_available_models_without_repeating_manufacturers() {
        assert_eq!(
            model_label(Some("Samsung".into()), Some("SM-S928N".into())),
            Some("Samsung SM-S928N".into())
        );
        assert_eq!(
            model_label(Some("Dell".into()), Some("Dell Example".into())),
            Some("Dell Example".into())
        );
        assert_eq!(
            model_label(None, Some(" Mac16,1\0\n".into())),
            Some("Mac16,1".into())
        );
        assert_eq!(model_label(Some("Example".into()), None), None);
        assert_eq!(
            model_label(Some("Apple".into()), Some("arm64".into())),
            None
        );
        assert_eq!(
            model_label(Some("Example".into()), Some("System Product Name".into())),
            None
        );
    }
}
