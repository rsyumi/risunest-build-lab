//! Main-document renderer exits and unanswered close requests, shared by the engine adapters.
//! Each adapter reports an exit and reloads; this module decides what the reload means and
//! when a desktop window has to be closed without the document's consent.
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::Manager;

/// A second exit this soon after a reload makes the next document open through the startup
/// choice, so a document that keeps failing cannot reload forever.
const RELOAD_LOOP_WINDOW: Duration = Duration::from_secs(60);
/// How long a close request may go unacknowledged before a repeated close destroys the window.
#[cfg(any(windows, target_os = "linux"))]
const CLOSE_ACK_LIMIT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
enum Exit {
    First,
    Repeated,
}

#[derive(Default)]
struct Recovery {
    /// No document can answer a close request until a load succeeds again.
    #[cfg(any(windows, target_os = "linux"))]
    document_gone: bool,
    last_reload: Option<Instant>,
    /// The next document reports that it replaced one that stopped.
    recovered: bool,
    #[cfg(any(windows, target_os = "linux"))]
    unacknowledged_close: Option<Instant>,
}

impl Recovery {
    fn renderer_exited(&mut self, now: Instant) -> Exit {
        let repeated = self
            .last_reload
            .is_some_and(|reload| now.duration_since(reload) < RELOAD_LOOP_WINDOW);
        #[cfg(any(windows, target_os = "linux"))]
        {
            self.document_gone = true;
            self.unacknowledged_close = None;
        }
        self.recovered = true;
        self.last_reload = Some(now);
        if repeated { Exit::Repeated } else { Exit::First }
    }

    #[cfg(windows)]
    fn engine_closed(&mut self) {
        self.document_gone = true;
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn load_succeeded(&mut self) {
        self.document_gone = false;
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn document_started(&mut self) {
        self.unacknowledged_close = None;
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn acknowledge_close(&mut self) {
        self.unacknowledged_close = None;
    }

    /// Whether this close request must destroy the window natively.
    #[cfg(any(windows, target_os = "linux"))]
    fn close_requested(&mut self, now: Instant) -> bool {
        if self.document_gone {
            return true;
        }
        match self.unacknowledged_close {
            Some(since) => now.duration_since(since) >= CLOSE_ACK_LIMIT,
            None => {
                self.unacknowledged_close = Some(now);
                false
            }
        }
    }

    fn take_recovered(&mut self) -> bool {
        std::mem::take(&mut self.recovered)
    }
}

#[derive(Default)]
pub(crate) struct RendererRecovery(Mutex<Recovery>);

impl RendererRecovery {
    fn with<T>(&self, change: impl FnOnce(&mut Recovery) -> T) -> T {
        change(&mut self.0.lock().unwrap_or_else(|error| error.into_inner()))
    }
}

/// Records a main-document renderer exit before the adapter reloads it. A repeated exit records
/// an unfinished start, so the reloaded document opens through the startup choice.
pub(crate) fn renderer_exited(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<RendererRecovery>() else { return; };
    if state.with(|recovery| recovery.renderer_exited(Instant::now())) == Exit::Repeated {
        crate::nlog!("warn", "Renderer stopped again soon after a reload; the next start offers the startup choice");
        let version = app.package_info().version.to_string();
        let recorded = crate::boot_marker_root(app).and_then(|root| {
            crate::boot_marker::record_interrupted(&root, &version, crate::boot_marker_now())
                .map_err(|error| error.to_string())
        });
        if let Err(error) = recorded {
            crate::nlog!("warn", "Could not record the interrupted start: {error}");
        }
    }
}

/// The engine closed the document and cannot reload it.
#[cfg(windows)]
pub(crate) fn engine_closed(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<RendererRecovery>() {
        state.with(Recovery::engine_closed);
    }
}

#[cfg(any(windows, target_os = "linux"))]
pub(crate) fn load_succeeded(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<RendererRecovery>() {
        state.with(Recovery::load_succeeded);
    }
}

/// A new main document replaced any request the previous one left unanswered.
#[cfg(any(windows, target_os = "linux"))]
pub(crate) fn document_started(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<RendererRecovery>() {
        state.with(Recovery::document_started);
    }
}

/// Destroys the main window when no live document can settle its close request.
#[cfg(any(windows, target_os = "linux"))]
pub(crate) fn on_main_close_requested(app: &tauri::AppHandle, api: &tauri::CloseRequestApi) {
    let Some(state) = app.try_state::<RendererRecovery>() else { return; };
    if !state.with(|recovery| recovery.close_requested(Instant::now())) { return; }
    if let Some(window) = app.get_webview_window("main") {
        api.prevent_close();
        crate::nlog!("warn", "Closing the window because the renderer cannot answer the close request");
        if let Err(error) = window.destroy() {
            crate::nlog!("error", "Could not close the window natively: {error}");
        }
    }
}

// Blocking on purpose: it runs on the main thread, after the close request it answers was recorded.
#[cfg(any(windows, target_os = "linux"))]
#[tauri::command]
pub(crate) fn desktop_close_ack(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, RendererRecovery>,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Close requests belong to the main window".into());
    }
    state.with(Recovery::acknowledge_close);
    Ok(())
}

/// Whether this document replaced one whose renderer stopped. Answers once.
#[tauri::command]
pub(crate) fn renderer_recovery_take(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, RendererRecovery>,
) -> Result<bool, String> {
    if window.label() != "main" {
        return Err("Renderer recovery belongs to the main window".into());
    }
    Ok(state.with(Recovery::take_recovered))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_exit_soon_after_a_reload_counts_as_repeated() {
        let start = Instant::now();
        let mut recovery = Recovery::default();
        assert_eq!(recovery.renderer_exited(start), Exit::First);
        assert_eq!(recovery.renderer_exited(start + Duration::from_secs(59)), Exit::Repeated);
        assert_eq!(
            recovery.renderer_exited(start + Duration::from_secs(59) + RELOAD_LOOP_WINDOW),
            Exit::First
        );
    }

    #[test]
    fn the_recovery_notice_is_reported_once_per_exit() {
        let mut recovery = Recovery::default();
        assert!(!recovery.take_recovered());
        recovery.renderer_exited(Instant::now());
        assert!(recovery.take_recovered());
        assert!(!recovery.take_recovered());
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn a_departed_document_is_closed_natively_until_a_load_succeeds() {
        let now = Instant::now();
        let mut recovery = Recovery::default();
        recovery.renderer_exited(now);
        assert!(recovery.close_requested(now));
        recovery.load_succeeded();
        assert!(!recovery.close_requested(now));
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn a_repeated_close_without_acknowledgement_is_closed_natively() {
        let start = Instant::now();
        let mut recovery = Recovery::default();
        assert!(!recovery.close_requested(start));
        assert!(!recovery.close_requested(start + CLOSE_ACK_LIMIT - Duration::from_millis(1)));
        assert!(recovery.close_requested(start + CLOSE_ACK_LIMIT));
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn an_acknowledged_or_replaced_document_starts_the_wait_again() {
        let start = Instant::now();
        let late = start + CLOSE_ACK_LIMIT * 2;
        let mut recovery = Recovery::default();
        assert!(!recovery.close_requested(start));
        recovery.acknowledge_close();
        assert!(!recovery.close_requested(late));
        recovery.document_started();
        assert!(!recovery.close_requested(late + CLOSE_ACK_LIMIT));
        assert!(recovery.close_requested(late + CLOSE_ACK_LIMIT * 2));
    }
}
