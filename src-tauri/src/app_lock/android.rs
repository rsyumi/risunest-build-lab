use super::{AuthOutcome, Capabilities, CoverPolicy, LifecycleSnapshot};
use jni::objects::{JObject, JString, JValue};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{atomic::{AtomicU64, Ordering}, Mutex, OnceLock};
use tauri::{AppHandle, Manager, WebviewWindow};
use tokio::sync::oneshot;

type Reply = oneshot::Sender<Result<Value, String>>;
static REQUESTS: OnceLock<Mutex<HashMap<u64, Reply>>> = OnceLock::new();
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

fn requests() -> &'static Mutex<HashMap<u64, Reply>> { REQUESTS.get_or_init(Mutex::default) }

struct PendingRequest(u64);
impl Drop for PendingRequest {
    fn drop(&mut self) {
        if let Ok(mut pending) = requests().lock() { pending.remove(&self.0); }
    }
}

pub(super) fn setup(_app: &AppHandle) -> Result<(), String> { Ok(()) }

fn window(app: &AppHandle) -> Result<WebviewWindow, String> {
    app.get_webview_window("main").ok_or_else(|| "app-lock-window-unavailable".to_owned())
}

async fn call<T: DeserializeOwned>(window: &WebviewWindow, operation: &'static str, payload: Value) -> Result<T, String> {
    let id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
    let (sender, receiver) = oneshot::channel();
    requests().lock().map_err(|_| "app-lock-bridge-poisoned".to_owned())?.insert(id, sender);
    let _pending = PendingRequest(id);
    let payload = payload.to_string();
    window.with_webview(move |native| {
        native.jni_handle().exec(move |env, activity, _| {
            let dispatched = (|| {
                let operation = env.new_string(operation)?;
                let payload = env.new_string(payload)?;
                env.call_method(activity, "appLockCommand", "(JLjava/lang/String;Ljava/lang/String;)V", &[
                    JValue::Long(id as i64), JValue::Object(&operation), JValue::Object(&payload),
                ])?;
                Ok::<(), jni::errors::Error>(())
            })();
            if dispatched.is_err() {
                let _ = env.exception_clear();
                if let Ok(mut pending) = requests().lock() {
                    if let Some(sender) = pending.remove(&id) {
                        let _ = sender.send(Err("app-lock-bridge-failed".to_owned()));
                    }
                }
            }
        });
    }).map_err(|_| "app-lock-dispatch-failed".to_owned())?;
    let result = receiver.await.map_err(|_| "app-lock-callback-lost".to_owned())??;
    serde_json::from_value(result).map_err(|_| "app-lock-invalid-native-reply".to_owned())
}

pub(super) async fn check(app: &AppHandle) -> Result<Capabilities, String> {
    call(&window(app)?, "check", json!({})).await
}

pub(super) async fn authenticate(window: &WebviewWindow, reason: String) -> Result<AuthOutcome, String> {
    call(window, "authenticate", json!({ "reason": reason })).await
}

pub(super) async fn set_policy(app: &AppHandle, policy: &CoverPolicy) -> Result<LifecycleSnapshot, String> {
    call(&window(app)?, "setPolicy", serde_json::to_value(policy).map_err(|_| "app-lock-invalid-policy".to_owned())?).await
}

pub(super) async fn ready(app: &AppHandle, epoch: u64, locked: bool) -> Result<bool, String> {
    call(&window(app)?, "ready", json!({ "epoch": epoch, "locked": locked })).await
}

#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn Java_io_github_rsyumi_risunest_MainActivity_appLockReply(
    mut env: jni::JNIEnv,
    _activity: JObject,
    id: jni::sys::jlong,
    response: JString,
) {
    let result = (|| {
        let text: String = env.get_string(&response).map_err(|_| "app-lock-invalid-native-reply".to_owned())?.into();
        let mut value: Value = serde_json::from_str(&text).map_err(|_| "app-lock-invalid-native-reply".to_owned())?;
        if value.get("error").is_some() { return Err("app-lock-native-failed".to_owned()); }
        value.get_mut("value").map(Value::take).ok_or_else(|| "app-lock-invalid-native-reply".to_owned())
    })();
    if let Ok(mut pending) = requests().lock() {
        if let Some(sender) = pending.remove(&(id as u64)) { let _ = sender.send(result); }
    }
}
