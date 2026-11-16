use super::{inputs::{effective_settings, text, truth, OllamaIntent, PreparationError, PreparationSnapshot}, operation::{Owner, Phase, Start, Status, Ticket}, prepare::PreparedSend, preparation_effects::PreparedVariablePlan};
use super::operation::{GenerationLease, NormalizationWitness, NormalizationResolution, NormalizationProjectionResult, OwnerStatus, DispatchState, OutputState, ActiveGeneration};
use super::{ollama_transport::{Think, ThinkLevel}, owner::{self, Lifecycle, OwnerHost, ReadyOllama, ReadyOllamaConfig}};
use crate::persistent_store::{generation::{CancelReason, TerminalDecision}, sync_selection::SyncTarget, PersistentStore, StoreError, StoreResult};
use serde_json::Value;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};
#[cfg(test)]
use super::prepare::{prepare_ollama_owned, PreparedOllamaTurn};
#[cfg(test)]
use std::{collections::BTreeMap, sync::{Mutex, OnceLock, TryLockError, Weak}};

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

#[cfg(test)]
enum Outcome<T> {
    Ready(T),
    Existing(Status),
    Failed { error: PreparationError, status: Status },
}

#[derive(Debug)]
pub(crate) struct PreparationServiceError {
    pub(crate) source: StoreError,
    pub(crate) diagnostic: Option<PreparationError>,
}

pub(crate) type PreparationServiceResult<T> = Result<T, PreparationServiceError>;

impl From<StoreError> for PreparationServiceError {
    fn from(source: StoreError) -> Self { Self { source, diagnostic: None } }
}

struct PreparationDiagnostic {
    known: bool,
    discovered: Option<PreparationError>,
}
impl PreparationDiagnostic {
    fn record(&mut self, error: &PreparationError) {
        if !self.known && self.discovered.is_none() { self.discovered = Some(error.clone()); }
    }
}

// Known diagnostics remain in the awaiting owner even if the whole blocking task panics.
// A diagnostic first discovered inside is handed back independently of the store acknowledgement.
async fn preparing_segment<T: Send + 'static>(access: &Arc<dyn owner::OwnerStore>, known: Option<PreparationError>,
    work: impl FnOnce(&mut PersistentStore, &mut PreparationDiagnostic) -> StoreResult<T> + Send + 'static)
    -> PreparationServiceResult<T> {
    let access = access.clone();
    let already_known = known.is_some();
    let handle = tokio::task::spawn_blocking(move || {
        let mut diagnostic = PreparationDiagnostic { known: already_known, discovered: None };
        let mut work = Some(work);
        let mut output = None;
        let access_result = access.segment(&mut |store| {
            if let Some(work) = work.take() { output = Some(work(store, &mut diagnostic)); }
        });
        let result = access_result.and_then(|()| output.unwrap_or_else(|| Err(StoreError::Validation { message: "native-generation:segment-skipped".into() })));
        (result, diagnostic.discovered)
    });
    let (result, discovered) = match handle.await {
        Ok(result) => result,
        Err(_) => (Err(StoreError::Validation { message: "native-generation:segment-failed".into() }), None),
    };
    result.map_err(|source| PreparationServiceError { source, diagnostic: known.or(discovered) })
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
        let preparing = store.generation_preparing(ticket)?;
        let output = if preparing.is_some() && status.pending_commit { OutputState::Unavailable } else { OutputState::None };
        return Ok(OwnerStatus { status, owned: preparing.is_some(), dispatch: DispatchState::None, output,
            cancel_decision: preparing.and_then(|preparing| preparing.cancel), failure: None });
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

#[cfg(test)]
fn failed<T>(store: &mut PersistentStore, owner: &Owner, error: PreparationError) -> StoreResult<Outcome<T>> {
    let status = store.generation_fail_preparation(owner, error.clone())?;
    Ok(if status.phase == Phase::Failed { Outcome::Failed { error, status } } else { Outcome::Existing(status) })
}

#[cfg(test)]
fn failed_with_variables<T>(store: &mut PersistentStore, owner: &Owner, error: PreparationError, variables: &PreparedVariablePlan) -> StoreResult<Outcome<T>> {
    let status = store.generation_fail_preparation_with_variables(owner, error.clone(), variables)?;
    Ok(if status.phase == Phase::Failed { Outcome::Failed { error, status } } else { Outcome::Existing(status) })
}

// The start trigger stopped the send: its writes are stored as a failed preparation stores them, and the send ends
// Cancelled with no request and no error.
#[cfg(test)]
fn stopped_with_variables<T>(store: &mut PersistentStore, owner: &Owner, variables: &PreparedVariablePlan) -> StoreResult<Outcome<T>> {
    Ok(Outcome::Existing(store.generation_stop_preparation_with_variables(owner, variables)?))
}

// This gate owns preparation execution, while the independent store gate admits cancellable segments.
#[cfg(test)]
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
#[cfg(test)]
fn with_preparation<T>(store: &mut PersistentStore, ticket: &Ticket,
    ready: impl FnOnce(&mut PersistentStore, Owner, PreparedOllamaTurn, PreparationSnapshot, PreparedVariablePlan) -> StoreResult<Outcome<T>>) -> StoreResult<Outcome<T>> {
    store.waiting_for_admission(|store| prepare_under_gate(store, ticket, ready))
}

#[cfg(test)]
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
        Ok(PreparedSend::Turn(prepared)) => prepared,
        Ok(PreparedSend::Stopped) => return stopped_with_variables(store, &owner, &attempt.variables),
        Err(error) => return failed_with_variables(store, &owner, error, &attempt.variables),
    };
    ready(store, owner, prepared, snapshot, attempt.variables)
}


struct PreparingSession {
    owner: Owner,
    lifecycle: Lifecycle,
    operation: super::memory_http::Operation,
}

struct PreparationAbandonment {
    deny: super::ollama_decode::Cancellation,
    work: super::ollama_decode::Cancellation,
    armed: bool,
}
impl Drop for PreparationAbandonment {
    fn drop(&mut self) {
        if self.armed {
            self.deny.cancel_as(super::ollama_decode::CancelOrigin::Owner);
            self.work.cancel_as(super::ollama_decode::CancelOrigin::Owner);
        }
    }
}

enum PreparingBegin { Started(PreparingSession), Existing(Status) }
enum CapturedPreparation { Snapshot { session: PreparingSession, snapshot: PreparationSnapshot }, Finished(ReadyPreparation) }
enum CompletedPreparation {
    Promote { session: PreparingSession, payload: PreparationPayload, captured: CapturedSettings },
    Finished(ReadyPreparation),
}

// Payload data always precedes the admitted wrapper and its operation in destruction order.
struct PreparationPayload {
    value: super::preparation_owner::CompletionValue,
    retention: super::preparation_owner::PreparationRetention,
}

fn owner_error(error: &super::preparation_owner::OwnerError) -> PreparationError {
    use super::preparation_owner::OwnerError;
    use risunest_ir::stage::StageError;
    use risunest_providers::embeddings::Failure;
    match error {
        OwnerError::Http(Failure::Unsupported(name)) => PreparationError::Unsupported((*name).into()),
        OwnerError::Http(Failure::Invalid(name)) => PreparationError::Invalid((*name).into()),
        OwnerError::Http(Failure::Limit(name)) => PreparationError::Invalid(format!("limit:{name}")),
        OwnerError::Http(Failure::Cancelled) | OwnerError::Stage(StageError::Cancelled) => PreparationError::Invalid("cancelled".into()),
        OwnerError::Stage(StageError::Unsupported(name)) => PreparationError::Unsupported(name.to_string()),
        OwnerError::Stage(StageError::Invalid(name)) => PreparationError::Invalid(name.to_string()),
        OwnerError::Stage(StageError::Limit(name)) => PreparationError::Invalid(format!("limit:{name}")),
        OwnerError::Join => PreparationError::Invalid("preparation-worker-join".into()),
        _ => PreparationError::Invalid("preparation-owner".into()),
    }
}

fn retained_attempt(value: &super::preparation_owner::CompletionValue) -> Option<(&PreparationSnapshot, &super::prepare::PreparationAttempt)> {
    use super::preparation_owner::{CompletionValue, FailureState, TaskValue};
    match value {
        CompletionValue::Prepared { snapshot, attempt }
        | CompletionValue::Failed { state: FailureState::Prepared { snapshot, attempt }, .. } => Some((snapshot, attempt)),
        CompletionValue::Failed { state: FailureState::Task(output), .. } => match &output.value {
            TaskValue::Finalized { snapshot, attempt } => Some((snapshot, attempt)),
            _ => None,
        },
        _ => None,
    }
}

fn retained_error(value: &super::preparation_owner::CompletionValue) -> PreparationError {
    use super::preparation_owner::{CompletionValue, FailureState, TaskValue, Work, HttpResume};
    use super::prepare::CoreYield;
    if let Some((_, attempt)) = retained_attempt(value) {
        if let Err(error) = &attempt.result { return error.clone(); }
    }
    if let CompletionValue::Raw(raw) = value {
        return raw.diagnostic().unwrap_or_else(|| PreparationError::Invalid("preparation-finalization-context".into()));
    }
    let CompletionValue::Failed { error, state } = value else { return PreparationError::Invalid("preparation-terminal".into()) };
    let raw = match state {
        FailureState::Raw(raw) | FailureState::Progress(CoreYield::Done(raw)) | FailureState::Work(Work::Finalize { raw, .. }) => Some(raw),
        FailureState::Task(output) => match &output.value {
            TaskValue::Built { start: Err(error), .. } => return error.clone(),
            TaskValue::Built { start: Ok(start), .. } => match &start.progress { CoreYield::Done(raw) => Some(raw), _ => None },
            TaskValue::Core(CoreYield::Done(raw)) | TaskValue::Http { result: HttpResume::Resumed { progress: CoreYield::Done(raw), .. }, .. }
                | TaskValue::Rejected { work: Work::Finalize { raw, .. }, .. } => Some(raw),
            _ => None,
        },
        _ => None,
    };
    raw.and_then(super::prepare::RawMachineCompletion::diagnostic).unwrap_or_else(|| owner_error(error))
}

fn completion_diagnostic(value: &super::preparation_owner::CompletionValue) -> Option<PreparationError> {
    use super::preparation_owner::CompletionValue;
    match value {
        CompletionValue::Prepared { attempt, .. } => attempt.result.as_ref().err().cloned(),
        CompletionValue::Raw(_) | CompletionValue::Failed { .. } => Some(retained_error(value)),
    }
}

fn initialization_failure(value: &super::preparation_owner::CompletionValue) -> bool {
    use super::preparation_owner::{CompletionValue, FailureState, TaskValue, Work, OwnerError};
    use risunest_providers::embeddings::Failure;
    match value {
        CompletionValue::Failed { error: OwnerError::Http(Failure::Limit(_) | Failure::Invalid(_) | Failure::Unsupported(_)), state: FailureState::Work(Work::Build(_)) } => true,
        CompletionValue::Failed { state: FailureState::Task(output), .. } => matches!(&output.value,
            TaskValue::Built { start: Err(_), .. }
                | TaskValue::Rejected { work: Work::Build(_), error: Failure::Limit(_) | Failure::Invalid(_) | Failure::Unsupported(_) }),
        _ => false,
    }
}

fn joined_preparing_control(store: &mut PersistentStore, session: &PreparingSession)
    -> StoreResult<Option<crate::persistent_store::generation::PreparingOwnership>> {
    // Only callbacks with no outstanding machine/IO enter here. Generic abandonment never records a joined decision.
    session.lifecycle.retain_joined_preparing_hard();
    let result = session.lifecycle.flush_preparing(store);
    if result.is_err() { session.lifecycle.retain_joined_preparing_hard(); }
    result
}

fn settle_preparing_hard(store: &mut PersistentStore, session: PreparingSession) -> StoreResult<Status> {
    let result = store.generation_settle_preparing_hard(&session.owner, session.lifecycle.id());
    match &result {
        Ok(status) if status.phase.terminal() && !status.pending_commit => session.lifecycle.release(),
        _ => session.lifecycle.defer_preparing_hard(),
    }
    result
}

fn settle_preparing_lost(store: &mut PersistentStore, session: PreparingSession) -> StoreResult<Status> {
    let status = store.generation_settle_preparing_lost(&session.owner, session.lifecycle.id())?;
    if status.phase.terminal() && !status.pending_commit { session.lifecycle.release(); }
    Ok(status)
}

fn admission_rejected(error: &StoreError) -> bool {
    matches!(error, StoreError::Validation { message } if message == "native-generation:preparing-cancel-pending")
}

fn rejected_preparing_admission(store: &mut PersistentStore, session: PreparingSession) -> StoreResult<ReadyPreparation> {
    let preparing = joined_preparing_control(store, &session)?;
    let status = if preparing.is_some_and(|preparing| preparing.hard_cancel) {
        settle_preparing_hard(store, session)?
    } else { settle_preparing_lost(store, session)? };
    Ok(ReadyPreparation::Existing(status))
}

fn finish_initial_failure(store: &mut PersistentStore, session: PreparingSession, error: PreparationError, diagnostic: &mut PreparationDiagnostic) -> StoreResult<ReadyPreparation> {
    diagnostic.record(&error);
    let preparing = joined_preparing_control(store, &session)?;
    if preparing.is_some_and(|preparing| preparing.hard_cancel) {
        let status = settle_preparing_hard(store, session)?;
        return Ok(ReadyPreparation::Existing(status));
    }
    let guard = session.lifecycle.preparing_guard();
    let status = match store.generation_complete_preparing_guarded(&session.owner, session.lifecycle.id(),
        crate::persistent_store::generation::PreparingOutcome::Failed(&error), &PreparedVariablePlan::default(), &guard) {
        Ok(status) => status,
        Err(error) if admission_rejected(&error) => return rejected_preparing_admission(store, session),
        Err(error) => return Err(error),
    };
    if status.phase.terminal() && !status.pending_commit { session.lifecycle.release(); }
    Ok(ReadyPreparation::Failed { error: status.preparation_error.clone().unwrap_or(error), status })
}

fn finish_completion(store: &mut PersistentStore, session: PreparingSession, payload: PreparationPayload, diagnostic: &mut PreparationDiagnostic) -> StoreResult<CompletedPreparation> {
    use crate::persistent_store::generation::PreparingOutcome;
    use super::preparation_owner::CompletionValue;
    // External requests are durable before any new prefix. Internal driver cleanup is not such a request.
    let preparing = joined_preparing_control(store, &session)?;
    if preparing.is_some_and(|preparing| preparing.hard_cancel) {
        let status = settle_preparing_hard(store, session)?;
        return Ok(CompletedPreparation::Finished(ReadyPreparation::Existing(status)));
    }
    let Some((snapshot, attempt)) = retained_attempt(&payload.value) else {
        let error = retained_error(&payload.value);
        if initialization_failure(&payload.value) { return finish_initial_failure(store, session, error, diagnostic).map(CompletedPreparation::Finished); }
        // A raw or suspended carrier has no admitted conversion left. Retain it through exact recovery.
        let status = settle_preparing_lost(store, session)?;
        return Ok(CompletedPreparation::Finished(ReadyPreparation::Failed { error: status.preparation_error.clone().unwrap_or(error), status }));
    };
    let outer_failed = matches!(&payload.value, CompletionValue::Failed { .. });
    let captured = if !outer_failed && matches!(&attempt.result, Ok(PreparedSend::Turn(_))) {
        Some(effective_settings(snapshot).and_then(|settings| capture_settings(&settings)))
    } else { None };
    let error = match (&attempt.result, outer_failed, &captured) {
        (Err(error), _, _) => Some(error.clone()),
        (Ok(PreparedSend::Stopped), _, _) => None,
        (_, true, _) => Some(retained_error(&payload.value)),
        (_, _, Some(Err(error))) => Some(error.clone()),
        _ => None,
    };
    if let Some(error) = &error { diagnostic.record(error); }
    let outcome = match &error {
        Some(error) => PreparingOutcome::Failed(error),
        None if matches!(&attempt.result, Ok(PreparedSend::Stopped)) => PreparingOutcome::Stopped,
        None => PreparingOutcome::Ready,
    };
    let guard = session.lifecycle.preparing_guard();
    let status = match store.generation_complete_preparing_guarded(&session.owner, session.lifecycle.id(), outcome, &attempt.variables, &guard) {
        Ok(status) => status,
        Err(error) if admission_rejected(&error) => return rejected_preparing_admission(store, session).map(CompletedPreparation::Finished),
        Err(error) => return Err(error),
    };
    if let Some(error) = error {
        if status.phase.terminal() && !status.pending_commit { session.lifecycle.release(); }
        return Ok(CompletedPreparation::Finished(ReadyPreparation::Failed { error: status.preparation_error.clone().unwrap_or(error), status }));
    }
    if status.phase.terminal() || status.pending_commit {
        if status.phase.terminal() && !status.pending_commit { session.lifecycle.release(); }
        return Ok(CompletedPreparation::Finished(ReadyPreparation::Existing(status)));
    }
    let Some(Ok(captured)) = captured else { unreachable!("only a real configured turn can be promoted") };
    Ok(CompletedPreparation::Promote { session, payload, captured })
}

fn promote_completion(store: &mut PersistentStore, session: PreparingSession, payload: PreparationPayload, captured: CapturedSettings, host: &OwnerHost) -> StoreResult<ReadyPreparation> {
    // Recheck pending external hard requests after the prefix segment and before same-ID promotion.
    let preparing = joined_preparing_control(store, &session)?;
    if preparing.is_some_and(|preparing| preparing.hard_cancel) {
        let status = settle_preparing_hard(store, session)?;
        return Ok(ReadyPreparation::Existing(status));
    }
    let guard = session.lifecycle.preparing_guard();
    let status = match store.generation_promote_preparing_guarded(&session.owner, session.lifecycle.id(), &guard) {
        Ok(status) => status,
        Err(error) if admission_rejected(&error) => return rejected_preparing_admission(store, session),
        Err(error) => return Err(error),
    };
    #[cfg(test)]
    if promotion_fails(&session.owner.ticket) { return Err(StoreError::Validation { message: "native-generation:synthetic-promotion-failure".into() }); }
    if status.phase != Phase::Running || !status.preparation_ready || !session.lifecycle.observe_execution(store)? {
        if status.phase.terminal() && !status.pending_commit { session.lifecycle.release(); }
        return Ok(ReadyPreparation::Existing(status));
    }
    #[cfg(test)]
    super::owner_tests::ready_handoff_observed();
    joined_preparing_control(store, &session)?;
    if let Some(reason) = store.generation_execution(&session.owner.ticket)?.and_then(|execution| execution.cancel) {
        let decision = TerminalDecision::Cancelled { reason };
        return match store.generation_settle_owned(&session.owner, decision.clone()) {
            Ok(status) => { session.lifecycle.release(); Ok(ReadyPreparation::Existing(status)) }
            Err(error) => { session.lifecycle.defer_decision(decision); Err(error) }
        };
    }
    let PreparationPayload { value, retention } = payload;
    let super::preparation_owner::CompletionValue::Prepared { snapshot, attempt } = value else { unreachable!("failed carriers never promote") };
    let Ok(PreparedSend::Turn(prepared)) = attempt.result else { unreachable!("only an actual turn promotes") };
    let config = ReadyOllamaConfig::new(captured.host, captured.model, captured.think, captured.model_label, host);
    let mut ready = ReadyOllama::new(session.owner, session.lifecycle, prepared, config, status, snapshot, attempt.variables);
    ready.retain_preparation(retention);
    Ok(ReadyPreparation::Ready(ready))
}

// The command supervisor's entry. `detached` is set by an execute caller that left before its lifecycle signal could
// reach a registered lifecycle; begin then starts nothing.
pub(crate) async fn prepare_ready_ollama_async(ticket: Ticket, host: OwnerHost, access: Arc<dyn owner::OwnerStore>, detached: Arc<AtomicBool>) -> PreparationServiceResult<ReadyPreparation> {
    prepare_ready_ollama_async_limited(ticket, host, access, detached, owner::RegistryLimits::PRODUCTION).await
}

pub(super) async fn prepare_ready_ollama_async_limited(ticket: Ticket, host: OwnerHost, access: Arc<dyn owner::OwnerStore>, detached: Arc<AtomicBool>, limits: owner::RegistryLimits) -> PreparationServiceResult<ReadyPreparation> {
    let begin = owner::joined(owner::segment(&access, move |store| {
        owner::reconcile_limited(store, limits.pass, &mut owner::resolve_owner)?;
        if owner::registered(&owner::scope(store)?, &ticket) { return Ok(PreparingBegin::Existing(owner::resolve_owner(store, &ticket)?)); }
        if store.generation_status(&ticket)?.phase != Phase::Reserved { return Ok(PreparingBegin::Existing(owner::resolve_owner(store, &ticket)?)); }
        let slot = owner::admit(store, limits.capacity)?;
        let owner = Owner { ticket: ticket.clone(), session: super::operation::runtime_session_id().into(), token: uuid::Uuid::new_v4().to_string() };
        let live = Arc::new(());
        let operation = super::preparation_owner::admit(live.clone())
            .map_err(|_| StoreError::Validation { message: "native-generation:preparation-owner-admission".into() })?;
        let lifecycle = Lifecycle::register_preparing(slot, &owner, live, operation.cancel().clone())?;
        // Read after registration: a caller that left earlier set the flag before signalling.
        if detached.load(Ordering::SeqCst) { lifecycle.release(); return Ok(PreparingBegin::Existing(store.generation_status(&ticket)?)); }
        match store.generation_begin_preparing(&owner, lifecycle.id())? {
            Start::Started { .. } => Ok(PreparingBegin::Started(PreparingSession { owner, lifecycle, operation })),
            Start::Existing(status) => { lifecycle.release(); Ok(PreparingBegin::Existing(status)) }
        }
    })).await?;
    let session = match begin { PreparingBegin::Started(session) => session, PreparingBegin::Existing(status) => return Ok(ReadyPreparation::Existing(status)) };
    let mut abandonment = PreparationAbandonment { deny: session.lifecycle.preparing_cancel(), work: session.operation.cancel().clone(), armed: true };
    let result = prepare_started_ollama(session, host, access).await;
    if result.is_ok() { abandonment.armed = false; }
    result
}

async fn prepare_started_ollama(session: PreparingSession, host: OwnerHost, access: Arc<dyn owner::OwnerStore>) -> PreparationServiceResult<ReadyPreparation> {
    let captured = preparing_segment(&access, None, move |store, diagnostic| {
        store.waiting_for_admission(|store| {
            let preparing = joined_preparing_control(store, &session)?;
            if preparing.is_some_and(|preparing| preparing.hard_cancel) {
                let status = settle_preparing_hard(store, session)?;
                return Ok(CapturedPreparation::Finished(ReadyPreparation::Existing(status)));
            }
            let guard = session.lifecycle.preparing_guard();
            match store.generation_ensure_history_ids_guarded(&session.owner, &guard) {
                Ok(status) if status.phase != Phase::Running => { if !status.pending_commit { session.lifecycle.release(); } return Ok(CapturedPreparation::Finished(ReadyPreparation::Existing(status))); }
                Ok(_) => {}
                Err(GenerationError::Preparation(error)) => return finish_initial_failure(store, session, error, diagnostic).map(CapturedPreparation::Finished),
                Err(GenerationError::Store(error)) if admission_rejected(&error) => return rejected_preparing_admission(store, session).map(CapturedPreparation::Finished),
                Err(GenerationError::Store(error)) => return Err(error),
            }
            let Some(snapshot) = store.generation_owned_preparation_snapshot(&session.owner)? else {
                let status = store.generation_status(&session.owner.ticket)?; if status.phase.terminal() && !status.pending_commit { session.lifecycle.release(); }
                return Ok(CapturedPreparation::Finished(ReadyPreparation::Existing(status)));
            };
            Ok(CapturedPreparation::Snapshot { session, snapshot })
        })
    }).await?;
    let (session, snapshot) = match captured { CapturedPreparation::Snapshot { session, snapshot } => (session, snapshot), CapturedPreparation::Finished(result) => return Ok(result) };
    let resources = match super::preparation_owner::Resources::no_memory(session.operation.clone(), owner::OwnerLimits::production().transport) {
        Ok(resources) => resources,
        Err(error) => {
            let error = owner_error(&super::preparation_owner::OwnerError::Http(error));
            return preparing_segment(&access, Some(error.clone()), move |store, diagnostic| finish_initial_failure(store, session, error, diagnostic)).await;
        }
    };
    let (value, retention) = super::preparation_owner::prepare(snapshot, resources).await.into_parts();
    let payload = PreparationPayload { value, retention };
    let diagnostic = completion_diagnostic(&payload.value);
    let completed = preparing_segment(&access, diagnostic, move |store, diagnostic| store.waiting_for_admission(|store| finish_completion(store, session, payload, diagnostic))).await?;
    match completed {
        CompletedPreparation::Finished(result) => Ok(result),
        CompletedPreparation::Promote { session, payload, captured } =>
            preparing_segment(&access, completion_diagnostic(&payload.value), move |store, _| store.waiting_for_admission(|store| promote_completion(store, session, payload, captured, &host))).await,
    }
}

// Configuration comes from the same snapshot as the prompt; later root or preset edits cannot reach this wire.
// A full lifecycle registry returns `native-generation:lifecycle-limit` and leaves the ticket Reserved.
// The unowned compatibility entry, kept for its preparation and owner tests; commands prepare through the async entry.
#[cfg(test)]
pub(crate) fn prepare_ready_ollama(store: &mut PersistentStore, ticket: &Ticket, host: &OwnerHost) -> StoreResult<ReadyPreparation> {
    prepare_ready_ollama_limited(store, ticket, host, owner::RegistryLimits::PRODUCTION)
}

#[cfg(test)]
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
type PromotionFault = (Ticket, tokio::sync::oneshot::Sender<()>, std::sync::mpsc::Receiver<()>);

#[cfg(test)]
fn promotion_faults() -> &'static Mutex<Vec<PromotionFault>> {
    static FAULTS: OnceLock<Mutex<Vec<PromotionFault>>> = OnceLock::new();
    FAULTS.get_or_init(Mutex::default)
}

// Fails the promotion of `ticket` once, after its commit and before its lifecycle observes it, when the returned sender
// fires.
#[cfg(test)]
pub(super) fn fail_promotion_after_commit(ticket: &Ticket) -> (tokio::sync::oneshot::Receiver<()>, std::sync::mpsc::Sender<()>) {
    let (reached, entered) = tokio::sync::oneshot::channel();
    let (release, resume) = std::sync::mpsc::channel();
    promotion_faults().lock().unwrap().push((ticket.clone(), reached, resume));
    (entered, release)
}

#[cfg(test)]
fn promotion_fails(ticket: &Ticket) -> bool {
    let fault = {
        let mut faults = promotion_faults().lock().unwrap();
        faults.iter().position(|(entry, ..)| entry == ticket).map(|index| faults.remove(index))
    };
    let Some((_, reached, resume)) = fault else { return false };
    let _ = reached.send(());
    let _ = resume.recv();
    true
}

#[cfg(test)]
std::thread_local! {
    pub(crate) static BEFORE_PREPARATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
