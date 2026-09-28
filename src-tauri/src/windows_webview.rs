use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, RunEvent, WindowEvent, Wry,
};
use webview2_com::{
    Microsoft::Web::WebView2::Win32::*, NavigationCompletedEventHandler, ProcessFailedEventHandler,
};
use windows::core::Interface;

#[derive(Default)]
struct FailureState {
    main_process_exited: AtomicBool,
}

impl FailureState {
    fn record(&self, kind: COREWEBVIEW2_PROCESS_FAILED_KIND) {
        if matches!(
            kind,
            COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED
                | COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED
        ) {
            self.main_process_exited.store(true, Ordering::Relaxed);
        }
    }

    fn navigation_completed(&self, success: bool) {
        if success {
            self.main_process_exited.store(false, Ordering::Relaxed);
        }
    }

    fn needs_native_close(&self) -> bool {
        self.main_process_exited.load(Ordering::Relaxed)
    }
}

fn failure_name(kind: COREWEBVIEW2_PROCESS_FAILED_KIND) -> &'static str {
    match kind {
        COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED => "browser-exited",
        COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED => "renderer-exited",
        COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE => "renderer-unresponsive",
        COREWEBVIEW2_PROCESS_FAILED_KIND_FRAME_RENDER_PROCESS_EXITED => "subframe-exited",
        COREWEBVIEW2_PROCESS_FAILED_KIND_GPU_PROCESS_EXITED => "gpu-exited",
        COREWEBVIEW2_PROCESS_FAILED_KIND_UTILITY_PROCESS_EXITED => "utility-exited",
        _ => "other-process-exited",
    }
}

pub(crate) fn init() -> TauriPlugin<Wry> {
    let state = Arc::new(FailureState::default());
    let close_state = state.clone();
    Builder::new("windows-webview")
        .on_webview_ready(move |webview| {
            if webview.label() != "main" { return; }
            let failure_state = state.clone();
            let navigation_state = state.clone();
            if let Err(error) = webview.with_webview(move |native| {
                let install = || -> windows::core::Result<()> {
                    // WebView2 callbacks and registration run on its owning UI thread.
                    unsafe {
                        let view = native.controller().CoreWebView2()?;
                        let mut token = 0;
                        view.add_ProcessFailed(&ProcessFailedEventHandler::create(Box::new(move |_, args| {
                            let Some(args) = args else { return Ok(()); };
                            let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                            args.ProcessFailedKind(&mut kind)?;
                            failure_state.record(kind);
                            let mut reason = None;
                            let mut exit_code = None;
                            if let Ok(details) = args.cast::<ICoreWebView2ProcessFailedEventArgs2>() {
                                let mut value = COREWEBVIEW2_PROCESS_FAILED_REASON::default();
                                if details.Reason(&mut value).is_ok() { reason = Some(value.0); }
                                let mut value = 0;
                                if details.ExitCode(&mut value).is_ok() { exit_code = Some(value); }
                            }
                            crate::nlog!("error", "WebView2 process failure: kind={} ({}) reason={reason:?} exit_code={exit_code:?}", failure_name(kind), kind.0);
                            Ok(())
                        })), &mut token)?;
                        view.add_NavigationCompleted(&NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                            if let Some(args) = args {
                                let mut success = windows::core::BOOL::default();
                                args.IsSuccess(&mut success)?;
                                navigation_state.navigation_completed(success.as_bool());
                            }
                            Ok(())
                        })), &mut token)?;
                    }
                    Ok(())
                };
                if let Err(error) = install() {
                    crate::nlog!("error", "Could not install WebView2 failure diagnostics: {}", error.code().0);
                }
            }) {
                crate::nlog!("error", "Could not schedule WebView2 failure diagnostics: {error}");
            }
        })
        .on_event(move |app, event| {
            if let RunEvent::WindowEvent { label, event: WindowEvent::CloseRequested { api, .. }, .. } = event {
                if label != "main" || !close_state.needs_native_close() { return; }
                if let Some(window) = app.get_webview_window("main") {
                    // The departed renderer cannot acknowledge the normal save/close request.
                    api.prevent_close();
                    crate::nlog!("warn", "Closing window after WebView2 main process exit");
                    if let Err(error) = window.destroy() {
                        crate::nlog!("error", "Could not close failed WebView2 window: {error}");
                    }
                }
            }
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_main_process_exit_enables_native_close() {
        for kind in [
            COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE,
            COREWEBVIEW2_PROCESS_FAILED_KIND_FRAME_RENDER_PROCESS_EXITED,
            COREWEBVIEW2_PROCESS_FAILED_KIND_GPU_PROCESS_EXITED,
            COREWEBVIEW2_PROCESS_FAILED_KIND_UTILITY_PROCESS_EXITED,
            COREWEBVIEW2_PROCESS_FAILED_KIND_UNKNOWN_PROCESS_EXITED,
        ] {
            let state = FailureState::default();
            state.record(kind);
            assert!(!state.needs_native_close(), "{}", failure_name(kind));
        }
        for kind in [
            COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED,
            COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED,
        ] {
            let state = FailureState::default();
            assert!(!state.needs_native_close());
            state.record(kind);
            assert!(state.needs_native_close());
            state.record(COREWEBVIEW2_PROCESS_FAILED_KIND_GPU_PROCESS_EXITED);
            assert!(state.needs_native_close());
        }
    }

    #[test]
    fn only_a_successful_navigation_restores_normal_close() {
        let state = FailureState::default();
        state.record(COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED);
        state.navigation_completed(false);
        assert!(state.needs_native_close());
        state.navigation_completed(true);
        assert!(!state.needs_native_close());
    }
}
