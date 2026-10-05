//! macOS window lifetime and an acknowledged, cancellable application quit.
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SESSION_END_LIMIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExitOrigin {
    Native,
    Runtime(i32),
}

#[derive(Debug, PartialEq, Eq)]
enum ExitEffect {
    NativeReply(bool),
    RuntimeExit(i32),
}

#[derive(Clone)]
struct PendingExit {
    token: String,
    origin: ExitOrigin,
    session_deadline: Option<Instant>,
}

#[derive(Default)]
struct ExitDecision {
    ready: bool,
    pending: Option<PendingExit>,
    allowed: bool,
}

impl ExitDecision {
    fn document_started(&mut self) -> Option<ExitEffect> {
        self.ready = false;
        if self.pending.as_ref().is_some_and(|pending| pending.session_deadline.is_some()) {
            return None;
        }
        self.pending.take().and_then(|pending| {
            (pending.origin == ExitOrigin::Native).then_some(ExitEffect::NativeReply(false))
        })
    }

    fn request(&mut self, code: i32) -> Option<String> {
        if !self.ready || self.allowed || self.pending.is_some() {
            return None;
        }
        Some(self.begin(ExitOrigin::Runtime(code)))
    }

    fn request_native(&mut self) -> Option<String> {
        if !self.ready || self.allowed {
            return None;
        }
        if let Some(pending) = self.pending.as_mut() {
            pending.origin = ExitOrigin::Native;
            return None;
        }
        Some(self.begin(ExitOrigin::Native))
    }

    fn begin(&mut self, origin: ExitOrigin) -> String {
        let token = uuid::Uuid::new_v4().to_string();
        self.pending = Some(PendingExit { token: token.clone(), origin, session_deadline: None });
        token
    }

    fn begin_session_end(&mut self, now: Instant) -> Option<(String, Instant)> {
        let pending = self.pending.as_mut()?;
        if pending.session_deadline.is_some() { return None; }
        let deadline = now + SESSION_END_LIMIT;
        pending.session_deadline = Some(deadline);
        Some((pending.token.clone(), deadline))
    }

    fn expire_session_end(&mut self, token: &str, now: Instant) -> Option<ExitEffect> {
        let pending = self.pending.as_ref()?;
        if pending.token != token || !pending.session_deadline.is_some_and(|deadline| now >= deadline) {
            return None;
        }
        self.respond(token, true).ok().flatten()
    }

    fn response_effect(&self, token: &str, exit: bool) -> Result<Option<ExitEffect>, String> {
        let pending = self.pending.as_ref().filter(|pending| pending.token == token)
            .ok_or("No matching macOS quit request")?;
        if pending.session_deadline.is_some() && !exit {
            return Err("Session-end termination cannot be cancelled".into());
        }
        Ok(match pending.origin {
            ExitOrigin::Native => Some(ExitEffect::NativeReply(exit)),
            ExitOrigin::Runtime(code) => exit.then_some(ExitEffect::RuntimeExit(code)),
        })
    }

    fn respond(&mut self, token: &str, exit: bool) -> Result<Option<ExitEffect>, String> {
        let effect = self.response_effect(token, exit)?;
        self.pending = None;
        self.allowed = exit;
        Ok(effect)
    }

    fn discard(&mut self, token: &str) {
        if self.pending.as_ref().map(|pending| pending.token.as_str()) == Some(token) {
            self.pending = None;
        }
    }
}

#[derive(Default)]
pub(crate) struct ExitState(Mutex<ExitDecision>);

#[cfg(target_os = "macos")]
use tauri::{Emitter, Manager};

#[cfg(target_os = "macos")]
static APP: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn risunest_reply_termination(approve: std::ffi::c_int) -> std::ffi::c_int;
    fn risunest_termination_pending() -> std::ffi::c_int;
    fn risunest_queue_termination_deadline(
        callback: extern "C" fn(*mut std::ffi::c_void),
        context: *mut std::ffi::c_void,
        seconds: f64,
    ) -> std::ffi::c_int;
    fn risunest_queue_termination_response(
        callback: extern "C" fn(*mut std::ffi::c_void),
        context: *mut std::ffi::c_void,
    ) -> std::ffi::c_int;
}

#[cfg(target_os = "macos")]
extern "C" fn request_native_quit(session_ending: std::ffi::c_int) -> std::ffi::c_int {
    let Some(app) = APP.get() else { return -1; };
    let Some(state) = app.try_state::<ExitState>() else { return -1; };
    let requested_at = Instant::now();
    let previous = state.0.lock().unwrap_or_else(|error| error.into_inner()).pending.clone();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let request = {
            let mut decision = state.0.lock().unwrap_or_else(|error| error.into_inner());
            if !decision.ready || decision.allowed { None }
            else { Some(decision.request_native()) }
        };
        match request {
            None => { crate::cancel_incomplete_boot(app); 0 }
            Some(token) => {
                let session = if session_ending != 0 {
                    state.0.lock().unwrap_or_else(|error| error.into_inner()).begin_session_end(requested_at)
                } else { None };
                if let Some((token, deadline)) = session.as_ref() {
                    if !queue_session_deadline(app, token, *deadline) {
                        state.0.lock().unwrap_or_else(|error| error.into_inner()).discard(token);
                        return 0;
                    }
                }
                if let Some(token) = session.map(|(token, _)| token).or(token) {
                    if let Err(error) = notify_quit(app, &token, session_ending != 0) {
                        crate::nlog!("warn", "macOS quit notification failed: {error}");
                        if session_ending == 0 {
                            state.0.lock().unwrap_or_else(|error| error.into_inner()).discard(&token);
                            return -1;
                        }
                    }
                }
                1
            }
        }
    })).unwrap_or_else(|_| {
        state.0.lock().unwrap_or_else(|error| error.into_inner()).pending = previous;
        -1
    })
}

#[cfg(target_os = "macos")]
struct SessionDeadline { app: tauri::AppHandle, token: String }

#[cfg(target_os = "macos")]
fn queue_session_deadline(app: &tauri::AppHandle, token: &str, deadline: Instant) -> bool {
    let context = Box::into_raw(Box::new(SessionDeadline { app: app.clone(), token: token.to_owned() }));
    if unsafe { risunest_queue_termination_deadline(expire_session_end, context.cast(),
        deadline.saturating_duration_since(Instant::now()).as_secs_f64()) } == 1 { return true; }
    unsafe { drop(Box::from_raw(context)); }
    false
}

#[cfg(target_os = "macos")]
extern "C" fn expire_session_end(context: *mut std::ffi::c_void) {
    let SessionDeadline { app, token } = *unsafe { Box::from_raw(context.cast::<SessionDeadline>()) };
    let state = app.state::<ExitState>();
    let deadline = state.0.lock().unwrap_or_else(|error| error.into_inner()).pending.as_ref()
        .filter(|pending| pending.token == token).and_then(|pending| pending.session_deadline);
    let Some(deadline) = deadline else { return; };
    if Instant::now() < deadline && queue_session_deadline(&app, &token, deadline) { return; }
    let effect = state.0.lock().unwrap_or_else(|error| error.into_inner())
        .expire_session_end(&token, deadline);
    if let Some(ExitEffect::NativeReply(true)) = effect {
        unsafe { risunest_reply_termination(1); }
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn install_native_quit(app: &tauri::AppHandle) -> Result<(), String> {
    unsafe extern "C" {
        fn risunest_install_termination_handler(
            callback: extern "C" fn(std::ffi::c_int) -> std::ffi::c_int,
        ) -> std::ffi::c_int;
    }
    APP.set(app.clone()).map_err(|_| "macOS quit bridge already installed")?;
    // Setup runs on the AppKit thread after Tao has installed its delegate.
    if unsafe { risunest_install_termination_handler(request_native_quit) } != 1 {
        return Err("Unable to install macOS native quit protection".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn document_started(app: &tauri::AppHandle) {
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || {
        if let Some(state) = handle.try_state::<ExitState>() {
            let effect = state.0.lock().unwrap_or_else(|error| error.into_inner()).document_started();
            if let Some(ExitEffect::NativeReply(false)) = effect {
                if unsafe { risunest_reply_termination(0) } != 1 {
                    crate::nlog!("warn", "macOS reload quit cancellation failed");
                }
            }
        }
    }) {
        crate::nlog!("warn", "macOS reload quit cancellation could not be scheduled: {error}");
    }
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub(crate) fn macos_lifecycle_ready(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, ExitState>,
) -> Result<(), String> {
    crate::native_log::logged_without_detail("macos_lifecycle_ready", (|| {
        if window.label() != "main" {
            return Err("macOS lifecycle belongs to the main window".into());
        }
        state.0.lock().map_err(|_| "Quit state unavailable")?.ready = true;
        Ok(())
    })())
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub(crate) async fn macos_exit_response(
    window: tauri::WebviewWindow,
    token: String,
    exit: bool,
) -> Result<(), String> {
    crate::native_log::logged_without_detail("macos_exit_response", async {
        if window.label() != "main" {
            return Err("macOS lifecycle belongs to the main window".into());
        }
        let receiver = {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let context = Box::into_raw(Box::new(ExitResponse {
                app: window.app_handle().clone(), token, exit, sender,
            }));
            if unsafe { risunest_queue_termination_response(settle_response, context.cast()) } != 1 {
                unsafe { drop(Box::from_raw(context)); }
                return Err("Unable to schedule macOS quit response".into());
            }
            receiver
        };
        receiver.await.map_err(|_| "macOS quit response was not completed")?
    }.await)
}

#[cfg(target_os = "macos")]
struct ExitResponse {
    app: tauri::AppHandle,
    token: String,
    exit: bool,
    sender: tokio::sync::oneshot::Sender<Result<(), String>>,
}

#[cfg(target_os = "macos")]
extern "C" fn settle_response(context: *mut std::ffi::c_void) {
    // The queued native block calls this once with the Box it owns.
    let ExitResponse { app, token, exit, sender } =
        *unsafe { Box::from_raw(context.cast::<ExitResponse>()) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let state = app.state::<ExitState>();
        let effect = state.0.lock().map_err(|_| "Quit state unavailable")?
            .response_effect(&token, exit)?;
        if matches!(effect, Some(ExitEffect::NativeReply(_)))
            && unsafe { risunest_termination_pending() } != 1 {
            return Err("No pending macOS native quit reply".into());
        }
        let effect = state.0.lock().map_err(|_| "Quit state unavailable")?
            .respond(&token, exit)?;
        match effect {
            Some(ExitEffect::NativeReply(approve)) => {
                if unsafe { risunest_reply_termination(i32::from(approve)) } != 1 {
                    return Err("macOS native quit reply failed".into());
                }
            }
            Some(ExitEffect::RuntimeExit(code)) => app.exit(code),
            None => {}
        }
        Ok(())
    })).unwrap_or_else(|_| Err("macOS quit response failed".into()));
    let _ = sender.send(result);
}

#[cfg(target_os = "macos")]
/// A session-end quit asks the document to save locally and answer without sync or questions.
fn notify_quit(app: &tauri::AppHandle, token: &str, session_end: bool) -> Result<(), tauri::Error> {
    if !session_end { show_main(app); }
    let remaining = app.state::<ExitState>().0.lock().unwrap_or_else(|error| error.into_inner())
        .pending.as_ref().and_then(|pending| pending.session_deadline)
        .map(|deadline| deadline.saturating_duration_since(Instant::now()));
    let deadline_unix_millis = remaining.map(|remaining| {
        (std::time::SystemTime::now() + remaining).duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default().as_millis() as u64
    });
    app.emit_to("main", "risu-macos-exit-requested", serde_json::json!({ "token": token, "sessionEnd": session_end, "deadlineUnixMillis": deadline_unix_millis }))
}

#[cfg(target_os = "macos")]
fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn handle_run_event(app: &tauri::AppHandle, event: tauri::RunEvent) {
    match event {
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::CloseRequested { api, .. },
            ..
        } if label == "main" => {
            api.prevent_close();
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.hide();
            }
        }
        tauri::RunEvent::Reopen { .. } => show_main(app),
        tauri::RunEvent::Opened { urls } => {
            crate::opened_files::deliver_opened_urls(app, &urls);
            show_main(app);
        }
        tauri::RunEvent::ExitRequested { api, code, .. }
            // Tauri explicitly cannot prevent restart. Existing relaunch callers
            // settle their storage before requesting it.
            if code != Some(tauri::RESTART_EXIT_CODE) =>
        {
            let state = app.state::<ExitState>();
            let token = {
                let mut decision = state.0.lock().unwrap_or_else(|error| error.into_inner());
                if !decision.ready || decision.allowed { return; }
                api.prevent_exit();
                decision.request(code.unwrap_or(0))
            };
            if let Some(token) = token {
                if let Err(error) = notify_quit(app, &token, false) {
                    state.0.lock().unwrap_or_else(|error| error.into_inner()).discard(&token);
                    crate::nlog!("warn", "macOS quit notification failed: {error}");
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_deadline_survives_repeats_reload_and_settles_only_its_token() {
        let mut state = ExitDecision { ready: true, ..Default::default() };
        let token = state.request_native().unwrap();
        let now = Instant::now();
        assert_eq!(state.begin_session_end(now), Some((token.clone(), now + Duration::from_secs(5))));
        assert_eq!(state.begin_session_end(now + Duration::from_secs(4)), None);
        assert_eq!(state.expire_session_end(&token, now + Duration::from_secs(4)), None);
        assert_eq!(state.document_started(), None);
        assert!(state.respond(&token, false).is_err());
        assert_eq!(state.expire_session_end("stale", now + SESSION_END_LIMIT), None);
        assert_eq!(state.expire_session_end(&token, now + SESSION_END_LIMIT), Some(ExitEffect::NativeReply(true)));
        assert_eq!(state.expire_session_end(&token, now + SESSION_END_LIMIT), None);
        assert!(state.respond(&token, true).is_err());
    }

    #[test]
    fn ordinary_quit_has_no_deadline_and_a_completed_session_timer_cannot_approve_a_new_quit() {
        let mut state = ExitDecision { ready: true, ..Default::default() };
        let first = state.request_native().unwrap();
        let now = Instant::now();
        assert_eq!(state.expire_session_end(&first, now + SESSION_END_LIMIT), None);
        state.respond(&first, false).unwrap();
        let next = state.request_native().unwrap();
        state.begin_session_end(now).unwrap();
        assert_eq!(state.expire_session_end(&first, now + SESSION_END_LIMIT), None);
        assert_eq!(state.respond(&next, true).unwrap(), Some(ExitEffect::NativeReply(true)));
        assert_eq!(state.expire_session_end(&next, now + SESSION_END_LIMIT), None);
    }

    #[test]
    fn quit_requires_readiness_and_a_matching_single_response() {
        let mut state = ExitDecision::default();
        assert!(state.request(0).is_none());
        state.ready = true;
        let token = state.request(7).unwrap();
        assert!(state.request(0).is_none());
        assert!(state.respond("stale", true).is_err());
        assert!(!state.allowed);
        assert_eq!(state.respond(&token, true).unwrap(), Some(ExitEffect::RuntimeExit(7)));
        assert!(state.allowed);
        assert!(state.respond(&token, true).is_err());
    }

    #[test]
    fn cancellation_leaves_the_app_running_and_allows_retry() {
        let mut state = ExitDecision {
            ready: true,
            ..Default::default()
        };
        let first = state.request(0).unwrap();
        assert_eq!(state.respond(&first, false).unwrap(), None);
        assert!(!state.allowed);
        let next = state.request(0).unwrap();
        assert_ne!(first, next);
        assert!(state.respond(&first, true).is_err());
        assert_eq!(state.respond(&next, true).unwrap(), Some(ExitEffect::RuntimeExit(0)));
    }

    #[test]
    fn reloading_drops_the_departed_documents_quit_token() {
        let mut state = ExitDecision {
            ready: true,
            ..Default::default()
        };
        let departed = state.request(0).unwrap();
        assert_eq!(state.document_started(), None);
        assert!(!state.ready);
        assert!(state.respond(&departed, true).is_err());
        state.ready = true;
        assert!(state.request(0).is_some());
    }

    #[test]
    fn native_quit_requires_readiness_and_has_one_reply_effect() {
        let mut state = ExitDecision::default();
        assert_eq!(state.request_native(), None);
        assert!(state.pending.is_none());
        state.ready = true;
        let token = state.request_native().unwrap();
        assert_eq!(state.request_native(), None);
        assert!(state.request(0).is_none());
        assert!(state.respond("stale", true).is_err());
        assert_eq!(state.respond(&token, true).unwrap(), Some(ExitEffect::NativeReply(true)));
        assert!(state.allowed);
        assert!(state.pending.is_none());
        assert!(state.respond(&token, true).is_err());
        assert_eq!(state.document_started(), None);
        assert!(state.respond(&token, false).is_err());
    }

    #[test]
    fn native_cancellation_allows_a_new_request() {
        let mut state = ExitDecision { ready: true, ..Default::default() };
        let first = state.request_native().unwrap();
        assert_eq!(state.respond(&first, false).unwrap(), Some(ExitEffect::NativeReply(false)));
        assert!(!state.allowed);
        let next = state.request_native().unwrap();
        assert_ne!(first, next);
        assert!(state.respond(&first, true).is_err());
        assert_eq!(state.respond(&next, true).unwrap(), Some(ExitEffect::NativeReply(true)));
    }

    #[test]
    fn native_reload_cancels_once_and_invalidates_late_approval() {
        let mut state = ExitDecision { ready: true, ..Default::default() };
        let departed = state.request_native().unwrap();
        assert_eq!(state.document_started(), Some(ExitEffect::NativeReply(false)));
        assert!(!state.ready);
        assert!(!state.allowed);
        assert_eq!(state.document_started(), None);
        assert!(state.respond(&departed, true).is_err());
        state.ready = true;
        let next = state.request_native().unwrap();
        assert_ne!(departed, next);
        assert_eq!(state.respond(&next, true).unwrap(), Some(ExitEffect::NativeReply(true)));
    }

    #[test]
    fn cancellation_before_reload_does_not_reply_twice() {
        let mut state = ExitDecision { ready: true, ..Default::default() };
        let token = state.request_native().unwrap();
        assert_eq!(state.respond(&token, false).unwrap(), Some(ExitEffect::NativeReply(false)));
        assert_eq!(state.document_started(), None);
        assert!(state.respond(&token, true).is_err());
    }

    #[test]
    fn native_quit_coalesces_an_existing_zero_code_runtime_request() {
        let mut state = ExitDecision { ready: true, ..Default::default() };
        let token = state.request(0).unwrap();
        assert_eq!(state.request_native(), None);
        assert_eq!(state.respond(&token, true).unwrap(), Some(ExitEffect::NativeReply(true)));
    }

    #[test]
    fn unconsumed_response_and_failed_notification_preserve_newer_work() {
        let mut state = ExitDecision { ready: true, ..Default::default() };
        let token = state.request_native().unwrap();
        assert_eq!(state.response_effect(&token, true).unwrap(), Some(ExitEffect::NativeReply(true)));
        assert!(!state.allowed);
        assert_eq!(state.pending.as_ref().unwrap().token, token);
        state.discard(&token);
        assert!(state.pending.is_none());
        let next = state.request_native().unwrap();
        state.discard(&token);
        assert_eq!(state.pending.as_ref().unwrap().token, next);
        assert!(state.respond(&token, true).is_err());
        assert_eq!(state.respond(&next, false).unwrap(), Some(ExitEffect::NativeReply(false)));
    }

    #[test]
    fn native_quit_owns_a_simultaneous_nonzero_runtime_request() {
        let mut state = ExitDecision { ready: true, ..Default::default() };
        let token = state.request(7).unwrap();
        assert_eq!(state.request_native(), None);
        assert_eq!(state.pending.as_ref().unwrap().token, token);
        assert_eq!(state.request_native(), None);
        assert!(state.request(7).is_none());
        assert_eq!(state.respond(&token, true).unwrap(), Some(ExitEffect::NativeReply(true)));
        assert!(state.respond(&token, true).is_err());
    }

    #[test]
    fn runtime_quit_during_native_pending_keeps_native_ownership() {
        let mut state = ExitDecision { ready: true, ..Default::default() };
        let token = state.request_native().unwrap();
        assert!(state.request(7).is_none());
        assert_eq!(state.pending.as_ref().unwrap().token, token);
        assert_eq!(state.respond(&token, false).unwrap(), Some(ExitEffect::NativeReply(false)));
        assert!(!state.allowed);
        let retry = state.request(7).unwrap();
        assert_eq!(state.respond(&retry, true).unwrap(), Some(ExitEffect::RuntimeExit(7)));
    }
}
