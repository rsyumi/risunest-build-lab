use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Wry,
};
use webview2_com::{
    Microsoft::Web::WebView2::Win32::*, NavigationCompletedEventHandler, ProcessFailedEventHandler,
};
use windows::core::Interface;

#[derive(Debug, PartialEq, Eq)]
enum Response {
    /// The main document is gone, and the engine can load it again.
    Reload,
    /// The engine closed its controls, so only the native close remains.
    NativeClose,
    Log,
}

fn response(kind: COREWEBVIEW2_PROCESS_FAILED_KIND) -> Response {
    match kind {
        COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED => Response::Reload,
        COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED => Response::NativeClose,
        _ => Response::Log,
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
    Builder::new("windows-webview")
        .on_webview_ready(|webview| {
            if webview.label() != "main" { return; }
            let failure_app = webview.app_handle().clone();
            let navigation_app = failure_app.clone();
            if let Err(error) = webview.with_webview(move |native| {
                let install = || -> windows::core::Result<()> {
                    // WebView2 callbacks and registration run on its owning UI thread.
                    unsafe {
                        let view = native.controller().CoreWebView2()?;
                        let mut token = 0;
                        view.add_ProcessFailed(&ProcessFailedEventHandler::create(Box::new(move |sender, args| {
                            let Some(args) = args else { return Ok(()); };
                            let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                            args.ProcessFailedKind(&mut kind)?;
                            let mut reason = None;
                            let mut exit_code = None;
                            if let Ok(details) = args.cast::<ICoreWebView2ProcessFailedEventArgs2>() {
                                let mut value = COREWEBVIEW2_PROCESS_FAILED_REASON::default();
                                if details.Reason(&mut value).is_ok() { reason = Some(value.0); }
                                let mut value = 0;
                                if details.ExitCode(&mut value).is_ok() { exit_code = Some(value); }
                            }
                            crate::nlog!("error", "WebView2 process failure: kind={} ({}) reason={reason:?} exit_code={exit_code:?}", failure_name(kind), kind.0);
                            match response(kind) {
                                Response::Reload => {
                                    crate::renderer_recovery::renderer_exited(&failure_app);
                                    if let Some(Err(error)) = sender.map(|view| view.Reload()) {
                                        crate::nlog!("error", "Could not reload after the WebView2 renderer exit: {}", error.code().0);
                                    }
                                }
                                Response::NativeClose => crate::renderer_recovery::engine_closed(&failure_app),
                                Response::Log => {}
                            }
                            Ok(())
                        })), &mut token)?;
                        view.add_NavigationCompleted(&NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                            if let Some(args) = args {
                                let mut success = windows::core::BOOL::default();
                                args.IsSuccess(&mut success)?;
                                if success.as_bool() {
                                    crate::renderer_recovery::load_succeeded(&navigation_app);
                                }
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
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_main_process_exits_change_the_document() {
        for kind in [
            COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE,
            COREWEBVIEW2_PROCESS_FAILED_KIND_FRAME_RENDER_PROCESS_EXITED,
            COREWEBVIEW2_PROCESS_FAILED_KIND_GPU_PROCESS_EXITED,
            COREWEBVIEW2_PROCESS_FAILED_KIND_UTILITY_PROCESS_EXITED,
            COREWEBVIEW2_PROCESS_FAILED_KIND_UNKNOWN_PROCESS_EXITED,
        ] {
            assert_eq!(response(kind), Response::Log, "{}", failure_name(kind));
        }
        assert_eq!(response(COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED), Response::Reload);
        assert_eq!(response(COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED), Response::NativeClose);
    }
}
