use super::{
    ollama_decode::{CancelOrigin, Cancellation, Coding, DecodeError, DecodeLimits},
    ollama_transport::{self as transport, BodyFailure, CapturedOllamaConfig, ConsumerStop, Failure, OfferResult, OutputSink, Settlement, Snapshot, Stage, Think, TransportContext, TransportLimits},
    operation::{Checkpoint, Owner, Phase, Status, Ticket},
    owner_response::{self, descriptor, Refusal},
    prepare::PreparedOllamaTurn,
};
use crate::persistent_store::{
    generation::{CancelReason, DispatchClaim, FailureDescriptor, OutputInfo, OutputReceipt, OutputResolution, TerminalDecision},
    PersistentStore, StoreError, StoreResult,
};
use async_trait::async_trait;
use risunest_ir::{self as ir, Budget, Error as CompatError, Text};
use std::{sync::{atomic::AtomicBool, Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak}, time::Instant};
use tokio::task::JoinHandle;

// Production bounds for the buffered local route. There is no timeout, as in the local TS route.
pub(crate) const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_REQUEST_BYTES: usize = 9 * 1024 * 1024;
pub(crate) const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_DECODED_BYTES: u64 = 32 * 1024 * 1024;
pub(crate) const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_OUTPUT_VALUE_UNITS: usize = 4 * 1024 * 1024;
pub(crate) const MAX_URL_BYTES: usize = 8 * 1024;
pub(crate) const MAX_USER_AGENT_BYTES: usize = 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_WINDOW_SUM: u64 = 64 * 1024 * 1024;
const ZSTD_WINDOW_LOG_MAX: u32 = 23;
const MAX_OWNED_BYTES: usize = 96 * 1024 * 1024;
const MAX_TEXT_UNITS: usize = 16 * 1024 * 1024;
const MAX_VALUES: usize = 4 * 1024 * 1024;
const MAX_WORK: usize = 1 << 30;
const MAX_RESPONSE_OWNED_BYTES: usize = 64 * 1024 * 1024;
// Per device store: one per active-or-unresolved record (16 per journal) with room for records of a replaced journal.
const MAX_LIFECYCLES: usize = 64;
const MAX_RECONCILED: usize = 16;
const MAX_DIAGNOSTIC_UNITS: usize = 4096;

fn invalid(code: &str) -> StoreError {
    StoreError::Validation { message: format!("native-generation:{code}") }
}

fn is_code(error: &StoreError, code: &str) -> bool {
    matches!(error, StoreError::Validation { message } if message.strip_prefix("native-generation:") == Some(code))
}

#[derive(Clone, Copy)]
pub(crate) struct OwnerLimits {
    pub(crate) transport: TransportLimits,
    pub(super) compat: ir::Limits,
    // Response application work and the storage copy, separate from the transport's operation budget.
    pub(super) response: ir::Limits,
}

impl OwnerLimits {
    pub(crate) fn production() -> Self {
        Self {
            transport: TransportLimits {
                decode: DecodeLimits { input_quantum: 64 * 1024, output_quantum: 64 * 1024, max_encoded_bytes: MAX_BODY_BYTES as u64,
                    max_layer_decoded_bytes: MAX_DECODED_BYTES, max_total_decoded_bytes: MAX_DECODED_BYTES, max_layers: 4,
                    max_header_bytes: MAX_HEADER_BYTES, max_window_sum: MAX_WINDOW_SUM, zstd_window_log_max: ZSTD_WINDOW_LOG_MAX },
                max_request_bytes: MAX_REQUEST_BYTES, max_url_bytes: MAX_URL_BYTES, max_user_agent_bytes: MAX_USER_AGENT_BYTES,
                max_buffered_bytes: MAX_BODY_BYTES, max_line_bytes: MAX_LINE_BYTES, max_output_bytes: MAX_OUTPUT_BYTES,
                max_output_value_units: MAX_OUTPUT_VALUE_UNITS,
            },
            compat: ir::Limits { max_owned_bytes: MAX_OWNED_BYTES, max_json_bytes: MAX_BODY_BYTES, max_text_units: MAX_TEXT_UNITS,
                max_values: MAX_VALUES, max_depth: 128, max_work: MAX_WORK },
            response: ir::Limits { max_owned_bytes: MAX_RESPONSE_OWNED_BYTES, max_json_bytes: MAX_BODY_BYTES, max_text_units: MAX_TEXT_UNITS,
                max_values: MAX_VALUES, max_depth: 128, max_work: MAX_WORK },
        }
    }
}

// Raw navigator metadata captured once at native host initialization.
#[derive(Clone)]
pub(crate) struct OwnerHost { user_agent: Arc<str> }

impl OwnerHost {
    pub(crate) fn new(user_agent: &str) -> StoreResult<Self> {
        if user_agent.len() > MAX_USER_AGENT_BYTES || reqwest::header::HeaderValue::from_str(user_agent).is_err() {
            return Err(invalid("user-agent"));
        }
        Ok(Self { user_agent: user_agent.into() })
    }
}

pub(crate) struct ReadyOllamaConfig {
    host: String,
    model: String,
    think: Option<Think>,
    model_label: String,
    user_agent: Arc<str>,
}

impl ReadyOllamaConfig {
    pub(super) fn new(host: String, model: String, think: Option<Think>, model_label: String, owner: &OwnerHost) -> Self {
        Self { host, model, think, model_label, user_agent: owner.user_agent.clone() }
    }
    pub(crate) fn model_label(&self) -> &str { &self.model_label }
    // Effective streaming is refused at capture, so the admitted wire is always buffered.
    pub(crate) fn captured(&self) -> CapturedOllamaConfig<'_> {
        CapturedOllamaConfig { host: &self.host, model: &self.model, streaming: false, think: self.think, browser_user_agent: &self.user_agent }
    }
}

// The admitted store access. Each call runs on a blocking thread and holds nothing across HTTP.
pub(crate) trait OwnerStore: Send + Sync + 'static {
    fn segment(&self, work: &mut dyn FnMut(&mut PersistentStore)) -> StoreResult<()>;
}

fn segment<T: Send + 'static>(access: &Arc<dyn OwnerStore>, work: impl FnOnce(&mut PersistentStore) -> StoreResult<T> + Send + 'static) -> JoinHandle<StoreResult<T>> {
    let access = Arc::clone(access);
    tokio::task::spawn_blocking(move || {
        let mut work = Some(work);
        let mut output = None;
        access.segment(&mut |store| if let Some(work) = work.take() { output = Some(work(store)); })?;
        output.unwrap_or_else(|| Err(invalid("segment-skipped")))
    })
}

async fn joined<T>(handle: JoinHandle<StoreResult<T>>) -> StoreResult<T> {
    handle.await.unwrap_or_else(|_| Err(invalid("segment-failed")))
}

#[derive(Debug, Clone)]
enum Pending {
    Decision(TerminalDecision),
    // An offered output whose commit is unknown; recovery resolves it before choosing the decision.
    Offer { sequence: u64, output_hash: String, committed: TerminalDecision, uncommitted: TerminalDecision },
}

struct Entry {
    scope: String,
    owner: Owner,
    lifecycle_id: String,
    // A liveness witness, not a strong reference that would keep an abandoned capability alive.
    live: Weak<()>,
    cancel: Cancellation,
    reason: Option<CancelReason>,
    pending: Option<Pending>,
}

// Entries rotate: a failed resolution moves to the back. A slot is capacity reserved before preparation starts.
#[derive(Default)]
struct Registry { entries: Vec<Entry>, slots: Vec<String> }

fn registry() -> MutexGuard<'static, Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default())).lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RegistryLimits { pub(super) capacity: usize, pub(super) pass: usize }

impl RegistryLimits {
    pub(super) const PRODUCTION: Self = Self { capacity: MAX_LIFECYCLES, pass: MAX_RECONCILED };
}

pub(super) fn registered(scope: &str, ticket: &Ticket) -> bool {
    registry().entries.iter().any(|entry| entry.scope == scope && &entry.owner.ticket == ticket)
}

// Capacity for one lifecycle; dropping it unused returns the capacity.
pub(super) struct Slot { scope: Option<String> }

impl Drop for Slot {
    fn drop(&mut self) {
        let Some(scope) = self.scope.take() else { return };
        let mut registry = registry();
        if let Some(index) = registry.slots.iter().position(|value| *value == scope) { registry.slots.remove(index); }
    }
}

// A full registry first settles every abandoned entry of this store; still full, it refuses before any record changes.
pub(super) fn admit(store: &mut PersistentStore, capacity: usize) -> StoreResult<Slot> {
    let scope = scope(store)?;
    for full_pass in [false, true] {
        if full_pass { reconcile_limited(store, usize::MAX, &mut resolve_owner)?; }
        let mut registry = registry();
        let used = registry.entries.iter().filter(|entry| entry.scope == scope).count()
            + registry.slots.iter().filter(|value| **value == scope).count();
        if used < capacity {
            registry.slots.push(scope.clone());
            return Ok(Slot { scope: Some(scope) });
        }
    }
    Err(invalid("lifecycle-limit"))
}

// Tickets are unique per journal, and a journal belongs to one device store.
pub(crate) fn scope(store: &PersistentStore) -> StoreResult<String> {
    Ok(store.device_store()?.connection().path().unwrap_or_default().to_owned())
}

pub(crate) struct Lifecycle { scope: String, ticket: Ticket, id: String, live: Option<Arc<()>>, cancel: Cancellation }

impl Lifecycle {
    // Registered before ReadyOwned is persisted, so a lost acknowledgement still leaves a recovery entry.
    pub(super) fn register(mut slot: Slot, owner: &Owner) -> StoreResult<Self> {
        let scope = slot.scope.take().ok_or_else(|| invalid("lifecycle-slot"))?;
        let mut registry = registry();
        if let Some(index) = registry.slots.iter().position(|value| *value == scope) { registry.slots.remove(index); }
        if registry.entries.iter().any(|entry| entry.scope == scope && entry.owner.ticket == owner.ticket) { return Err(invalid("lifecycle-registered")); }
        let live = Arc::new(());
        let cancel = Cancellation::new();
        let id = uuid::Uuid::new_v4().to_string();
        registry.entries.push(Entry { scope: scope.clone(), owner: owner.clone(), lifecycle_id: id.clone(), live: Arc::downgrade(&live),
            cancel: cancel.clone(), reason: None, pending: None });
        Ok(Self { scope, ticket: owner.ticket.clone(), id, live: Some(live), cancel })
    }
    pub(super) fn id(&self) -> &str { &self.id }
    fn owns(&self, entry: &Entry) -> bool { entry.scope == self.scope && entry.owner.ticket == self.ticket && entry.lifecycle_id == self.id }
    fn reason(&self) -> CancelReason {
        registry().entries.iter().find(|entry| self.owns(entry)).and_then(|entry| entry.reason)
            .unwrap_or(match self.cancel.origin() { CancelOrigin::Owner => CancelReason::Lifecycle, CancelOrigin::Caller => CancelReason::User })
    }
    pub(super) fn release(mut self) {
        self.live = None;
        registry().entries.retain(|entry| !self.owns(entry));
    }
    fn defer(mut self, pending: Pending) {
        if let Some(entry) = registry().entries.iter_mut().find(|entry| self.owns(entry)) { entry.pending = Some(pending); }
        self.live = None;
    }
}

impl Drop for Lifecycle {
    // Losing the capability only marks it abandoned; recovery settles it later without HTTP.
    fn drop(&mut self) {
        if self.live.take().is_some() { self.cancel.cancel_as(CancelOrigin::Owner); }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Presence { Live, Abandoned, Absent }

// Signals the live transport first; the store decision and settlement follow.
pub(crate) fn signal(scope: &str, ticket: &Ticket, reason: CancelReason) -> Presence {
    let mut registry = registry();
    let Some(entry) = registry.entries.iter_mut().find(|entry| entry.scope == scope && &entry.owner.ticket == ticket) else { return Presence::Absent };
    entry.reason.get_or_insert(reason);
    entry.cancel.cancel_as(if reason == CancelReason::Lifecycle { CancelOrigin::Owner } else { CancelOrigin::Caller });
    if entry.live.strong_count() > 0 { Presence::Live } else { Presence::Abandoned }
}

pub(crate) struct ReadyOllama {
    variables: super::preparation_effects::PreparedVariablePlan,
    owner: Owner,
    lifecycle: Lifecycle,
    prepared: PreparedOllamaTurn,
    config: ReadyOllamaConfig,
    status: Status,
    response: owner_response::Response,
}

impl std::fmt::Debug for ReadyOllama {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ReadyOllama").field("status", &self.status).finish_non_exhaustive()
    }
}

impl ReadyOllama {
    pub(super) fn new(owner: Owner, lifecycle: Lifecycle, prepared: PreparedOllamaTurn, config: ReadyOllamaConfig, status: Status,
        snapshot: super::inputs::PreparationSnapshot, variables: super::preparation_effects::PreparedVariablePlan) -> Self {
        Self { variables, owner, lifecycle, prepared, config, status, response: owner_response::Response::new(snapshot) }
    }
    #[cfg(test)]
    pub(super) fn pin_response_entropy(&mut self, entropy: super::cbs::Entropy) { self.response.pin_entropy(entropy); }
    #[cfg(test)]
    pub(super) fn set_prepared_variables(&mut self, variables: super::preparation_effects::PreparedVariablePlan) { self.variables = variables; }
    pub(crate) fn ticket(&self) -> &Ticket { &self.owner.ticket }
    pub(crate) fn status(&self) -> &Status { &self.status }
    pub(crate) fn config(&self) -> &ReadyOllamaConfig { &self.config }
}

#[derive(Debug, Clone)]
pub(crate) struct Diagnostic {
    pub(crate) failure: FailureDescriptor,
    // Bounded diagnostic text as UTF-16 units; response bodies are never retained here.
    pub(crate) message: Option<Vec<u16>>,
}

#[derive(Debug)]
pub(crate) struct OwnerExecution {
    // An error leaves the recovery obligation in the lifecycle registry for `resolve_owner`.
    pub(crate) result: StoreResult<Status>,
    pub(crate) dispatch_id: Option<String>,
    pub(crate) output: Option<OutputReceipt>,
    pub(crate) diagnostic: Option<Diagnostic>,
}

fn token(value: &str) -> Option<String> {
    (!value.is_empty() && value.len() <= 64 && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
        .then(|| value.to_owned())
}

fn stage_token(stage: Stage) -> &'static str {
    match stage {
        Stage::Request => "request", Stage::Headers => "headers", Stage::Body => "body", Stage::BufferedJson => "buffered-json",
        Stage::StreamJson => "stream-json", Stage::SdkIterator => "sdk-iterator", Stage::Transform => "transform",
        Stage::Credit => "credit", Stage::Offer => "offer",
    }
}

fn compat_detail(error: &CompatError) -> &'static str {
    match error {
        CompatError::Syntax { .. } => "syntax", CompatError::Type { operation } => *operation, CompatError::Limit(kind) => *kind,
        CompatError::Cancelled => "cancelled", CompatError::Allocation => "allocation", CompatError::IsolatedUtf16 => "isolated-utf16",
        CompatError::DecoderFinished => "decoder-finished", CompatError::Invariant(kind) => *kind,
    }
}

fn decode_detail(error: &DecodeError) -> &'static str {
    match error {
        DecodeError::Cancelled => "cancelled", DecodeError::Limit(kind) => *kind, DecodeError::Allocation => "allocation",
        DecodeError::Compat(error) => compat_detail(error), DecodeError::Reader(_) => "reader", DecodeError::InvalidLimits => "invalid-limits",
        DecodeError::Codec { coding, .. } | DecodeError::NoProgress(coding) => match coding {
            Coding::Gzip => "gzip", Coding::Deflate => "deflate", Coding::Brotli => "br", Coding::Zstd => "zstd" },
    }
}

fn units(text: &Text) -> Vec<u16> { text.units().take(MAX_DIAGNOSTIC_UNITS).collect() }

// The journal keeps the typed class; the live message stays in this result only.
fn transport_failure(failure: &Failure) -> Diagnostic {
    let classified = |category: &str, detail: Option<&str>, stage: Option<Stage>| FailureDescriptor {
        category: category.into(), detail: detail.and_then(token), stage: stage.map(|stage| stage_token(stage).into()), http_status: None };
    let (failure, message) = match failure {
        Failure::Limit(kind) => (classified("limit", Some(*kind), None), None),
        Failure::Allocation => (classified("limit", Some("allocation"), None), None),
        Failure::Compat { stage, error: CompatError::Syntax { byte } } => (
            classified("decode", Some("syntax"), Some(*stage)),
            Some(format!("Invalid JSON at byte {byte}").encode_utf16().take(MAX_DIAGNOSTIC_UNITS).collect()),
        ),
        Failure::Compat { stage, error } => (classified("compat", Some(compat_detail(error)), Some(*stage)), None),
        Failure::Decode(error) => (
            classified("decode", Some(decode_detail(error)), Some(Stage::Body)),
            Some(format!("Response decoding failed ({})", decode_detail(error)).encode_utf16().take(MAX_DIAGNOSTIC_UNITS).collect()),
        ),
        Failure::Network { stage } => (classified("network", None, Some(*stage)), None),
        Failure::Type { stage, operation } => (classified("type", Some(*operation), Some(*stage)), None),
        Failure::Sdk { stage, message } => (classified("sdk", None, Some(*stage)), Some(units(message))),
        Failure::Http { status_code, message, body_failure, .. } => {
            let detail = body_failure.map(|value| match value { BodyFailure::Reader => "reader", BodyFailure::Codec => "codec", BodyFailure::Json => "json", BodyFailure::Property => "property" });
            let mut failure = classified("http", detail, None);
            failure.http_status = (100..=599).contains(status_code).then_some(*status_code);
            (failure, Some(units(message)))
        }
        Failure::Consumer { stage } => (classified("consumer", None, Some(*stage)), None),
        Failure::DeliveryUnknown => (classified("consumer", Some("delivery-unknown"), Some(Stage::Offer)), None),
        Failure::InvalidHost => (classified("type", Some("invalid-host"), Some(Stage::Request)), None),
        Failure::Userinfo => (classified("type", Some("userinfo"), Some(Stage::Request)), None),
        Failure::Cancelled | Failure::ConsumerCancelled => (classified("consumer", Some("cancelled"), None), None),
    };
    Diagnostic { failure, message }
}

enum Offered { Committed(OutputReceipt), Uncommitted { closed: bool }, Unresolved }

struct PendingOffer { sequence: u64, output_hash: String, failure: Option<FailureDescriptor>, handle: JoinHandle<StoreResult<Offered>> }

enum OfferState {
    Idle,
    Pending(PendingOffer),
    Committed { sequence: u64, output_hash: String, receipt: OutputReceipt, failure: Option<FailureDescriptor> },
    Uncommitted,
    Unresolved { sequence: u64, output_hash: String, failure: Option<FailureDescriptor> },
}

// One segment admits the checkpoint and resolves its exact receipt; the store gate is its linearization point.
fn offer_segment(store: &mut PersistentStore, owner: &Owner, checkpoint: &Checkpoint, output_hash: &str, info: &OutputInfo) -> StoreResult<Offered> {
    let written = store.generation_checkpoint_with(owner, checkpoint, Some(info));
    let closed = matches!(&written, Err(error) if is_code(error, "execution-closed"));
    Ok(match store.generation_resolve_output(owner, checkpoint.sequence, output_hash)? {
        OutputResolution::Committed(receipt) => Offered::Committed(receipt),
        OutputResolution::Uncommitted => Offered::Uncommitted { closed },
        OutputResolution::Unresolved => Offered::Unresolved,
    })
}

// The store segment handle lives here, not in the offer future, so a dropped offer is still joined.
struct CheckpointSink {
    access: Arc<dyn OwnerStore>,
    owner: Owner,
    budget: Budget,
    response: owner_response::Response,
    max_output_bytes: usize,
    state: OfferState,
    refusal: Option<FailureDescriptor>,
    // The row's generation metadata; stage 3 runs from the dispatch claim to each offer.
    info: OutputInfo,
    dispatched: Instant,
}

impl CheckpointSink {
    async fn resolve_pending(&mut self) -> OfferResult {
        let OfferState::Pending(pending) = &mut self.state else { return OfferResult::Unknown };
        let joined = joined_ref(&mut pending.handle).await;
        let OfferState::Pending(PendingOffer { sequence, output_hash, failure, .. }) = std::mem::replace(&mut self.state, OfferState::Idle) else {
            return OfferResult::Unknown;
        };
        match joined {
            Ok(Offered::Committed(receipt)) => {
                self.state = OfferState::Committed { sequence, output_hash, receipt, failure };
                OfferResult::Accepted
            }
            Ok(Offered::Uncommitted { closed }) => {
                self.state = OfferState::Uncommitted;
                OfferResult::Rejected(if closed { ConsumerStop::Cancelled } else { ConsumerStop::Failed })
            }
            Ok(Offered::Unresolved) | Err(_) => {
                self.state = OfferState::Unresolved { sequence, output_hash, failure };
                OfferResult::Unknown
            }
        }
    }

    // Joins a segment left by a dropped offer, then retries an unknown outcome once by exact resolution.
    async fn join(&mut self) {
        if matches!(self.state, OfferState::Pending(_)) { self.resolve_pending().await; }
        let OfferState::Unresolved { sequence, output_hash, .. } = &self.state else { return };
        let (owner, sequence, hash) = (self.owner.clone(), *sequence, output_hash.clone());
        let resolved = joined(segment(&self.access, move |store| store.generation_resolve_output(&owner, sequence, &hash))).await;
        let OfferState::Unresolved { sequence, output_hash, failure } = std::mem::replace(&mut self.state, OfferState::Idle) else { return };
        self.state = match resolved {
            Ok(OutputResolution::Committed(receipt)) => OfferState::Committed { sequence, output_hash, receipt, failure },
            Ok(OutputResolution::Uncommitted) => OfferState::Uncommitted,
            Ok(OutputResolution::Unresolved) | Err(_) => OfferState::Unresolved { sequence, output_hash, failure },
        };
    }
}

async fn joined_ref<T>(handle: &mut JoinHandle<StoreResult<T>>) -> StoreResult<T> {
    handle.await.unwrap_or_else(|_| Err(invalid("segment-failed")))
}

#[async_trait]
impl OutputSink for CheckpointSink {
    async fn credit(&mut self) -> Result<(), ConsumerStop> {
        if self.refusal.is_some() || matches!(self.state, OfferState::Pending(_) | OfferState::Unresolved { .. }) { return Err(ConsumerStop::Failed); }
        Ok(())
    }

    async fn offer(&mut self, snapshot: Arc<Snapshot>) -> OfferResult {
        if matches!(self.state, OfferState::Pending(_) | OfferState::Unresolved { .. }) { return OfferResult::Unknown; }
        if self.response.needs_refresh() {
            let owner = self.owner.clone();
            let refreshed = match joined(segment(&self.access, move |store| store.generation_owned_preparation_snapshot(&owner))).await {
                Ok(Some(current)) => self.response.refresh(current),
                // The read keeps the same owner/target fences and settles a changed target as stale.
                Ok(None) => return OfferResult::Rejected(ConsumerStop::Cancelled),
                Err(_) => Err(Refusal::Failed(descriptor("store", Some("response-context"), Some("output")))),
            };
            match refreshed {
                Ok(()) => {},
                Err(Refusal::Cancelled) => return OfferResult::Rejected(ConsumerStop::Cancelled),
                Err(Refusal::Failed(failure)) => { self.refusal = Some(failure); return OfferResult::Rejected(ConsumerStop::Failed); }
            }
        }
        let storage = match self.response.storage_text(&snapshot.value, &self.budget, self.max_output_bytes) {
            Ok(storage) => storage,
            Err(Refusal::Cancelled) => return OfferResult::Rejected(ConsumerStop::Cancelled),
            Err(Refusal::Failed(failure)) => { self.refusal = Some(failure); return OfferResult::Rejected(ConsumerStop::Failed); }
        };
        #[cfg(all(test, target_os = "windows"))]
        crate::ollama_ui_verification::adapters::observe(crate::ollama_ui_verification::controller::Kind::StoragePolicyApplied);
        let output_hash = risunest_sync_wire::hash(storage.text.as_bytes());
        let checkpoint = Checkpoint { sequence: snapshot.sequence, text: storage.text };
        drop(snapshot);
        let (owner, hash) = (self.owner.clone(), output_hash.clone());
        // A fallback-error row never reaches TS completion timing updates.
        let info = OutputInfo { stage3: if storage.failure.is_none() { u64::try_from(self.dispatched.elapsed().as_millis()).unwrap_or(u64::MAX) } else { 0 },
            stage4_started: storage.failure.is_none().then(Instant::now), ..self.info.clone() };
        let sequence = checkpoint.sequence;
        #[cfg(not(all(test, target_os = "windows")))]
        let handle = segment(&self.access, move |store| offer_segment(store, &owner, &checkpoint, &hash, &info));
        #[cfg(all(test, target_os = "windows"))]
        let handle = {
            use crate::ollama_ui_verification::{adapters, controller::{Kind, ResultKind}};
            let observation = adapters::current();
            let access = adapters::output_store(Arc::clone(&self.access), observation.as_ref());
            segment(&access, move |store| {
                let result = offer_segment(store, &owner, &checkpoint, &hash, &info);
                if let Some(session) = observation {
                    session.record(Kind::OutputResolved, Some(match &result {
                        Ok(Offered::Committed(_)) => ResultKind::Committed,
                        Ok(Offered::Uncommitted { .. }) => ResultKind::Uncommitted,
                        Ok(Offered::Unresolved) => ResultKind::Unresolved,
                        Err(_) => ResultKind::Error,
                    }));
                }
                result
            })
        };
        self.state = OfferState::Pending(PendingOffer { sequence, output_hash, failure: storage.failure, handle });
        self.resolve_pending().await
    }
}

// The persisted caller reason names a cancellation over the provisional one.
fn settle(store: &mut PersistentStore, owner: &Owner, decision: TerminalDecision) -> StoreResult<Status> {
    let decision = match decision {
        TerminalDecision::Cancelled { reason } => TerminalDecision::Cancelled {
            reason: store.generation_execution(&owner.ticket)?.and_then(|execution| execution.cancel).unwrap_or(reason) },
        decision => decision,
    };
    store.generation_settle_owned(owner, decision)
}

async fn settle_now(access: &Arc<dyn OwnerStore>, owner: &Owner, lifecycle: Lifecycle, decision: TerminalDecision) -> StoreResult<Status> {
    let (owner, retained) = (owner.clone(), decision.clone());
    let result = joined(segment(access, move |store| settle(store, &owner, decision))).await;
    match &result {
        Ok(_) => lifecycle.release(),
        Err(_) => lifecycle.defer(Pending::Decision(retained)),
    }
    result
}

fn preflight(limits: &OwnerLimits, config: &ReadyOllamaConfig, cancel: &Cancellation) -> Result<(TransportContext, Budget), FailureDescriptor> {
    let limit = |detail| descriptor("limit", Some(detail), Some("preflight"));
    let transport = &limits.transport;
    if transport.decode.validate().is_err() { return Err(limit("invalid-decode-limits")); }
    if [transport.max_request_bytes, transport.max_url_bytes, transport.max_user_agent_bytes, transport.max_buffered_bytes,
        transport.max_line_bytes, transport.max_output_bytes, transport.max_output_value_units].contains(&0)
        || transport.max_output_bytes > MAX_OUTPUT_BYTES {
        return Err(limit("invalid-transport-limits"));
    }
    if config.host.len() > transport.max_url_bytes { return Err(limit("url-bytes")); }
    if config.user_agent.len() > transport.max_user_agent_bytes { return Err(limit("user-agent-bytes")); }
    // The transport's own URL rules, so an invalid host or userinfo is refused before any dispatch is claimed.
    // Its scratch budget has its own flag: a cancel already recorded settles as a cancellation, not this check.
    let scratch = Budget::new(limits.compat, Arc::new(AtomicBool::new(false))).map_err(|_| limit("invalid-compat-limits"))?;
    if let Err(error) = transport::endpoint(&config.host, *transport, &scratch) {
        let mut failure = transport_failure(&error).failure;
        failure.stage = Some("preflight".into());
        return Err(failure);
    }
    drop(scratch);
    let context = TransportContext::new(limits.compat, cancel.clone()).map_err(|_| limit("invalid-compat-limits"))?;
    let budget = Budget::new(limits.response, cancel.flag()).map_err(|_| limit("invalid-response-limits"))?;
    Ok((context, budget))
}

// Drives the one admitted dispatch. Normal is decided only after the exact output receipt was accepted.
pub(crate) async fn execute_ready_ollama(ready: ReadyOllama, access: Arc<dyn OwnerStore>, limits: OwnerLimits) -> OwnerExecution {
    // The capability, prompt and sink live on the heap so a caller can await this without a large frame.
    let mut execution = Box::pin(drive(ready, access, limits)).await;
    // A diagnostic explains only a Failed outcome; a record that ended Stale or Cancelled carries none.
    if matches!(&execution.result, Ok(status) if status.phase != Phase::Failed) { execution.diagnostic = None; }
    execution
}

async fn drive(ready: ReadyOllama, access: Arc<dyn OwnerStore>, limits: OwnerLimits) -> OwnerExecution {
    let ReadyOllama { variables, owner, lifecycle, prepared, config, response, .. } = ready;
    let mut execution = OwnerExecution { result: Err(invalid("unsettled")), dispatch_id: None, output: None, diagnostic: None };
    // Validation has no send effects. Consume its result after the prepared variable and memory effects.
    let checked = preflight(&limits, &config, &lifecycle.cancel);
    let can_dispatch = checked.is_ok();
    let variables_result = {
        let (owner, id) = (owner.clone(), lifecycle.id.clone());
        joined(segment(&access, move |store| store.waiting_for_admission(|store| store.generation_send_variables(&owner, &id, &variables)))).await
    };
    match variables_result {
        Ok(status) if status.phase.terminal() => {
            lifecycle.release();
            execution.result = Ok(status);
            return execution;
        }
        Ok(_) => {}
        Err(_) => {
            let failure = descriptor("interrupted", Some("variables"), Some("preparation"));
            execution.diagnostic = Some(Diagnostic { failure: failure.clone(), message: None });
            execution.result = settle_now(&access, &owner, lifecycle, TerminalDecision::Failed { failure }).await;
            return execution;
        }
    }
    let claim = {
        let (owner, id, memory, cancel) = (owner.clone(), lifecycle.id.clone(), prepared.last_memory.clone(), lifecycle.cancel.clone());
        // A renderer command may hold device admission for a moment; one try would end the send as interrupted.
        joined(segment(&access, move |store| store.waiting_for_admission(|store| {
            let status = store.generation_send_memory(&owner, &id, memory.as_deref())?;
            if !can_dispatch || cancel.is_cancelled() { return Ok(DispatchClaim::Refused(status)); }
            store.generation_claim_dispatch(&owner, &id)
        }))).await
    };
    let (context, budget) = match checked {
        Ok(value) => value,
        Err(failure) => {
            execution.diagnostic = Some(Diagnostic { failure: failure.clone(), message: None });
            execution.result = settle_now(&access, &owner, lifecycle, TerminalDecision::Failed { failure }).await;
            return execution;
        }
    };
    let dispatched = Instant::now();
    match claim {
        Ok(DispatchClaim::Issued { dispatch_id, .. }) => execution.dispatch_id = Some(dispatch_id),
        Ok(DispatchClaim::Refused(status)) if status.phase.terminal() => {
            lifecycle.release();
            execution.result = Ok(status);
            return execution;
        }
        // A refused live claim means a cancel or terminal decision was admitted first.
        Ok(DispatchClaim::Refused(_)) => {
            let decision = TerminalDecision::Cancelled { reason: lifecycle.reason() };
            execution.result = settle_now(&access, &owner, lifecycle, decision).await;
            return execution;
        }
        // No HTTP follows a failed or unacknowledged claim.
        Err(_) => {
            let failure = descriptor("interrupted", Some("dispatch-claim"), Some("dispatch"));
            execution.diagnostic = Some(Diagnostic { failure: failure.clone(), message: None });
            execution.result = settle_now(&access, &owner, lifecycle, TerminalDecision::Failed { failure }).await;
            return execution;
        }
    }
    let info = OutputInfo { model: format!("Ollama Local {}", config.model_label()), input_tokens: prepared.input_tokens,
        output_tokens: prepared.output_tokens, max_context: prepared.max_context, stage3: 0, stage4_started: None };
    let mut sink = CheckpointSink { access: Arc::clone(&access), owner: owner.clone(), budget, response,
        max_output_bytes: limits.transport.max_output_bytes, state: OfferState::Idle, refusal: None, info, dispatched };
    #[cfg(not(all(test, target_os = "windows")))]
    let outcome = Box::pin(transport::generate(&prepared, &config.captured(), limits.transport, &context, &mut sink)).await;
    #[cfg(all(test, target_os = "windows"))]
    let outcome = {
        let mut observed = crate::ollama_ui_verification::adapters::ObservedSink::new(&mut sink);
        Box::pin(transport::generate(&prepared, &config.captured(), limits.transport, &context, &mut observed)).await
    };
    drop(context);
    drop(prepared);
    sink.join().await;
    let interrupted = matches!(outcome.settlement, Settlement::Cancelled(_) | Settlement::ConsumerCancelled);
    let cancelled = TerminalDecision::Cancelled { reason: lifecycle.reason() };
    let mut diagnostic = None;
    let mut fail = |value: Diagnostic| {
        let failure = value.failure.clone();
        diagnostic = Some(value);
        TerminalDecision::Failed { failure }
    };
    let transport_failed = |settlement: &Settlement| match settlement {
        Settlement::Failed(failure) => transport_failure(failure),
        _ => Diagnostic { failure: descriptor("consumer", Some("output-missing"), Some("offer")), message: None },
    };
    let decision = match std::mem::replace(&mut sink.state, OfferState::Idle) {
        OfferState::Committed { sequence, output_hash, receipt, failure } => {
            execution.output = Some(receipt);
            match failure {
                // Policy A: the fallback row is retained and the operation still fails.
                Some(failure) => fail(Diagnostic { failure, message: None }),
                None if interrupted => cancelled,
                None => match &outcome.settlement {
                    Settlement::Normal | Settlement::Failed(Failure::DeliveryUnknown) => TerminalDecision::Normal { sequence, output_hash },
                    settlement => fail(transport_failed(settlement)),
                },
            }
        }
        OfferState::Unresolved { sequence, output_hash, failure } => {
            let committed = match failure {
                Some(failure) => TerminalDecision::Failed { failure },
                None if interrupted => cancelled.clone(),
                None => TerminalDecision::Normal { sequence, output_hash: output_hash.clone() },
            };
            let uncommitted = if interrupted { cancelled } else { fail(transport_failure(&Failure::DeliveryUnknown)) };
            lifecycle.defer(Pending::Offer { sequence, output_hash, committed, uncommitted });
            execution.diagnostic = diagnostic;
            execution.result = Err(invalid("output-unresolved"));
            return execution;
        }
        OfferState::Idle | OfferState::Uncommitted | OfferState::Pending(_) => match sink.refusal.take() {
            _ if interrupted => cancelled,
            Some(failure) => fail(Diagnostic { failure, message: None }),
            None => fail(transport_failed(&outcome.settlement)),
        },
    };
    execution.diagnostic = diagnostic;
    drop(sink);
    execution.result = settle_now(&access, &owner, lifecycle, decision).await;
    execution
}

fn settle_abandoned(store: &mut PersistentStore, owner: &Owner, reason: Option<CancelReason>, pending: Option<Pending>) -> StoreResult<Status> {
    let decision = match pending {
        Some(Pending::Decision(decision)) => decision,
        Some(Pending::Offer { sequence, output_hash, committed, uncommitted }) => match store.generation_resolve_output(owner, sequence, &output_hash)? {
            OutputResolution::Committed(_) => committed,
            OutputResolution::Uncommitted => uncommitted,
            OutputResolution::Unresolved => {
                let status = store.generation_status(&owner.ticket)?;
                if status.phase.terminal() { return Ok(status); }
                return Err(StoreError::CommitBusy);
            },
        },
        None => match (reason, store.generation_execution(&owner.ticket)?) {
            (Some(reason), _) => TerminalDecision::Cancelled { reason },
            // Without a caller's signal an issued request is uncertain, as it is for a foreign owner.
            (None, Some(execution)) if execution.dispatch_id.is_some() && execution.cancel.is_none() => return store.generation_settle_lost(owner),
            (None, _) => TerminalDecision::Cancelled { reason: CancelReason::Lifecycle },
        },
    };
    settle(store, owner, decision)
}

// Live owners keep running; an abandoned or deferred lifecycle settles here without HTTP or preparation.
pub(crate) fn resolve_owner(store: &mut PersistentStore, ticket: &Ticket) -> StoreResult<Status> {
    let scope = scope(store)?;
    let entry = registry().entries.iter().find(|entry| entry.scope == scope && &entry.owner.ticket == ticket)
        .map(|entry| (entry.owner.clone(), entry.lifecycle_id.clone(), entry.live.strong_count() > 0, entry.reason, entry.pending.clone()));
    let Some((owner, id, live, reason, pending)) = entry else { return store.generation_recover(ticket) };
    if live { return store.generation_status(ticket); }
    let remove = || registry().entries.retain(|entry| !(entry.scope == scope && &entry.owner.ticket == ticket && entry.lifecycle_id == id));
    match settle_abandoned(store, &owner, reason, pending) {
        Ok(status) => { remove(); Ok(status) }
        // Without execution evidence, or with a record this owner no longer holds, the lifecycle has nothing
        // left to settle; recovery answers for the record.
        Err(error) if ["execution-evidence-lost", "stale-target", "stale-owner", "stale-lifecycle"].iter().any(|code| is_code(&error, code)) => {
            remove();
            store.generation_recover(ticket)
        }
        Err(error) if is_code(&error, "expired-ticket") || is_code(&error, "unknown-ticket") => { remove(); Err(error) }
        // An unresolved offer or a store failure stays a recovery obligation.
        Err(error) => Err(error),
    }
}

// The bounded pass before reservation and capacity admission; a failed resolution stays pending.
pub(crate) fn reconcile(store: &mut PersistentStore) -> StoreResult<()> {
    reconcile_limited(store, MAX_RECONCILED, &mut resolve_owner)
}

// A failed entry moves behind the others, so entries that keep failing cannot hold the front of every pass.
pub(super) fn reconcile_limited(store: &mut PersistentStore, pass: usize,
    resolve: &mut dyn FnMut(&mut PersistentStore, &Ticket) -> StoreResult<Status>) -> StoreResult<()> {
    let scope = scope(store)?;
    let entries: Vec<(Ticket, String)> = registry().entries.iter().filter(|entry| entry.scope == scope && entry.live.strong_count() == 0)
        .take(pass).map(|entry| (entry.owner.ticket.clone(), entry.lifecycle_id.clone())).collect();
    for (ticket, id) in entries {
        if resolve(store, &ticket).is_ok() { continue; }
        let mut registry = registry();
        if let Some(index) = registry.entries.iter().position(|entry| entry.scope == scope && entry.owner.ticket == ticket && entry.lifecycle_id == id) {
            let entry = registry.entries.remove(index);
            registry.entries.push(entry);
        }
    }
    Ok(())
}
