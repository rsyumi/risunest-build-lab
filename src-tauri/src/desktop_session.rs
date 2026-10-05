//! A local flush the OS session end asks the main document for, without sync or dialogs.
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::Manager;

const SETTLE_LIMIT: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(crate) struct PendingFlush {
    token: Option<String>,
    deadline: Option<Instant>,
    exit_when_settled: bool,
    acknowledged: bool,
}

impl PendingFlush {
    fn request(&mut self, now: Instant, exit_when_settled: bool) -> (String, Instant) {
        self.exit_when_settled |= exit_when_settled;
        let deadline = *self.deadline.get_or_insert(now + SETTLE_LIMIT);
        let token = self.token.get_or_insert_with(|| uuid::Uuid::new_v4().to_string()).clone();
        (token, if self.acknowledged { now.min(deadline) } else { deadline })
    }
    /// Whether the app exits now that the matching flush finished.
    fn acknowledge(&mut self, token: &str) -> bool {
        if self.token.as_deref() != Some(token) || self.acknowledged { return false; }
        let exit = self.exit_when_settled;
        self.acknowledged = true;
        exit
    }
    #[cfg(any(windows, test))]
    pub(crate) fn waiting(&self, now: Instant) -> bool {
        !self.acknowledged && self.token.is_some() && self.deadline.is_some_and(|deadline| now < deadline)
    }
    pub(crate) fn clear(&mut self) {
        self.token = None;
        self.deadline = None;
        self.exit_when_settled = false;
        self.acknowledged = false;
    }
}

#[derive(Default)]
pub(crate) struct SessionState(pub(crate) Mutex<PendingFlush>);

/// Asks the main document to save local data and returns when the wait for its answer ends.
/// With `exit_when_settled`, the answer exits the app.
pub(crate) fn request_flush(app: &tauri::AppHandle, exit_when_settled: bool) -> Option<Instant> {
    let state = app.try_state::<SessionState>()?;
    let (token, deadline) = state.0.lock().unwrap_or_else(|error| error.into_inner())
        .request(Instant::now(), exit_when_settled);
    if let Some(window) = app.get_webview_window("main") {
        let detail = serde_json::json!({ "reason": "stop", "ackToken": token });
        let _ = window.eval(format!("window.dispatchEvent(new CustomEvent('risu-native-lifecycle',{{detail:{detail}}}))"));
    }
    Some(deadline)
}

#[tauri::command]
pub(crate) fn desktop_flush_complete(window: tauri::WebviewWindow, token: String) -> Result<(), String> {
    if window.label() != "main" { return Err("Lifecycle belongs to the main window".into()); }
    let exit = window.state::<SessionState>().0.lock().map_err(|_| "Session state unavailable")?.acknowledge(&token);
    if exit { window.app_handle().exit(0); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_wait_is_bounded_and_only_matching_acknowledgement_settles_it() {
        let now = Instant::now();
        let mut state = PendingFlush::default();
        let (token, _) = state.request(now, false);
        assert_eq!(state.request(now + Duration::from_secs(1), false).0, token);
        assert!(!state.acknowledge("stale"));
        assert!(state.waiting(now + Duration::from_secs(1)), "a stale answer leaves the wait in place");
        assert!(!state.waiting(now + SETTLE_LIMIT));
        assert!(!state.acknowledge(&token));
        assert!(!state.waiting(now), "the matching answer ends the wait");
        assert_eq!(state.request(now, false).0, token);
        state.clear();
        assert!(!state.waiting(now));
    }

    #[test]
    fn a_repeated_request_keeps_the_first_deadline() {
        let now = Instant::now();
        let mut state = PendingFlush::default();
        let (token, deadline) = state.request(now, false);
        assert_eq!(deadline, now + SETTLE_LIMIT);
        assert_eq!(state.request(now + Duration::from_secs(1), true), (token, deadline));
    }

    #[test]
    fn a_request_after_an_unanswered_deadline_keeps_the_original_wait() {
        let now = Instant::now();
        let later = now + Duration::from_secs(5);
        let mut state = PendingFlush::default();
        let (first, _) = state.request(now, false);
        let (second, deadline) = state.request(later, true);
        assert_eq!(first, second);
        assert_eq!(deadline, now + SETTLE_LIMIT);
        assert!(!state.waiting(later));
        assert!(state.acknowledge(&second));
        state.clear();
        let (next, next_deadline) = state.request(later, false);
        assert_ne!(next, first);
        assert_eq!(next_deadline, later + SETTLE_LIMIT);
        assert!(!state.acknowledge(&first));
        assert!(state.waiting(later));
        assert!(!state.acknowledge(&next));
    }

    #[test]
    fn only_the_matching_answer_to_an_exiting_flush_exits() {
        let now = Instant::now();
        let mut state = PendingFlush::default();
        let (token, _) = state.request(now, false);
        state.request(now, true);
        assert!(!state.acknowledge("stale"));
        assert!(state.waiting(now));
        assert!(state.acknowledge(&token));
        assert!(!state.waiting(now));
        state.clear();
        let (token, _) = state.request(now, false);
        assert!(!state.acknowledge(&token));
    }
}
