use std::time::Duration;
use tauri::Manager;

const TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) async fn preflight(app: &tauri::AppHandle) -> Result<(), String> {
    if app.webview_windows().is_empty() {
        return Err("cleanup-webview-unavailable".into());
    }
    #[cfg(target_os = "android")]
    for webview in app.webview_windows().into_values() {
        if android_call(&webview, "cleanupWebviewSupported").await? != 1 {
            return Err("cleanup-webview-update-required".into());
        }
    }
    Ok(())
}

pub(crate) async fn clear(app: &tauri::AppHandle) -> Result<(), String> {
    preflight(app).await?;
    for webview in app.webview_windows().into_values() {
        #[cfg(target_os = "android")]
        {
            if android_call(&webview, "cleanupWebviewStart").await? < 0 {
                return Err("cleanup-webview-failed".into());
            }
            tokio::time::timeout(TIMEOUT, async {
                loop {
                    match android_call(&webview, "cleanupWebviewStatus").await? {
                        1 => return Ok(()),
                        0 => tokio::time::sleep(Duration::from_millis(100)).await,
                        _ => return Err("cleanup-webview-failed".to_owned()),
                    }
                }
            }).await.map_err(|_| "cleanup-webview-timeout".to_owned())??;
        }
        #[cfg(not(target_os = "android"))]
        {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            webview.with_webview(move |native| start(native, sender))
                .map_err(|_| "cleanup-webview-dispatch-failed".to_owned())?;
            await_completion(receiver, TIMEOUT).await?;
        }
    }
    Ok(())
}

#[cfg(any(not(target_os = "android"), test))]
async fn await_completion(
    receiver: tokio::sync::oneshot::Receiver<Result<(), String>>,
    timeout: Duration,
) -> Result<(), String> {
    tokio::time::timeout(timeout, receiver).await
        .map_err(|_| "cleanup-webview-timeout".to_owned())?
        .map_err(|_| "cleanup-webview-callback-lost".to_owned())?
}

#[cfg(windows)]
fn start(native: tauri::webview::PlatformWebview, sender: tokio::sync::oneshot::Sender<Result<(), String>>) {
    use std::sync::{Arc, Mutex};
    use windows::core::Interface;
    use webview2_com::{ClearBrowsingDataCompletedHandler, Microsoft::Web::WebView2::Win32::{ICoreWebView2_13, ICoreWebView2Profile2}};
    let sender = Arc::new(Mutex::new(Some(sender)));
    let completed = sender.clone();
    let result = unsafe {
        (|| {
            native.controller().CoreWebView2()?.cast::<ICoreWebView2_13>()?
                .Profile()?.cast::<ICoreWebView2Profile2>()?
                .ClearBrowsingDataAll(&ClearBrowsingDataCompletedHandler::create(Box::new(move |result| {
                    if let Some(sender) = completed.lock().unwrap().take() {
                        let _ = sender.send(result.map_err(|_| "cleanup-webview-failed".into()));
                    }
                    Ok(())
                })))
        })()
    };
    if result.is_err() {
        if let Some(sender) = sender.lock().unwrap().take() {
            let _ = sender.send(Err("cleanup-webview-failed".into()));
        }
    }
}

#[cfg(target_os = "linux")]
fn start(native: tauri::webview::PlatformWebview, sender: tokio::sync::oneshot::Sender<Result<(), String>>) {
    use webkit2gtk::{WebViewExt, WebContextExt, WebsiteDataManagerExtManual};
    let manager = native.inner().context().and_then(|context| context.website_data_manager());
    let Some(manager) = manager else {
        let _ = sender.send(Err("cleanup-webview-unavailable".into()));
        return;
    };
    manager.clear(webkit2gtk::WebsiteDataTypes::ALL, webkit2gtk::glib::TimeSpan::from_seconds(0),
        None::<&webkit2gtk::gio::Cancellable>, move |result| {
            let _ = sender.send(result.map_err(|_| "cleanup-webview-failed".into()));
        });
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn start(native: tauri::webview::PlatformWebview, sender: tokio::sync::oneshot::Sender<Result<(), String>>) {
    use std::ffi::c_void;
    type Sender = tokio::sync::oneshot::Sender<Result<(), String>>;
    extern "C" fn completed(context: *mut c_void, success: bool) {
        let sender = unsafe { Box::from_raw(context.cast::<Sender>()) };
        let _ = sender.send(if success { Ok(()) } else { Err("cleanup-webview-failed".into()) });
    }
    extern "C" {
        fn risunest_clear_webview(view: *mut c_void, context: *mut c_void, callback: extern "C" fn(*mut c_void, bool));
    }
    unsafe { risunest_clear_webview(native.inner(), Box::into_raw(Box::new(sender)).cast(), completed); }
}

#[cfg(target_os = "android")]
async fn android_call(webview: &tauri::WebviewWindow, method: &'static str) -> Result<i32, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    webview.with_webview(move |native| {
        native.jni_handle().exec(move |env, activity, _| {
            let result = env.call_method(activity, method, "()I", &[]).and_then(|value| value.i())
                .map_err(|_| "cleanup-webview-bridge-failed".to_owned());
            if result.is_err() { let _ = env.exception_clear(); }
            let _ = sender.send(result);
        });
    }).map_err(|_| "cleanup-webview-dispatch-failed".to_owned())?;
    tokio::time::timeout(TIMEOUT, receiver).await
        .map_err(|_| "cleanup-webview-timeout".to_owned())?
        .map_err(|_| "cleanup-webview-callback-lost".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_alone_does_not_complete_cleanup() {
        let (_sender, receiver) = tokio::sync::oneshot::channel();
        assert_eq!(await_completion(receiver, Duration::from_millis(5)).await,
            Err("cleanup-webview-timeout".into()));
    }

    #[tokio::test]
    async fn callback_failure_is_not_reported_as_success() {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        sender.send(Err("cleanup-webview-failed".into())).unwrap();
        assert_eq!(await_completion(receiver, TIMEOUT).await, Err("cleanup-webview-failed".into()));
    }

    #[tokio::test]
    async fn completed_callback_allows_cleanup_to_continue() {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        sender.send(Ok(())).unwrap();
        assert_eq!(await_completion(receiver, TIMEOUT).await, Ok(()));
    }
}
