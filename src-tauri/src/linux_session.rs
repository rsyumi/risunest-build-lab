//! Linux session end. The desktop portal's logout query and SIGTERM both ask the main document to
//! save local data; neither blocks nor vetoes the session end.
use std::cell::{Cell, RefCell};
use std::time::Instant;
use webkit2gtk::{gio, glib::{self, prelude::*}};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const INHIBIT: &str = "org.freedesktop.portal.Inhibit";
const MONITOR_TOKEN: &str = "risunest_session_end";
/// The session asks applications before it logs out, restarts or shuts down.
const QUERY_END: u32 = 2;

thread_local! {
    // Keeps the connection, and with it the monitor's signal subscriptions, for the app's lifetime.
    static SESSION_BUS: RefCell<Option<gio::DBusConnection>> = const { RefCell::new(None) };
}

/// Runs on the GTK main thread, where the portal signals and SIGTERM are delivered.
pub(crate) fn install(app: &tauri::AppHandle) {
    install_sigterm(app.clone());
    let app = app.clone();
    gio::bus_get(gio::BusType::Session, None::<&gio::Cancellable>, move |result| match result {
        Ok(bus) => monitor(app, bus),
        Err(error) => crate::nlog!("warn", "Session-end monitor unavailable: {error}"),
    });
}

fn install_sigterm(app: tauri::AppHandle) {
    let terminating = Cell::new(false);
    glib::unix_signal_add_local(libc::SIGTERM, move || {
        let deadline = if terminating.replace(true) {
            None
        } else {
            crate::desktop_session::request_flush(&app, true)
        };
        match deadline {
            Some(deadline) => {
                let app = app.clone();
                glib::timeout_add_local_once(deadline.saturating_duration_since(Instant::now()), move || app.exit(0));
            }
            None => app.exit(0),
        }
        glib::ControlFlow::Continue
    });
}

fn monitor(app: tauri::AppHandle, bus: gio::DBusConnection) {
    let Some(sender) = bus.unique_name() else { return; };
    let request_path = format!(
        "{PORTAL_PATH}/request/{}/{MONITOR_TOKEN}",
        sender.trim_start_matches(':').replace('.', "_")
    );
    bus.signal_subscribe(Some(PORTAL), Some("org.freedesktop.portal.Request"), Some("Response"),
        Some(&request_path), None, gio::DBusSignalFlags::NONE, |_, _, _, _, _, parameters| {
            if parameters.type_().as_str() != "(ua{sv})" { return; }
            let response = parameters.child_value(0).get::<u32>();
            if response != Some(0) {
                crate::nlog!("warn", "Session-end monitor refused by the desktop portal: {response:?}");
            }
        });
    bus.signal_subscribe(Some(PORTAL), Some(INHIBIT), Some("StateChanged"), Some(PORTAL_PATH), None,
        gio::DBusSignalFlags::NONE, move |bus, _, _, _, _, parameters| {
            if parameters.type_().as_str() != "(oa{sv})" { return; }
            let state = glib::VariantDict::new(Some(&parameters.child_value(1)));
            if state.lookup::<u32>("session-state").ok().flatten() != Some(QUERY_END) { return; }
            crate::desktop_session::request_flush(&app, false);
            // The portal expects an answer within a second, so the document saves while logout goes on.
            bus.call(Some(PORTAL), PORTAL_PATH, INHIBIT, "QueryEndResponse",
                Some(&glib::Variant::tuple_from_iter([parameters.child_value(0)])), None,
                gio::DBusCallFlags::NONE, -1, None::<&gio::Cancellable>, |result| {
                    if let Err(error) = result {
                        crate::nlog!("warn", "Session-end response failed: {error}");
                    }
                });
        });
    let options = glib::VariantDict::new(None);
    options.insert("handle_token", MONITOR_TOKEN);
    options.insert("session_handle_token", MONITOR_TOKEN);
    bus.call(Some(PORTAL), PORTAL_PATH, INHIBIT, "CreateMonitor",
        Some(&glib::Variant::tuple_from_iter(["".to_variant(), options.end()])), None,
        gio::DBusCallFlags::NONE, 5000, None::<&gio::Cancellable>, |result| {
            if let Err(error) = result {
                crate::nlog!("warn", "Session-end monitor unavailable: {error}");
            }
        });
    SESSION_BUS.with(|slot| slot.replace(Some(bus)));
}
