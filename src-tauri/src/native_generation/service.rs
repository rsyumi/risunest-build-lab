use super::{inputs::{effective_settings, text, truth, OllamaIntent, PreparationError, PreparationSnapshot}, operation::{Owner, Phase, Start, Status, Ticket}, prepare::{prepare_ollama_owned, PreparedOllamaTurn}, preparation_effects::PreparedVariablePlan};
use super::operation::{GenerationLease, NormalizationWitness, NormalizationResolution, NormalizationProjectionResult, OwnerStatus, DispatchState, OutputState, ActiveGeneration};
use super::{ollama_transport::{Think, ThinkLevel}, owner::{self, Lifecycle, OwnerHost, ReadyOllama, ReadyOllamaConfig}};
use crate::persistent_store::{generation::{CancelReason, TerminalDecision}, sync_selection::SyncTarget, PersistentStore, StoreError, StoreResult};
use serde_json::Value;
use std::{collections::BTreeMap, sync::{Arc, Mutex, OnceLock, TryLockError, Weak}};

#[derive(Debug)]
pub(crate) enum GenerationError {
    Store(StoreError),
    Preparation(PreparationError),
}

impl From<StoreError> for GenerationError {
    fn from(value: StoreError) -> Self { Self::Store(value) }
}

impl From<PreparationError> for GenerationError {
    fn from(value: PreparationError) -> Self { Self::Preparation(value) }
}

// Execution-absent preparation-only results, kept for the store's legacy preparation tests.
#[cfg(test)]
#[derive(Debug)]
pub(crate) enum PreparedGeneration {
    Ready { owner: Owner, prepared: PreparedOllamaTurn, read_revision: i64, status: Status },
    Existing(Status),
    Failed { error: PreparationError, status: Status },
}

#[derive(Debug)]
pub(crate) enum ReadyPreparation {
    Ready(ReadyOllama),
    Existing(Status),
    Failed { error: PreparationError, status: Status },
}

enum Outcome<T> {
    Ready(T),
    Existing(Status),
    Failed { error: PreparationError, status: Status },
}

struct CapturedSettings { host: String, model: String, think: Option<Think>, model_label: String }

// Ordinary Send requests streaming, so the effective value is `db.useStreaming`; this route refuses it rather than turning it off.
fn capture_settings(settings: &Value) -> Result<CapturedSettings, PreparationError> {
    if truth(&settings["useStreaming"]) { return Err(PreparationError::Unsupported("useStreaming".into())); }
    let host = text(settings, "ollamaURL", "")?.to_owned();
    let model = text(settings, "ollamaModel", "")?.to_owned();
    let think = match settings["ollamaThinkingMode"].as_str() {
        Some("off") => Some(Think::Boolean(false)),
        Some("on") => Some(Think::Boolean(true)),
        Some("low") => Some(Think::Level(ThinkLevel::Low)),
        Some("medium") => Some(Think::Level(ThinkLevel::Medium)),
        Some("high") => Some(Think::Level(ThinkLevel::High)),
        _ => None,
    };
    let model_label = match settings["ollamaModelName"].as_str() { Some(name) if !name.is_empty() => name.to_owned(), _ => model.clone() };
    Ok(CapturedSettings { host, model, think, model_label })
}

pub(crate) fn reserve_ollama(store: &mut PersistentStore, intent: &OllamaIntent) -> Result<Ticket, GenerationError> {
    owner::reconcile(store)?;
    // Decision O4: sync-bound devices stay on the TS route until native receive protection lands.
    if matches!(store.external_selection()?.target, SyncTarget::Server(_) | SyncTarget::External(_)) {
        return Err(PreparationError::Unsupported("syncBinding".into()).into());
    }
    let lease = store.acquire_revision(intent.target.expected_revision)?.lease;
    let root = store.read_root(Some(&lease));
    let released = store.release_revision(&lease);
    let root = root?.value;
    released?;
    capture_settings(&root)?;
    store.generation_reserve_ollama(intent)
}

pub(crate) fn cancel_generation(store: &mut PersistentStore, ticket: &Ticket) -> StoreResult<Status> {
    cancel_generation_for(store, ticket, CancelReason::User)
}

// The live owner hears the signal first; without one, the recorded decision settles through recovery.
pub(crate) fn cancel_generation_for(store: &mut PersistentStore, ticket: &Ticket, reason: CancelReason) -> StoreResult<Status> {
    let presence = owner::signal(&owner::scope(store)?, ticket, reason);
    let status = store.generation_cancel_for(ticket, reason)?;
    match presence {
        owner::Presence::Live => Ok(status),
        owner::Presence::Abandoned => owner::resolve_owner(store, ticket),
        owner::Presence::Absent if !status.phase.terminal() && store.generation_execution(ticket)?.is_some() => store.generation_recover(ticket),
        owner::Presence::Absent => Ok(status),
    }
}

pub(crate) fn resolve_owner(store: &mut PersistentStore, ticket: &Ticket) -> StoreResult<Status> {
    owner::resolve_owner(store, ticket)
}

pub(crate) fn active_generations(store: &PersistentStore) -> StoreResult<Vec<ActiveGeneration>> {
    store.generation_active()
}

// Read-only evidence for status reporting; it never settles or resolves anything.
pub(crate) fn owner_status(store: &PersistentStore, ticket: &Ticket) -> StoreResult<OwnerStatus> {
    let status = store.generation_status(ticket)?;
    let Some(execution) = store.generation_execution(ticket)? else {
        return Ok(OwnerStatus { status, owned: false, dispatch: DispatchState::None, output: OutputState::None, cancel_decision: None, failure: None });
    };
    let dispatch = match (&execution.dispatch_id, &status.phase) {
        (None, _) => DispatchState::None,
        (Some(_), Phase::Interrupted) => DispatchState::Uncertain,
        (Some(_), _) => DispatchState::Issued,
    };
    let output = match &execution.output_receipt {
        Some(receipt) if !status.pending_commit => OutputState::Committed(receipt.clone()),
        _ if status.checkpoint.0 == 0 && !status.pending_commit => OutputState::None,
        _ => OutputState::Unavailable,
    };
    let failure = match execution.terminal { Some(TerminalDecision::Failed { failure }) => Some(failure), _ => None };
    Ok(OwnerStatus { status, owned: true, dispatch, output, cancel_decision: execution.cancel, failure })
}

pub(crate) fn capture_normalization_witness(store: &PersistentStore, ticket: &Ticket, original: &GenerationLease, indices: &[usize]) -> StoreResult<NormalizationWitness> {
    store.generation_capture_normalization_witness(ticket, original, indices)
}

pub(crate) fn resolve_normalization(store: &mut PersistentStore, ticket: &Ticket) -> StoreResult<NormalizationResolution> {
    store.generation_resolve_normalization(ticket)
}

pub(crate) fn project_normalization(store: &PersistentStore, ticket: &Ticket, witness: &NormalizationWitness, original: &GenerationLease, current: &GenerationLease) -> StoreResult<NormalizationProjectionResult> {
    store.generation_project_normalization(ticket, witness, original, current)
}

fn failed<T>(store: &mut PersistentStore, owner: &Owner, error: PreparationError) -> StoreResult<Outcome<T>> {
    let status = store.generation_fail_preparation(owner, error.clone())?;
    Ok(if status.phase == Phase::Failed { Outcome::Failed { error, status } } else { Outcome::Existing(status) })
}

fn failed_with_variables<T>(store: &mut PersistentStore, owner: &Owner, error: PreparationError, variables: &PreparedVariablePlan) -> StoreResult<Outcome<T>> {
    let status = store.generation_fail_preparation_with_variables(owner, error.clone(), variables)?;
    Ok(if status.phase == Phase::Failed { Outcome::Failed { error, status } } else { Outcome::Existing(status) })
}

// This gate owns preparation execution, while the independent store gate admits cancellable segments.
fn preparation_gate(ticket: &Ticket) -> StoreResult<Arc<Mutex<()>>> {
    static GATES: OnceLock<Mutex<BTreeMap<String, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut gates = GATES.get_or_init(|| Mutex::new(BTreeMap::new())).lock()
        .map_err(|_| StoreError::Validation { message: "native-generation:preparation-poisoned".into() })?;
    gates.retain(|_, gate| gate.strong_count() > 0);
    let key = format!("{}:{}", ticket.epoch, ticket.sequence.0);
    if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) { return Ok(gate); }
    let gate = Arc::new(Mutex::new(()));
    gates.insert(key, Arc::downgrade(&gate));
    Ok(gate)
}

// The request body the owner sends for `prepared`, configured from `snapshot` as `prepare_ready_ollama` configures it.
#[cfg(test)]
pub(crate) fn wire_body(snapshot: &PreparationSnapshot, prepared: &PreparedOllamaTurn) -> Vec<u8> {
    let Ok(captured) = effective_settings(snapshot).and_then(|settings| capture_settings(&settings)) else { panic!("settings") };
    let config = ReadyOllamaConfig::new(captured.host, captured.model, captured.think, captured.model_label, &OwnerHost::new("synthetic").unwrap());
    let limits = owner::OwnerLimits::production();
    super::ollama_transport::request_bytes(prepared, &config.captured(), limits.transport, limits.compat)
}

// Runs preparation under its gate; `ready` publishes the result while the gate is still held. Its send-effect and
// history-ID commits wait briefly for device admission a renderer command holds instead of failing the prepare.
fn with_preparation<T>(store: &mut PersistentStore, ticket: &Ticket,
    ready: impl FnOnce(&mut PersistentStore, Owner, PreparedOllamaTurn, PreparationSnapshot, PreparedVariablePlan) -> StoreResult<Outcome<T>>) -> StoreResult<Outcome<T>> {
    store.waiting_for_admission(|store| prepare_under_gate(store, ticket, ready))
}

fn prepare_under_gate<T>(store: &mut PersistentStore, ticket: &Ticket,
    ready: impl FnOnce(&mut PersistentStore, Owner, PreparedOllamaTurn, PreparationSnapshot, PreparedVariablePlan) -> StoreResult<Outcome<T>>) -> StoreResult<Outcome<T>> {
    let gate = preparation_gate(ticket)?;
    let _execution = match gate.try_lock() {
        Ok(execution) => execution,
        Err(TryLockError::WouldBlock) => return Ok(Outcome::Existing(owner::resolve_owner(store, ticket)?)),
        Err(TryLockError::Poisoned(_)) => return Err(StoreError::Validation { message: "native-generation:preparation-poisoned".into() }),
    };
    let owner = match store.generation_resume_preparation(ticket)? {
        Start::Started { owner, .. } => owner,
        Start::Existing(status) => return Ok(Outcome::Existing(status)),
    };
    let status = match store.generation_ensure_history_ids(&owner) {
        Ok(status) => status,
        Err(GenerationError::Preparation(error)) => return failed(store, &owner, error),
        Err(GenerationError::Store(error)) => return Err(error),
    };
    if status.phase != Phase::Running { return Ok(Outcome::Existing(status)); }
    let Some(snapshot) = store.generation_owned_preparation_snapshot(&owner)? else {
        return Ok(Outcome::Existing(store.generation_status(ticket)?));
    };
    // Token work holds no segment gate. Cancellation can settle independently.
    #[cfg(test)]
    BEFORE_PREPARATION.with(|hook| hook.borrow_mut().take()).map(|hook| hook());
    let attempt = prepare_ollama_owned(&snapshot);
    let prepared = match attempt.result {
        Ok(prepared) => prepared,
        Err(error) => return failed_with_variables(store, &owner, error, &attempt.variables),
    };
    ready(store, owner, prepared, snapshot, attempt.variables)
}

// Configuration comes from the same snapshot as the prompt; later root or preset edits cannot reach this wire.
// A full lifecycle registry returns `native-generation:lifecycle-limit` and leaves the ticket Reserved.
pub(crate) fn prepare_ready_ollama(store: &mut PersistentStore, ticket: &Ticket, host: &OwnerHost) -> StoreResult<ReadyPreparation> {
    prepare_ready_ollama_limited(store, ticket, host, owner::RegistryLimits::PRODUCTION)
}

pub(super) fn prepare_ready_ollama_limited(store: &mut PersistentStore, ticket: &Ticket, host: &OwnerHost, limits: owner::RegistryLimits) -> StoreResult<ReadyPreparation> {
    owner::reconcile_limited(store, limits.pass, &mut owner::resolve_owner)?;
    // A ticket that already has a lifecycle answers through it and takes no new capacity.
    if owner::registered(&owner::scope(store)?, ticket) { return Ok(ReadyPreparation::Existing(owner::resolve_owner(store, ticket)?)); }
    let slot = owner::admit(store, limits.capacity)?;
    let outcome = with_preparation(store, ticket, |store, owner, prepared, snapshot, variables| {
        let captured = match effective_settings(&snapshot).and_then(|settings| capture_settings(&settings)) {
            Ok(captured) => captured,
            Err(error) => return failed_with_variables(store, &owner, error, &variables),
        };
        let config = ReadyOllamaConfig::new(captured.host, captured.model, captured.think, captured.model_label, host);
        let Ok(lifecycle) = Lifecycle::register(slot, &owner) else {
            return failed_with_variables(store, &owner, PreparationError::Invalid("lifecycle".into()), &variables);
        };
        // A failed acknowledgement drops the lifecycle, which leaves an abandoned entry for recovery.
        let status = store.generation_mark_ready_owned(&owner, lifecycle.id())?;
        if status.phase != Phase::Running || !status.preparation_ready {
            lifecycle.release();
            return Ok(Outcome::Existing(status));
        }
        Ok(Outcome::Ready(ReadyOllama::new(owner, lifecycle, prepared, config, status, snapshot, variables)))
    })?;
    Ok(match outcome {
        Outcome::Ready(ready) => ReadyPreparation::Ready(ready),
        Outcome::Existing(status) => ReadyPreparation::Existing(status),
        Outcome::Failed { error, status } => ReadyPreparation::Failed { error, status },
    })
}

#[cfg(test)]
pub(crate) fn prepare_reserved_ollama(store: &mut PersistentStore, ticket: &Ticket) -> StoreResult<PreparedGeneration> {
    let outcome = with_preparation(store, ticket, |store, owner, prepared, snapshot, variables| {
        assert!(variables.assignments.is_empty(), "preparation-only test entry cannot own variable effects");
        let status = store.generation_mark_prepared(&owner)?;
        if status.phase != Phase::Running || !status.preparation_ready { return Ok(Outcome::Existing(status)); }
        Ok(Outcome::Ready(PreparedGeneration::Ready { owner, prepared, read_revision: snapshot.history.revision, status }))
    })?;
    Ok(match outcome {
        Outcome::Ready(ready) => ready,
        Outcome::Existing(status) => PreparedGeneration::Existing(status),
        Outcome::Failed { error, status } => PreparedGeneration::Failed { error, status },
    })
}

#[cfg(test)]
std::thread_local! {
    pub(crate) static BEFORE_PREPARATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
