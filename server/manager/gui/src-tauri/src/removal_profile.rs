use std::time::Duration;

async fn await_completion(
    receiver: tokio::sync::oneshot::Receiver<Result<(), String>>,
    timeout: Duration,
) -> Result<(), String> {
    tokio::time::timeout(timeout, receiver)
        .await
        .map_err(|_| "removal-profile-timeout".to_owned())?
        .map_err(|_| "removal-profile-callback-lost".to_owned())?
}

#[cfg(target_os = "macos")]
pub(crate) fn run() {
    let mut context = tauri::generate_context!();
    context.config_mut().app.windows.clear();
    let result = tauri::Builder::default()
        .setup(|app| {
            let window = tauri::WebviewWindowBuilder::new(
                app,
                "removal-profile",
                tauri::WebviewUrl::External("about:blank".parse()?),
            )
            .visible(false)
            .build()?;
            let app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let (sender, receiver) = tokio::sync::oneshot::channel();
                let result = match window.with_webview(move |native| start(native, sender)) {
                    Ok(()) => await_completion(receiver, Duration::from_secs(60)).await,
                    Err(_) => Err("removal-profile-dispatch-failed".into()),
                };
                if let Err(error) = &result { eprintln!("{error}"); }
                app.exit(if result.is_ok() { 0 } else { 1 });
            });
            Ok(())
        })
        .run(context);
    if let Err(error) = result {
        eprintln!("removal-profile-unavailable: {error}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "macos")]
fn start(
    native: tauri::webview::PlatformWebview,
    sender: tokio::sync::oneshot::Sender<Result<(), String>>,
) {
    use std::ffi::c_void;
    type Sender = tokio::sync::oneshot::Sender<Result<(), String>>;
    extern "C" fn completed(context: *mut c_void, success: bool) {
        let sender = unsafe { Box::from_raw(context.cast::<Sender>()) };
        let _ = sender.send(if success { Ok(()) } else { Err("removal-profile-failed".into()) });
    }
    extern "C" {
        fn risunest_sync_clear_removal_profile(
            view: *mut c_void,
            context: *mut c_void,
            callback: extern "C" fn(*mut c_void, bool),
        );
    }
    unsafe {
        risunest_sync_clear_removal_profile(
            native.inner(), Box::into_raw(Box::new(sender)).cast(), completed,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn dispatch_without_completion_cannot_report_success() {
        let (_sender, receiver) = tokio::sync::oneshot::channel();
        assert_eq!(await_completion(receiver, Duration::from_millis(5)).await,
            Err("removal-profile-timeout".into()));
    }
    #[tokio::test]
    async fn callback_failure_prevents_removal() {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        sender.send(Err("removal-profile-failed".into())).unwrap();
        assert_eq!(await_completion(receiver, Duration::from_secs(1)).await,
            Err("removal-profile-failed".into()));
    }
    #[tokio::test]
    async fn completed_callback_allows_removal() {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        sender.send(Ok(())).unwrap();
        assert_eq!(await_completion(receiver, Duration::from_secs(1)).await, Ok(()));
    }
}
