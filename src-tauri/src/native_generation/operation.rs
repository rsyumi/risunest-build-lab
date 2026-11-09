use risunest_sync_wire::stamp::DecimalU64;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Target {
    pub character_id: String,
    pub conversation_id: String,
    pub expected_revision: i64,
    pub library_identity: String,
    pub binding_authority: DecimalU64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Ticket {
    pub epoch: String,
    pub sequence: DecimalU64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Phase {
    Reserved,
    Running,
    Completed,
    Cancelled,
    Stale,
    Interrupted,
    Failed,
}

impl Phase {
    pub(crate) fn terminal(&self) -> bool {
        !matches!(self, Self::Reserved | Self::Running)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Status {
    pub ticket: Ticket,
    pub phase: Phase,
    pub message_id: String,
    pub checkpoint: DecimalU64,
    pub committed_revision: Option<i64>,
    pub pending_commit: bool,
    pub preparation_ready: bool,
    pub preparation_error: Option<super::inputs::PreparationError>,
}

pub(crate) fn runtime_session_id() -> &'static str {
    static SESSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SESSION.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

#[derive(Debug, Clone)]
pub(crate) struct Owner {
    pub(crate) ticket: Ticket,
    pub(crate) session: String,
    pub(crate) token: String,
}

// This is a native store snapshot, not a prepared provider prompt or a renderer projection.
#[derive(Debug)]
pub(crate) struct StoredHistory {
    pub revision: i64,
    pub character: Value,
    pub conversation: Value,
    pub messages: Vec<Value>,
}

#[derive(Debug)]
pub(crate) enum Start {
    Started { owner: Owner, history: StoredHistory, status: Status },
    Existing(Status),
}

#[derive(Debug, Clone)]
pub(crate) struct Checkpoint {
    pub sequence: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenerationLease {
    pub token: String,
    pub revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectionIdentity {
    pub store_id: String,
    pub library_epoch: String,
    pub generation: String,
    pub selection_epoch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OriginalIdState { Missing, Null, Present }

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OriginalRowWitness {
    pub original_index: usize,
    pub row_hash: String,
    pub id_state: OriginalIdState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizationWitness {
    pub ticket: Ticket,
    pub target: Target,
    pub identity: ProjectionIdentity,
    pub original_revision: i64,
    pub original_count: usize,
    pub rows: Vec<OriginalRowWitness>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizationReceipt {
    pub target: Target,
    pub request_id: String,
    pub intent_digest: String,
    pub receipt_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NormalizationEvidence {
    Unknown,
    Pending,
    NoIdEffects,
    Recorded(NormalizationReceipt),
    Verified(NormalizationReceipt),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizationResolution {
    pub status: Status,
    pub evidence: NormalizationEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProjectionUnavailable {
    ExpiredTicket, Pending, LeaseUnavailable, StaleLease, IdentityChanged,
    InvalidWitness, HistoryChanged, MessageFenceChanged, Limit, ReceiptIntegrity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedResidentRow {
    pub original_index: usize,
    pub current_index: usize,
    pub original_row_hash: String,
    pub original_id_state: OriginalIdState,
    pub assigned_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizationProjection {
    pub baseline_revision: i64,
    pub receipt_revision: i64,
    pub projected_revision: i64,
    pub original_count: usize,
    pub current_count: usize,
    // The native transition preserves precisely this prefix's original row origins.
    pub prefix_count: usize,
    pub rows: Vec<NormalizedResidentRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NormalizationMapping {
    NoIdEffects,
    Mapped(NormalizationProjection),
    Unavailable(ProjectionUnavailable),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizationProjectionResult {
    pub status: Option<Status>,
    pub evidence: NormalizationEvidence,
    pub mapping: NormalizationMapping,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DispatchState { None, Issued, Uncertain }

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutputState {
    None,
    // The receipt recorded when its output attempt resolved against the LWW request receipt.
    Committed(crate::persistent_store::generation::OutputReceipt),
    Unavailable,
}

// Persisted owner evidence for one ticket. Live transport diagnostics are not part of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnerStatus {
    pub status: Status,
    pub owned: bool,
    pub dispatch: DispatchState,
    pub output: OutputState,
    pub cancel_decision: Option<crate::persistent_store::generation::CancelReason>,
    pub failure: Option<crate::persistent_store::generation::FailureDescriptor>,
}

// A reserved or running record of this device store, for status reporting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActiveGeneration {
    pub ticket: Ticket,
    pub library_identity: String,
    pub character_id: String,
    pub conversation_id: String,
    pub phase: Phase,
}
