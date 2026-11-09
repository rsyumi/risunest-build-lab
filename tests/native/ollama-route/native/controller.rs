use crate::native_generation::operation::Ticket;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, Runtime};
use tokio::sync::Notify;

const TRACE_LIMIT: usize = 128;
const HOLD_LEASE: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Gate { None, Credit, Offer, PreAdmission, Admitted }

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Token {
    pub boot: String,
    pub generation: u32,
    pub ticket: Ticket,
    pub gate: Gate,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Kind {
    RequestFormatted, BufferDecoded, JsonParsed, ResponseTransformed, StoragePolicyApplied,
    CreditEntered, CreditDelegated, CreditForwarded, CreditDropped,
    OfferEntered, OfferDelegated, OfferForwarded, OfferDropped,
    OutputSegmentEntered, OutputAdmitted, CancelWaiting, OutputResolved, OwnerCompleted,
    GateEntered, GateReleased, GateDropped, Retired,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ResultKind {
    Ok, Cancelled, Failed, Accepted, RejectedCancelled, RejectedFailed, Unknown,
    Committed, Uncommitted, Unresolved, Error, Completed, Stale, Interrupted, Running,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct Event { pub sequence: u32, pub kind: Kind, pub result: Option<ResultKind> }

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Snapshot {
    pub token: Token,
    pub events: Vec<Event>,
    pub retired: bool,
    pub drained: bool,
    pub actors: u32,
    pub holds: u32,
    pub hooks: u32,
    pub invalid: Option<&'static str>,
}

struct State {
    events: Vec<Event>,
    retired: bool,
    actors: u32,
    hooks: u32,
    held: bool,
    released: bool,
    invalid: Option<&'static str>,
}

impl State {
    fn invalid(&mut self, reason: &'static str) { self.invalid.get_or_insert(reason); }
    fn has(&self, kind: Kind) -> bool { self.events.iter().any(|event| event.kind == kind) }
    fn record(&mut self, kind: Kind, result: Option<ResultKind>) {
        if self.events.len() == TRACE_LIMIT { self.invalid("trace-overflow"); return; }
        if self.has(kind) { self.invalid("duplicate-event"); }
        let prior = match kind {
            Kind::BufferDecoded => Some(Kind::RequestFormatted),
            Kind::JsonParsed => Some(Kind::BufferDecoded),
            Kind::ResponseTransformed => Some(Kind::JsonParsed),
            Kind::CreditEntered => Some(Kind::ResponseTransformed),
            Kind::CreditDelegated | Kind::CreditDropped => Some(Kind::CreditEntered),
            Kind::CreditForwarded => Some(Kind::CreditDelegated),
            Kind::OfferEntered => Some(Kind::CreditForwarded),
            Kind::OfferDelegated | Kind::OfferDropped => Some(Kind::OfferEntered),
            Kind::OfferForwarded => Some(Kind::OfferDelegated),
            Kind::StoragePolicyApplied => Some(Kind::OfferDelegated),
            Kind::OutputSegmentEntered => Some(Kind::StoragePolicyApplied),
            Kind::OutputAdmitted | Kind::OutputResolved => Some(Kind::OutputSegmentEntered),
            Kind::CancelWaiting => Some(Kind::OutputAdmitted),
            Kind::GateReleased | Kind::GateDropped => Some(Kind::GateEntered),
            _ => None,
        };
        if prior.is_some_and(|prior| !self.has(prior)) { self.invalid("out-of-order-event"); }
        self.events.push(Event { sequence: self.events.len() as u32 + 1, kind, result });
    }
    fn drained(&self) -> bool { self.actors == 0 && self.hooks == 0 && !self.held }
}

pub(crate) struct Session {
    pub token: Token,
    state: Mutex<State>,
    wake: Notify,
    condition: Condvar,
    lease: Duration,
}

impl Session {
    fn new(token: Token, lease: Duration) -> Self {
        Self { token, state: Mutex::new(State { events: Vec::new(), retired: false, actors: 0,
            hooks: 0, held: false, released: false, invalid: None }), wake: Notify::new(),
            condition: Condvar::new(), lease }
    }
    pub fn snapshot(&self) -> Snapshot {
        let state = self.state.lock().unwrap();
        Snapshot { token: self.token.clone(), events: state.events.clone(), retired: state.retired,
            drained: state.drained(), actors: state.actors, holds: u32::from(state.held),
            hooks: state.hooks, invalid: state.invalid }
    }
    pub fn record(&self, kind: Kind, result: Option<ResultKind>) {
        self.state.lock().unwrap().record(kind, result);
    }
    pub fn enroll(self: &Arc<Self>) -> Option<Actor> {
        let mut state = self.state.lock().unwrap();
        if state.retired { return None; }
        state.actors += 1;
        Some(Actor { session: self.clone() })
    }
    pub fn hook(self: &Arc<Self>) -> Hook {
        self.state.lock().unwrap().hooks += 1;
        Hook { session: self.clone() }
    }
    fn enter(self: &Arc<Self>, gate: Gate) -> Option<Hold> {
        if self.token.gate != gate { return None; }
        let mut state = self.state.lock().unwrap();
        if state.retired { state.invalid("entry-after-retirement"); return None; }
        if state.has(Kind::GateEntered) { state.invalid("duplicate-gate"); return None; }
        state.held = true;
        state.record(Kind::GateEntered, None);
        Some(Hold { session: self.clone(), finished: false })
    }
    fn expired(&self) {
        let mut state = self.state.lock().unwrap();
        state.invalid("lease-expired");
        state.released = true;
        drop(state);
        self.wake.notify_one();
        self.condition.notify_all();
    }
    pub async fn wait(self: &Arc<Self>, gate: Gate) {
        let Some(mut hold) = self.enter(gate) else { return };
        let deadline = tokio::time::Instant::now() + self.lease;
        loop {
            let notified = self.wake.notified();
            if self.state.lock().unwrap().released { break; }
            if tokio::time::timeout_at(deadline, notified).await.is_err() { self.expired(); break; }
        }
        hold.finish();
    }
    pub fn wait_blocking(self: &Arc<Self>, gate: Gate) {
        let Some(mut hold) = self.enter(gate) else { return };
        let deadline = Instant::now() + self.lease;
        let mut state = self.state.lock().unwrap();
        while !state.released {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() { drop(state); self.expired(); hold.finish(); return; }
            state = self.condition.wait_timeout(state, remaining).unwrap().0;
        }
        drop(state);
        hold.finish();
    }
    fn release(&self) -> Result<(), &'static str> {
        let mut state = self.state.lock().unwrap();
        if state.retired || !state.held || state.released { return Err("native-ui:hold-not-owned"); }
        state.released = true;
        state.record(Kind::GateReleased, None);
        drop(state);
        self.wake.notify_one();
        self.condition.notify_all();
        Ok(())
    }
    fn retire(&self) {
        let mut state = self.state.lock().unwrap();
        if state.retired { return; }
        state.retired = true;
        if state.held { state.invalid("forced-unblock"); }
        else if !state.drained() { state.invalid("retire-before-drain"); }
        state.released = true;
        state.record(Kind::Retired, None);
        drop(state);
        self.wake.notify_one();
        self.condition.notify_all();
    }
}

pub(crate) struct Actor { pub session: Arc<Session> }
impl Drop for Actor { fn drop(&mut self) { self.session.state.lock().unwrap().actors -= 1; } }
pub(crate) struct Hook { session: Arc<Session> }
impl Drop for Hook { fn drop(&mut self) { self.session.state.lock().unwrap().hooks -= 1; } }
struct Hold { session: Arc<Session>, finished: bool }
impl Hold {
    fn finish(&mut self) { self.session.state.lock().unwrap().held = false; self.finished = true; }
}
impl Drop for Hold {
    fn drop(&mut self) {
        if !self.finished {
            let mut state = self.session.state.lock().unwrap();
            state.held = false;
            state.record(Kind::GateDropped, None);
        }
    }
}

struct Registry { generation: u32, current: Option<Arc<Session>> }
pub(crate) struct Controller { boot: String, registry: Mutex<Registry> }
impl Default for Controller {
    fn default() -> Self { Self { boot: uuid::Uuid::new_v4().to_string(), registry: Mutex::new(Registry { generation: 0, current: None }) } }
}
impl Controller {
    pub fn arm(&self, ticket: Ticket, gate: Gate) -> Result<Token, &'static str> {
        let mut registry = self.registry.lock().unwrap();
        if let Some(current) = &registry.current {
            let snapshot = current.snapshot();
            if !snapshot.retired || !snapshot.drained { return Err("native-ui:session-active"); }
        }
        registry.generation = registry.generation.checked_add(1).ok_or("native-ui:generation-exhausted")?;
        let token = Token { boot: self.boot.clone(), generation: registry.generation, ticket, gate };
        registry.current = Some(Arc::new(Session::new(token.clone(), HOLD_LEASE)));
        Ok(token)
    }
    pub fn session(&self, token: &Token) -> Result<Arc<Session>, &'static str> {
        self.registry.lock().unwrap().current.as_ref().filter(|current| current.token == *token)
            .cloned().ok_or("native-ui:stale-token")
    }
    fn capture(&self, ticket: &Ticket) -> Option<Actor> {
        self.registry.lock().unwrap().current.as_ref().filter(|current| current.token.ticket == *ticket)
            .and_then(|current| current.enroll())
    }
}

pub(crate) fn capture<R: Runtime>(app: &AppHandle<R>, ticket: &Ticket) -> Option<Actor> {
    app.try_state::<Controller>().and_then(|controller| controller.capture(ticket))
}

fn controller<R: Runtime>(app: &AppHandle<R>) -> Result<tauri::State<'_, Controller>, &'static str> {
    app.try_state::<Controller>().ok_or("native-ui:controller-unavailable")
}

#[tauri::command]
pub(crate) async fn ollama_ui_arm<R: Runtime>(app: AppHandle<R>, ticket: Ticket, gate: Gate) -> Result<Token, &'static str> {
    controller(&app)?.arm(ticket, gate)
}
#[tauri::command]
pub(crate) async fn ollama_ui_snapshot<R: Runtime>(app: AppHandle<R>, token: Token) -> Result<Snapshot, &'static str> {
    Ok(controller(&app)?.session(&token)?.snapshot())
}
#[tauri::command]
pub(crate) async fn ollama_ui_release<R: Runtime>(app: AppHandle<R>, token: Token) -> Result<Snapshot, &'static str> {
    let session = controller(&app)?.session(&token)?;
    session.release()?;
    Ok(session.snapshot())
}
#[tauri::command]
pub(crate) async fn ollama_ui_retire<R: Runtime>(app: AppHandle<R>, token: Token) -> Result<Snapshot, &'static str> {
    let session = controller(&app)?.session(&token)?;
    session.retire();
    Ok(session.snapshot())
}

pub(crate) fn handler<R: Runtime>() -> impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![ollama_ui_arm, ollama_ui_snapshot, ollama_ui_release, ollama_ui_retire]
}

#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;
