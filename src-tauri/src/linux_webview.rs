use std::{cell::Cell, rc::Rc};
use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Wry,
};
use webkit2gtk::{LoadEvent, WebProcessTerminationReason, WebViewExt};

fn reason_name(reason: WebProcessTerminationReason) -> &'static str {
    match reason {
        WebProcessTerminationReason::Crashed => "crashed",
        WebProcessTerminationReason::ExceededMemoryLimit => "exceeded-memory-limit",
        WebProcessTerminationReason::TerminatedByApi => "terminated-by-api",
        _ => "other",
    }
}

/// Whether a main-document load event finishes a load that did not fail. WebKitGTK also finishes
/// a load after reporting its failure.
fn load_succeeded(failed: &Cell<bool>, event: LoadEvent) -> bool {
    match event {
        LoadEvent::Started => {
            failed.set(false);
            false
        }
        LoadEvent::Finished => !failed.get(),
        _ => false,
    }
}

pub(crate) fn init() -> TauriPlugin<Wry> {
    Builder::new("linux-webview")
        .on_webview_ready(|webview| {
            if webview.label() != "main" { return; }
            let load_app = webview.app_handle().clone();
            let exit_app = load_app.clone();
            if let Err(error) = webview.with_webview(move |native| {
                let view = native.inner();
                view.connect_web_process_terminated(move |view, reason| {
                    crate::nlog!("error", "WebKitGTK web process ended: reason={}", reason_name(reason));
                    crate::renderer_recovery::renderer_exited(&exit_app);
                    view.reload();
                });
                let failed = Rc::new(Cell::new(false));
                let load_failed = failed.clone();
                view.connect_load_failed(move |_, _, _, _| {
                    load_failed.set(true);
                    false
                });
                view.connect_load_changed(move |_, event| {
                    if load_succeeded(&failed, event) {
                        crate::renderer_recovery::load_succeeded(&load_app);
                    }
                });
                view.connect_is_web_process_responsive_notify(|view| {
                    if view.is_web_process_responsive() {
                        crate::nlog!("info", "WebKitGTK web process responds again");
                    } else {
                        crate::nlog!("warn", "WebKitGTK web process is unresponsive");
                    }
                });
            }) {
                crate::nlog!("error", "Could not schedule WebKitGTK failure diagnostics: {error}");
            }
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_load_without_a_failure_succeeds() {
        let failed = Cell::new(false);
        assert!(!load_succeeded(&failed, LoadEvent::Started));
        failed.set(true);
        assert!(!load_succeeded(&failed, LoadEvent::Committed));
        assert!(!load_succeeded(&failed, LoadEvent::Finished));
        assert!(!load_succeeded(&failed, LoadEvent::Started));
        assert!(load_succeeded(&failed, LoadEvent::Finished));
    }
}
