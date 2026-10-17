//! Bounded byte parts for CAS uploads and native inlay input.
use tauri::ipc::{InvokeBody, Request};

pub(crate) const BINARY_PART_BYTES: usize = 256 * 1024;

#[derive(serde::Serialize)]
pub(crate) struct ByteTransferError {
    code: &'static str,
    message: String,
}

impl From<String> for ByteTransferError {
    fn from(message: String) -> Self { Self { code: "transfer-failed", message } }
}

impl crate::native_log::CommandFailure for ByteTransferError {
    fn code(&self) -> std::borrow::Cow<'_, str> { self.code.into() }
    fn detail(&self) -> Option<std::borrow::Cow<'_, str>> { Some(self.message.as_str().into()) }
}

#[tauri::command(async)]
pub(crate) fn native_asset_bytes_probe(request: Request<'_>) -> bool {
    matches!(request.body(), InvokeBody::Raw(bytes) if bytes.is_empty())
}

pub(crate) fn raw_part<'a>(request: &'a Request<'_>) -> Result<(String, u64, &'a [u8]), ByteTransferError> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err(ByteTransferError { code: "raw-body-unavailable", message: "Raw byte bodies are unavailable".to_owned() });
    };
    if bytes.is_empty() || bytes.len() > BINARY_PART_BYTES {
        return Err("Invalid native byte part length".to_owned().into());
    }
    let id = request.headers().get("x-risunest-upload-id").and_then(|value| value.to_str().ok())
        .filter(|value| uuid::Uuid::parse_str(value).is_ok()).ok_or_else(|| "Invalid native byte transfer ID".to_owned())?;
    let offset = request.headers().get("x-risunest-offset").and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok()).ok_or_else(|| "Invalid native byte offset".to_owned())?;
    Ok((id.to_owned(), offset, bytes))
}

pub(crate) type AndroidReceiver = dyn Fn(u64, &[u8]) -> Result<u64, String> + Send + Sync;

fn receivers() -> &'static std::sync::Mutex<std::collections::HashMap<String, std::sync::Weak<AndroidReceiver>>> {
    static RECEIVERS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, std::sync::Weak<AndroidReceiver>>>> = std::sync::OnceLock::new();
    RECEIVERS.get_or_init(Default::default)
}

pub(crate) fn register_android_receiver(id: &str, receiver: std::sync::Arc<AndroidReceiver>) -> Result<std::sync::Arc<AndroidReceiver>, String> {
    let mut receivers = receivers().lock().map_err(|_| "Android byte receivers poisoned")?;
    receivers.retain(|_, receiver| receiver.strong_count() > 0);
    if receivers.len() >= 8 || receivers.contains_key(id) {
        return Err("Android byte receiver capacity is unavailable".to_owned());
    }
    receivers.insert(id.to_owned(), std::sync::Arc::downgrade(&receiver));
    Ok(receiver)
}

pub(crate) fn append_android(id: &str, offset: u64, bytes: &[u8]) -> Option<Result<u64, String>> {
    let receiver = match receivers().lock() {
        Ok(receivers) => receivers.get(id).and_then(std::sync::Weak::upgrade),
        Err(_) => return Some(Err("Android byte receivers poisoned".to_owned())),
    }?;
    // Receiver I/O never holds the routing lock.
    Some(receiver(offset, bytes))
}
