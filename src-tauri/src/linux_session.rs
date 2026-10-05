//! Linux session end. The desktop portal's logout query and SIGTERM both ask the main document to
//! save local data; neither blocks nor vetoes the session end.
use std::cell::RefCell;
use std::time::{Duration, Instant};
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

#[derive(Default)]
struct Termination {
    requested: bool,
}

impl Termination {
    /// How long a SIGTERM waits before the app exits. The first one waits for the local flush it
    /// requests, until that flush's deadline; a later one, or one without a flush, exits at once.
    fn signal(&mut self, request_flush: impl FnOnce() -> Option<Instant>, now: Instant) -> Option<Duration> {
        if std::mem::replace(&mut self.requested, true) {
            return None;
        }
        request_flush().map(|deadline| deadline.saturating_duration_since(now))
    }
}

fn install_sigterm(app: tauri::AppHandle) {
    let mut termination = Termination::default();
    glib::unix_signal_add_local(libc::SIGTERM, move || {
        let wait = termination.signal(|| crate::desktop_session::request_flush(&app, true), Instant::now());
        match wait {
            Some(wait) => {
                let app = app.clone();
                glib::timeout_add_local_once(wait, move || app.exit(0));
            }
            None => app.exit(0),
        }
        glib::ControlFlow::Continue
    });
}

/// The object path of the portal request that `handle_token` names on this connection.
fn request_path(unique_name: &str, handle_token: &str) -> String {
    format!("{PORTAL_PATH}/request/{}/{handle_token}", unique_name.trim_start_matches(':').replace('.', "_"))
}

/// Whether an Inhibit `StateChanged` signal asks applications before the session ends.
fn is_query_end(parameters: &glib::Variant) -> bool {
    if parameters.type_().as_str() != "(oa{sv})" { return false; }
    let state = glib::VariantDict::new(Some(&parameters.child_value(1)));
    state.lookup::<u32>("session-state").ok().flatten() == Some(QUERY_END)
}

fn monitor(app: tauri::AppHandle, bus: gio::DBusConnection) {
    let Some(sender) = bus.unique_name() else { return; };
    let request_path = request_path(&sender, MONITOR_TOKEN);
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
            if !is_query_end(parameters) { return; }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn state_changed(session_state: u32) -> glib::Variant {
        let state = glib::VariantDict::new(None);
        state.insert("screensaver-active", false);
        state.insert("session-state", session_state);
        let session = glib::variant::ObjectPath::try_from("/org/freedesktop/portal/desktop/session/1_42/risunest_session_end")
            .unwrap();
        glib::Variant::tuple_from_iter([session.to_variant(), state.end()])
    }

    #[test]
    fn the_monitor_listens_on_the_request_path_of_its_own_connection() {
        assert_eq!(
            request_path(":1.42", MONITOR_TOKEN),
            "/org/freedesktop/portal/desktop/request/1_42/risunest_session_end",
        );
    }

    #[test]
    fn only_a_query_end_state_asks_for_a_flush() {
        assert!(is_query_end(&state_changed(QUERY_END)));
        assert!(!is_query_end(&state_changed(1)));
        assert!(!is_query_end(&state_changed(3)));
        let without_state = glib::Variant::tuple_from_iter([
            glib::variant::ObjectPath::try_from("/session").unwrap().to_variant(),
            glib::VariantDict::new(None).end(),
        ]);
        assert!(!is_query_end(&without_state));
        assert!(!is_query_end(&(QUERY_END, "session").to_variant()));
    }

    #[test]
    fn the_first_sigterm_waits_for_its_flush_and_a_second_exits_at_once() {
        let now = Instant::now();
        let mut termination = Termination::default();
        assert_eq!(termination.signal(|| Some(now + Duration::from_secs(2)), now), Some(Duration::from_secs(2)));
        assert_eq!(termination.signal(|| unreachable!(), now), None);
    }

    #[test]
    fn a_sigterm_without_a_flush_or_past_its_deadline_exits_at_once() {
        let now = Instant::now();
        assert_eq!(Termination::default().signal(|| None, now), None);
        assert_eq!(Termination::default().signal(|| Some(now), now + Duration::from_secs(1)), Some(Duration::ZERO));
    }
}
