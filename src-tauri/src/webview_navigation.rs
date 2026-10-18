use tauri::{plugin::{Builder, TauriPlugin}, Manager, Wry};

use crate::webview_navigation_policy::allows_navigation;

pub(crate) fn init() -> TauriPlugin<Wry> {
    Builder::new("webview-navigation")
        .on_navigation(|webview, url| {
            if webview.label() != "main" { return true; }
            let dev_url = if tauri::is_dev() { webview.app_handle().config().build.dev_url.as_ref() } else { None };
            // Explicit links use the opener. Unsolicited document changes only get cancelled.
            allows_navigation(url, dev_url)
        })
        .build()
}
