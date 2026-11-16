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
    pub(crate) stopped: bool,
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

// The claiming job owns this guard before returning across an await. Until a begin may have started, dropping it
// returns the ticket to Reserved. Afterwards the owner or its recovery holds the ticket: a dropped claim only
// publishes, so no reader waits for a supervisor that is gone and no caller can start a second machine.
struct RunClaim { scope: String, ticket: Ticket, id: u64, durable: bool }

impl Drop for RunClaim {
    fn drop(&mut self) {
        let durable = self.durable;
        update_operation(&self.scope, &self.ticket, self.id, |operation| {
            if operation.run != Run::Running { return false; }
            if durable {
                operation.run = Run::Finished;
                operation.diagnostic_ready = true;
            } else { operation.run = Run::Reserved; }
            true
        });
    }
}

// ---- Caller handoff ----

// The supervisor prepares while the execute caller is attached and dispatches only after the caller accepts its
// ready offer. A caller dropped before accepting abandons the operation; one dropped after leaves the owner running.
enum Handoff { Attached, Offered, Accepted, Abandoned, Released(Option<StoreError>) }

struct Caller { handoff: Arc<watch::Sender<Handoff>>, detached: Arc<AtomicBool>, scope: String, ticket: Ticket }

impl Drop for Caller {
    fn drop(&mut self) {
        let abandoned = self.handoff.send_if_modified(|state| {
            if !matches!(state, Handoff::Attached | Handoff::Offered) { return false; }
            *state = Handoff::Abandoned;
            true
        });
        if abandoned {
            // Published before the live signal: a begin registered after this signal still sees the flag.
            self.detached.store(true, Ordering::SeqCst);
            // Only a lifecycle its driver still holds hears it; one already dropped is left to its recovery.
            owner::signal_live(&self.scope, &self.ticket, CancelReason::Lifecycle);
        }
    }
}

impl Caller {
    // Accepts an offer in the same step that observes it, so no await separates acceptance from disarming.
    async fn attend(&self) -> StoreResult<()> {
        let mut updates = self.handoff.subscribe();
        loop {
            #[cfg(test)]
            if matches!(*self.handoff.borrow(), Handoff::Offered) { offer_pause(&self.ticket).await; }
            let mut outcome = None;
            self.handoff.send_if_modified(|state| match state {
                Handoff::Offered => { *state = Handoff::Accepted; outcome = Some(Ok(())); true }
                Handoff::Released(error) => { outcome = Some(error.take().map_or(Ok(()), Err)); false }
                _ => false,
            });
            if let Some(outcome) = outcome { return outcome; }
            // The supervisor's own guard releases this wait if it ends without an offer.
            let _ = updates.changed().await;
        }
    }
}

// Releases a caller that is still waiting when the supervisor ends without an offer, including by panic.
struct Release(Arc<watch::Sender<Handoff>>);

impl Release {
    fn release(&self, error: Option<StoreError>) {
        self.0.send_if_modified(|state| {
            if !matches!(state, Handoff::Attached) { return false; }
            *state = Handoff::Released(error);
            true
        });
    }
}

impl Drop for Release {
    fn drop(&mut self) { self.release(None); }
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

// `entered` records whether any segment work ran, which is the evidence that a refused begin started nothing.
struct AdmittedStore<R: Runtime> {
    app: AppHandle<R>,
    entered: Arc<AtomicBool>,
    #[cfg(test)]
    ticket: Ticket,
}

impl<R: Runtime> OwnerStore for AdmittedStore<R> {
    fn segment(&self, work: &mut dyn FnMut(&mut PersistentStore)) -> StoreResult<()> {
        #[cfg(test)]
        let mut cut = SegmentCut::new(&self.ticket);
        let result = with_job_store(&self.app, |store| {
            self.entered.store(true, Ordering::Release);
            #[cfg(test)]
            if let Some(wrap) = cut.wrap.take() { return wrap(store, work); }
            work(store);
            Ok(())
        });
        changed();
        #[cfg(test)]
        if cut.finish() && result.is_ok() { return Err(invalid("synthetic-lost-acknowledgement")); }
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
        stopped: status.stopped,
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
                Ok(Some(RunClaim { scope, ticket: claimed, id: claim_id, durable: false }))
            }
            None => Err(invalid("target-unavailable")),
        }
    }).await?;
    if let Some(claim) = claim {
        #[cfg(test)]
        preparation_pause(&ticket, false).await;
        // Installed before the supervisor can begin, which then owns the claim, preparation, maintenance and recovery.
        let handoff = Arc::new(watch::channel(Handoff::Attached).0);
        let caller = Caller { handoff: handoff.clone(), detached: Arc::default(), scope: claim.scope.clone(), ticket: ticket.clone() };
        tokio::spawn(supervise(app.clone(), ticket.clone(), host, claim, handoff, caller.detached.clone()));
        caller.attend().await?;
        #[cfg(test)]
        accepted_pause(&ticket).await;
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

// Admission is closed during maintenance, so the live owner hears the signal before any store attempt and the
// decision is persisted once admission reopens.
async fn cancel_for_maintenance<R: Runtime>(app: &AppHandle<R>, scope: &str, ticket: &Ticket) {
    owner::signal(scope, ticket, CancelReason::Lifecycle);
    let ticket = ticket.clone();
    let _ = blocking(app, move |store| service::cancel_generation_for(store, &ticket, CancelReason::Lifecycle)).await;
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

// Publication precedes every terminal status a reader can observe for this claim.
async fn publish(claim: &RunClaim, diagnostic: Option<LiveDiagnostic>) {
    #[cfg(test)]
    publication_pause(&claim.ticket).await;
    update_operation(&claim.scope, &claim.ticket, claim.id, |operation| {
        operation.diagnostic = diagnostic;
        operation.diagnostic_ready = true;
        true
    });
}

// Device maintenance or an abandoning caller stops a preparation before its handoff. The live hard control hears it
// first; a signal that found no lifecycle is repeated, since begin may still be registering one. A lifecycle its
// driver already dropped is not signalled, so a stop it never heard leaves its recovery alone.
#[derive(Default)]
struct Stop { lifecycle: bool, abandoned: bool, heard: bool }

impl Stop {
    fn observe(&mut self, maintenance: &AtomicBool, handoff: &watch::Sender<Handoff>, scope: &str, ticket: &Ticket) {
        self.lifecycle |= maintenance.load(Ordering::Acquire);
        self.abandoned |= matches!(*handoff.borrow(), Handoff::Abandoned);
        if self.requested() && !self.heard {
            self.heard = owner::signal_live(scope, ticket, CancelReason::Lifecycle) != owner::Presence::Absent;
        }
    }

    fn requested(&self) -> bool { self.lifecycle || self.abandoned }
}

// A begin that never entered the store, or a record left with no owner or lifecycle once settled, started nothing.
// `None` is a read that failed, which decides nothing.
async fn absent<R: Runtime>(app: &AppHandle<R>, scope: &str, ticket: &Ticket, entered: &AtomicBool) -> Option<bool> {
    if !entered.load(Ordering::Acquire) { return Some(true); }
    let (scope, requested) = (scope.to_owned(), ticket.clone());
    let read = blocking(app, move |store| {
        #[cfg(test)]
        if absence_read_fails(&requested) { return Err(invalid("synthetic-absence-read")); }
        service::resolve_owner(store, &requested)?;
        Ok(store.generation_status(&requested)?.phase == Phase::Reserved && store.generation_preparing(&requested)?.is_none()
            && store.generation_execution(&requested)?.is_none() && !owner::registered(&scope, &requested))
    }).await;
    match read {
        Ok(absent) => Some(absent),
        // An expired journal leaves no ticket to return.
        Err(error) if ticket_gone(&error) => Some(false),
        Err(_) => None,
    }
}

// Decides an absence that a failed read left open, within the settlement bound.
async fn absent_later<R: Runtime>(app: &AppHandle<R>, scope: &str, ticket: &Ticket, entered: &AtomicBool) -> bool {
    let mut delay = Duration::from_millis(100);
    for _ in 0..SETTLE_ATTEMPTS {
        tokio::time::sleep(delay).await;
        if let Some(absent) = absent(app, scope, ticket, entered).await { return absent; }
        delay = (delay * 2).min(Duration::from_secs(5));
    }
    false
}

// Ends a preparation that may have started nothing. Only an established absence (a full owner registry, closed
// admission) returns the ticket to Reserved, before the caller hears the error, so the caller can execute again or
// cancel. A failed read keeps the claim until a later read decides, so an untouched ticket is not left finished while
// its record stays Reserved.
async fn conclude_unstarted<R: Runtime>(app: &AppHandle<R>, ticket: &Ticket, entered: &AtomicBool, mut claim: RunClaim,
    release: &Release, error: StoreError, lifecycle: bool) {
    let absence = absent(app, &claim.scope, ticket, entered).await;
    if absence == Some(true) {
        claim.durable = false;
        drop(claim);
        release.release(Some(error));
        return;
    }
    release.release(Some(error));
    publish(&claim, None).await;
    if absence.is_none() && absent_later(app, &claim.scope, ticket, entered).await {
        claim.durable = false;
        return;
    }
    settle_later(app, ticket, lifecycle).await;
    finish(&claim, None);
}

// Owns the claim from before the first owned begin: preparation, maintenance, the caller handoff, the accepted owner,
// recovery and publication. A preparation that ends without a ready offer releases the caller to the stored phase.
async fn supervise<R: Runtime>(app: AppHandle<R>, ticket: Ticket, host: OwnerHost, mut claim: RunClaim,
    handoff: Arc<watch::Sender<Handoff>>, detached: Arc<AtomicBool>) {
    // Dropped on every exit, including a panic, so an attached caller is never left waiting.
    let release = Release(handoff.clone());
    let scope = claim.scope.clone();
    // Registered before begin, so maintenance that starts while a segment is admitted still reaches this owner.
    let maintenance = Arc::new(AtomicBool::new(false));
    if let Err(error) = app.state::<PersistentStoreState>().cancel_on_device_maintenance(&maintenance) {
        // Nothing began, so the ticket returns to Reserved and the caller can execute again or cancel.
        drop(claim);
        release.release(Some(error));
        return;
    }
    let entered = Arc::new(AtomicBool::new(false));
    let access: Arc<dyn OwnerStore> = Arc::new(AdmittedStore { app: app.clone(), entered: entered.clone(), #[cfg(test)] ticket: ticket.clone() });
    // A begin may be in flight from here, so only an established absence returns the ticket to Reserved.
    claim.durable = true;
    let mut preparing = tokio::spawn(service::prepare_ready_ollama_async(ticket.clone(), host, access.clone(), detached));
    let mut updates = handoff.subscribe();
    let mut stop = Stop::default();
    let joined = loop {
        stop.observe(&maintenance, &handoff, &scope, &ticket);
        tokio::select! {
            joined = &mut preparing => break joined,
            _ = updates.changed() => {}
            _ = tokio::time::sleep(MAINTENANCE_POLL) => {
                #[cfg(test)]
                maintenance_checked(&ticket);
            }
        }
    };
    #[cfg(test)]
    join_pause(&ticket).await;
    stop.observe(&maintenance, &handoff, &scope, &ticket);
    let (source, joined) = match joined {
        Ok(Ok(ReadyPreparation::Ready(ready))) => {
            // Offered only to an attached caller; nothing dispatches before it accepts.
            let offered = handoff.send_if_modified(|state| {
                if !matches!(state, Handoff::Attached) { return false; }
                *state = Handoff::Offered;
                true
            });
            let accepted = offered && loop {
                let decided = match &*updates.borrow_and_update() { Handoff::Accepted => Some(true), Handoff::Abandoned => Some(false), _ => None };
                if let Some(accepted) = decided { break accepted; }
                stop.observe(&maintenance, &handoff, &scope, &ticket);
                tokio::select! {
                    _ = updates.changed() => {}
                    _ = tokio::time::sleep(MAINTENANCE_POLL) => {}
                }
            };
            if accepted { return supervise_ready(app, ticket, ready, access, maintenance, stop.lifecycle, claim).await; }
            // An abandoned offer never dispatches; recovery settles the dropped lifecycle as a cancellation.
            stop.observe(&maintenance, &handoff, &scope, &ticket);
            drop(ready);
            publish(&claim, None).await;
            settle_later(&app, &ticket, stop.lifecycle).await;
            finish(&claim, None);
            return;
        }
        Ok(Ok(ReadyPreparation::Existing(status) | ReadyPreparation::Failed { status, .. })) => {
            if status.phase == Phase::Reserved {
                // Begin found the caller gone before its lifecycle could hear the signal. A caller still attached is told
                // so, since a Reserved success would leave it waiting for a phase that never comes.
                return conclude_unstarted(&app, &ticket, &entered, claim, &release, invalid("preparation-not-started"), stop.lifecycle).await;
            }
            // The caller reads the stored phase, so an interrupted preparation is never reported as failed.
            release.release(None);
            publish(&claim, None).await;
            if !status.phase.terminal() || status.pending_commit { settle_later(&app, &ticket, stop.lifecycle).await; }
            finish(&claim, None);
            return;
        }
        Ok(Err(error)) => (error.source, true),
        // A panicked driver may leave segment work behind, so its lifecycle stays a generic recovery obligation.
        Err(_) => (invalid("preparation-stopped"), false),
    };
    // A joined driver holds no work, so a stop its live lifecycle heard settles as that cancellation, not as a loss. Only
    // a live lifecycle is signalled, so a stop first seen once it was gone leaves the loss to recovery, and the stop
    // the lifecycle heard is already recorded for it.
    if joined && stop.requested() { owner::retain_joined_preparing_hard(&scope, &ticket); }
    #[cfg(test)]
    preparation_pause(&ticket, true).await;
    conclude_unstarted(&app, &ticket, &entered, claim, &release, source, false).await;
}

async fn supervise_ready<R: Runtime>(app: AppHandle<R>, ticket: Ticket, ready: ReadyOllama, access: Arc<dyn OwnerStore>,
    maintenance: Arc<AtomicBool>, lifecycle: bool, claim: RunClaim) {
    let scope = claim.scope.clone();
    let mut lifecycle = lifecycle || maintenance.load(Ordering::Acquire);
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
                maintenance_checked(&ticket);
            }
        }
    };
    drop(maintenance);
    let pending = execution.as_ref().is_none_or(|execution| execution.result.is_err());
    // The phase decides the outcome; the diagnostic only explains a failure and carries the live SDK text.
    let diagnostic = execution.and_then(|execution| execution.diagnostic).map(|diagnostic| LiveDiagnostic {
        failure: diagnostic.failure, sdk_message: diagnostic.message.map(|units| String::from_utf16_lossy(&units)) });
    publish(&claim, diagnostic.clone()).await;
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
fn maintenance_check_counts() -> MutexGuard<'static, Vec<(Ticket, usize)>> {
    static CHECKS: OnceLock<Mutex<Vec<(Ticket, usize)>>> = OnceLock::new();
    CHECKS.get_or_init(Mutex::default).lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
fn maintenance_checked(ticket: &Ticket) {
    let mut checks = maintenance_check_counts();
    match checks.iter_mut().find(|(entry, _)| entry == ticket) {
        Some((_, count)) => *count += 1,
        None => checks.push((ticket.clone(), 1)),
    }
}

// The maintenance polls the supervisors of `ticket` ran, so a test waits on its own owner's poll.
#[cfg(test)]
pub(super) fn maintenance_checks(ticket: &Ticket) -> usize {
    maintenance_check_counts().iter().find(|(entry, _)| entry == ticket).map_or(0, |(_, count)| *count)
}

#[cfg(test)]
fn absence_faults() -> MutexGuard<'static, Vec<Ticket>> {
    static FAULTS: OnceLock<Mutex<Vec<Ticket>>> = OnceLock::new();
    FAULTS.get_or_init(Mutex::default).lock().unwrap_or_else(PoisonError::into_inner)
}

// Fails the next read of whether the begin of `ticket` started anything, as closed admission or a store error would.
#[cfg(test)]
pub(super) fn fail_absence_read(ticket: &Ticket) { absence_faults().push(ticket.clone()); }

#[cfg(test)]
fn absence_read_fails(ticket: &Ticket) -> bool {
    let mut faults = absence_faults();
    faults.iter().position(|entry| entry == ticket).map(|index| faults.remove(index)).is_some()
}

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

// Holds one admitted segment of a ticket, counted from its first segment, before admission or after its work ran.
#[cfg(test)]
#[derive(Default)]
pub(super) struct SegmentGate {
    pub(super) reached: tokio::sync::Notify,
    released: Mutex<Option<bool>>,
    resumed: std::sync::Condvar,
}

#[cfg(test)]
impl SegmentGate {
    // `lose` drops the acknowledgement of work that ran, as a lost reply would.
    pub(super) fn release(&self, lose: bool) {
        *self.released.lock().unwrap() = Some(lose);
        self.resumed.notify_all();
    }

    fn hold(&self) -> bool {
        self.reached.notify_one();
        let released = self.released.lock().unwrap();
        let (released, waited) = self.resumed.wait_timeout_while(released, Duration::from_secs(60), |released| released.is_none()).unwrap();
        assert!(!waited.timed_out(), "segment gate was not released");
        released.unwrap()
    }
}

#[cfg(test)]
#[derive(Default)]
struct SegmentCuts { counts: Vec<(Ticket, usize)>, cuts: Vec<(Ticket, usize, bool, Arc<SegmentGate>)>, wraps: Vec<(Ticket, usize, SegmentWrap)> }

// Runs one admitted segment of a ticket, counted as `hold_segment` counts, in place of its plain work: the wrapper gets
// the admitted store and the work, and its result is the segment's.
#[cfg(test)]
pub(super) type SegmentWrap = Box<dyn FnOnce(&mut PersistentStore, &mut dyn FnMut(&mut PersistentStore)) -> StoreResult<()> + Send>;

#[cfg(test)]
pub(super) fn wrap_segment(ticket: &Ticket, index: usize, wrap: SegmentWrap) {
    segment_cuts().wraps.push((ticket.clone(), index, wrap));
}

#[cfg(test)]
fn segment_cuts() -> MutexGuard<'static, SegmentCuts> {
    static CUTS: OnceLock<Mutex<SegmentCuts>> = OnceLock::new();
    CUTS.get_or_init(Mutex::default).lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
pub(super) fn hold_segment(ticket: &Ticket, index: usize, after: bool) -> Arc<SegmentGate> {
    let gate = Arc::new(SegmentGate::default());
    segment_cuts().cuts.push((ticket.clone(), index, after, gate.clone()));
    gate
}

#[cfg(test)]
struct SegmentCut { after: bool, gate: Option<Arc<SegmentGate>>, lose: bool, wrap: Option<SegmentWrap> }

#[cfg(test)]
impl SegmentCut {
    fn new(ticket: &Ticket) -> Self {
        let mut cuts = segment_cuts();
        let index = match cuts.counts.iter_mut().find(|(entry, _)| entry == ticket) {
            Some((_, count)) => { *count += 1; *count - 1 }
            None => { cuts.counts.push((ticket.clone(), 1)); 0 }
        };
        let position = cuts.cuts.iter().position(|(entry, at, _, _)| entry == ticket && *at == index);
        let (after, gate) = position.map_or((false, None), |position| { let (_, _, after, gate) = cuts.cuts.remove(position); (after, Some(gate)) });
        let wrap = cuts.wraps.iter().position(|(entry, at, _)| entry == ticket && *at == index).map(|position| cuts.wraps.remove(position).2);
        drop(cuts);
        let mut cut = Self { after, gate, lose: false, wrap };
        if !cut.after { cut.lose = cut.gate.as_ref().is_some_and(|gate| gate.hold()); }
        cut
    }

    fn finish(&self) -> bool {
        if self.after { self.gate.as_ref().is_some_and(|gate| gate.hold()) } else { self.lose }
    }
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct HandoffGate {
    pub(super) reached: tokio::sync::Notify,
    pub(super) release: tokio::sync::Notify,
}

#[cfg(test)]
fn handoff_gates() -> &'static Mutex<Vec<(Ticket, bool, Arc<HandoffGate>)>> {
    static GATES: OnceLock<Mutex<Vec<(Ticket, bool, Arc<HandoffGate>)>>> = OnceLock::new();
    GATES.get_or_init(Mutex::default)
}

// Holds the execute caller once, either at an offer it has not yet accepted or right after it accepted one.
#[cfg(test)]
pub(super) fn hold_handoff(ticket: &Ticket, accepted: bool) -> Arc<HandoffGate> {
    let gate = Arc::new(HandoffGate::default());
    handoff_gates().lock().unwrap().push((ticket.clone(), accepted, gate.clone()));
    gate
}

#[cfg(test)]
async fn handoff_pause(ticket: &Ticket, accepted: bool) {
    let gate = {
        let mut gates = handoff_gates().lock().unwrap();
        gates.iter().position(|(entry, at, _)| entry == ticket && *at == accepted).map(|index| gates.remove(index).2)
    };
    if let Some(gate) = gate {
        gate.reached.notify_one();
        gate.release.notified().await;
    }
}

#[cfg(test)]
fn join_gates() -> &'static Mutex<Vec<(Ticket, Arc<HandoffGate>)>> {
    static GATES: OnceLock<Mutex<Vec<(Ticket, Arc<HandoffGate>)>>> = OnceLock::new();
    GATES.get_or_init(Mutex::default)
}

// Holds the supervisor once, after its preparation joined and before it looks for a stop again.
#[cfg(test)]
pub(super) fn hold_join(ticket: &Ticket) -> Arc<HandoffGate> {
    let gate = Arc::new(HandoffGate::default());
    join_gates().lock().unwrap().push((ticket.clone(), gate.clone()));
    gate
}

#[cfg(test)]
async fn join_pause(ticket: &Ticket) {
    let gate = {
        let mut gates = join_gates().lock().unwrap();
        gates.iter().position(|(entry, _)| entry == ticket).map(|index| gates.remove(index).1)
    };
    if let Some(gate) = gate {
        gate.reached.notify_one();
        gate.release.notified().await;
    }
}

#[cfg(test)]
async fn offer_pause(ticket: &Ticket) { handoff_pause(ticket, false).await }

#[cfg(test)]
async fn accepted_pause(ticket: &Ticket) { handoff_pause(ticket, true).await }

#[cfg(test)]
pub(super) fn forget_host(ticket: &Ticket) {
    registry().operations.retain(|operation| &operation.ticket != ticket);
}

#[cfg(test)]
pub(super) fn finished(ticket: &Ticket) -> bool {
    registry().operations.iter().any(|operation| &operation.ticket == ticket && operation.run == Run::Finished)
}

// Whether the host would let another execute claim this ticket.
#[cfg(test)]
pub(super) fn reserved(ticket: &Ticket) -> bool {
    registry().operations.iter().any(|operation| &operation.ticket == ticket && operation.run == Run::Reserved)
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
