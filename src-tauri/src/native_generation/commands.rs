use super::{
    cbs::HostCapture,
    inputs::{OllamaIntent, PreparationError},
    operation::{
        DispatchState, GenerationLease, NormalizationEvidence, NormalizationMapping, NormalizationReceipt, NormalizationWitness,
        NormalizedResidentRow, OriginalIdState, OriginalRowWitness, OutputState, OwnerStatus, Phase, ProjectionIdentity, Status, Target, Ticket,
    },
    owner::{self, OwnerHost, OwnerLimits, OwnerStore, ReadyOllama},
    service::{self, GenerationError, ReadyPreparation},
};
use crate::native_log::logged;
use crate::persistent_store::{
    commands::with_store_if_ready,
    generation::{CancelReason, Execution, FailureDescriptor, OutputReceipt, TerminalDecision},
    PersistentStore, PersistentStoreState, StoreError, StoreResult,
};
use risunest_sync_wire::{stamp::DecimalU64, unit::UnitKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex, MutexGuard, OnceLock, PoisonError},
    time::Duration,
};
use tauri::{AppHandle, Manager, Runtime};
use tokio::sync::watch;

// Caps the requested change-notification wait, not store access or terminal publication work.
pub(crate) const MAX_STATUS_WAIT_MS: u64 = 25_000;
// One per reserved, running or recently settled ticket; journals keep at most 16 records each.
const MAX_OPERATIONS: usize = 64;
const MAINTENANCE_POLL: Duration = Duration::from_millis(100);
const SETTLE_ATTEMPTS: usize = 40;
// The text the WebView fetch rejects with when the connection fails, which the TS route shows.
const NETWORK_MESSAGE: &str = "Failed to fetch";

fn invalid(code: &str) -> StoreError {
    StoreError::Validation { message: format!("native-generation:{code}") }
}

fn is_code(error: &StoreError, code: &str) -> bool {
    matches!(error, StoreError::Validation { message } if message.strip_prefix("native-generation:") == Some(code))
}

fn ticket_gone(error: &StoreError) -> bool {
    is_code(error, "expired-ticket") || is_code(error, "unknown-ticket")
}

// ---- Frozen command DTOs ----

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReserveRequest {
    character_id: String,
    conversation_id: String,
    expected_revision: i64,
    library_identity: String,
    binding_authority: DecimalU64,
    effective_preset_id: String,
    #[serde(default)]
    host: Option<HostCapture>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub(crate) enum Reservation {
    Ticket { ticket: Ticket },
    Unsupported { unsupported: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RouteDispatch { None, Issued, Uncertain }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RouteOutput { None, Committed, Unavailable }

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteFailure {
    pub(crate) category: String,
    pub(crate) field: Option<String>,
    pub(crate) stage: Option<String>,
    pub(crate) http_status: Option<u16>,
    pub(crate) sdk_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteStatus {
    pub(crate) ticket: Ticket,
    pub(crate) character_id: String,
    pub(crate) conversation_id: String,
    pub(crate) phase: Phase,
    pub(crate) message_id: String,
    pub(crate) checkpoint: DecimalU64,
    pub(crate) committed_revision: Option<i64>,
    pub(crate) pending_commit: bool,
    pub(crate) preparation_ready: bool,
    pub(crate) preparation_error: Option<PreparationError>,
    pub(crate) dispatch: RouteDispatch,
    pub(crate) output: RouteOutput,
    pub(crate) cancel_decision: Option<CancelReason>,
    pub(crate) failure: Option<RouteFailure>,
    pub(crate) diagnostics_available: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StatusObservation {
    phase: Phase,
    dispatch: RouteDispatch,
    checkpoint: DecimalU64,
    committed_revision: Option<i64>,
    pending_commit: bool,
    preparation_ready: bool,
    output: RouteOutput,
    cancel_decision: Option<CancelReason>,
    diagnostics_available: bool,
}

impl From<&RouteStatus> for StatusObservation {
    fn from(status: &RouteStatus) -> Self {
        Self { phase: status.phase.clone(), dispatch: status.dispatch, checkpoint: status.checkpoint,
            committed_revision: status.committed_revision, pending_commit: status.pending_commit,
            preparation_ready: status.preparation_ready, output: status.output, cancel_decision: status.cancel_decision,
            diagnostics_available: status.diagnostics_available }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Lease {
    token: String,
    revision: i64,
}

impl From<Lease> for GenerationLease {
    fn from(value: Lease) -> Self { Self { token: value.token, revision: value.revision } }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum IdState { Missing, Null, Present }

impl From<&OriginalIdState> for IdState {
    fn from(value: &OriginalIdState) -> Self {
        match value { OriginalIdState::Missing => Self::Missing, OriginalIdState::Null => Self::Null, OriginalIdState::Present => Self::Present }
    }
}

impl From<IdState> for OriginalIdState {
    fn from(value: IdState) -> Self {
        match value { IdState::Missing => Self::Missing, IdState::Null => Self::Null, IdState::Present => Self::Present }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WitnessIdentity { store_id: String, library_epoch: String, generation: String, selection_epoch: String }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WitnessRow { original_index: usize, row_hash: String, id_state: IdState }

// Handed back unchanged by the caller; the store revalidates every member against its own record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Witness {
    ticket: Ticket,
    target: Target,
    identity: WitnessIdentity,
    original_revision: i64,
    original_count: usize,
    rows: Vec<WitnessRow>,
}

impl From<NormalizationWitness> for Witness {
    fn from(value: NormalizationWitness) -> Self {
        let ProjectionIdentity { store_id, library_epoch, generation, selection_epoch } = value.identity;
        Self {
            ticket: value.ticket, target: value.target, identity: WitnessIdentity { store_id, library_epoch, generation, selection_epoch },
            original_revision: value.original_revision, original_count: value.original_count,
            rows: value.rows.iter().map(|row| WitnessRow { original_index: row.original_index, row_hash: row.row_hash.clone(), id_state: (&row.id_state).into() }).collect(),
        }
    }
}

impl From<Witness> for NormalizationWitness {
    fn from(value: Witness) -> Self {
        let WitnessIdentity { store_id, library_epoch, generation, selection_epoch } = value.identity;
        Self {
            ticket: value.ticket, target: value.target, identity: ProjectionIdentity { store_id, library_epoch, generation, selection_epoch },
            original_revision: value.original_revision, original_count: value.original_count,
            rows: value.rows.into_iter().map(|row| OriginalRowWitness { original_index: row.original_index, row_hash: row.row_hash, id_state: row.id_state.into() }).collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NormalizationInput {
    witness: Witness,
    original: Lease,
    current: Lease,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NormalizationReceiptProof { request_id: String, intent_digest: String, receipt_revision: i64 }

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OutputReceiptProof { request_id: String, intent_digest: String, receipt_revision: i64, source_sequence: DecimalU64, output_hash: String }

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProofReceipts {
    pub(crate) normalization: Option<NormalizationReceiptProof>,
    pub(crate) effects: Vec<Value>,
    pub(crate) output: Option<OutputReceiptProof>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IdDelta { original_index: usize, current_index: usize, original_row_hash: String, original_id_state: IdState, assigned_id: String }

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Proof {
    pub(crate) ticket: Ticket,
    pub(crate) revision: i64,
    pub(crate) at: &'static str,
    pub(crate) affected_unit_keys: Vec<String>,
    pub(crate) output_message_id: Option<String>,
    pub(crate) receipts: ProofReceipts,
    pub(crate) id_deltas: Option<Vec<IdDelta>>,
    pub(crate) mapping_unavailable: bool,
    pub(crate) failed_with_output: bool,
}

// ---- Host registry ----

#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetIds { character_id: String, conversation_id: String, library_identity: String }

#[derive(Debug, Clone)]
struct LiveDiagnostic { failure: FailureDescriptor, sdk_message: Option<String> }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Run { Reserved, Running, Finished }

struct Operation { scope: String, ticket: Ticket, target: Option<TargetIds>, run: Run, diagnostic: Option<LiveDiagnostic>, diagnostic_ready: bool, claim_id: u64, order: u64 }

#[derive(Default)]
struct Registry { operations: Vec<Operation>, order: u64 }

fn registry() -> MutexGuard<'static, Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default).lock().unwrap_or_else(PoisonError::into_inner)
}

impl Registry {
    fn find(&mut self, scope: &str, ticket: &Ticket) -> Option<&mut Operation> {
        self.operations.iter_mut().find(|operation| operation.scope == scope && &operation.ticket == ticket)
    }

    // Settled entries go first, then reservations that never ran; a running owner is never evicted.
    fn insert(&mut self, scope: &str, ticket: &Ticket, target: Option<TargetIds>, run: Run) -> StoreResult<()> {
        if self.operations.len() >= MAX_OPERATIONS {
            let evicted = [Run::Finished, Run::Reserved].iter().find_map(|kind| self.operations.iter().enumerate()
                .filter(|(_, operation)| operation.run == *kind).min_by_key(|(_, operation)| operation.order).map(|(index, _)| index));
            let Some(index) = evicted else { return Err(invalid("host-limit")) };
            self.operations.remove(index);
        }
        self.order += 1;
        self.operations.push(Operation { scope: scope.to_owned(), ticket: ticket.clone(), target, run, diagnostic: None, diagnostic_ready: false, claim_id: 0, order: self.order });
        Ok(())
    }
}

// Update first, then wake readers after releasing the registry lock.
fn update_operation(scope: &str, ticket: &Ticket, claim_id: u64, update: impl FnOnce(&mut Operation) -> bool) {
    let notify = {
        let mut registry = registry();
        registry.find(scope, ticket).filter(|operation| operation.claim_id == claim_id).is_some_and(update)
    };
    if notify { changed(); }
}

// The claiming job owns this guard before returning across an await. If its caller
// disappears during preparation, no terminal reader waits for a supervisor that never started.
struct RunClaim { scope: String, ticket: Ticket, id: u64 }

impl Drop for RunClaim {
    fn drop(&mut self) {
        update_operation(&self.scope, &self.ticket, self.id, |operation| {
            if operation.run != Run::Running { return false; }
            operation.run = Run::Reserved;
            true
        });
    }
}

// ---- Change notification for the status long poll ----

fn changes() -> &'static watch::Sender<u64> {
    static CHANGES: OnceLock<watch::Sender<u64>> = OnceLock::new();
    CHANGES.get_or_init(|| watch::channel(0).0)
}

fn changed() {
    changes().send_modify(|value| *value = value.wrapping_add(1));
}

// ---- Store access ----

fn owner_host() -> &'static OnceLock<OwnerHost> {
    static HOST: OnceLock<OwnerHost> = OnceLock::new();
    &HOST
}

// Every segment is admitted on its own; nothing is held across the provider wait.
fn with_job_store<R: Runtime, T>(app: &AppHandle<R>, work: impl FnOnce(&mut PersistentStore) -> StoreResult<T>) -> StoreResult<T> {
    let state = app.state::<PersistentStoreState>();
    let operation = state.admit_native_job_operation()?;
    let mut store = state.open_admitted_native_job_store(&operation)?;
    work(&mut store)
}

async fn blocking<R: Runtime, T: Send + 'static>(app: &AppHandle<R>, work: impl FnOnce(&mut PersistentStore) -> StoreResult<T> + Send + 'static) -> StoreResult<T> {
    let app = app.clone();
    tokio::task::spawn_blocking(move || with_job_store(&app, work)).await.unwrap_or_else(|_| Err(invalid("worker-stopped")))
}

struct AdmittedStore<R: Runtime> { app: AppHandle<R> }

impl<R: Runtime> OwnerStore for AdmittedStore<R> {
    fn segment(&self, work: &mut dyn FnMut(&mut PersistentStore)) -> StoreResult<()> {
        let result = with_job_store(&self.app, |store| { work(store); Ok(()) });
        changed();
        result
    }
}

struct ListedRecord { ticket: Ticket, target: TargetIds }

fn listed_records(store: &PersistentStore) -> StoreResult<Vec<ListedRecord>> {
    Ok(service::active_generations(store)?.into_iter().map(|record| ListedRecord {
        ticket: record.ticket,
        target: TargetIds { character_id: record.character_id, conversation_id: record.conversation_id, library_identity: record.library_identity },
    }).collect())
}

fn known(store: &PersistentStore, scope: &str, ticket: &Ticket) -> StoreResult<(TargetIds, Option<LiveDiagnostic>, bool)> {
    let target = store.generation_target(ticket)?;
    let target = TargetIds { character_id: target.character_id, conversation_id: target.conversation_id, library_identity: target.library_identity };
    let mut registry = registry();
    match registry.find(scope, ticket) {
        Some(operation) => {
            operation.target = Some(target.clone());
            Ok((target, operation.diagnostic.clone(), operation.run == Run::Running && !operation.diagnostic_ready))
        }
        None => { registry.insert(scope, ticket, Some(target.clone()), Run::Reserved)?; Ok((target, None, false)) }
    }
}

// ---- DTO conversion ----

fn simple(category: &str) -> RouteFailure {
    RouteFailure { category: category.into(), field: None, stage: None, http_status: None, sdk_message: None }
}

fn preparation_failure(error: &PreparationError) -> RouteFailure {
    let (category, field) = match error {
        PreparationError::Unsupported(field) => ("unsupported", Some(field.clone())),
        PreparationError::Invalid(field) => ("invalid", Some(field.clone())),
        PreparationError::Tokenizer(field) => ("tokenizer", Some(field.clone())),
        PreparationError::ContextLimit => ("context-limit", None),
    };
    RouteFailure { field, stage: Some("preparation".into()), ..simple(category) }
}

// `sdkMessage` is what the TS route shows for the same outcome; the live message never reaches the journal.
fn route_failure(status: &Status, descriptor: Option<&FailureDescriptor>, live: Option<&LiveDiagnostic>) -> Option<RouteFailure> {
    if let Some(descriptor) = descriptor {
        let sdk_message = match descriptor.category.as_str() {
            "network" => Some(NETWORK_MESSAGE.to_owned()),
            "http" | "sdk" | "decode" => live.filter(|live| &live.failure == descriptor).and_then(|live| live.sdk_message.clone()),
            "type" => live.filter(|live| &live.failure == descriptor).and_then(|live| live.sdk_message.clone()).or_else(|| {
                match descriptor.detail.as_deref() {
                    Some("trim") if descriptor.stage.as_deref() == Some("output") => Some("data.trim is not a function"),
                    Some("indexOf") => Some("text.indexOf is not a function"),
                    Some("substring") => Some("text.substring is not a function"),
                    Some("message-get") => Some("Cannot read properties of null (reading 'message')"),
                    Some("object-to-primitive") => Some("Cannot convert object to primitive value"),
                    _ => None,
                }.map(str::to_owned)
            }),
            _ => None,
        };
        return Some(RouteFailure { category: descriptor.category.clone(), field: descriptor.detail.clone(), stage: descriptor.stage.clone(),
            http_status: descriptor.http_status, sdk_message });
    }
    match (&status.phase, &status.preparation_error) {
        (Phase::Failed, Some(error)) => Some(preparation_failure(error)),
        (Phase::Stale, _) => Some(simple("stale")),
        (Phase::Interrupted, _) => Some(simple("interrupted")),
        _ => None,
    }
}

fn route_status(evidence: OwnerStatus, execution: Option<&Execution>, target: TargetIds, live: Option<&LiveDiagnostic>) -> RouteStatus {
    let OwnerStatus { status, dispatch, output, cancel_decision, failure, .. } = evidence;
    // An owner or recovery that settled on a cancellation names its reason even when no caller decision was persisted.
    let cancel_decision = cancel_decision.or_else(|| match execution.and_then(|execution| execution.terminal.as_ref()) {
        Some(TerminalDecision::Cancelled { reason }) => Some(*reason),
        _ => None,
    });
    let failure = route_failure(&status, failure.as_ref(), live);
    RouteStatus {
        ticket: status.ticket, character_id: target.character_id, conversation_id: target.conversation_id, phase: status.phase,
        message_id: status.message_id, checkpoint: status.checkpoint, committed_revision: status.committed_revision,
        pending_commit: status.pending_commit, preparation_ready: status.preparation_ready, preparation_error: status.preparation_error,
        dispatch: match dispatch { DispatchState::None => RouteDispatch::None, DispatchState::Issued => RouteDispatch::Issued, DispatchState::Uncertain => RouteDispatch::Uncertain },
        output: match output { OutputState::None => RouteOutput::None, OutputState::Committed(_) => RouteOutput::Committed, OutputState::Unavailable => RouteOutput::Unavailable },
        cancel_decision, failure, diagnostics_available: live.is_some(),
    }
}

fn report(store: &PersistentStore, scope: &str, ticket: &Ticket) -> StoreResult<(RouteStatus, bool)> {
    let evidence = service::owner_status(store, ticket)?;
    let execution = store.generation_execution(ticket)?;
    let (target, live, publishing) = known(store, scope, ticket)?;
    let pending = publishing && evidence.status.phase.terminal();
    Ok((route_status(evidence, execution.as_ref(), target, live.as_ref()), pending))
}

// Settles a deferred or abandoned owner first, so a waiting record progresses without HTTP; an unresolved offer stays pending.
fn read(store: &mut PersistentStore, ticket: &Ticket) -> StoreResult<(RouteStatus, bool)> {
    match service::resolve_owner(store, ticket) {
        Ok(_) | Err(StoreError::CommitBusy) => {}
        Err(error) => return Err(error),
    }
    let scope = owner::scope(store)?;
    report(store, &scope, ticket)
}

// A terminal store write precedes the owner result. Publish it only with its live diagnostic.
async fn read_published<R: Runtime>(app: &AppHandle<R>, ticket: &Ticket) -> StoreResult<RouteStatus> {
    let mut updates = changes().subscribe();
    loop {
        let requested = ticket.clone();
        let (status, publishing) = blocking(app, move |store| read(store, &requested)).await?;
        if !publishing { return Ok(status); }
        #[cfg(test)]
        publication_observed(ticket);
        let _ = updates.changed().await;
    }
}

fn phase_name(phase: &Phase) -> &'static str {
    match phase {
        Phase::Reserved => "reserved", Phase::Running => "running", Phase::Completed => "completed", Phase::Cancelled => "cancelled",
        Phase::Stale => "stale", Phase::Interrupted => "interrupted", Phase::Failed => "failed",
    }
}

// ---- Operations ----

// The first valid value is kept for the process; later documents send the same navigator value.
pub(crate) fn host_init(user_agent: &str) -> StoreResult<()> {
    let host = OwnerHost::new(user_agent)?;
    let _ = owner_host().set(host);
    Ok(())
}

pub(crate) async fn reserve<R: Runtime>(app: AppHandle<R>, request: ReserveRequest) -> StoreResult<Reservation> {
    blocking(&app, move |store| {
        let ReserveRequest { character_id, conversation_id, expected_revision, library_identity, binding_authority, effective_preset_id, host } = request;
        let target = TargetIds { character_id: character_id.clone(), conversation_id: conversation_id.clone(), library_identity: library_identity.clone() };
        let intent = OllamaIntent { effective_preset_id, host, target: Target { character_id, conversation_id, expected_revision, library_identity, binding_authority } };
        let ticket = match service::reserve_ollama(store, &intent) {
            Ok(ticket) => ticket,
            Err(GenerationError::Preparation(PreparationError::Unsupported(field))) => return Ok(Reservation::Unsupported { unsupported: field }),
            Err(GenerationError::Preparation(PreparationError::Invalid(field))) => return Err(invalid(&format!("invalid:{field}"))),
            Err(GenerationError::Preparation(PreparationError::Tokenizer(field))) => return Err(invalid(&format!("tokenizer:{field}"))),
            Err(GenerationError::Preparation(PreparationError::ContextLimit)) => return Err(invalid("context-limit")),
            Err(GenerationError::Store(error)) => return Err(error),
        };
        let scope = owner::scope(store)?;
        registry().insert(&scope, &ticket, Some(target), Run::Reserved)?;
        Ok(Reservation::Ticket { ticket })
    }).await
}

pub(crate) async fn execute<R: Runtime>(app: AppHandle<R>, ticket: Ticket) -> StoreResult<RouteStatus> {
    let host = owner_host().get().cloned().ok_or_else(|| invalid("host-uninitialized"))?;
    let claimed = ticket.clone();
    let claim = blocking(&app, move |store| {
        let scope = owner::scope(store)?;
        // An unknown or expired ticket, or one whose target this host cannot report, starts nothing.
        if registry().find(&scope, &claimed).is_none() { read(store, &claimed)?; }
        let mut registry = registry();
        registry.order = registry.order.wrapping_add(1);
        let claim_id = registry.order;
        match registry.find(&scope, &claimed) {
            Some(operation) if operation.run != Run::Reserved => Ok(None),
            Some(operation) => {
                operation.run = Run::Running;
                operation.diagnostic_ready = false;
                operation.claim_id = claim_id;
                Ok(Some(RunClaim { scope, ticket: claimed, id: claim_id }))
            }
            None => Err(invalid("target-unavailable")),
        }
    }).await?;
    if let Some(claim) = claim {
        let scope = claim.scope.clone();
        // Armed before preparation, so maintenance that waits for it still cancels the owner.
        let maintenance = Arc::new(AtomicBool::new(false));
        #[cfg(test)]
        preparation_pause(&ticket, false).await;
        let prepared = match app.state::<PersistentStoreState>().cancel_on_device_maintenance(&maintenance) {
            Ok(()) => { let ticket = ticket.clone(); blocking(&app, move |store| service::prepare_ready_ollama(store, &ticket, &host)).await }
            Err(error) => Err(error),
        };
        changed();
        match prepared {
            Ok(ReadyPreparation::Ready(ready)) => { tokio::spawn(supervise(app.clone(), scope, ticket.clone(), ready, maintenance, claim)); }
            Ok(_) => finish(&claim, None),
            // Nothing started (a full owner registry, closed admission), so the caller can execute again or cancel.
            Err(error) => {
                #[cfg(test)]
                preparation_pause(&ticket, true).await;
                drop(claim);
                return Err(error);
            }
        }
    }
    read_published(&app, &ticket).await
}

pub(crate) async fn cancel<R: Runtime>(app: AppHandle<R>, ticket: Ticket, reason: CancelReason) -> StoreResult<RouteStatus> {
    let requested = ticket.clone();
    #[cfg(all(test, target_os = "windows"))]
    let observation = crate::ollama_ui_verification::controller::capture(&app, &ticket);
    blocking(&app, move |store| {
        #[cfg(all(test, target_os = "windows"))]
        return crate::ollama_ui_verification::adapters::cancel_scope(observation,
            || service::cancel_generation_for(store, &requested, reason));
        #[cfg(not(all(test, target_os = "windows")))]
        service::cancel_generation_for(store, &requested, reason)
    }).await?;
    changed();
    read_published(&app, &ticket).await
}

pub(crate) async fn status<R: Runtime>(app: AppHandle<R>, ticket: Ticket, wait_ms: Option<u64>) -> StoreResult<RouteStatus> {
    status_after(app, ticket, wait_ms, None).await
}

pub(crate) async fn status_after<R: Runtime>(app: AppHandle<R>, ticket: Ticket, wait_ms: Option<u64>, after: Option<StatusObservation>) -> StoreResult<RouteStatus> {
    let wait = Duration::from_millis(wait_ms.unwrap_or(0).min(MAX_STATUS_WAIT_MS));
    let deadline = tokio::time::Instant::now() + wait;
    let mut updates = changes().subscribe();
    let first = read_published(&app, &ticket).await?;
    if wait.is_zero() || first.phase.terminal() || after.as_ref().is_some_and(|after| after != &StatusObservation::from(&first)) { return Ok(first); }
    #[cfg(test)]
    status_waiting(&ticket);
    loop {
        if tokio::time::timeout_at(deadline, updates.changed()).await.is_err() {
            return read_published(&app, &ticket).await;
        }
        let latest = read_published(&app, &ticket).await?;
        if latest != first { return Ok(latest); }
    }
}

pub(crate) async fn active<R: Runtime>(app: AppHandle<R>) -> StoreResult<Vec<RouteStatus>> {
    let tickets = blocking(&app, move |store| {
        let library = store.library_identity()?;
        Ok(listed_records(store)?.into_iter().filter(|record| record.target.library_identity == library)
            .map(|record| record.ticket).collect::<Vec<_>>())
    }).await?;
    let mut statuses = Vec::new();
    for ticket in tickets {
        match read_published(&app, &ticket).await {
            Ok(status) if status.phase == Phase::Running || status.pending_commit => statuses.push(status),
            Ok(_) => {}
            Err(error) if ticket_gone(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(statuses)
}

// Leases belong to the renderer's own store, so the witness and the projection read that store, not a job store.
fn with_renderer_store<R: Runtime, T>(app: &AppHandle<R>, work: impl FnOnce(&PersistentStore) -> StoreResult<T>) -> StoreResult<T> {
    with_store_if_ready(&app.state::<PersistentStoreState>(), work)?.ok_or_else(|| invalid("store-unavailable"))
}

pub(crate) async fn capture_witness<R: Runtime>(app: AppHandle<R>, ticket: Ticket, original: Lease, indices: Vec<usize>) -> StoreResult<Witness> {
    tokio::task::spawn_blocking(move || with_renderer_store(&app, |store| {
        service::capture_normalization_witness(store, &ticket, &original.into(), &indices).map(Witness::from)
    })).await.unwrap_or_else(|_| Err(invalid("worker-stopped")))
}

fn normalization_receipt_proof(receipt: &NormalizationReceipt) -> NormalizationReceiptProof {
    NormalizationReceiptProof { request_id: receipt.request_id.clone(), intent_digest: receipt.intent_digest.clone(), receipt_revision: receipt.receipt_revision }
}

fn output_receipt_proof(receipt: &OutputReceipt) -> OutputReceiptProof {
    OutputReceiptProof { request_id: receipt.request_id.clone(), intent_digest: receipt.intent_digest.clone(), receipt_revision: receipt.receipt_revision,
        source_sequence: DecimalU64(receipt.source_sequence), output_hash: receipt.output_hash.clone() }
}

fn id_delta(row: &NormalizedResidentRow) -> IdDelta {
    IdDelta { original_index: row.original_index, current_index: row.current_index, original_row_hash: row.original_row_hash.clone(),
        original_id_state: (&row.original_id_state).into(), assigned_id: row.assigned_id.clone() }
}

pub(crate) async fn proof<R: Runtime>(app: AppHandle<R>, ticket: Ticket, normalization: Option<NormalizationInput>) -> StoreResult<Proof> {
    let mut updates = changes().subscribe();
    let (status, normalization_evidence, output, effects, revision) = loop {
        let read_ticket = ticket.clone();
        let ((status, publishing), normalization, output, effects, revision) = blocking(&app, move |store| {
            let status = read(store, &read_ticket)?;
            let normalization = service::resolve_normalization(store, &read_ticket)?.evidence;
            let output = service::owner_status(store, &read_ticket)?.output;
            let effects = store.generation_effects_evidence(&read_ticket)?;
            Ok((status, normalization, output, effects, store.revision()?))
        }).await?;
        if !publishing {
            break (status, normalization, output, effects.ok_or_else(|| invalid("proof-unavailable"))?, revision);
        }
        #[cfg(test)]
        publication_observed(&ticket);
        let _ = updates.changed().await;
    };
    let at = if status.phase.terminal() { phase_name(&status.phase) }
        else if status.dispatch == RouteDispatch::Issued { "issued" }
        else { return Err(invalid("proof-unavailable")) };
    let mapping = match normalization {
        Some(NormalizationInput { witness, original, current }) => {
            let (ticket, witness) = (ticket.clone(), NormalizationWitness::from(witness));
            let app = app.clone();
            Some(tokio::task::spawn_blocking(move || with_renderer_store(&app, |store| {
                service::project_normalization(store, &ticket, &witness, &original.into(), &current.into())
            })).await.unwrap_or_else(|_| Err(invalid("worker-stopped")))?.mapping)
        }
        None => None,
    };
    let messages = UnitKey::new(&["messages", status.character_id.as_str(), status.conversation_id.as_str()]).map_err(|_| invalid("unit-key"))?.as_str().to_owned();
    let mut affected_unit_keys = effects.affected_unit_keys;
    let (normalization_receipt, id_deltas, mapping_unavailable) = match &normalization_evidence {
        NormalizationEvidence::NoIdEffects => (None, Some(Vec::new()), false),
        NormalizationEvidence::Verified(receipt) => {
            if !affected_unit_keys.contains(&messages) { affected_unit_keys.push(messages.clone()); }
            match &mapping {
                Some(NormalizationMapping::Mapped(projection)) => (Some(normalization_receipt_proof(receipt)), Some(projection.rows.iter().map(id_delta).collect()), false),
                Some(NormalizationMapping::NoIdEffects) => (Some(normalization_receipt_proof(receipt)), Some(Vec::new()), false),
                Some(NormalizationMapping::Unavailable(_)) | None => (Some(normalization_receipt_proof(receipt)), None, true),
            }
        }
        // Effects that cannot be certified here still name their unit, so adoption refreshes it.
        NormalizationEvidence::Recorded(_) | NormalizationEvidence::Pending | NormalizationEvidence::Unknown => {
            if !affected_unit_keys.contains(&messages) { affected_unit_keys.push(messages.clone()); }
            (None, None, true)
        }
    };
    let output_receipt = match &output { OutputState::Committed(receipt) => Some(output_receipt_proof(receipt)), _ => None };
    if !matches!(output, OutputState::None) && !affected_unit_keys.contains(&messages) { affected_unit_keys.push(messages); }
    Ok(Proof {
        ticket, revision, at, affected_unit_keys,
        output_message_id: output_receipt.as_ref().map(|_| status.message_id.clone()),
        failed_with_output: status.phase == Phase::Failed && output_receipt.is_some(),
        receipts: ProofReceipts {
            normalization: normalization_receipt,
            effects: effects.receipts.into_iter().map(|receipt| serde_json::json!({
                "requestId": receipt.request_id, "intentDigest": receipt.digest, "receiptRevision": receipt.revision,
            })).collect(),
            output: output_receipt,
        },
        id_deltas, mapping_unavailable,
    })
}

// ---- Owner supervision ----

fn finish(claim: &RunClaim, diagnostic: Option<LiveDiagnostic>) {
    update_operation(&claim.scope, &claim.ticket, claim.id, |operation| {
        operation.run = Run::Finished;
        operation.diagnostic_ready = true;
        if diagnostic.is_some() { operation.diagnostic = diagnostic; }
        true
    });
}

// Admission is closed during maintenance, so the live owner hears the signal now and the decision is persisted once it reopens.
async fn cancel_for_maintenance<R: Runtime>(app: &AppHandle<R>, scope: &str, ticket: &Ticket) {
    let cancelled = { let ticket = ticket.clone(); blocking(app, move |store| service::cancel_generation_for(store, &ticket, CancelReason::Lifecycle)).await };
    if cancelled.is_err() { owner::signal(scope, ticket, CancelReason::Lifecycle); }
    changed();
}

async fn settle_later<R: Runtime>(app: &AppHandle<R>, ticket: &Ticket, lifecycle: bool) {
    let mut delay = Duration::from_millis(100);
    for _ in 0..SETTLE_ATTEMPTS {
        let ticket = ticket.clone();
        let result = blocking(app, move |store| if lifecycle {
            service::cancel_generation_for(store, &ticket, CancelReason::Lifecycle)
        } else {
            service::resolve_owner(store, &ticket)
        }).await;
        changed();
        match result {
            Ok(status) if status.phase.terminal() => return,
            Err(error) if ticket_gone(&error) => return,
            _ => {}
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(5));
    }
}

async fn supervise<R: Runtime>(app: AppHandle<R>, scope: String, ticket: Ticket, ready: ReadyOllama, maintenance: Arc<AtomicBool>, claim: RunClaim) {
    let access: Arc<dyn OwnerStore> = Arc::new(AdmittedStore { app: app.clone() });
    let mut lifecycle = maintenance.load(Ordering::Acquire);
    if lifecycle { cancel_for_maintenance(&app, &scope, &ticket).await; }
    #[cfg(all(test, target_os = "windows"))]
    let observation = crate::ollama_ui_verification::controller::capture(&app, &ticket);
    #[cfg(not(all(test, target_os = "windows")))]
    let mut running = tokio::spawn(owner::execute_ready_ollama(ready, access, OwnerLimits::production()));
    #[cfg(all(test, target_os = "windows"))]
    let mut running = tokio::spawn(async move {
        let execution = owner::execute_ready_ollama(ready, access, OwnerLimits::production());
        crate::ollama_ui_verification::adapters::owner_scope(observation, execution).await
    });
    let execution = loop {
        tokio::select! {
            joined = &mut running => break joined.ok(),
            _ = tokio::time::sleep(MAINTENANCE_POLL), if !lifecycle => {
                if maintenance.load(Ordering::Acquire) {
                    lifecycle = true;
                    cancel_for_maintenance(&app, &scope, &ticket).await;
                }
                #[cfg(test)]
                MAINTENANCE_CHECKED.fetch_add(1, Ordering::AcqRel);
            }
        }
    };
    drop(maintenance);
    let pending = execution.as_ref().is_none_or(|execution| execution.result.is_err());
    // The phase decides the outcome; the diagnostic only explains a failure and carries the live SDK text.
    let diagnostic = execution.and_then(|execution| execution.diagnostic).map(|diagnostic| LiveDiagnostic {
        failure: diagnostic.failure, sdk_message: diagnostic.message.map(|units| String::from_utf16_lossy(&units)) });
    #[cfg(test)]
    publication_pause(&ticket).await;
    update_operation(&scope, &ticket, claim.id, |operation| {
        operation.diagnostic = diagnostic.clone();
        operation.diagnostic_ready = true;
        true
    });
    if pending || lifecycle {
        settle_later(&app, &ticket, lifecycle).await;
    }
    finish(&claim, diagnostic);
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct PreparationGate {
    pub(super) reached: tokio::sync::Notify,
    pub(super) release: tokio::sync::Notify,
    pub(super) failed: tokio::sync::Notify,
    pub(super) release_failure: tokio::sync::Notify,
}

#[cfg(test)]
fn preparation_gates() -> &'static Mutex<Vec<(Ticket, Arc<PreparationGate>)>> {
    static GATES: OnceLock<Mutex<Vec<(Ticket, Arc<PreparationGate>)>>> = OnceLock::new();
    GATES.get_or_init(Mutex::default)
}

#[cfg(test)]
pub(super) fn hold_preparation(ticket: &Ticket) -> Arc<PreparationGate> {
    let gate = Arc::new(PreparationGate::default());
    preparation_gates().lock().unwrap().push((ticket.clone(), gate.clone()));
    gate
}

#[cfg(test)]
async fn preparation_pause(ticket: &Ticket, failed: bool) {
    let gate = preparation_gates().lock().unwrap().iter().find(|(entry, _)| entry == ticket).map(|(_, gate)| gate.clone());
    if let Some(gate) = gate {
        if failed {
            gate.failed.notify_one();
            gate.release_failure.notified().await;
            preparation_gates().lock().unwrap().retain(|(entry, _)| entry != ticket);
        } else {
            gate.reached.notify_one();
            gate.release.notified().await;
        }
    }
}

#[cfg(test)]
fn status_wait_observers() -> &'static Mutex<Vec<(Ticket, Arc<tokio::sync::Notify>)>> {
    static OBSERVERS: OnceLock<Mutex<Vec<(Ticket, Arc<tokio::sync::Notify>)>>> = OnceLock::new();
    OBSERVERS.get_or_init(Mutex::default)
}

#[cfg(test)]
pub(super) fn observe_status_wait(ticket: &Ticket) -> Arc<tokio::sync::Notify> {
    let observer = Arc::new(tokio::sync::Notify::new());
    status_wait_observers().lock().unwrap().push((ticket.clone(), observer.clone()));
    observer
}

#[cfg(test)]
fn status_waiting(ticket: &Ticket) {
    let mut observers = status_wait_observers().lock().unwrap();
    if let Some(index) = observers.iter().position(|(entry, _)| entry == ticket) {
        observers.remove(index).1.notify_one();
    }
}

#[cfg(test)]
pub(super) static MAINTENANCE_CHECKED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
#[derive(Default)]
pub(super) struct PublicationGate {
    pub(super) reached: tokio::sync::Notify,
    pub(super) observed: tokio::sync::Notify,
    pub(super) release: tokio::sync::Notify,
}

#[cfg(test)]
fn publication_gates() -> &'static Mutex<Vec<(Ticket, Arc<PublicationGate>)>> {
    static GATES: OnceLock<Mutex<Vec<(Ticket, Arc<PublicationGate>)>>> = OnceLock::new();
    GATES.get_or_init(Mutex::default)
}

#[cfg(test)]
pub(super) fn hold_publication(ticket: &Ticket) -> Arc<PublicationGate> {
    let gate = Arc::new(PublicationGate::default());
    publication_gates().lock().unwrap().push((ticket.clone(), gate.clone()));
    gate
}

#[cfg(test)]
fn publication_observed(ticket: &Ticket) {
    if let Some((_, gate)) = publication_gates().lock().unwrap().iter().find(|(entry, _)| entry == ticket) {
        gate.observed.notify_one();
    }
}

#[cfg(test)]
async fn publication_pause(ticket: &Ticket) {
    let gate = publication_gates().lock().unwrap().iter().find(|(entry, _)| entry == ticket).map(|(_, gate)| gate.clone());
    if let Some(gate) = gate {
        gate.reached.notify_one();
        gate.release.notified().await;
        publication_gates().lock().unwrap().retain(|(entry, _)| entry != ticket);
    }
}

#[cfg(test)]
pub(super) fn forget_host(ticket: &Ticket) {
    registry().operations.retain(|operation| &operation.ticket != ticket);
}

#[cfg(test)]
pub(super) fn finished(ticket: &Ticket) -> bool {
    registry().operations.iter().any(|operation| &operation.ticket == ticket && operation.run == Run::Finished)
}

// ---- Tauri commands ----

#[tauri::command]
pub(crate) async fn native_generation_host_init(user_agent: String) -> Result<(), StoreError> {
    logged("native_generation_host_init", host_init(&user_agent))
}

#[tauri::command]
pub(crate) async fn native_generation_reserve_ollama(app: AppHandle, request: ReserveRequest) -> Result<Reservation, StoreError> {
    logged("native_generation_reserve_ollama", Box::pin(reserve(app, request)).await)
}

#[tauri::command]
pub(crate) async fn native_generation_execute(app: AppHandle, ticket: Ticket) -> Result<RouteStatus, StoreError> {
    logged("native_generation_execute", Box::pin(execute(app, ticket)).await)
}

#[tauri::command]
pub(crate) async fn native_generation_cancel(app: AppHandle, ticket: Ticket, reason: CancelReason) -> Result<RouteStatus, StoreError> {
    logged("native_generation_cancel", Box::pin(cancel(app, ticket, reason)).await)
}

#[tauri::command]
pub(crate) async fn native_generation_status(app: AppHandle, ticket: Ticket, wait_ms: Option<u64>, after: Option<StatusObservation>) -> Result<RouteStatus, StoreError> {
    logged("native_generation_status", Box::pin(status_after(app, ticket, wait_ms, after)).await)
}

#[tauri::command]
pub(crate) async fn native_generation_active(app: AppHandle) -> Result<Vec<RouteStatus>, StoreError> {
    logged("native_generation_active", Box::pin(active(app)).await)
}

#[tauri::command]
pub(crate) async fn native_generation_capture_witness(app: AppHandle, ticket: Ticket, original: Lease, indices: Vec<usize>) -> Result<Witness, StoreError> {
    logged("native_generation_capture_witness", Box::pin(capture_witness(app, ticket, original, indices)).await)
}

#[tauri::command]
pub(crate) async fn native_generation_proof(app: AppHandle, ticket: Ticket, normalization: Option<NormalizationInput>) -> Result<Proof, StoreError> {
    logged("native_generation_proof", Box::pin(proof(app, ticket, normalization)).await)
}
