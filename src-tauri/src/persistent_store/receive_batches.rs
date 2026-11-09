use super::{lww, receive_evidence::{self, DevicePluginFence, NativeReceiveEvidence}, sync_selection, PersistentStore, StoreError, StoreResult};
use risunest_sync_wire::{stamp::DecimalU64, unit::{UnitKey, MAX_UNIT_KEY_BYTES}};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::BTreeMap;

pub(crate) const SETTING_PREFIX: &str = "__risunest_receive_v1__:";
const COORDINATOR_KEY: &str = "__risunest_receive_v1__:coordinator";
const MAX_RECORD_BYTES: usize = 2 * 1024 * 1024;
const MAX_DECODED_BYTES: usize = 1024 * 1024;
pub(super) const MAX_STEP_ROWS: usize = 128;
const MAX_PAGE_KEYS: usize = 256;
const EMPTY_ROOT: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn invalid(message: &'static str) -> StoreError {
    StoreError::Validation { message: message.into() }
}
fn digest(value: &impl Serialize) -> StoreResult<String> {
    Ok(risunest_sync_wire::hash(&serde_json::to_vec(value)?))
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Mode { Stored, Staged, Drain }

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Partition { Library, Device }
impl Partition {
    fn table(self) -> &'static str {
        match self { Self::Library => "app_kv", Self::Device => "device_settings" }
    }
    fn name(self) -> &'static str {
        match self { Self::Library => "library", Self::Device => "device" }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Capability {
    pub incarnation: String,
    pub sequence: DecimalU64,
    pub nonce: String,
    pub mode: Mode,
    pub source_digest: String,
    pub identity_digest: String,
}
impl Capability {
    fn validate(&self) -> StoreResult<()> {
        if uuid::Uuid::parse_str(&self.incarnation).is_err() ||
            uuid::Uuid::parse_str(&self.nonce).is_err() || self.sequence.0 == 0 ||
            !hash_valid(&self.source_digest) || !hash_valid(&self.identity_digest) {
            return Err(invalid("native-receive-capability-invalid"));
        }
        Ok(())
    }
    fn key(&self) -> StoreResult<String> {
        self.validate()?;
        Ok(format!("{}operation:{}:{}:", SETTING_PREFIX, self.incarnation, self.sequence.0))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "kebab-case", rename_all_fields = "camelCase", deny_unknown_fields)]
pub(crate) enum Source {
    Stored { header: lww::Header },
    Staged { header: lww::Header },
    Drain { binding_authority: DecimalU64 },
}
impl Source {
    fn mode(&self) -> Mode {
        match self { Self::Stored { .. } => Mode::Stored, Self::Staged { .. } => Mode::Staged, Self::Drain { .. } => Mode::Drain }
    }
    fn authority(&self) -> DecimalU64 {
        match self { Self::Stored { header } | Self::Staged { header } => header.binding_authority,
            Self::Drain { binding_authority } => *binding_authority }
    }
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BeginRequest {
    pub candidate: Capability,
    pub source: Source,
    pub native_baseline: Option<NativeReceiveEvidence>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AdvanceRequest {
    pub capability: Capability,
    pub expected_progress_token: String,
    #[serde(default)]
    pub generating: Vec<lww::MessageLocator>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationState {
    pub capability: Capability,
    pub progress_token: String,
    pub phase: Phase,
    pub revision: i64,
    pub outcome: Option<Outcome>,
    pub foreign: Foreign,
    pub roots: Option<Roots>,
    pub device_plugins: Option<DevicePluginFence>,
    pub device_revision: Option<DecimalU64>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub(crate) enum BeginResult {
    Admitted { state: OperationState },
    Existing { state: OperationState },
    Recovery { state: OperationState },
}
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StepFence {
    pub keys: Vec<UnitKey>,
    pub unknown_library: bool,
    pub unknown_device: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub(crate) enum AdvanceResult {
    Advanced { state: OperationState, fence: StepFence },
    Stale { state: OperationState, fence: StepFence },
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PageKind { Effects, Dispositions }
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReadPage {
    pub capability: Capability,
    pub partition: Partition,
    pub kind: PageKind,
    pub ordinal: DecimalU64,
    pub previous_digest: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum HistoryPage {
    Effects { page: EffectPage, digest: String },
    Dispositions { page: DispositionPage, digest: String },
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DeliveryRequest {
    pub capability: Capability,
    pub roots: Roots,
    pub projection: NativeReceiveEvidence,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RetireRequest { pub capability: Capability, pub roots: Roots }
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub(crate) enum RetireResult { Pending, Retired }
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Completion { pub capability: Capability, pub revision: i64, pub outcome: Outcome }

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Origin {
    pub request_digest: String,
    pub request_bytes: u64,
    pub source_rowid: Option<i64>,
    pub body_digest: String,
    #[serde(default)]
    pub body_bytes: u64,
    pub authority: DecimalU64,
    pub progress: Option<ProgressWitness>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ProgressWitness {
    pub external: bool,
    pub cursor: DecimalU64,
    pub writer_digest: String,
    pub writer_bytes: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Phase { Admitting, Producing, Finalizing, Sealed, Delivered, Settled, Cleanup }

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Outcome { Processed, Superseded }

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClosedRange { first: u64, last: u64 }

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Coordinator {
    incarnation: String,
    next_sequence: u64,
    closed: Vec<ClosedRange>,
    server_history: Option<Capability>,
    local_history: Option<Capability>,
    active_producer: Option<Capability>,
    cleanup: Vec<CleanupProof>,
}
impl Coordinator {
    pub(super) fn new() -> Self {
        Self { incarnation: uuid::Uuid::new_v4().to_string(), next_sequence: 1,
            closed: Vec::new(), server_history: None, local_history: None, active_producer: None, cleanup: Vec::new() }
    }
    fn slot(&self, mode: Mode) -> &Option<Capability> {
        if mode == Mode::Stored { &self.server_history } else { &self.local_history }
    }
    fn slot_mut(&mut self, mode: Mode) -> &mut Option<Capability> {
        if mode == Mode::Stored { &mut self.server_history } else { &mut self.local_history }
    }
    pub(super) fn candidate(&self, mode: Mode, source_digest: String, identity_digest: String) -> StoreResult<Capability> {
        self.validate()?;
        let candidate = Capability { incarnation: self.incarnation.clone(), sequence: self.next_sequence.into(),
            nonce: uuid::Uuid::new_v4().to_string(), mode, source_digest, identity_digest };
        candidate.validate()?;
        Ok(candidate)
    }
    pub(super) fn validate(&self) -> StoreResult<()> {
        if uuid::Uuid::parse_str(&self.incarnation).is_err() || self.next_sequence == 0 || self.closed.len() > 3 || self.cleanup.len() > 2 {
            return Err(invalid("native-receive-coordinator-integrity"));
        }
        let mut coverage = self.closed.iter().map(|range| (range.first, range.last)).collect::<Vec<_>>();
        if self.server_history.as_ref().is_some_and(|cap| cap.mode != Mode::Stored) ||
            self.local_history.as_ref().is_some_and(|cap| cap.mode == Mode::Stored) {
            return Err(invalid("native-receive-coordinator-integrity"));
        }
        let mut last = 0;
        for range in &self.closed {
            if range.first == 0 || range.first > range.last || range.first <= last || range.last >= self.next_sequence ||
                (last != 0 && last.checked_add(1) == Some(range.first)) {
                return Err(invalid("native-receive-coordinator-integrity"));
            }
            last = range.last;
        }
        for cap in [self.server_history.as_ref(), self.local_history.as_ref()].into_iter().flatten() {
            cap.validate()?;
            if cap.incarnation != self.incarnation || cap.sequence.0 >= self.next_sequence {
                return Err(invalid("native-receive-coordinator-integrity"));
            }
            coverage.push((cap.sequence.0, cap.sequence.0));
        }
        coverage.sort_unstable();
        let mut expected = 1u64;
        for (first, last) in coverage {
            if first != expected { return Err(invalid("native-receive-coordinator-integrity")); }
            expected = last.checked_add(1).ok_or_else(|| invalid("native-receive-coordinator-integrity"))?;
        }
        if expected != self.next_sequence { return Err(invalid("native-receive-coordinator-integrity")); }
        if let Some(active) = &self.active_producer {
            if self.slot(active.mode).as_ref() != Some(active) {
                return Err(invalid("native-receive-coordinator-integrity"));
            }
        }
        for (index, proof) in self.cleanup.iter().enumerate() {
            proof.roots.validate()?;
            if self.slot(proof.capability.mode).as_ref() != Some(&proof.capability) ||
                self.active_producer.as_ref() == Some(&proof.capability) ||
                self.cleanup[..index].iter().any(|other| other.capability == proof.capability) {
                return Err(invalid("native-receive-cleanup-integrity"));
            }
        }
        Ok(())
    }
    pub(super) fn require(&self, cap: &Capability) -> StoreResult<()> {
        self.validate()?;
        cap.validate()?;
        if cap.incarnation != self.incarnation ||
            self.closed.iter().any(|range| range.first <= cap.sequence.0 && cap.sequence.0 <= range.last) {
            return Err(invalid("native-receive-capability-retired"));
        }
        if self.slot(cap.mode).as_ref() != Some(cap) {
            return Err(invalid("native-receive-capability-unavailable"));
        }
        Ok(())
    }
    pub(super) fn admit(&mut self, candidate: &Capability) -> StoreResult<Admission> {
        self.validate()?;
        candidate.validate()?;
        if candidate.incarnation != self.incarnation {
            return Err(invalid("native-receive-candidate-consumed"));
        }
        if candidate.sequence.0 < self.next_sequence {
            return if self.slot(candidate.mode).as_ref() == Some(candidate) {
                Ok(Admission::Existing(candidate.clone()))
            } else { Err(invalid("native-receive-candidate-consumed")) };
        }
        if candidate.sequence.0 != self.next_sequence {
            return Err(invalid("native-receive-candidate-conflict"));
        }
        if let Some(previous) = self.active_producer.as_ref().or(self.slot(candidate.mode).as_ref()) {
            return Ok(Admission::Recovery(previous.clone()));
        }
        self.next_sequence = self.next_sequence.checked_add(1)
            .ok_or_else(|| invalid("native-receive-sequence-exhausted"))?;
        *self.slot_mut(candidate.mode) = Some(candidate.clone());
        self.active_producer = Some(candidate.clone());
        self.validate()?;
        Ok(Admission::Admitted(candidate.clone()))
    }
    pub(super) fn sealed(&mut self, cap: &Capability) -> StoreResult<()> {
        self.require(cap)?;
        if self.active_producer.as_ref() != Some(cap) {
            return Err(invalid("native-receive-producer-changed"));
        }
        self.active_producer = None;
        Ok(())
    }
    pub(super) fn close(&mut self, cap: &Capability) -> StoreResult<()> {
        self.require(cap)?;
        if self.active_producer.as_ref() == Some(cap) {
            return Err(invalid("native-receive-producer-active"));
        }
        let proof = self.cleanup.iter().position(|proof| proof.capability == *cap && proof.library_done && proof.device_done)
            .ok_or_else(|| invalid("native-receive-cleanup-incomplete"))?;
        self.cleanup.remove(proof);
        *self.slot_mut(cap.mode) = None;
        self.closed.push(ClosedRange { first: cap.sequence.0, last: cap.sequence.0 });
        self.closed.sort_by_key(|range| range.first);
        let mut merged: Vec<ClosedRange> = Vec::new();
        for range in self.closed.drain(..) {
            if let Some(prior) = merged.last_mut() {
                if prior.last.checked_add(1) == Some(range.first) { prior.last = range.last; continue; }
            }
            merged.push(range);
        }
        self.closed = merged;
        self.validate()
    }
}

#[derive(Clone, Debug)]
pub(super) enum Admission { Admitted(Capability), Existing(Capability), Recovery(Capability) }

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Foreign {
    pub library: bool,
    pub device: bool,
    pub plugins: bool,
    pub scope: bool,
}
impl Foreign {
    pub(super) fn unknown() -> Self { Self { library: true, device: true, plugins: true, scope: true } }
    pub(super) fn merge(&mut self, other: &Self) {
        self.library |= other.library; self.device |= other.device;
        self.plugins |= other.plugins; self.scope |= other.scope;
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Observation {
    pub library_revision: i64,
    pub device_revision: i64,
    pub plugins: DevicePluginFence,
}


#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InputCursor { offset: u64, ordinal: u64 }

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum InputCompletion { JsonEnd, FinishedProof }

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InputBatch {
    source_binding: String,
    start: InputCursor,
    end: InputCursor,
    span_digest: String,
    content_digest: String,
    ended: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InputProgress {
    cursor: InputCursor,
    completion: Option<InputCompletion>,
    last_batch: Option<InputBatch>,
}
impl InputProgress {
    fn validate(&self, cap: &Capability, origin: &Origin) -> StoreResult<()> {
        if self.cursor.offset > origin.body_bytes ||
            (self.cursor.offset == 0 && self.cursor.ordinal != 0) {
            return Err(invalid("native-receive-input-integrity"));
        }
        if let Some(batch) = &self.last_batch {
            if batch.source_binding != digest(&(cap, origin))? || batch.end != self.cursor ||
                batch.end.offset <= batch.start.offset || batch.start.ordinal > batch.end.ordinal ||
                batch.end.ordinal - batch.start.ordinal > MAX_STEP_ROWS as u64 ||
                !hash_valid(&batch.span_digest) || !hash_valid(&batch.content_digest) ||
                (self.completion == Some(InputCompletion::JsonEnd)) != batch.ended &&
                    self.completion != Some(InputCompletion::FinishedProof) {
                return Err(invalid("native-receive-input-integrity"));
            }
        } else if self.cursor != InputCursor::default() || self.completion == Some(InputCompletion::JsonEnd) {
            return Err(invalid("native-receive-input-integrity"));
        }
        if self.completion == Some(InputCompletion::JsonEnd) && self.cursor.offset != origin.body_bytes {
            return Err(invalid("native-receive-input-integrity"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum ScanPhase { Archives, Exists, ArchivesAfterParents, Remaining, Dispositions, Complete }

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Cursor {
    pub phase: ScanPhase,
    pub epoch: u64,
    pub frontier: i64,
    pub sentinel_digest: Option<String>,
    pub after_key: Option<String>,
    pub after_rowid: i64,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum TransitionDecision { ApplyRemote, Identical }
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Residency { Absent, Active, Archived }
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RowWitness {
    pub rowid: i64,
    pub digest: String,
    pub request_digest: String,
    pub request_bytes: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct LegacyIntentWitness {
    pub rowid: i64,
    pub body_digest: String,
    pub request_digest: String,
    pub request_bytes: u64,
    pub receipt_digest: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct TransitionTicket {
    pub capability: Capability,
    pub source: RowWitness,
    pub archive_key: String,
    pub restore: bool,
    pub decision: TransitionDecision,
    pub expected_residency: Residency,
    pub expected_revision: i64,
    pub expected_unit_digest: Option<String>,
    pub observed_stamp_digest: String,
    pub cas_job: Option<String>,
    pub legacy: Option<LegacyIntentWitness>,
}
impl TransitionTicket {
    fn validate(&self) -> StoreResult<()> {
        self.capability.validate()?;
        validate_key(&self.archive_key)?;
        let key: UnitKey = self.archive_key.clone().try_into().map_err(|_| invalid("native-receive-ticket-integrity"))?;
        if key.components()[0] != "archive" || self.source.rowid <= 0 || self.expected_revision < 0 ||
            !hash_valid(&self.source.digest) || !hash_valid(&self.source.request_digest) ||
            !hash_valid(&self.observed_stamp_digest) ||
            self.expected_unit_digest.as_ref().is_some_and(|value| !hash_valid(value)) ||
            self.cas_job.as_ref().is_some_and(|value| uuid::Uuid::parse_str(value).is_err()) {
            return Err(invalid("native-receive-ticket-integrity"));
        }
        if let Some(legacy) = &self.legacy {
            if legacy.rowid <= 0 || !hash_valid(&legacy.body_digest) || !hash_valid(&legacy.request_digest) ||
                !hash_valid(&legacy.receipt_digest) { return Err(invalid("native-receive-ticket-integrity")); }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Manifest {
    pub capability: Capability,
    pub phase: Phase,
    pub origin: Origin,
    #[serde(default)]
    input: Option<InputProgress>,
    pub cursor: Cursor,
    pub observation: Observation,
    pub foreign: Foreign,
    pub effect_pages: u64,
    pub effect_root: String,
    pub affected_count: u64,
    pub plugin_changed_count: u64,
    pub disposition_pages: u64,
    pub disposition_root: String,
    pub held_count: u64,
    pub deferred_count: u64,
    pub outcome: Option<Outcome>,
    pub delivered: Option<Roots>,
    pub local_finish_witness: bool,
    pub ticket: Option<TransitionTicket>,
    pub cleanup_cursor: Option<String>,
    pub validation: Option<HistoryValidation>,
    pub validated: Option<HistoryValidationProof>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum ValidationPurpose { Seal, Cleanup }
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct HistoryValidationProof { purpose: ValidationPurpose, root: PartitionRoot }
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct HistoryValidation {
    purpose: ValidationPurpose,
    root: PartitionRoot,
    effect_ordinal: u64,
    effect_previous: String,
    affected: u64,
    plugin_changed: u64,
    disposition_ordinal: u64,
    disposition_previous: String,
    held: u64,
    deferred: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PartitionRoot {
    pub pages: DecimalU64,
    pub root: String,
    pub disposition_pages: DecimalU64,
    pub disposition_root: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Roots { pub library: PartitionRoot, pub device: PartitionRoot }

impl PartitionRoot {
    fn validate(&self) -> StoreResult<()> {
        if !hash_valid(&self.root) || !hash_valid(&self.disposition_root) ||
            (self.pages.0 == 0 && self.root != EMPTY_ROOT) ||
            (self.disposition_pages.0 == 0 && self.disposition_root != EMPTY_ROOT) {
            return Err(invalid("native-receive-root-integrity"));
        }
        Ok(())
    }
}
impl Roots {
    fn validate(&self) -> StoreResult<()> { self.library.validate()?; self.device.validate() }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CleanupProof { capability: Capability, roots: Roots, library_done: bool, device_done: bool }

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Effect {
    pub key: String,
    pub affected: bool,
    pub plugin_storage_changed: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EffectPage {
    pub capability_digest: String,
    pub partition: Partition,
    pub ordinal: DecimalU64,
    pub previous_digest: String,
    pub effects: Vec<Effect>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EffectIndex {
    key: String,
    affected_at: Option<u64>,
    plugin_changed_at: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Disposition { Held, Deferred }
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DispositionEntry { pub key: String, pub disposition: Disposition }
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DispositionPage {
    pub capability_digest: String,
    pub partition: Partition,
    pub ordinal: DecimalU64,
    pub previous_digest: String,
    pub entries: Vec<DispositionEntry>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DispositionIndex { key: String, page: u64, disposition: Disposition }

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Envelope { version: u8, data: String }

fn hash_valid(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn encode(value: &impl Serialize) -> StoreResult<String> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_DECODED_BYTES { return Err(invalid("native-receive-record-too-large")); }
    let result = serde_json::to_string(&Envelope { version: 1, data: hex::encode(bytes) })?;
    if result.len() > MAX_RECORD_BYTES { return Err(invalid("native-receive-record-too-large")); }
    Ok(result)
}
fn decode<T: DeserializeOwned>(raw: &str) -> StoreResult<T> {
    if raw.len() > MAX_RECORD_BYTES { return Err(invalid("native-receive-record-too-large")); }
    let envelope: Envelope = serde_json::from_str(raw)?;
    if envelope.version != 1 || envelope.data.len() > MAX_DECODED_BYTES * 2 ||
        envelope.data.len() % 2 != 0 ||
        !envelope.data.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
        return Err(invalid("native-receive-record-integrity"));
    }
    let bytes = hex::decode(envelope.data).map_err(|_| invalid("native-receive-record-integrity"))?;
    Ok(serde_json::from_slice(&bytes)?)
}
fn read<T: DeserializeOwned>(db: &Connection, partition: Partition, key: &str) -> StoreResult<Option<T>> {
    let value: Option<(i64, Option<String>)> = db.query_row(&format!(
        "SELECT length(CAST(value AS BLOB)),CASE WHEN typeof(value)='text' AND length(CAST(value AS BLOB))<=?2 THEN value ELSE NULL END FROM {} WHERE key=?1", partition.table()),
        params![key, MAX_RECORD_BYTES as i64], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
    let Some((length, raw)) = value else { return Ok(None); };
    if length < 0 || length > MAX_RECORD_BYTES as i64 { return Err(invalid("native-receive-record-too-large")); }
    let raw = raw.ok_or_else(|| invalid("native-receive-record-integrity"))?;
    decode(&raw).map(Some)
}
fn write(db: &Connection, partition: Partition, key: &str, value: &impl Serialize) -> StoreResult<()> {
    require_transaction(db)?;
    let value = encode(value)?;
    db.execute(&format!("INSERT INTO {}(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", partition.table()),
        params![key, value])?;
    Ok(())
}

pub(super) fn read_coordinator(db: &Connection) -> StoreResult<Option<Coordinator>> {
    let value: Option<Coordinator> = read(db, Partition::Device, COORDINATOR_KEY)?;
    if let Some(value) = &value { value.validate()?; }
    else if namespace_exists(db, Partition::Device)? {
        return Err(invalid("native-receive-coordinator-missing"));
    }
    Ok(value)
}
pub(super) fn write_coordinator(db: &Connection, value: &Coordinator) -> StoreResult<()> {
    value.validate()?;
    write(db, Partition::Device, COORDINATOR_KEY, value)
}
impl Manifest {
    fn validate(&self) -> StoreResult<()> {
        self.capability.validate()?;
        if !hash_valid(&self.effect_root) || !hash_valid(&self.disposition_root) ||
            !hash_valid(&self.origin.request_digest) || !hash_valid(&self.origin.body_digest) ||
            self.origin.source_rowid.is_some_and(|row| row <= 0) || self.cursor.frontier < 0 || self.cursor.after_rowid < 0 ||
            self.cursor.sentinel_digest.as_ref().is_some_and(|value| !hash_valid(value)) ||
            (self.cursor.frontier == 0) != self.cursor.sentinel_digest.is_none() ||
            self.observation.library_revision < 0 || self.observation.device_revision < 0 ||
            (self.effect_pages == 0 && (self.effect_root != EMPTY_ROOT || self.affected_count != 0 || self.plugin_changed_count != 0)) ||
            (self.disposition_pages == 0 && (self.disposition_root != EMPTY_ROOT || self.held_count != 0 || self.deferred_count != 0)) {
            return Err(invalid("native-receive-manifest-integrity"));
        }
        if let Some(input) = &self.input { input.validate(&self.capability, &self.origin)?; }
        if let Some(ticket) = &self.ticket {
            ticket.validate()?;
            if ticket.capability != self.capability || self.phase != Phase::Producing {
                return Err(invalid("native-receive-ticket-integrity"));
            }
        }
        if let Some(progress) = &self.origin.progress {
            if !hash_valid(&progress.writer_digest) { return Err(invalid("native-receive-manifest-integrity")); }
        }
        if let Some(key) = &self.cursor.after_key { validate_key(key)?; }
        if let Some(roots) = &self.delivered { roots.validate()?; }
        if let Some(proof) = &self.validated {
            proof.root.validate()?;
            if proof.root != self.root() { return Err(invalid("native-receive-validation-integrity")); }
        }
        if let Some(cursor) = &self.validation {
            if cursor.root != self.root() || cursor.effect_ordinal > self.effect_pages ||
                cursor.disposition_ordinal > self.disposition_pages || !hash_valid(&cursor.effect_previous) ||
                !hash_valid(&cursor.disposition_previous) || cursor.affected > self.affected_count ||
                cursor.plugin_changed > self.plugin_changed_count || cursor.held > self.held_count || cursor.deferred > self.deferred_count {
                return Err(invalid("native-receive-validation-integrity"));
            }
        }
        if self.outcome == Some(Outcome::Superseded) &&
            (self.disposition_pages != 0 || self.held_count != 0 || self.deferred_count != 0) {
            return Err(invalid("native-receive-superseded-dispositions"));
        }
        let terminal = matches!(self.phase, Phase::Sealed | Phase::Delivered | Phase::Settled | Phase::Cleanup);
        if terminal != self.outcome.is_some() ||
            (matches!(self.phase, Phase::Delivered | Phase::Settled | Phase::Cleanup) && self.delivered.is_none()) {
            return Err(invalid("native-receive-manifest-integrity"));
        }
        Ok(())
    }
    pub(super) fn root(&self) -> PartitionRoot {
        PartitionRoot { pages: self.effect_pages.into(), root: self.effect_root.clone(),
            disposition_pages: self.disposition_pages.into(), disposition_root: self.disposition_root.clone() }
    }
}
pub(super) fn read_manifest(db: &Connection, partition: Partition, cap: &Capability) -> StoreResult<Option<Manifest>> {
    let value: Option<Manifest> = read(db, partition, &format!("{}manifest", cap.key()?))?;
    if let Some(value) = &value {
        value.validate()?;
        if value.capability != *cap { return Err(invalid("native-receive-manifest-integrity")); }
    }
    Ok(value)
}
pub(super) fn write_manifest(db: &Connection, partition: Partition, value: &Manifest) -> StoreResult<()> {
    value.validate()?;
    write(db, partition, &format!("{}manifest", value.capability.key()?), value)
}
pub(super) fn empty_manifest(cap: Capability, origin: Origin, observation: Observation, frontier: i64,
    sentinel_digest: Option<String>, foreign: Foreign) -> Manifest {
    let input = origin.source_rowid.map(|_| InputProgress::default());
    Manifest { capability: cap, phase: Phase::Admitting, origin, input,
        cursor: Cursor { phase: ScanPhase::Archives, epoch: 0, frontier, sentinel_digest, after_key: None, after_rowid: 0 },
        observation, foreign, effect_pages: 0, effect_root: EMPTY_ROOT.into(), affected_count: 0,
        plugin_changed_count: 0, disposition_pages: 0, disposition_root: EMPTY_ROOT.into(), held_count: 0,
        deferred_count: 0, outcome: None, delivered: None, local_finish_witness: false, ticket: None, cleanup_cursor: None, validation: None, validated: None }
}
fn effect_key(cap: &Capability, ordinal: u64) -> StoreResult<String> {
    Ok(format!("{}effect:{ordinal:020}", cap.key()?))
}
fn index_key(cap: &Capability, key: &str) -> StoreResult<String> {
    Ok(format!("{}index:{}", cap.key()?, risunest_sync_wire::hash(key.as_bytes())))
}
fn validate_key(key: &str) -> StoreResult<()> {
    if key.len() > MAX_UNIT_KEY_BYTES { return Err(invalid("native-receive-effect-invalid")); }
    let _: UnitKey = key.to_owned().try_into().map_err(|_| invalid("native-receive-effect-invalid"))?;
    Ok(())
}
fn validate_effect(effect: &Effect) -> StoreResult<()> {
    if !effect.affected && !effect.plugin_storage_changed {
        return Err(invalid("native-receive-effect-invalid"));
    }
    validate_key(&effect.key)
}
fn require_transaction(db: &Connection) -> StoreResult<()> {
    if db.is_autocommit() { return Err(invalid("native-receive-write-outside-transaction")); }
    Ok(())
}

pub(super) fn append_effects(db: &Connection, partition: Partition, manifest: &mut Manifest, effects: &[Effect]) -> StoreResult<()> {
    require_transaction(db)?;
    if manifest.phase != Phase::Producing || effects.len() > MAX_PAGE_KEYS {
        return Err(invalid("native-receive-effect-phase"));
    }
    let mut unique: BTreeMap<&str, (bool, bool)> = BTreeMap::new();
    let mut bytes = 0usize;
    for effect in effects {
        validate_effect(effect)?;
        bytes = bytes.checked_add(serde_json::to_vec(effect)?.len())
            .ok_or_else(|| invalid("native-receive-record-too-large"))?;
        if bytes > MAX_DECODED_BYTES / 2 { return Err(invalid("native-receive-step-too-large")); }
        let entry = unique.entry(&effect.key).or_default();
        entry.0 |= effect.affected; entry.1 |= effect.plugin_storage_changed;
    }
    let mut added = Vec::new();
    let mut indexes = Vec::new();
    for (effect_key, (affected, plugin_changed)) in unique {
        let key = index_key(&manifest.capability, effect_key)?;
        let mut old: EffectIndex = read(db, partition, &key)?.unwrap_or_else(|| EffectIndex {
            key: effect_key.to_owned(), affected_at: None, plugin_changed_at: None });
        if old.key != effect_key || old.affected_at.is_some_and(|page| page >= manifest.effect_pages) ||
            old.plugin_changed_at.is_some_and(|page| page >= manifest.effect_pages) {
            return Err(invalid("native-receive-effect-index-integrity"));
        }
        for (ordinal, plugin) in [(old.affected_at, false), (old.plugin_changed_at, true)] {
            if let Some(ordinal) = ordinal {
                let page: EffectPage = read(db, partition, &self::effect_key(&manifest.capability, ordinal)?)?
                    .ok_or_else(|| invalid("native-receive-effect-page-missing"))?;
                if page.capability_digest != digest(&manifest.capability)? || page.partition != partition ||
                    page.ordinal.0 != ordinal || page.effects.len() > MAX_PAGE_KEYS ||
                    !page.effects.iter().any(|entry| entry.key == effect_key &&
                        if plugin { entry.plugin_storage_changed } else { entry.affected }) {
                    return Err(invalid("native-receive-effect-index-integrity"));
                }
            }
        }
        let next = Effect { key: effect_key.to_owned(), affected: affected && old.affected_at.is_none(),
            plugin_storage_changed: plugin_changed && old.plugin_changed_at.is_none() };
        if !next.affected && !next.plugin_storage_changed { continue; }
        if next.affected { old.affected_at = Some(manifest.effect_pages); }
        if next.plugin_storage_changed { old.plugin_changed_at = Some(manifest.effect_pages); }
        indexes.push((key, old));
        added.push(next);
    }
    if added.is_empty() { return Ok(()); }
    let page = EffectPage { capability_digest: digest(&manifest.capability)?, partition, ordinal: manifest.effect_pages.into(),
        previous_digest: manifest.effect_root.clone(), effects: added };
    let raw = encode(&page)?;
    let key = effect_key(&manifest.capability, manifest.effect_pages)?;
    let exists: bool = db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {} WHERE key=?1)", partition.table()), [&key], |row| row.get(0))?;
    if exists { return Err(invalid("native-receive-effect-page-exists")); }
    let mut next = manifest.clone();
    for effect in &page.effects {
        next.affected_count = next.affected_count.checked_add(u64::from(effect.affected))
            .ok_or_else(|| invalid("native-receive-count-overflow"))?;
        next.plugin_changed_count = next.plugin_changed_count.checked_add(u64::from(effect.plugin_storage_changed))
            .ok_or_else(|| invalid("native-receive-count-overflow"))?;
    }
    next.effect_root = digest(&page)?;
    next.effect_pages = next.effect_pages.checked_add(1).ok_or_else(|| invalid("native-receive-count-overflow"))?;
    if manifest.validated.as_ref().is_some_and(|proof| proof.purpose == ValidationPurpose::Seal && proof.root == manifest.root()) {
        next.validated = Some(HistoryValidationProof { purpose: ValidationPurpose::Seal, root: next.root() });
    }

    db.execute(&format!("INSERT INTO {}(key,value) VALUES(?1,?2)", partition.table()), params![key, raw])?;
    for (key, index) in indexes { write(db, partition, &key, &index)?; }
    write_manifest(db, partition, &next)?;
    *manifest = next;
    Ok(())
}
pub(super) fn read_effect_page(db: &Connection, partition: Partition, manifest: &Manifest, ordinal: u64,
    previous_digest: &str) -> StoreResult<EffectPage> {
    if ordinal >= manifest.effect_pages { return Err(invalid("native-receive-effect-page-outside-manifest")); }
    let page: EffectPage = read(db, partition, &effect_key(&manifest.capability, ordinal)?)?
        .ok_or_else(|| invalid("native-receive-effect-page-missing"))?;
    if page.capability_digest != digest(&manifest.capability)? || page.partition != partition ||
        page.ordinal.0 != ordinal || page.previous_digest != previous_digest ||
        page.effects.is_empty() || page.effects.len() > MAX_PAGE_KEYS {
        return Err(invalid("native-receive-effect-page-integrity"));
    }
    let mut previous_key: Option<&str> = None;
    for effect in &page.effects {
        validate_effect(effect)?;
        if previous_key.is_some_and(|key| key >= effect.key.as_str()) {
            return Err(invalid("native-receive-effect-page-integrity"));
        }
        previous_key = Some(&effect.key);
    }
    if ordinal + 1 == manifest.effect_pages && digest(&page)? != manifest.effect_root {
        return Err(invalid("native-receive-effect-root-integrity"));
    }
    Ok(page)
}
fn validate_effect_indexes(db: &Connection, partition: Partition, manifest: &Manifest, page: &EffectPage) -> StoreResult<(u64, u64)> {
    let mut affected = 0u64;
    let mut plugin_changed = 0u64;
    for effect in &page.effects {
        affected += u64::from(effect.affected);
        plugin_changed += u64::from(effect.plugin_storage_changed);
        let index: EffectIndex = read(db, partition, &index_key(&manifest.capability, &effect.key)?)?
            .ok_or_else(|| invalid("native-receive-effect-index-missing"))?;
        if index.key != effect.key || (effect.affected && index.affected_at != Some(page.ordinal.0)) ||
            (effect.plugin_storage_changed && index.plugin_changed_at != Some(page.ordinal.0)) {
            return Err(invalid("native-receive-effect-index-integrity"));
        }
    }
    Ok((affected, plugin_changed))
}
#[cfg(test)]
fn validate_effect_history(db: &Connection, partition: Partition, manifest: &Manifest) -> StoreResult<()> {
    let mut previous = EMPTY_ROOT.to_owned();
    let (mut affected, mut plugin_changed) = (0u64, 0u64);
    for ordinal in 0..manifest.effect_pages {
        let page = read_effect_page(db, partition, manifest, ordinal, &previous)?;
        let counts = validate_effect_indexes(db, partition, manifest, &page)?;
        affected = checked_count(affected, counts.0)?;
        plugin_changed = checked_count(plugin_changed, counts.1)?;
        previous = digest(&page)?;
    }
    if previous != manifest.effect_root || affected != manifest.affected_count || plugin_changed != manifest.plugin_changed_count {
        return Err(invalid("native-receive-effect-history-integrity"));
    }
    Ok(())
}

fn disposition_key(cap: &Capability, ordinal: u64) -> StoreResult<String> {
    Ok(format!("{}disposition:{ordinal:020}", cap.key()?))
}
fn disposition_index_key(cap: &Capability, key: &str) -> StoreResult<String> {
    Ok(format!("{}disposition-index:{}", cap.key()?, risunest_sync_wire::hash(key.as_bytes())))
}
pub(super) fn append_dispositions(db: &Connection, partition: Partition, manifest: &mut Manifest,
    entries: &[DispositionEntry]) -> StoreResult<()> {
    require_transaction(db)?;
    if manifest.phase != Phase::Finalizing || entries.is_empty() || entries.len() > MAX_PAGE_KEYS {
        return Err(invalid("native-receive-disposition-phase"));
    }
    let mut unique = BTreeMap::new();
    let mut bytes = 0usize;
    for entry in entries {
        validate_key(&entry.key)?;
        bytes = bytes.checked_add(serde_json::to_vec(entry)?.len())
            .ok_or_else(|| invalid("native-receive-record-too-large"))?;
        if bytes > MAX_DECODED_BYTES / 2 { return Err(invalid("native-receive-step-too-large")); }
        if unique.insert(entry.key.as_str(), entry.disposition).is_some_and(|prior| prior != entry.disposition) {
            return Err(invalid("native-receive-disposition-changed"));
        }
    }
    let mut added = Vec::new();
    for (key, disposition) in unique {
        let prior: Option<DispositionIndex> = read(db, partition, &disposition_index_key(&manifest.capability, key)?)?;
        if let Some(prior) = prior {
            if prior.key != key || prior.page >= manifest.disposition_pages {
                return Err(invalid("native-receive-disposition-index-integrity"));
            }
            let page: DispositionPage = read(db, partition, &disposition_key(&manifest.capability, prior.page)?)?
                .ok_or_else(|| invalid("native-receive-disposition-page-missing"))?;
            if page.capability_digest != digest(&manifest.capability)? || page.partition != partition ||
                page.ordinal.0 != prior.page || page.entries.len() > MAX_PAGE_KEYS ||
                !page.entries.iter().any(|entry| entry.key == key && entry.disposition == prior.disposition) {
                return Err(invalid("native-receive-disposition-index-integrity"));
            }
            if prior.disposition != disposition { return Err(invalid("native-receive-disposition-changed")); }
            continue;
        }
        added.push(DispositionEntry { key: key.to_owned(), disposition });
    }
    if added.is_empty() { return Ok(()); }
    let page = DispositionPage { capability_digest: digest(&manifest.capability)?, partition,
        ordinal: manifest.disposition_pages.into(), previous_digest: manifest.disposition_root.clone(), entries: added };
    let raw = encode(&page)?;
    let key = disposition_key(&manifest.capability, manifest.disposition_pages)?;
    let mut next = manifest.clone();
    for entry in &page.entries {
        let count = if entry.disposition == Disposition::Held { &mut next.held_count } else { &mut next.deferred_count };
        *count = count.checked_add(1).ok_or_else(|| invalid("native-receive-count-overflow"))?;
    }
    next.disposition_root = digest(&page)?;
    next.disposition_pages = next.disposition_pages.checked_add(1).ok_or_else(|| invalid("native-receive-count-overflow"))?;
    if manifest.validated.as_ref().is_some_and(|proof| proof.purpose == ValidationPurpose::Seal && proof.root == manifest.root()) {
        next.validated = Some(HistoryValidationProof { purpose: ValidationPurpose::Seal, root: next.root() });
    }
    db.execute(&format!("INSERT INTO {}(key,value) VALUES(?1,?2)", partition.table()), params![key, raw])?;
    for entry in &page.entries {
        write(db, partition, &disposition_index_key(&manifest.capability, &entry.key)?,
            &DispositionIndex { key: entry.key.clone(), page: page.ordinal.0, disposition: entry.disposition })?;
    }
    write_manifest(db, partition, &next)?;
    *manifest = next;
    Ok(())
}
pub(super) fn read_disposition_page(db: &Connection, partition: Partition, manifest: &Manifest,
    ordinal: u64, previous_digest: &str) -> StoreResult<DispositionPage> {
    if ordinal >= manifest.disposition_pages { return Err(invalid("native-receive-disposition-page-outside-manifest")); }
    let page: DispositionPage = read(db, partition, &disposition_key(&manifest.capability, ordinal)?)?
        .ok_or_else(|| invalid("native-receive-disposition-page-missing"))?;
    if page.capability_digest != digest(&manifest.capability)? || page.partition != partition ||
        page.ordinal.0 != ordinal || page.previous_digest != previous_digest ||
        page.entries.is_empty() || page.entries.len() > MAX_PAGE_KEYS {
        return Err(invalid("native-receive-disposition-page-integrity"));
    }
    let mut previous_key: Option<&str> = None;
    for entry in &page.entries {
        validate_key(&entry.key)?;
        if previous_key.is_some_and(|key| key >= entry.key.as_str()) {
            return Err(invalid("native-receive-disposition-page-integrity"));
        }
        previous_key = Some(&entry.key);
    }
    if ordinal + 1 == manifest.disposition_pages && digest(&page)? != manifest.disposition_root {
        return Err(invalid("native-receive-disposition-root-integrity"));
    }
    Ok(page)
}
fn validate_disposition_indexes(db: &Connection, partition: Partition, manifest: &Manifest, page: &DispositionPage) -> StoreResult<(u64, u64)> {
    let (mut held, mut deferred) = (0u64, 0u64);
    for entry in &page.entries {
        if entry.disposition == Disposition::Held { held += 1; } else { deferred += 1; }
        let index: DispositionIndex = read(db, partition, &disposition_index_key(&manifest.capability, &entry.key)?)?
            .ok_or_else(|| invalid("native-receive-disposition-index-missing"))?;
        if index.key != entry.key || index.page != page.ordinal.0 || index.disposition != entry.disposition {
            return Err(invalid("native-receive-disposition-index-integrity"));
        }
    }
    Ok((held, deferred))
}
#[cfg(test)]
fn validate_disposition_history(db: &Connection, partition: Partition, manifest: &Manifest) -> StoreResult<()> {
    let mut previous = EMPTY_ROOT.to_owned();
    let (mut held, mut deferred) = (0u64, 0u64);
    for ordinal in 0..manifest.disposition_pages {
        let page = read_disposition_page(db, partition, manifest, ordinal, &previous)?;
        let counts = validate_disposition_indexes(db, partition, manifest, &page)?;
        held = checked_count(held, counts.0)?;
        deferred = checked_count(deferred, counts.1)?;
        previous = digest(&page)?;
    }
    if previous != manifest.disposition_root || held != manifest.held_count || deferred != manifest.deferred_count {
        return Err(invalid("native-receive-disposition-history-integrity"));
    }
    Ok(())
}
fn checked_count(count: u64, added: u64) -> StoreResult<u64> {
    count.checked_add(added).ok_or_else(|| invalid("native-receive-count-overflow"))
}

pub(super) fn validate_history_step(db: &Connection, partition: Partition, manifest: &mut Manifest,
    purpose: ValidationPurpose) -> StoreResult<bool> {
    require_transaction(db)?;
    if (purpose == ValidationPurpose::Seal && !matches!(manifest.phase, Phase::Producing | Phase::Finalizing)) ||
        (purpose == ValidationPurpose::Cleanup && manifest.phase != Phase::Settled) || manifest.ticket.is_some() {
        return Err(invalid("native-receive-validation-phase"));
    }
    manifest.validate()?;
    if manifest.validated.as_ref().is_some_and(|proof| proof.purpose == purpose && proof.root == manifest.root()) { return Ok(true); }
    let mut cursor = manifest.validation.clone().unwrap_or_else(|| HistoryValidation {
        purpose, root: manifest.root(), effect_ordinal: 0, effect_previous: EMPTY_ROOT.into(), affected: 0, plugin_changed: 0,
        disposition_ordinal: 0, disposition_previous: EMPTY_ROOT.into(), held: 0, deferred: 0,
    });
    if cursor.purpose != purpose || cursor.root != manifest.root() { return Err(invalid("native-receive-validation-changed")); }
    if cursor.effect_ordinal < manifest.effect_pages {
        let page = read_effect_page(db, partition, manifest, cursor.effect_ordinal, &cursor.effect_previous)?;
        let counts = validate_effect_indexes(db, partition, manifest, &page)?;
        cursor.affected = checked_count(cursor.affected, counts.0)?;
        cursor.plugin_changed = checked_count(cursor.plugin_changed, counts.1)?;
        cursor.effect_previous = digest(&page)?;
        cursor.effect_ordinal += 1;
    } else if cursor.disposition_ordinal < manifest.disposition_pages {
        let page = read_disposition_page(db, partition, manifest, cursor.disposition_ordinal, &cursor.disposition_previous)?;
        let counts = validate_disposition_indexes(db, partition, manifest, &page)?;
        cursor.held = checked_count(cursor.held, counts.0)?;
        cursor.deferred = checked_count(cursor.deferred, counts.1)?;
        cursor.disposition_previous = digest(&page)?;
        cursor.disposition_ordinal += 1;
    }
    let complete = cursor.effect_ordinal == manifest.effect_pages && cursor.disposition_ordinal == manifest.disposition_pages;
    let mut next = manifest.clone();
    if complete {
        if cursor.effect_previous != manifest.effect_root || cursor.disposition_previous != manifest.disposition_root ||
            cursor.affected != manifest.affected_count || cursor.plugin_changed != manifest.plugin_changed_count ||
            cursor.held != manifest.held_count || cursor.deferred != manifest.deferred_count {
            return Err(invalid("native-receive-history-integrity"));
        }
        next.validation = None;
        next.validated = Some(HistoryValidationProof { purpose, root: manifest.root() });
    } else { next.validation = Some(cursor); }
    write_manifest(db, partition, &next)?;
    *manifest = next;
    Ok(complete)
}

pub(super) fn begin_cleanup(device: &Connection, library: &Connection, coordinator: &mut Coordinator,
    cap: &Capability, roots: &Roots) -> StoreResult<()> {
    require_transaction(device)?;
    require_transaction(library)?;
    coordinator.require(cap)?;
    roots.validate()?;
    if coordinator.active_producer.as_ref() == Some(cap) { return Err(invalid("native-receive-producer-active")); }
    if let Some(proof) = coordinator.cleanup.iter().find(|proof| proof.capability == *cap) {
        if proof.roots != *roots { return Err(invalid("native-receive-delivery-changed")); }
        return Ok(());
    }
    for (db, partition, expected) in [(library, Partition::Library, &roots.library), (device, Partition::Device, &roots.device)] {
        let manifest = read_manifest(db, partition, cap)?.ok_or_else(|| invalid("native-receive-manifest-missing"))?;
        if manifest.phase != Phase::Settled || manifest.root() != *expected || manifest.delivered.as_ref() != Some(roots) {
            return Err(invalid("native-receive-settlement-required"));
        }
        if !manifest.validated.as_ref().is_some_and(|proof| proof.purpose == ValidationPurpose::Cleanup && proof.root == *expected) {
            return Err(invalid("native-receive-validation-incomplete"));
        }
    }
    coordinator.cleanup.push(CleanupProof { capability: cap.clone(), roots: roots.clone(), library_done: false, device_done: false });
    write_coordinator(device, coordinator)
}

fn delete_history_rows(db: &Connection, partition: Partition, cap: &Capability, limit: usize) -> StoreResult<bool> {
    require_transaction(db)?;
    let prefix = cap.key()?;
    let sql = format!("SELECT key FROM {} WHERE key>=?1 AND key<?2 ORDER BY key LIMIT ?3", partition.table());
    let mut statement = db.prepare(&sql)?;
    let keys = statement.query_map(params![prefix, prefix_end(&prefix)?, i64::try_from(limit).map_err(|_| invalid("native-receive-step-too-large"))?],
        |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for key in keys { db.execute(&format!("DELETE FROM {} WHERE key=?1", partition.table()), [key])?; }
    let remaining: bool = db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {} WHERE key>=?1 AND key<?2)", partition.table()),
        params![prefix, prefix_end(&prefix)?], |row| row.get(0))?;
    Ok(!remaining)
}

#[cfg(test)]
fn cleanup_step(device: &mut Connection, library: &mut Connection, cap: &Capability, limit: usize) -> StoreResult<bool> {
    cleanup_step_inner(device, library, cap, limit, &|| Ok(()))
}
pub(super) fn cleanup_step_holding(admission: &lww::IntentAdmissionGuard, device: &Connection,
    library: &Connection, cap: &Capability, limit: usize) -> StoreResult<bool> {
    admission.require(device)?;
    cleanup_step_inner(device, library, cap, limit, &|| admission.require_identity())
}
fn cleanup_step_inner(device: &Connection, library: &Connection, cap: &Capability,
    limit: usize, check_admission: &dyn Fn() -> StoreResult<()>) -> StoreResult<bool> {
    if !device.is_autocommit() || !library.is_autocommit() || !(1..=MAX_STEP_ROWS).contains(&limit) {
        return Err(invalid("native-receive-cleanup-committed-proof-required"));
    }
    let device = kernel_guard(device)?;
    let library = kernel_guard(library)?;
    let mut coordinator = read_coordinator(&device)?.ok_or_else(|| invalid("native-receive-coordinator-missing"))?;
    coordinator.require(cap)?;
    let index = coordinator.cleanup.iter().position(|proof| proof.capability == *cap)
        .ok_or_else(|| invalid("native-receive-cleanup-proof-required"))?;
    check_admission()?;
    let library_done = if coordinator.cleanup[index].library_done { true }
        else { delete_history_rows(&library, Partition::Library, cap, limit)? };
    // The cleanup proof was committed before this call. Library deletion must become
    // durable before the device coordinator records that the partition is gone.
    library.commit()?;
    #[cfg(test)]
    if tests::interrupt_cleanup_after_library() { return Err(invalid("synthetic-cleanup-interruption")); }
    check_admission()?;
    coordinator.cleanup[index].library_done = library_done;
    let device_done = if library_done { delete_history_rows(&device, Partition::Device, cap, limit)? } else { false };
    coordinator.cleanup[index].device_done = device_done;
    let complete = library_done && device_done;
    if complete { coordinator.close(cap)?; }
    write_coordinator(&device, &coordinator)?;
    device.commit()?;
    Ok(complete)
}

fn prefix_end(prefix: &str) -> StoreResult<String> {
    let stem = prefix.strip_suffix(':').ok_or_else(|| invalid("native-receive-key-prefix"))?;
    Ok(format!("{stem};"))
}
fn namespace_exists(db: &Connection, partition: Partition) -> StoreResult<bool> {
    Ok(db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {} WHERE key>=?1 AND key<?2)", partition.table()),
        params![SETTING_PREFIX, prefix_end(SETTING_PREFIX)?], |row| row.get(0))?)
}
fn identity_digest(device: &Connection, library: &Connection) -> StoreResult<String> {
    let identity = sync_selection::identity(library)?;
    digest(&(identity.store_id, identity.library_epoch, identity.generation, identity.selection_epoch, lww::authority(device)?))
}
fn operation_namespace_exists(db: &Connection, partition: Partition, cap: &Capability) -> StoreResult<bool> {
    let prefix = cap.key()?;
    Ok(db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {} WHERE key>=?1 AND key<?2)", partition.table()),
        params![prefix, prefix_end(&prefix)?], |row| row.get(0))?)
}
fn admission_has_history(db: &Connection, partition: Partition, cap: &Capability) -> StoreResult<bool> {
    let prefix = cap.key()?;
    Ok(db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {} WHERE key>=?1 AND key<?2 AND key<>?3)", partition.table()),
        params![prefix, prefix_end(&prefix)?, format!("{prefix}manifest")], |row| row.get(0))?)
}
fn current_observation(device: &Connection, library: &Connection) -> StoreResult<Observation> {
    Ok(Observation { library_revision: sync_selection::identity(library)?.revision,
        device_revision: i64::try_from(receive_evidence::device_revision(device)?.0)
            .map_err(|_| invalid("native-receive-device-revision"))?, plugins: receive_evidence::device_fence(device)? })
}
fn observation_gap(previous: &Observation, current: &Observation) -> Foreign {
    Foreign { library: previous.library_revision != current.library_revision,
        device: previous.device_revision != current.device_revision, plugins: previous.plugins != current.plugins, scope: false }
}
fn baseline_gap(device: &Connection, library: &Connection, baseline: Option<&NativeReceiveEvidence>) -> StoreResult<Foreign> {
    let Some(baseline) = baseline else { return Ok(Foreign::unknown()); };
    let identity = sync_selection::identity(library)?;
    let authority = lww::authority(device)?;
    let scope = baseline.binding_authority != authority || baseline.identity.store_id != identity.store_id ||
        baseline.identity.library_epoch != identity.library_epoch || baseline.identity.generation != identity.generation ||
        baseline.identity.selection_epoch != identity.selection_epoch;
    Ok(Foreign { library: scope || baseline.identity.revision != identity.revision,
        device: scope || baseline.device_revision != Some(receive_evidence::device_revision(device)?),
        plugins: scope || baseline.device_plugins != receive_evidence::device_fence(device)?, scope })
}
fn source_frontier(db: &Connection) -> StoreResult<(i64, Option<String>)> {
    let rowid: Option<i64> = db.query_row("SELECT max(rowid) FROM lww_receive_rows", [], |row| row.get(0))?;
    let Some(rowid) = rowid else { return Ok((0, None)); };
    if rowid <= 0 { return Err(invalid("native-receive-source-integrity")); }
    let mut cells = Vec::with_capacity(4);
    for column in ["request_id", "key", "stamp", "value"] {
        cells.push(hash_cell(db, "lww_receive_rows", column, rowid)?);
    }
    Ok((rowid, Some(digest(&(rowid, cells))?)))
}
fn require_empty_admission(manifest: &Manifest) -> StoreResult<()> {
    if !matches!(manifest.phase, Phase::Admitting | Phase::Producing) || manifest.effect_pages != 0 ||
        manifest.disposition_pages != 0 || manifest.ticket.is_some() || manifest.outcome.is_some() ||
        manifest.cursor.phase != ScanPhase::Archives || manifest.cursor.after_key.is_some() || manifest.cursor.after_rowid != 0 {
        return Err(invalid("native-receive-admission-integrity"));
    }
    Ok(())
}
fn require_current_identity(device: &Connection, library: &Connection, cap: &Capability) -> StoreResult<()> {
    if identity_digest(device, library)? != cap.identity_digest {
        return Err(invalid("native-receive-identity-changed"));
    }
    Ok(())
}
fn validate_admitted_origin(device: &Connection, manifest: &Manifest) -> StoreResult<()> {
    let origin = &manifest.origin;
    if lww::authority(device)? != origin.authority { return Err(invalid("native-receive-identity-changed")); }
    if let Some(rowid) = origin.source_rowid {
        let present: bool = device.query_row("SELECT EXISTS(SELECT 1 FROM lww_receive WHERE rowid=?1)", [rowid], |row| row.get(0))?;
        if !present { return Err(invalid("native-receive-source-missing")); }
        let authority: DecimalU64 = bounded_cell(device, "lww_receive", "authority", rowid, 20)?.try_into().map_err(lww::error)?;
        let stored = bounded_cell(device, "lww_receive", "digest", rowid, 64)?;
        let body_bytes = cell_length(device, "lww_receive", "body", rowid)?;
        let (request, bytes) = hash_cell(device, "lww_receive", "request_id", rowid)?;
        let kind = bounded_cell(device, "lww_receive", "kind", rowid, 8)?;
        let cursor: DecimalU64 = bounded_cell(device, "lww_receive", "cursor", rowid, 20)?.try_into().map_err(lww::error)?;
        let (writer, writer_bytes) = hash_cell(device, "lww_receive", "writer_id", rowid)?;
        let Some(progress) = &origin.progress else { return Err(invalid("native-receive-source-integrity")); };
        if authority != origin.authority || stored != origin.body_digest || body_bytes != origin.body_bytes ||
            request != origin.request_digest || bytes != origin.request_bytes ||
            kind != (if progress.external { "external" } else { "server" }) || cursor != progress.cursor ||
            writer != progress.writer_digest || writer_bytes != progress.writer_bytes {
            return Err(invalid("native-receive-source-integrity"));
        }
    } else if manifest.capability.mode != Mode::Drain || origin.progress.is_some() || origin.request_bytes != 0 ||
        origin.body_bytes != 0 || origin.request_digest != risunest_sync_wire::hash(b"") ||
        origin.body_digest != digest(&("drain", origin.authority))? {
        return Err(invalid("native-receive-source-integrity"));
    }
    if digest(&(manifest.capability.mode, origin))? != manifest.capability.source_digest {
        return Err(invalid("native-receive-source-integrity"));
    }
    Ok(())
}

fn validate_producer_pair(coordinator: &Coordinator, device: &Manifest, library: Option<&Manifest>) -> StoreResult<()> {
    let producing = matches!(device.phase, Phase::Admitting | Phase::Producing | Phase::Finalizing);
    if (coordinator.active_producer.as_ref() == Some(&device.capability)) != producing {
        return Err(invalid("native-receive-producer-changed"));
    }
    // The library half of a superseded seal ends the pair; the device half follows it.
    if producing && library.is_some_and(|value| value.phase == Phase::Sealed && value.outcome == Some(Outcome::Superseded)) {
        return Ok(());
    }
    if (device.phase == Phase::Admitting && library.is_some_and(|value| !matches!(value.phase, Phase::Admitting | Phase::Producing))) ||
        (device.phase == Phase::Producing && library.is_some_and(|value| value.phase == Phase::Admitting)) {
        return Err(invalid("native-receive-partition-phase-changed"));
    }
    if matches!(device.phase, Phase::Admitting | Phase::Producing) {
        if device.origin.source_rowid.is_some() {
            let acknowledged = device.input.as_ref().ok_or_else(|| invalid("native-receive-input-missing"))?;
            if (device.phase == Phase::Producing) != acknowledged.completion.is_some() {
                return Err(invalid("native-receive-partition-phase-changed"));
            }
            if let Some(library) = library {
                let written = library.input.as_ref().ok_or_else(|| invalid("native-receive-input-missing"))?;
                if (library.phase != Phase::Admitting) != written.completion.is_some() {
                    return Err(invalid("native-receive-partition-phase-changed"));
                }
                if acknowledged != written {
                    let finished_proof = written.completion == Some(InputCompletion::FinishedProof) &&
                        written.cursor == acknowledged.cursor && written.last_batch == acknowledged.last_batch;
                    let one_ahead = written.last_batch.as_ref().is_some_and(|batch|
                        batch.start == acknowledged.cursor && batch.end == written.cursor);
                    if device.phase != Phase::Admitting || (!finished_proof && !one_ahead) {
                        return Err(invalid("native-receive-input-batch-changed"));
                    }
                }
            } else if acknowledged != &InputProgress::default() {
                return Err(invalid("native-receive-library-manifest-missing"));
            }
        } else if device.input.is_some() || library.is_some_and(|value| value.input.is_some()) {
            return Err(invalid("native-receive-input-integrity"));
        }
    }

    Ok(())
}
fn operation_state(device: &Connection, library: &Connection, coordinator: &Coordinator, cap: &Capability) -> StoreResult<OperationState> {
    coordinator.require(cap)?;
    let device_manifest = read_manifest(device, Partition::Device, cap)?
        .ok_or_else(|| invalid("native-receive-device-manifest-missing"))?;
    let library_manifest = read_manifest(library, Partition::Library, cap)?;
    validate_producer_pair(coordinator, &device_manifest, library_manifest.as_ref())?;
    if library_manifest.is_none() && (device_manifest.phase != Phase::Admitting ||
        operation_namespace_exists(library, Partition::Library, cap)?) {
        return Err(invalid("native-receive-library-manifest-missing"));
    }
    if let Some(manifest) = &library_manifest {
        if digest(&manifest.origin)? != digest(&device_manifest.origin)? {
            return Err(invalid("native-receive-partition-origin-changed"));
        }
    } else { require_empty_admission(&device_manifest)?; }
    let current = current_observation(device, library)?;
    let mut foreign = device_manifest.foreign.clone();
    foreign.device |= current.device_revision != device_manifest.observation.device_revision;
    foreign.plugins |= current.plugins != device_manifest.observation.plugins;
    foreign.scope |= identity_digest(device, library)? != cap.identity_digest;
    let revision = if let Some(manifest) = &library_manifest {
        foreign.merge(&manifest.foreign);
        foreign.library |= current.library_revision != manifest.observation.library_revision;
        manifest.observation.library_revision
    } else { device_manifest.observation.library_revision };
    let terminal = matches!(device_manifest.phase, Phase::Sealed | Phase::Delivered | Phase::Settled | Phase::Cleanup);
    let roots = if terminal {
        let manifest = library_manifest.as_ref().ok_or_else(|| invalid("native-receive-library-manifest-missing"))?;
        if manifest.outcome != device_manifest.outcome || !matches!(manifest.phase, Phase::Sealed | Phase::Delivered | Phase::Settled | Phase::Cleanup) {
            return Err(invalid("native-receive-partition-phase-changed"));
        }
        Some(Roots { library: manifest.root(), device: device_manifest.root() })
    } else { None };
    Ok(OperationState { capability: cap.clone(), progress_token: digest(&(&device_manifest, &library_manifest))?,
        phase: device_manifest.phase, revision, outcome: device_manifest.outcome, foreign, roots,
        device_plugins: Some(current.plugins), device_revision: Some(DecimalU64(u64::try_from(current.device_revision)
            .map_err(|_| invalid("native-receive-device-revision"))?)) })
}

pub(super) fn require_unowned_intent_admission(device: &Connection, library: &Connection) -> StoreResult<()> {
    for db in [device, library] {
        if db.transaction_state(Some(rusqlite::MAIN_DB))? != rusqlite::TransactionState::Write {
            return Err(invalid("native-receive-write-guards-required"));
        }
    }
    unowned_intent_custody(device, library)
}
/// Recovery that prunes and replays nothing validates the same custody from read transactions.
pub(super) fn observe_unowned_intent_admission(device: &Connection, library: &Connection) -> StoreResult<()> {
    if device.is_autocommit() || library.is_autocommit() { return Err(invalid("native-receive-read-guards-required")); }
    unowned_intent_custody(device, library)
}
fn unowned_intent_custody(device: &Connection, library: &Connection) -> StoreResult<()> {
    let Some(coordinator) = read_coordinator(device)? else {
        if namespace_exists(library, Partition::Library)? { return Err(invalid("native-receive-coordinator-missing")); }
        return Ok(());
    };
    for cap in [coordinator.server_history.as_ref(), coordinator.local_history.as_ref()].into_iter().flatten() {
        if let Some(proof) = coordinator.cleanup.iter().find(|proof| &proof.capability == cap) {
            for (db, partition, root, done) in [(library, Partition::Library, &proof.roots.library, proof.library_done),
                (device, Partition::Device, &proof.roots.device, proof.device_done)] {
                if let Some(manifest) = read_manifest(db, partition, cap)? {
                    if done || manifest.phase != Phase::Settled || manifest.root() != *root || manifest.delivered.as_ref() != Some(&proof.roots) {
                        return Err(invalid("native-receive-cleanup-integrity"));
                    }
                } else if done && operation_namespace_exists(db, partition, cap)? {
                    return Err(invalid("native-receive-cleanup-integrity"));
                }
            }
        } else {
            operation_state(device, library, &coordinator, cap)?;
        }
    }
    if coordinator.active_producer.is_some() { return Err(StoreError::CommitBusy); }
    Ok(())
}

const INPUT_BUFFER_BYTES: usize = 64 * 1024;
const INPUT_STEP_BYTES: u64 = 512 * 1024;
// Limits end between complete units. One existing unit, its object work and
// source metadata/request identity remain intrinsic input costs, not page buffers.

fn input_limits() -> (usize, u64) {
    #[cfg(test)]
    { tests::input_limits() }
    #[cfg(not(test))]
    { (MAX_STEP_ROWS, INPUT_STEP_BYTES) }
}

struct SourceReader<'a> {
    db: &'a Connection,
    table: &'static str,
    rowid: i64,
    length: u64,
    position: u64,
    buffer_start: u64,
    buffer: Vec<u8>,
}
impl<'a> SourceReader<'a> {
    fn new(db: &'a Connection, rowid: i64, length: u64, position: u64) -> StoreResult<Self> {
        if position > length { return Err(invalid("native-receive-input-integrity")); }
        Ok(Self { db, table: "lww_receive", rowid, length, position, buffer_start: 0, buffer: Vec::new() })
    }
    fn seek(&mut self, position: u64) -> StoreResult<()> {
        if position > self.length { return Err(invalid("native-receive-input-integrity")); }
        self.position = position;
        Ok(())
    }
    fn byte(&mut self) -> StoreResult<Option<u8>> {
        use std::io::Read;
        let mut byte = [0];
        Ok(if self.read(&mut byte)? == 0 { None } else { Some(byte[0]) })
    }
    fn nonspace(&mut self) -> StoreResult<Option<u8>> {
        loop {
            match self.byte()? {
                Some(b' ' | b'\t' | b'\r' | b'\n') => {}
                byte => return Ok(byte),
            }
        }
    }
    fn expect(&mut self, expected: u8) -> StoreResult<()> {
        if self.nonspace()? != Some(expected) { return Err(invalid("native-receive-input-json")); }
        Ok(())
    }
    fn value<T: DeserializeOwned>(&mut self) -> StoreResult<T> {
        let start = self.position;
        let (value, consumed) = {
            let mut stream = serde_json::Deserializer::from_reader(&mut *self).into_iter::<T>();
            let value = stream.next().ok_or_else(|| invalid("native-receive-input-json"))??;
            (value, stream.byte_offset())
        };
        // Serde may peek beyond the value. Its consumed offset, not our read-ahead
        // position, is the durable boundary used by the next parser invocation.
        self.seek(start.checked_add(consumed as u64).ok_or_else(|| invalid("native-receive-input-integrity"))?)?;
        Ok(value)
    }
    fn key(&mut self) -> StoreResult<String> {
        let start = self.position;
        let key: String = self.value()?;
        if self.position - start > 128 { return Err(invalid("native-receive-input-json")); }
        Ok(key)
    }
    fn changes_start(&mut self) -> StoreResult<()> {
        self.expect(b'{')?;
        loop {
            let key = self.key()?;
            self.expect(b':')?;
            if key == "changes" {
                self.expect(b'[')?;
                return Ok(());
            }
            self.value::<serde::de::IgnoredAny>()?;
            self.expect(b',')?;
        }
    }
    fn tail(&mut self) -> StoreResult<()> {
        loop {
            match self.nonspace()? {
                Some(b'}') => break,
                Some(b',') => {
                    if self.key()? == "changes" { return Err(invalid("native-receive-input-json")); }
                    self.expect(b':')?;
                    self.value::<serde::de::IgnoredAny>()?;
                }
                _ => return Err(invalid("native-receive-input-json")),
            }
        }
        if self.nonspace()?.is_some() || self.position != self.length {
            return Err(invalid("native-receive-input-json"));
        }
        Ok(())
    }
}
impl std::io::Read for SourceReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() || self.position == self.length { return Ok(0); }
        if self.position < self.buffer_start || self.position >= self.buffer_start + self.buffer.len() as u64 {
            let start = self.position.checked_add(1).and_then(|value| i64::try_from(value).ok())
                .ok_or_else(|| std::io::Error::other("native-receive-input-integrity"))?;
            let count = (self.length - self.position).min(INPUT_BUFFER_BYTES as u64);
            self.buffer = self.db.query_row(&format!("SELECT substr(CAST(body AS BLOB),?2,?3) FROM {} WHERE rowid=?1", self.table),
                params![self.rowid, start, count as i64], |row| row.get(0))
                .map_err(std::io::Error::other)?;
            if self.buffer.len() as u64 != count {
                return Err(std::io::Error::other("native-receive-input-integrity"));
            }
            self.buffer_start = self.position;
        }
        let at = (self.position - self.buffer_start) as usize;
        let count = output.len().min(self.buffer.len() - at);
        output[..count].copy_from_slice(&self.buffer[at..at + count]);
        self.position += count as u64;
        Ok(count)
    }
}

#[derive(Clone, Copy)]
struct SourceStatus { applied: bool, finished: bool }
fn source_status(device: &Connection, origin: &Origin) -> StoreResult<SourceStatus> {
    let Some(rowid) = origin.source_rowid else { return Ok(SourceStatus { applied: false, finished: false }); };
    let (applied, finished): (bool, bool) = device.query_row("SELECT applied,finished FROM lww_receive WHERE rowid=?1",
        [rowid], |row| Ok((row.get(0)?, row.get(1)?)))?;
    if finished && !applied { return Err(invalid("native-receive-source-integrity")); }
    Ok(SourceStatus { applied, finished })
}
/// A source row is retained only while every witnessed cell still names the admitted origin.
fn retained_source(device: &Connection, origin: &Origin) -> StoreResult<Option<SourceStatus>> {
    let (Some(rowid), Some(progress)) = (origin.source_rowid, origin.progress.as_ref()) else { return Ok(None) };
    let present: bool = device.query_row("SELECT EXISTS(SELECT 1 FROM lww_receive WHERE rowid=?1)", [rowid], |row| row.get(0))?;
    if !present { return Ok(None); }
    let decimal = |column| bounded_cell(device, "lww_receive", column, rowid, 20).ok().and_then(|value| DecimalU64::try_from(value).ok());
    let same = decimal("authority") == Some(origin.authority) && decimal("cursor") == Some(progress.cursor) &&
        bounded_cell(device, "lww_receive", "digest", rowid, 64).ok().as_ref() == Some(&origin.body_digest) &&
        bounded_cell(device, "lww_receive", "kind", rowid, 8).ok().as_deref() == Some(if progress.external { "external" } else { "server" }) &&
        cell_length(device, "lww_receive", "body", rowid).ok() == Some(origin.body_bytes) &&
        hash_cell(device, "lww_receive", "request_id", rowid)? == (origin.request_digest.clone(), origin.request_bytes) &&
        hash_cell(device, "lww_receive", "writer_id", rowid)? == (progress.writer_digest.clone(), progress.writer_bytes);
    if !same { return Ok(None); }
    source_status(device, origin).map(Some)
}
fn no_pending_intents(device: &Connection) -> StoreResult<()> {
    let pending: bool = device.query_row("SELECT EXISTS(SELECT 1 FROM lww_intents WHERE complete=0)", [], |row| row.get(0))?;
    if pending { return Err(StoreError::CommitBusy); }
    Ok(())
}
fn source_span_digest(device: &Connection, origin: &Origin, start: u64, end: u64) -> StoreResult<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    if start > end || end > origin.body_bytes { return Err(invalid("native-receive-input-integrity")); }
    let mut reader = SourceReader::new(device, origin.source_rowid.ok_or_else(|| invalid("native-receive-input-integrity"))?,
        origin.body_bytes, start)?;
    let mut remaining = end - start;
    let mut hash = Sha256::new();
    let mut buffer = [0; INPUT_BUFFER_BYTES];
    while remaining > 0 {
        let count = remaining.min(buffer.len() as u64) as usize;
        reader.read_exact(&mut buffer[..count])?;
        hash.update(&buffer[..count]);
        remaining -= count as u64;
    }
    Ok(hex::encode(hash.finalize()))
}
fn stage_input_row(db: &Connection, request_id: &str, change: &lww::Change, must_exist: bool,
    finished: bool) -> StoreResult<()> {
    let stamp = serde_json::to_string(&change.stamp)?;
    let value = serde_json::to_string(&change.value)?;
    let previous: Option<(String, String)> = db.query_row(
        "SELECT stamp,value FROM lww_receive_rows WHERE request_id=?1 AND key=?2",
        params![request_id, change.key.as_str()], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
    if let Some((old_stamp, old_value)) = previous {
        if old_stamp != stamp || old_value != value { return Err(invalid("native-receive-input-row-changed")); }
    } else if must_exist && !finished {
        return Err(invalid("native-receive-input-row-missing"));
    } else if !finished {
        db.execute("INSERT INTO lww_receive_rows VALUES(?1,?2,?3,?4,'staged')",
            params![request_id, change.key.as_str(), stamp, value])?;
    }
    Ok(())
}
fn stage_input_batch(device: &Connection, library: &Connection, cap: &Capability, origin: &Origin,
    start: InputCursor, ahead: Option<&InputBatch>, finished: bool) -> StoreResult<InputBatch> {
    use sha2::{Digest, Sha256};
    let rowid = origin.source_rowid.ok_or_else(|| invalid("native-receive-input-integrity"))?;
    let request_id: String = device.query_row("SELECT request_id FROM lww_receive WHERE rowid=?1", [rowid], |row| row.get(0))?;
    if request_id.len() as u64 != origin.request_bytes || risunest_sync_wire::hash(request_id.as_bytes()) != origin.request_digest {
        return Err(invalid("native-receive-source-integrity"));
    }
    let mut reader = SourceReader::new(device, rowid, origin.body_bytes, start.offset)?;
    if start == InputCursor::default() { reader.changes_start()?; }
    let mut ordinal = start.ordinal;
    let (row_limit, byte_limit) = input_limits();
    let mut content = Sha256::new();
    let ended = loop {
        let value_start = reader.position;
        match reader.nonspace()? {
            Some(b']') if ordinal == 0 => { reader.tail()?; break true; }
            Some(_) => reader.seek(value_start)?,
            None => return Err(invalid("native-receive-input-json")),
        }
        let change: lww::Change = reader.value()?;
        let device_value = lww::is_device_unit(&change.key);
        let encoded = serde_json::to_vec(&change)?;
        content.update([u8::from(device_value)]);
        content.update((encoded.len() as u64).to_be_bytes());
        content.update(&encoded);
        if device_value {
            if !finished { lww::copy_device_unit_body(library, device, &change)?; }
            stage_input_row(device, &request_id, &change, false, finished)?;
        } else {
            stage_input_row(library, &request_id, &change, ahead.is_some(), finished)?;
        }
        ordinal = ordinal.checked_add(1).ok_or_else(|| invalid("native-receive-input-integrity"))?;
        match reader.nonspace()? {
            Some(b']') => { reader.tail()?; break true; }
            Some(b',') => {}
            _ => return Err(invalid("native-receive-input-json")),
        }
        if let Some(ahead) = ahead {
            if ordinal == ahead.end.ordinal { break false; }
            if ordinal > ahead.end.ordinal || reader.position >= ahead.end.offset {
                return Err(invalid("native-receive-input-integrity"));
            }
        } else if ordinal - start.ordinal >= row_limit as u64 || reader.position - start.offset >= byte_limit {
            break false;
        }
    };
    let end = InputCursor { offset: reader.position, ordinal };
    drop(reader);
    let batch = InputBatch { source_binding: digest(&(cap, origin))?, start, end,
        span_digest: source_span_digest(device, origin, start.offset, end.offset)?,
        content_digest: hex::encode(content.finalize()), ended };
    if ahead.is_some_and(|expected| expected != &batch) { return Err(invalid("native-receive-input-batch-changed")); }
    Ok(batch)
}

fn bounded_cell(db: &Connection, table: &'static str, column: &'static str, rowid: i64, limit: usize) -> StoreResult<String> {
    let value: Option<String> = db.query_row(&format!(
        "SELECT CASE WHEN typeof({column})='text' AND length(CAST({column} AS BLOB))<=?2 THEN {column} ELSE NULL END FROM {table} WHERE rowid=?1"),
        params![rowid, i64::try_from(limit).map_err(|_| invalid("native-receive-record-too-large"))?], |row| row.get(0))?;
    value.ok_or_else(|| invalid("native-receive-source-integrity"))
}
fn cell_length(db: &Connection, table: &'static str, column: &'static str, rowid: i64) -> StoreResult<u64> {
    let length: Option<i64> = db.query_row(&format!(
        "SELECT CASE WHEN typeof({column})='text' THEN length(CAST({column} AS BLOB)) ELSE NULL END FROM {table} WHERE rowid=?1"),
        [rowid], |row| row.get(0))?;
    length.and_then(|length| u64::try_from(length).ok()).ok_or_else(|| invalid("native-receive-source-integrity"))
}

fn hash_cell(db: &Connection, table: &'static str, column: &'static str, rowid: i64) -> StoreResult<(String, u64)> {
    use sha2::{Digest, Sha256};
    let length: i64 = db.query_row(&format!("SELECT length(CAST({column} AS BLOB)) FROM {table} WHERE rowid=?1"), [rowid], |row| row.get(0))?;
    let length = u64::try_from(length).map_err(|_| invalid("native-receive-source-integrity"))?;
    let mut hash = Sha256::new();
    let mut offset = 0u64;
    let mut query = db.prepare(&format!("SELECT substr(CAST({column} AS BLOB),?2,?3) FROM {table} WHERE rowid=?1"))?;
    while offset < length {
        let count = (length - offset).min(64 * 1024);
        let start = offset.checked_add(1).and_then(|value| i64::try_from(value).ok())
            .ok_or_else(|| invalid("native-receive-source-integrity"))?;
        let bytes: Vec<u8> = query.query_row(params![rowid, start, count as i64], |row| row.get(0))?;
        if bytes.len() as u64 != count { return Err(invalid("native-receive-source-integrity")); }
        hash.update(&bytes);
        offset += count;
    }
    Ok((hex::encode(hash.finalize()), length))
}
fn source_origin(device: &Connection, source: &Source, verify_body: bool) -> StoreResult<Origin> {
    if lww::authority(device)? != source.authority() { return Err(lww::error("binding-authority-changed")); }
    let header = match source {
        Source::Stored { header } | Source::Staged { header } => header,
        Source::Drain { binding_authority } => return Ok(Origin {
            request_digest: risunest_sync_wire::hash(b""), request_bytes: 0, source_rowid: None,
            body_digest: digest(&("drain", binding_authority))?, body_bytes: 0, authority: *binding_authority, progress: None,
        }),
    };
    if header.request_id.is_empty() { return Err(lww::error("request-id-required")); }
    let rowid: i64 = device.query_row("SELECT rowid FROM lww_receive WHERE request_id=?1 AND authority=?2",
        params![header.request_id, header.binding_authority.0.to_string()], |row| row.get(0)).optional()?
        .ok_or_else(|| invalid("native-receive-source-missing"))?;
    let stored_digest = bounded_cell(device, "lww_receive", "digest", rowid, 64)?;
    let body_bytes = cell_length(device, "lww_receive", "body", rowid)?;
    if !hash_valid(&stored_digest) || (verify_body && hash_cell(device, "lww_receive", "body", rowid)?.0 != stored_digest) {
        return Err(invalid("native-receive-source-integrity"));
    }
    let body_digest = stored_digest;
    let kind = bounded_cell(device, "lww_receive", "kind", rowid, 8)?;
    if !matches!(kind.as_str(), "server" | "external") { return Err(invalid("native-receive-source-integrity")); }
    let cursor: DecimalU64 = bounded_cell(device, "lww_receive", "cursor", rowid, 20)?.try_into().map_err(lww::error)?;
    let (writer_digest, writer_bytes) = hash_cell(device, "lww_receive", "writer_id", rowid)?;
    let (request_digest, request_bytes) = hash_cell(device, "lww_receive", "request_id", rowid)?;
    if request_digest != risunest_sync_wire::hash(header.request_id.as_bytes()) || request_bytes != header.request_id.len() as u64 {
        return Err(invalid("native-receive-source-integrity"));
    }
    Ok(Origin { request_digest, request_bytes, source_rowid: Some(rowid), body_digest, body_bytes, authority: header.binding_authority,
        progress: Some(ProgressWitness { external: kind == "external", cursor, writer_digest, writer_bytes }) })
}

impl PersistentStore {
    pub(super) fn initialize_receive_history(&self) -> StoreResult<()> {
        {
            let device = Transaction::new_unchecked(self.device_store()?.connection(), TransactionBehavior::Deferred)?;
            if read_coordinator(&device)?.is_some() { return Ok(()); }
        }
        let admission = self.acquire_intent_admission()?;
        admission.require(self.device_store()?.connection())?;
        let device = Transaction::new_unchecked(self.device_store()?.connection(), TransactionBehavior::Immediate)?;
        let library = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        if read_coordinator(&device)?.is_none() {
            if namespace_exists(&library, Partition::Library)? { return Err(invalid("native-receive-coordinator-missing")); }
            write_coordinator(&device, &Coordinator::new())?;
        }
        library.commit()?;
        device.commit()?;
        Ok(())
    }
    pub(super) fn read_native_receive_state(&self, cap: &Capability) -> StoreResult<OperationState> {
        let device = Transaction::new_unchecked(self.device_store()?.connection(), TransactionBehavior::Immediate)?;
        let library = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let coordinator = read_coordinator(&device)?.ok_or_else(|| invalid("native-receive-coordinator-missing"))?;
        operation_state(&device, &library, &coordinator, cap)
    }
    #[cfg(test)]
    fn resume_native_receive_admission(&self, cap: &Capability) -> StoreResult<OperationState> {
        let admission = self.acquire_intent_admission()?;
        self.resume_native_receive_admission_holding(&admission, cap, None)
    }
    fn resume_native_receive_admission_holding(&self, admission: &lww::IntentAdmissionGuard,
        cap: &Capability, log: Option<&Connection>) -> StoreResult<OperationState> {
        admission.require(self.device_store()?.connection())?;
        {
            let device = Transaction::new_unchecked(self.device_store()?.connection(), TransactionBehavior::Immediate)?;
            let library = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
            let log = reserve_log(log, cap.mode)?;
            let coordinator = read_coordinator(&device)?.ok_or_else(|| invalid("native-receive-coordinator-missing"))?;
            coordinator.require(cap)?;
            let mut device_manifest = read_manifest(&device, Partition::Device, cap)?
                .ok_or_else(|| invalid("native-receive-device-manifest-missing"))?;
            if device_manifest.phase != Phase::Admitting { return operation_state(&device, &library, &coordinator, cap); }
            if coordinator.active_producer.as_ref() != Some(cap) { return Err(invalid("native-receive-producer-changed")); }
            require_empty_admission(&device_manifest)?;
            no_pending_intents(&device)?;
            if admission_has_history(&device, Partition::Device, cap)? || admission_has_history(&library, Partition::Library, cap)? {
                return Err(invalid("native-receive-admission-integrity"));
            }
            require_current_identity(&device, &library, cap)?;
            validate_admitted_origin(&device, &device_manifest)?;
            require_stored_page(log.as_ref(), &device_manifest.origin)?;
            let observation = current_observation(&device, &library)?;
            let mut library_manifest = match read_manifest(&library, Partition::Library, cap)? {
                Some(manifest) => {
                    validate_producer_pair(&coordinator, &device_manifest, Some(&manifest))?;
                    require_empty_admission(&manifest)?;
                    if digest(&manifest.origin)? != digest(&device_manifest.origin)? {
                        return Err(invalid("native-receive-partition-origin-changed"));
                    }
                    manifest
                }
                None => {
                    if operation_namespace_exists(&library, Partition::Library, cap)? ||
                        device_manifest.input.as_ref().is_some_and(|input| input != &InputProgress::default()) {
                        return Err(invalid("native-receive-library-manifest-missing"));
                    }
                    empty_manifest(cap.clone(), device_manifest.origin.clone(), observation.clone(), 0, None,
                        device_manifest.foreign.clone())
                }
            };
            let mut foreign = observation_gap(&device_manifest.observation, &observation);
            foreign.merge(&observation_gap(&library_manifest.observation, &observation));
            let complete = if device_manifest.origin.source_rowid.is_some() {
                let acknowledged = device_manifest.input.as_ref().ok_or_else(|| invalid("native-receive-input-missing"))?;
                let written = library_manifest.input.as_ref().ok_or_else(|| invalid("native-receive-input-missing"))?;
                let status = source_status(&device, &device_manifest.origin)?;
                let finished = status.applied && status.finished;
                let mut next = if acknowledged == written {
                    if acknowledged.completion.is_some() { acknowledged.clone() }
                    else if finished {
                        let mut next = acknowledged.clone();
                        next.completion = Some(InputCompletion::FinishedProof);
                        next
                    } else {
                        let batch = stage_input_batch(&device, &library, cap, &device_manifest.origin, acknowledged.cursor, None, false)?;
                        InputProgress { cursor: batch.end,
                            completion: batch.ended.then_some(InputCompletion::JsonEnd), last_batch: Some(batch) }
                    }
                } else if finished && written.completion == Some(InputCompletion::FinishedProof) &&
                    written.cursor == acknowledged.cursor && written.last_batch == acknowledged.last_batch {
                    written.clone()
                } else {
                    let ahead = written.last_batch.as_ref().ok_or_else(|| invalid("native-receive-input-batch-changed"))?;
                    if acknowledged.completion.is_some() || ahead.start != acknowledged.cursor || ahead.end != written.cursor {
                        return Err(invalid("native-receive-input-batch-changed"));
                    }
                    stage_input_batch(&device, &library, cap, &device_manifest.origin, acknowledged.cursor, Some(ahead), finished)?;
                    written.clone()
                };
                if finished {
                    next.completion = Some(InputCompletion::FinishedProof);
                    foreign.merge(&Foreign { library: true, device: true, plugins: true, scope: false });
                } else if next.completion == Some(InputCompletion::FinishedProof) {
                    return Err(invalid("native-receive-source-integrity"));
                }
                let complete = next.completion.is_some();
                library_manifest.input = Some(next.clone());
                device_manifest.input = Some(next);
                complete
            } else {
                if device_manifest.input.is_some() || library_manifest.input.is_some() {
                    return Err(invalid("native-receive-input-integrity"));
                }
                true
            };
            // No content has been projected. Preserve foreign observations before
            // recording the starting observation of the owned row executor.
            library_manifest.foreign.merge(&foreign);
            device_manifest.foreign.merge(&foreign);
            library_manifest.observation = observation.clone();
            device_manifest.observation = observation;
            if complete {
                let (frontier, sentinel) = source_frontier(&library)?;
                library_manifest.cursor.frontier = frontier;
                library_manifest.cursor.sentinel_digest = sentinel;
                let (frontier, sentinel) = source_frontier(&device)?;
                device_manifest.cursor.frontier = frontier;
                device_manifest.cursor.sentinel_digest = sentinel;
                library_manifest.phase = Phase::Producing;
                device_manifest.phase = Phase::Producing;
            }
            write_manifest(&library, Partition::Library, &library_manifest)?;
            admission.require_identity()?;
            library.commit()?;
            #[cfg(test)]
            if tests::interrupt_admission(2) { return Err(invalid("synthetic-admission-interruption")); }
            admission.require_identity()?;
            write_manifest(&device, Partition::Device, &device_manifest)?;
            device.commit()?;
            #[cfg(test)]
            if tests::interrupt_admission(3) { return Err(invalid("synthetic-admission-interruption")); }
        }
        self.read_native_receive_state(cap)
    }

    /// Without the transport log guard a Stored operation is only observed.
    fn receive_recovery_state(&self, admission: &lww::IntentAdmissionGuard, cap: &Capability,
        log: Option<&Connection>) -> StoreResult<OperationState> {
        if cap.mode == Mode::Stored && log.is_none() { self.read_native_receive_state(cap) }
        else { self.resume_native_receive_admission_holding(admission, cap, log) }
    }

    pub(super) fn begin_local_native_receive(&mut self, request: &BeginRequest) -> StoreResult<BeginResult> {
        if request.source.mode() == Mode::Stored { return Err(invalid("native-receive-source-mode")); }
        self.begin_native_receive(request, None)
    }
    /// `log` is the transport operation log, whose exact originating page must stay in place.
    pub(crate) fn begin_stored_native_receive(&mut self, log: &Connection, request: &BeginRequest) -> StoreResult<BeginResult> {
        if request.source.mode() != Mode::Stored { return Err(invalid("native-receive-source-mode")); }
        self.begin_native_receive(request, Some(log))
    }
    fn begin_native_receive(&mut self, request: &BeginRequest, log: Option<&Connection>) -> StoreResult<BeginResult> {
        if request.candidate.mode != request.source.mode() { return Err(invalid("native-receive-source-mode")); }
        let admission = self.acquire_intent_admission()?;
        // Exact retry is resolved before recovery or source staging can create fresh effects.
        let previous = {
            let device = Transaction::new_unchecked(self.device_store()?.connection(), TransactionBehavior::Deferred)?;
            let mut coordinator = read_coordinator(&device)?.ok_or_else(|| invalid("native-receive-coordinator-missing"))?;
            coordinator.admit(&request.candidate)?
        };
        match previous {
            Admission::Existing(cap) => return Ok(BeginResult::Existing { state: self.resume_native_receive_admission_holding(&admission, &cap, log)? }),
            Admission::Recovery(cap) => return Ok(BeginResult::Recovery { state: self.receive_recovery_state(&admission, &cap, log)? }),
            Admission::Admitted(_) => {}
        }
        {
            let device = Transaction::new_unchecked(self.device_store()?.connection(), TransactionBehavior::Immediate)?;
            let library = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
            require_current_identity(&device, &library, &request.candidate)?;
            no_pending_intents(&device)?;
            let origin = source_origin(&device, &request.source, false)?;
            if request.candidate.source_digest != digest(&(request.candidate.mode, origin))? {
                return Err(invalid("native-receive-candidate-source-changed"));
            }
        }
        admission.require(self.device_store()?.connection())?;
        let admitted = {
            let device = Transaction::new_unchecked(self.device_store()?.connection(), TransactionBehavior::Immediate)?;
            let library = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
            let log_guard = reserve_log(log, request.candidate.mode)?;
            let mut coordinator = read_coordinator(&device)?.ok_or_else(|| invalid("native-receive-coordinator-missing"))?;
            let admission = coordinator.admit(&request.candidate)?;
            if let Admission::Admitted(cap) = &admission {
                no_pending_intents(&device)?;
                require_current_identity(&device, &library, cap)?;
                let origin = source_origin(&device, &request.source, false)?;
                if cap.source_digest != digest(&(cap.mode, &origin))? { return Err(invalid("native-receive-candidate-source-changed")); }
                require_stored_page(log_guard.as_ref(), &origin)?;
                if operation_namespace_exists(&device, Partition::Device, cap)? || operation_namespace_exists(&library, Partition::Library, cap)? {
                    return Err(invalid("native-receive-admission-integrity"));
                }
                let observation = current_observation(&device, &library)?;
                let foreign = baseline_gap(&device, &library, request.native_baseline.as_ref())?;
                let manifest = empty_manifest(cap.clone(), origin, observation, 0, None, foreign);
                write_manifest(&device, Partition::Device, &manifest)?;
                write_coordinator(&device, &coordinator)?;
                library.commit()?;
                device.commit()?;
                #[cfg(test)]
                if tests::interrupt_admission(1) { return Err(invalid("synthetic-admission-interruption")); }
            }
            admission
        };
        match admitted {
            Admission::Admitted(cap) => Ok(BeginResult::Admitted { state: self.resume_native_receive_admission_holding(&admission, &cap, log)? }),
            Admission::Existing(cap) => Ok(BeginResult::Existing { state: self.resume_native_receive_admission_holding(&admission, &cap, log)? }),
            Admission::Recovery(cap) => Ok(BeginResult::Recovery { state: self.receive_recovery_state(&admission, &cap, log)? }),
        }
    }
    pub(crate) fn prepare_native_receive(&self, source: &Source) -> StoreResult<Capability> {
        let device = Transaction::new_unchecked(self.device_store()?.connection(), TransactionBehavior::Deferred)?;
        let library = Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)?;
        let coordinator = read_coordinator(&device)?.ok_or_else(|| invalid("native-receive-coordinator-missing"))?;
        let origin = source_origin(&device, source, true)?;
        coordinator.candidate(source.mode(), digest(&(source.mode(), origin))?, identity_digest(&device, &library)?)
    }
}

// Only row witnesses and bounded keys survive the clock-observation transaction.
struct PreparedRow {
    witness: RowWitness,
    key: UnitKey,
    stamp: risunest_sync_wire::stamp::Stamp,
}
struct PreparedRows { rows: Vec<PreparedRow>, exhausted: bool }
fn row_witness(db: &Connection, rowid: i64) -> StoreResult<Option<RowWitness>> {
    let status: Option<String> = db.query_row("SELECT status FROM lww_receive_rows WHERE rowid=?1", [rowid], |row| row.get(0)).optional()?;
    let Some(status) = status else { return Ok(None) };
    if !matches!(status.as_str(), "staged" | "held" | "deferred" | "done") { return Err(invalid("native-receive-row-status")); }
    let request = hash_cell(db, "lww_receive_rows", "request_id", rowid)?;
    let mut cells = vec![request.clone()];
    for column in ["key", "stamp", "value"] { cells.push(hash_cell(db, "lww_receive_rows", column, rowid)?); }
    Ok(Some(RowWitness { rowid, digest: digest(&(rowid, cells, status))?, request_digest: request.0, request_bytes: request.1 }))
}
fn frontier_matches(db: &Connection, cursor: &Cursor) -> StoreResult<bool> {
    if cursor.frontier == 0 { return Ok(cursor.sentinel_digest.is_none()); }
    let present: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM lww_receive_rows WHERE rowid=?1)", [cursor.frontier], |row| row.get(0))?;
    if !present { return Ok(false); }
    let mut cells = Vec::with_capacity(4);
    for column in ["request_id", "key", "stamp", "value"] { cells.push(hash_cell(db, "lww_receive_rows", column, cursor.frontier)?); }
    Ok(cursor.sentinel_digest.as_ref() == Some(&digest(&(cursor.frontier, cells))?))
}
fn row_limits() -> (usize, usize) {
    #[cfg(test)]
    { kernel_test::limits() }
    #[cfg(not(test))]
    { (MAX_STEP_ROWS, MAX_DECODED_BYTES / 2) }
}
fn prepare_rows(db: &Connection, manifest: &Manifest) -> StoreResult<PreparedRows> {
    let phase = match manifest.cursor.phase {
        ScanPhase::Archives | ScanPhase::ArchivesAfterParents => "json_extract(key,'$[0]')='archive'",
        ScanPhase::Exists => "json_extract(key,'$[0]')='exists'",
        ScanPhase::Remaining => "1",
        ScanPhase::Dispositions | ScanPhase::Complete => return Ok(PreparedRows { rows: Vec::new(), exhausted: false }),
    };
    let (limit, byte_limit) = row_limits();
    let mut statement = db.prepare(&format!("SELECT rowid FROM lww_receive_rows WHERE rowid<=?1 AND status IN ('staged','held','deferred') AND {phase} AND (?2 IS NULL OR key>?2 OR (key=?2 AND rowid>?3)) ORDER BY key,rowid LIMIT ?4"))?;
    let mut cursor = statement.query(params![manifest.cursor.frontier, manifest.cursor.after_key, manifest.cursor.after_rowid,
        i64::try_from(limit + 1).map_err(|_| invalid("native-receive-step-too-large"))?])?;
    let (mut rows, mut bytes) = (Vec::new(), 0usize);
    while let Some(row) = cursor.next()? {
        if rows.len() == limit { return Ok(PreparedRows { rows, exhausted: false }); }
        let rowid: i64 = row.get(0)?;
        let key: UnitKey = bounded_cell(db, "lww_receive_rows", "key", rowid, MAX_UNIT_KEY_BYTES)?.try_into().map_err(lww::error)?;
        let effect = Effect { key: key.as_str().into(), affected: true, plugin_storage_changed: false };
        let cost = serde_json::to_vec(&effect)?.len().checked_add(serde_json::to_vec(key.as_str())?.len())
            .ok_or_else(|| invalid("native-receive-step-too-large"))?;
        if cost > MAX_DECODED_BYTES / 2 { return Err(invalid("native-receive-step-too-large")); }
        if !rows.is_empty() && bytes.checked_add(cost).is_none_or(|total| total > byte_limit) {
            return Ok(PreparedRows { rows, exhausted: false });
        }
        // Released writers store canonical stamps; the input row itself remains one intrinsic unit.
        let raw_stamp: String = db.query_row("SELECT stamp FROM lww_receive_rows WHERE rowid=?1", [rowid], |row| row.get(0))?;
        let stamp: risunest_sync_wire::stamp::Stamp = serde_json::from_str(&raw_stamp)?;
        stamp.validate().map_err(lww::error)?;
        let witness = row_witness(db, rowid)?.ok_or_else(|| invalid("native-receive-row-missing"))?;
        rows.push(PreparedRow { witness, key, stamp });
        bytes += cost;
    }
    Ok(PreparedRows { rows, exhausted: true })
}
/// A peer write lock past the busy timeout is retryable contention, not a store failure.
fn kernel_guard(db: &Connection) -> StoreResult<Transaction<'_>> {
    Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(|error| match error {
        rusqlite::Error::SqliteFailure(ref code, _) if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => StoreError::CommitBusy,
        error => error.into(),
    })
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoggedSource {
    binding_authority: DecimalU64,
    request_id: String,
    progress: lww::Progress,
    admitted_time_upper_ms: DecimalU64,
}
struct LoggedChanges<'a>(std::cell::RefCell<SourceReader<'a>>);
impl Serialize for LoggedChanges<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::{ser::Error, ser::SerializeSeq};
        let mut reader = self.0.borrow_mut();
        reader.changes_start().map_err(S::Error::custom)?;
        let mut sequence = serializer.serialize_seq(None)?;
        let first = reader.nonspace().map_err(S::Error::custom)?;
        if first != Some(b']') {
            if first.is_none() { return Err(S::Error::custom("native-receive-input-json")); }
            let position = reader.position - 1;
            reader.seek(position).map_err(S::Error::custom)?;
            loop {
                let change: lww::Change = reader.value().map_err(S::Error::custom)?;
                sequence.serialize_element(&change)?;
                match reader.nonspace().map_err(S::Error::custom)? {
                    Some(b',') => {}
                    Some(b']') => break,
                    _ => return Err(S::Error::custom("native-receive-input-json")),
                }
            }
        }
        reader.tail().map_err(S::Error::custom)?;
        sequence.end()
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoggedPage<'a> {
    binding_authority: DecimalU64,
    request_id: &'a str,
    changes: LoggedChanges<'a>,
    progress: &'a lww::Progress,
    admitted_time_upper_ms: DecimalU64,
}
struct PageDigest { hash: sha2::Sha256, bytes: u64 }
impl std::io::Write for PageDigest {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        self.bytes = self.bytes.checked_add(bytes.len() as u64).ok_or_else(|| std::io::Error::other("native-receive-source-integrity"))?;
        self.hash.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}
/// Stored mutation reserves the transport log after both store guards.
fn reserve_log(log: Option<&Connection>, mode: Mode) -> StoreResult<Option<Transaction<'_>>> {
    if mode != Mode::Stored { return Ok(None); }
    kernel_guard(log.ok_or_else(|| invalid("native-receive-stored-guard-required"))?).map(Some)
}
fn stored_page_matches(log: &Connection, origin: &Origin) -> StoreResult<bool> {
    use sha2::Digest;
    let rowid: Option<i64> = log.query_row("SELECT rowid FROM receive_pages WHERE authority=?1",
        [origin.authority.0.to_string()], |row| row.get(0)).optional()?;
    let Some(rowid) = rowid else { return Ok(false) };
    let length = cell_length(log, "receive_pages", "body", rowid)?;
    let mut reader = SourceReader::new(log, rowid, length, 0)?;
    reader.table = "receive_pages";
    let source: LoggedSource = serde_json::from_reader(&mut reader).map_err(|_| invalid("native-receive-stored-page-integrity"))?;
    if source.binding_authority != origin.authority || source.request_id.len() as u64 != origin.request_bytes ||
        risunest_sync_wire::hash(source.request_id.as_bytes()) != origin.request_digest { return Ok(false); }
    reader.seek(0)?;
    // Recreate the staged source without collecting changes or including transport-only fields.
    let page = LoggedPage { binding_authority: source.binding_authority, request_id: &source.request_id,
        changes: LoggedChanges(std::cell::RefCell::new(reader)), progress: &source.progress,
        admitted_time_upper_ms: source.admitted_time_upper_ms };
    let mut body = PageDigest { hash: sha2::Sha256::new(), bytes: 0 };
    serde_json::to_writer(&mut body, &page).map_err(|_| invalid("native-receive-stored-page-integrity"))?;
    Ok(body.bytes == origin.body_bytes && hex::encode(body.hash.finalize()) == origin.body_digest)
}
/// A missing or replaced originating page is stale; it never implies a fresh Prepare.
fn require_stored_page(log: Option<&Transaction<'_>>, origin: &Origin) -> StoreResult<()> {
    match log {
        Some(log) if !stored_page_matches(log, origin)? => Err(invalid("native-receive-stored-page-changed")),
        _ => Ok(()),
    }
}
fn unknown_step() -> StepFence { StepFence { keys: Vec::new(), unknown_library: true, unknown_device: true } }
fn known_step(state: &OperationState) -> StepFence {
    StepFence { keys: Vec::new(), unknown_library: state.foreign.library || state.foreign.scope,
        unknown_device: state.foreign.device || state.foreign.plugins || state.foreign.scope }
}
fn advance_phase(cursor: &mut Cursor) {
    cursor.phase = match cursor.phase {
        ScanPhase::Archives => ScanPhase::Exists,
        ScanPhase::Exists => ScanPhase::ArchivesAfterParents,
        ScanPhase::ArchivesAfterParents => ScanPhase::Remaining,
        ScanPhase::Remaining => ScanPhase::Dispositions,
        other => other,
    };
    cursor.after_key = None;
    cursor.after_rowid = 0;
}
fn row_generation_protected(device: &Connection, library: &Connection, change: &lww::Change,
    generating: &[lww::MessageLocator]) -> StoreResult<bool> {
    use super::generation::{has_protected_target, NativeGenerationTarget};
    let p = change.key.components();
    let target = match p[0].as_str() {
        "messages" | "conversation" => Some(NativeGenerationTarget::Conversation { character_id: &p[1], conversation_id: &p[2] }),
        "archive" => Some(NativeGenerationTarget::Character { character_id: &p[1] }),
        "exists" if p[1] == "conversation" => Some(NativeGenerationTarget::Conversation { character_id: &p[2], conversation_id: &p[3] }),
        "exists" if p[1] == "character" && matches!(change.value, risunest_sync_wire::unit::UnitValue::Deleted) =>
            Some(NativeGenerationTarget::Character { character_id: &p[2] }),
        _ => None,
    };
    let Some(target) = target else { return Ok(false) };
    let renderer = generating.iter().any(|item| match target {
        NativeGenerationTarget::Character { character_id } => item.character_id == character_id,
        NativeGenerationTarget::Conversation { character_id, conversation_id } => item.character_id == character_id && item.conversation_id == conversation_id,
    });
    // Even a renderer-deferred row must not conceal malformed native ownership evidence.
    Ok(has_protected_target(device, library, target)? || renderer)
}
fn receive_row_residency(library: &Connection, generation: &str, character: &str) -> StoreResult<Residency> {
    let archived: Option<bool> = library.query_row("SELECT archived_object IS NOT NULL FROM characters WHERE generation=?1 AND character_id=?2",
        params![generation, character], |row| row.get(0)).optional()?;
    Ok(match archived { None => Residency::Absent, Some(false) => Residency::Active, Some(true) => Residency::Archived })
}
enum TicketCheck { Stale, Protected, Ready { change: lww::Change, request_id: String } }
/// Needs both paired write guards, so the generation check and the install read the same state.
fn check_ticket(device: &Connection, library: &Connection, ticket: &TransitionTicket,
    generating: &[lww::MessageLocator]) -> StoreResult<TicketCheck> {
    if row_witness(library, ticket.source.rowid)?.as_ref().map(|value| &value.digest) != Some(&ticket.source.digest) {
        return Ok(TicketCheck::Stale);
    }
    let (request_id, key, stamp, value, status): (String, String, String, String, String) = library.query_row(
        "SELECT request_id,key,stamp,value,status FROM lww_receive_rows WHERE rowid=?1", [ticket.source.rowid],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))?;
    let stamp: risunest_sync_wire::stamp::Stamp = serde_json::from_str(&stamp)?;
    if key != ticket.archive_key || digest(&stamp)? != ticket.observed_stamp_digest { return Ok(TicketCheck::Stale); }
    let change = lww::Change { key: key.try_into().map_err(lww::error)?, stamp, value: serde_json::from_str(&value)? };
    let plan = lww::receive_projection(library, &change, matches!(status.as_str(), "held" | "deferred"))?;
    if lww::parent_status(library, &change.key)? != "ready" || super::current_revision(library)? != ticket.expected_revision ||
        receive_row_residency(library, &super::active_generation(library)?, &change.key.components()[1])? != ticket.expected_residency ||
        lww::read_unit(library, &change.key)?.as_ref().map(digest).transpose()? != ticket.expected_unit_digest ||
        !plan.project || plan.update_unit != (ticket.decision == TransitionDecision::ApplyRemote) {
        return Ok(TicketCheck::Stale);
    }
    if row_generation_protected(device, library, &change, generating)? { return Ok(TicketCheck::Protected); }
    Ok(TicketCheck::Ready { change, request_id })
}
/// A generation that owns the target keeps the row for a later phase, as it would an unparked row.
fn defer_ticket(device: &Connection, library: &Connection, manifest: &mut Manifest) -> StoreResult<()> {
    let ticket = manifest.ticket.take().ok_or_else(|| invalid("native-receive-ticket-integrity"))?;
    library.execute("UPDATE lww_receive_rows SET status='deferred' WHERE rowid=?1", [ticket.source.rowid])?;
    manifest.cursor.after_key = Some(ticket.archive_key);
    manifest.cursor.after_rowid = ticket.source.rowid;
    manifest.observation = current_observation(device, library)?;
    write_manifest(library, Partition::Library, manifest)
}
enum StagedTransition {
    Archive(super::archive::StagedIncomingArchive),
    Restore(super::archive::ArchivedObject, super::archive::StagedArchivePayload),
}
fn partition_gap(partition: Partition, manifest: &Manifest, current: &Observation) -> Foreign {
    match partition {
        Partition::Library => Foreign { library: current.library_revision != manifest.observation.library_revision, ..Default::default() },
        Partition::Device => Foreign { device: current.device_revision != manifest.observation.device_revision,
            plugins: current.plugins != manifest.observation.plugins, ..Default::default() },
    }
}
fn kernel_pair(device: &Connection, library: &Connection, cap: &Capability) -> StoreResult<(Coordinator, Manifest, Manifest)> {
    let coordinator = read_coordinator(device)?.ok_or_else(|| invalid("native-receive-coordinator-missing"))?;
    coordinator.require(cap)?;
    let dm = read_manifest(device, Partition::Device, cap)?.ok_or_else(|| invalid("native-receive-device-manifest-missing"))?;
    let lm = read_manifest(library, Partition::Library, cap)?.ok_or_else(|| invalid("native-receive-library-manifest-missing"))?;
    validate_producer_pair(&coordinator, &dm, Some(&lm))?;
    if dm.phase != Phase::Producing || lm.phase != Phase::Producing || coordinator.active_producer.as_ref() != Some(cap) {
        return Err(invalid("native-receive-producing-required"));
    }
    require_current_identity(device, library, cap)?;
    no_pending_intents(device)?;
    Ok((coordinator, dm, lm))
}

/// Settlement work after the bounded scans, chosen from both committed manifests.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Settlement { Supersede, Enter, Finalize, CompleteSeal, Terminal }
fn scan_done(manifest: &Manifest) -> bool {
    manifest.ticket.is_none() && matches!(manifest.cursor.phase, ScanPhase::Dispositions | ScanPhase::Complete)
}
fn history_validated(manifest: &Manifest, purposes: &[ValidationPurpose]) -> bool {
    (manifest.effect_pages == 0 && manifest.disposition_pages == 0) ||
        manifest.validated.as_ref().is_some_and(|proof| purposes.contains(&proof.purpose) && proof.root == manifest.root())
}
fn settlement_kind(device: &Connection, library: &Connection, cap: &Capability) -> StoreResult<Option<Settlement>> {
    let dm = read_manifest(device, Partition::Device, cap)?.ok_or_else(|| invalid("native-receive-device-manifest-missing"))?;
    let lm = read_manifest(library, Partition::Library, cap)?;
    let unsealed = matches!(dm.phase, Phase::Admitting | Phase::Producing | Phase::Finalizing) &&
        lm.as_ref().is_none_or(|value| matches!(value.phase, Phase::Admitting | Phase::Producing | Phase::Finalizing));
    // Unknown projection evidence requires conservative fencing, not retirement of a current owner.
    if unsealed && identity_digest(device, library)? != cap.identity_digest { return Ok(Some(Settlement::Supersede)); }
    let Some(lm) = lm else { return Ok(None) };
    Ok(match (lm.phase, dm.phase) {
        (Phase::Producing, Phase::Producing) if scan_done(&lm) && scan_done(&dm) => Some(Settlement::Enter),
        (Phase::Finalizing, Phase::Producing) => Some(Settlement::Enter),
        (Phase::Finalizing, Phase::Finalizing) => Some(Settlement::Finalize),
        (Phase::Sealed, Phase::Finalizing) if lm.outcome == Some(Outcome::Processed) => Some(Settlement::CompleteSeal),
        (Phase::Sealed, Phase::Admitting | Phase::Producing | Phase::Finalizing) if lm.outcome == Some(Outcome::Superseded) => Some(Settlement::Supersede),
        (_, Phase::Sealed | Phase::Delivered | Phase::Settled | Phase::Cleanup) => Some(Settlement::Terminal),
        _ => None,
    })
}
fn record_gap(db: &Connection, partition: Partition, manifest: &mut Manifest, current: &Observation) -> StoreResult<()> {
    let mut gap = partition_gap(partition, manifest, current);
    if !frontier_matches(db, &manifest.cursor)? {
        match partition { Partition::Library => gap.library = true, Partition::Device => gap.device = true }
    }
    manifest.foreign.merge(&gap);
    manifest.observation = current.clone();
    Ok(())
}
/// The processed seal is the native counterpart of an applied receive, which a later finish requires.
fn mark_source_applied(device: &Connection, library: &Connection, manifest: &Manifest) -> StoreResult<()> {
    let Some(rowid) = manifest.origin.source_rowid else { return Ok(()) };
    if identity_digest(device, library)? == manifest.capability.identity_digest && retained_source(device, &manifest.origin)?.is_some() {
        device.execute("UPDATE lww_receive SET applied=1 WHERE rowid=?1", [rowid])?;
    }
    Ok(())
}
struct DispositionStep { entries: Vec<DispositionEntry>, exhausted: bool }
/// Final dispositions are recomputed from rows still held or deferred at the frontier; deferred wins
/// per key, so a key is decided whole even when its rows span steps.
fn disposition_step(db: &Connection, manifest: &Manifest) -> StoreResult<DispositionStep> {
    let (limit, byte_limit) = row_limits();
    let mut statement = db.prepare("SELECT rowid FROM lww_receive_rows WHERE rowid<=?1 AND status IN ('held','deferred') AND (?2 IS NULL OR key>?2) ORDER BY key,rowid LIMIT ?3")?;
    let mut rows = statement.query(params![manifest.cursor.frontier, manifest.cursor.after_key,
        i64::try_from(limit + 1).map_err(|_| invalid("native-receive-step-too-large"))?])?;
    let (mut entries, mut visited, mut bytes): (Vec<DispositionEntry>, usize, usize) = (Vec::new(), 0, 0);
    while let Some(row) = rows.next()? {
        if visited == limit { return Ok(DispositionStep { entries, exhausted: false }); }
        visited += 1;
        let key = bounded_cell(db, "lww_receive_rows", "key", row.get(0)?, MAX_UNIT_KEY_BYTES)?;
        if entries.last().is_some_and(|entry| entry.key == key) { continue; }
        let deferred: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM lww_receive_rows WHERE status='deferred' AND key=?1 AND rowid<=?2)",
            params![key, manifest.cursor.frontier], |row| row.get(0))?;
        let entry = DispositionEntry { key, disposition: if deferred { Disposition::Deferred } else { Disposition::Held } };
        let cost = serde_json::to_vec(&entry)?.len();
        if !entries.is_empty() && (entries.len() == MAX_PAGE_KEYS || bytes.checked_add(cost).is_none_or(|total| total > byte_limit)) {
            return Ok(DispositionStep { entries, exhausted: false });
        }
        bytes += cost;
        entries.push(entry);
    }
    Ok(DispositionStep { entries, exhausted: true })
}
/// The library half commits first; recovery completes the device half before anything else.
fn commit_pair(admission: &lww::IntentAdmissionGuard, library: Transaction<'_>, device: Transaction<'_>, paired: bool) -> StoreResult<()> {
    admission.require_identity()?;
    library.commit()?;
    #[cfg(test)]
    if paired { kernel_test::settle_after_library()?; }
    #[cfg(not(test))]
    let _ = paired;
    admission.require_identity()?;
    device.commit()?;
    Ok(())
}
fn commit_advance(admission: &lww::IntentAdmissionGuard, library: Transaction<'_>, device: Transaction<'_>,
    coordinator: &Coordinator, cap: &Capability, paired: bool) -> StoreResult<AdvanceResult> {
    let state = operation_state(&device, &library, coordinator, cap)?;
    commit_pair(admission, library, device, paired)?;
    Ok(AdvanceResult::Advanced { fence: known_step(&state), state })
}
fn settle_production(admission: &lww::IntentAdmissionGuard, device: Transaction<'_>, library: Transaction<'_>,
    log: Option<&Transaction<'_>>, mut coordinator: Coordinator, cap: &Capability, kind: Settlement) -> StoreResult<AdvanceResult> {
    let mut dm = read_manifest(&device, Partition::Device, cap)?.ok_or_else(|| invalid("native-receive-device-manifest-missing"))?;
    let lm = read_manifest(&library, Partition::Library, cap)?;
    match kind {
        Settlement::Terminal => {
            let state = operation_state(&device, &library, &coordinator, cap)?;
            return Ok(AdvanceResult::Advanced { fence: known_step(&state), state });
        }
        Settlement::Supersede => return supersede_production(admission, device, library, coordinator, cap, dm, lm),
        _ => {}
    }
    let mut lm = lm.ok_or_else(|| invalid("native-receive-library-manifest-missing"))?;
    require_stored_page(log, &dm.origin)?;
    let observation = current_observation(&device, &library)?;
    match kind {
        Settlement::Enter => {
            if !scan_done(&lm) || !scan_done(&dm) { return Err(invalid("native-receive-partition-phase-changed")); }
            no_pending_intents(&device)?;
            lm.phase = Phase::Finalizing;
            dm.phase = Phase::Finalizing;
            write_manifest(&library, Partition::Library, &lm)?;
            write_manifest(&device, Partition::Device, &dm)?;
            commit_advance(admission, library, device, &coordinator, cap, true)
        }
        Settlement::Finalize => {
            // Each partition validates its owned history before its final disposition page.
            for partition in [Partition::Library, Partition::Device] {
                let (db, manifest) = match partition { Partition::Library => (&*library, &mut lm), Partition::Device => (&*device, &mut dm) };
                if manifest.cursor.phase != ScanPhase::Dispositions { continue; }
                record_gap(db, partition, manifest, &observation)?;
                if validate_history_step(db, partition, manifest, ValidationPurpose::Seal)? {
                    let step = disposition_step(db, manifest)?;
                    if let Some(last) = step.entries.last() { manifest.cursor.after_key = Some(last.key.clone()); }
                    if !step.entries.is_empty() { append_dispositions(db, partition, manifest, &step.entries)?; }
                    if step.exhausted {
                        manifest.cursor.phase = ScanPhase::Complete;
                        manifest.cursor.after_key = None;
                        manifest.cursor.after_rowid = 0;
                    }
                    write_manifest(db, partition, manifest)?;
                }
                return commit_advance(admission, library, device, &coordinator, cap, false);
            }
            for partition in [Partition::Library, Partition::Device] {
                let (db, manifest) = match partition { Partition::Library => (&*library, &mut lm), Partition::Device => (&*device, &mut dm) };
                if !history_validated(manifest, &[ValidationPurpose::Seal]) {
                    validate_history_step(db, partition, manifest, ValidationPurpose::Seal)?;
                    return commit_advance(admission, library, device, &coordinator, cap, false);
                }
            }
            no_pending_intents(&device)?;
            let changed = identity_digest(&device, &library)? != cap.identity_digest;
            for (db, partition, manifest) in [(&*library, Partition::Library, &mut lm), (&*device, Partition::Device, &mut dm)] {
                record_gap(db, partition, manifest, &observation)?;
                manifest.foreign.scope |= changed;
                manifest.phase = Phase::Sealed;
                manifest.outcome = Some(Outcome::Processed);
            }
            write_manifest(&library, Partition::Library, &lm)?;
            mark_source_applied(&device, &library, &dm)?;
            coordinator.sealed(cap)?;
            write_coordinator(&device, &coordinator)?;
            write_manifest(&device, Partition::Device, &dm)?;
            commit_advance(admission, library, device, &coordinator, cap, true)
        }
        Settlement::CompleteSeal => {
            if dm.cursor.phase != ScanPhase::Complete || !history_validated(&dm, &[ValidationPurpose::Seal]) || lm.outcome != Some(Outcome::Processed) {
                return Err(invalid("native-receive-partition-phase-changed"));
            }
            record_gap(&device, Partition::Device, &mut dm, &observation)?;
            dm.foreign.scope |= identity_digest(&device, &library)? != cap.identity_digest;
            dm.phase = Phase::Sealed;
            dm.outcome = lm.outcome;
            mark_source_applied(&device, &library, &dm)?;
            coordinator.sealed(cap)?;
            write_coordinator(&device, &coordinator)?;
            write_manifest(&device, Partition::Device, &dm)?;
            commit_advance(admission, library, device, &coordinator, cap, false)
        }
        Settlement::Terminal | Settlement::Supersede => unreachable!(),
    }
}
/// Dispositions are provisional until sealing. Remove one tail page without changing owned effects.
fn discard_disposition_tail(db: &Connection, partition: Partition, manifest: &mut Manifest) -> StoreResult<bool> {
    if manifest.disposition_pages == 0 { return Ok(false); }
    if manifest.phase != Phase::Finalizing { return Err(invalid("native-receive-disposition-phase")); }
    let ordinal = manifest.disposition_pages - 1;
    let key = disposition_key(&manifest.capability, ordinal)?;
    let page: DispositionPage = read(db, partition, &key)?.ok_or_else(|| invalid("native-receive-disposition-page-missing"))?;
    let page = read_disposition_page(db, partition, manifest, ordinal, &page.previous_digest)?;
    if digest(&page)? != manifest.disposition_root { return Err(invalid("native-receive-history-integrity")); }
    let (held, deferred) = validate_disposition_indexes(db, partition, manifest, &page)?;
    manifest.held_count = manifest.held_count.checked_sub(held).ok_or_else(|| invalid("native-receive-history-integrity"))?;
    manifest.deferred_count = manifest.deferred_count.checked_sub(deferred).ok_or_else(|| invalid("native-receive-history-integrity"))?;
    for entry in &page.entries {
        db.execute(&format!("DELETE FROM {} WHERE key=?1", partition.table()),
            [disposition_index_key(&manifest.capability, &entry.key)?])?;
    }
    db.execute(&format!("DELETE FROM {} WHERE key=?1", partition.table()), [key])?;
    manifest.disposition_pages = ordinal;
    manifest.disposition_root = page.previous_digest;
    manifest.validation = None;
    manifest.validated = None;
    write_manifest(db, partition, manifest)?;
    Ok(true)
}
/// Seals only the complete owned effects; no disposition, source flag or cursor is fabricated.
fn supersede_production(admission: &lww::IntentAdmissionGuard, device: Transaction<'_>, library: Transaction<'_>,
    mut coordinator: Coordinator, cap: &Capability, mut dm: Manifest, lm: Option<Manifest>) -> StoreResult<AdvanceResult> {
    no_pending_intents(&device)?;
    let mut lm = match lm {
        Some(manifest) => manifest,
        None if !operation_namespace_exists(&library, Partition::Library, cap)? =>
            empty_manifest(cap.clone(), dm.origin.clone(), dm.observation.clone(), 0, None, dm.foreign.clone()),
        None => return Err(invalid("native-receive-library-manifest-missing")),
    };
    for manifest in [&mut lm, &mut dm] {
        if let Some(ticket) = &manifest.ticket {
            // A parked native flip never installed; a legacy one still owns its intent.
            if ticket.legacy.is_some() { return Err(invalid("native-receive-legacy-ticket-unresolved")); }
            manifest.ticket = None;
        }
        manifest.foreign.scope = true;
    }
    for partition in [Partition::Library, Partition::Device] {
        let (db, manifest) = match partition { Partition::Library => (&*library, &mut lm), Partition::Device => (&*device, &mut dm) };
        if discard_disposition_tail(db, partition, manifest)? {
            return commit_advance(admission, library, device, &coordinator, cap, false);
        }
        if !history_validated(manifest, &[ValidationPurpose::Seal]) {
            validate_history_step(db, partition, manifest, ValidationPurpose::Seal)?;
            return commit_advance(admission, library, device, &coordinator, cap, false);
        }
    }
    for manifest in [&mut lm, &mut dm] {
        manifest.phase = Phase::Sealed;
        manifest.outcome = Some(Outcome::Superseded);
    }
    write_manifest(&library, Partition::Library, &lm)?;
    coordinator.sealed(cap)?;
    write_coordinator(&device, &coordinator)?;
    write_manifest(&device, Partition::Device, &dm)?;
    commit_advance(admission, library, device, &coordinator, cap, true)
}
fn settlement_manifests(library: &Connection, device: &Connection, cap: &Capability) -> StoreResult<(Manifest, Manifest)> {
    Ok((read_manifest(library, Partition::Library, cap)?.ok_or_else(|| invalid("native-receive-library-manifest-missing"))?,
        read_manifest(device, Partition::Device, cap)?.ok_or_else(|| invalid("native-receive-device-manifest-missing"))?))
}
/// Moves both partitions toward `phase` with the exact delivered roots; an earlier exact delivery is kept.
fn settle_manifests(library: &Connection, device: &Connection, lm: &mut Manifest, dm: &mut Manifest,
    roots: &Roots, phase: Phase) -> StoreResult<()> {
    for (db, partition, manifest) in [(library, Partition::Library, lm), (device, Partition::Device, dm)] {
        match manifest.phase {
            Phase::Sealed => manifest.delivered = Some(roots.clone()),
            Phase::Delivered | Phase::Settled if manifest.delivered.as_ref() == Some(roots) => {}
            Phase::Delivered | Phase::Settled => return Err(invalid("native-receive-delivery-changed")),
            _ => return Err(invalid("native-receive-seal-required")),
        }
        if manifest.phase != Phase::Settled { manifest.phase = phase; }
        write_manifest(db, partition, manifest)?;
    }
    Ok(())
}
/// The open server-slot history that processed exactly this transport page. Superseded history did
/// not process it, so its transport cleanup is not held back.
fn transport_history(device: &Connection, header: &lww::Header) -> StoreResult<Option<(Coordinator, Capability, Origin)>> {
    let Some(coordinator) = read_coordinator(device)? else { return Ok(None) };
    let Some(cap) = coordinator.server_history.clone() else { return Ok(None) };
    if coordinator.cleanup.iter().any(|proof| proof.capability == cap) { return Ok(None); }
    let manifest = read_manifest(device, Partition::Device, &cap)?.ok_or_else(|| invalid("native-receive-device-manifest-missing"))?;
    let origin = manifest.origin;
    if manifest.outcome == Some(Outcome::Superseded) || origin.authority != header.binding_authority ||
        origin.request_bytes != header.request_id.len() as u64 || origin.request_digest != risunest_sync_wire::hash(header.request_id.as_bytes()) {
        return Ok(None);
    }
    Ok(Some((coordinator, cap, origin)))
}
fn require_projection_identity(device: &Connection, library: &Connection, projection: &NativeReceiveEvidence) -> StoreResult<()> {
    let gap = baseline_gap(device, library, Some(projection))?;
    if gap.scope { return Err(invalid("native-receive-projection-identity-changed")); }
    if gap.library || gap.device || gap.plugins { return Err(invalid("native-receive-projection-changed")); }
    Ok(())
}
/// Same-identity retirement needs proof that the source finished here: this binary's witness, the
/// retained applied and finished row, or the exact rule by which a later finished receive pruned it.
fn retirement_proven(device: &Connection, library: &Connection, cap: &Capability, manifest: &Manifest) -> StoreResult<bool> {
    if identity_digest(device, library)? != cap.identity_digest || manifest.local_finish_witness { return Ok(true); }
    let origin = &manifest.origin;
    let (Some(rowid), Some(progress)) = (origin.source_rowid, origin.progress.as_ref()) else { return Ok(cap.mode == Mode::Drain) };
    if let Some(status) = retained_source(device, origin)? { return Ok(status.applied && status.finished); }
    let kind = if progress.external { "external" } else { "server" };
    let channel = (progress.writer_digest.clone(), progress.writer_bytes);
    let candidates = |sql: &str, values: &[&dyn rusqlite::ToSql]| -> StoreResult<Vec<i64>> {
        let mut statement = device.prepare(sql)?;
        let rows = statement.query_map(values, |row| row.get(0))?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    };
    let authority = origin.authority.0.to_string();
    let mut progressed = false;
    for row in candidates("SELECT rowid FROM lww_progress WHERE kind=?1 AND authority=?2", &[&kind, &authority])? {
        let cursor: DecimalU64 = bounded_cell(device, "lww_progress", "cursor", row, 20)?.try_into().map_err(lww::error)?;
        progressed |= hash_cell(device, "lww_progress", "writer_id", row)? == channel && cursor >= progress.cursor;
    }
    if !progressed { return Ok(false); }
    let pending = |db: &Connection| -> StoreResult<bool> {
        let mut rows = db.prepare("SELECT rowid FROM lww_receive_rows WHERE status IN ('held','deferred')")?;
        let mut rows = rows.query([])?;
        while let Some(row) = rows.next()? {
            if hash_cell(db, "lww_receive_rows", "request_id", row.get(0)?)? == (origin.request_digest.clone(), origin.request_bytes) { return Ok(true); }
        }
        Ok(false)
    };
    if pending(device)? || pending(library)? { return Ok(false); }
    let cursor = progress.cursor.0.to_string();
    for row in candidates("SELECT rowid FROM lww_receive WHERE finished=1 AND kind=?1 AND rowid>?2 AND cursor<>?3", &[&kind, &rowid, &cursor])? {
        if hash_cell(device, "lww_receive", "writer_id", row)? == channel { return Ok(true); }
    }
    Ok(false)
}
/// Runs inside the device transaction that finishes a receive, before the source is marked finished.
pub(super) fn witness_native_finish(device: &Connection, rowid: i64) -> StoreResult<()> {
    require_transaction(device)?;
    let Some(coordinator) = read_coordinator(device)? else { return Ok(()) };
    for cap in [coordinator.server_history.as_ref(), coordinator.local_history.as_ref()].into_iter().flatten() {
        let Some(mut manifest) = read_manifest(device, Partition::Device, cap)? else { continue };
        if manifest.origin.source_rowid != Some(rowid) { continue; }
        let Some(status) = retained_source(device, &manifest.origin)? else { continue };
        // Native production applied this source; its finish waits for the exact delivery.
        if manifest.phase == Phase::Sealed && manifest.outcome == Some(Outcome::Processed) && !status.finished {
            return Err(invalid("native-receive-delivery-required"));
        }
        if !manifest.local_finish_witness {
            manifest.local_finish_witness = true;
            write_manifest(device, Partition::Device, &manifest)?;
        }
    }
    Ok(())
}

impl PersistentStore {
    pub(super) fn advance_local_native_receive(&self, request: &AdvanceRequest) -> StoreResult<AdvanceResult> {
        if request.capability.mode == Mode::Stored { return Err(invalid("native-receive-stored-guard-required")); }
        self.advance_native_receive(request, None)
    }
    /// Every mutating block reserves `log` last and rechecks the exact originating page.
    pub(crate) fn advance_stored_native_receive(&self, log: &Connection, request: &AdvanceRequest) -> StoreResult<AdvanceResult> {
        if request.capability.mode != Mode::Stored { return Err(invalid("native-receive-source-mode")); }
        self.advance_native_receive(request, Some(log))
    }
    fn advance_native_receive(&self, request: &AdvanceRequest, log: Option<&Connection>) -> StoreResult<AdvanceResult> {
        let admission = self.acquire_intent_admission()?;
        let cap = &request.capability;
        let (partition, prepared) = {
            let device = kernel_guard(self.device_store()?.connection())?;
            let library = kernel_guard(&self.connection)?;
            let log_guard = reserve_log(log, cap.mode)?;
            let coordinator = read_coordinator(&device)?.ok_or_else(|| invalid("native-receive-coordinator-missing"))?;
            let state = operation_state(&device, &library, &coordinator, cap)?;
            if state.progress_token != request.expected_progress_token {
                return Ok(AdvanceResult::Stale { state, fence: unknown_step() });
            }
            if let Some(kind) = settlement_kind(&device, &library, cap)? {
                return settle_production(&admission, device, library, log_guard.as_ref(), coordinator, cap, kind);
            }
            let (_, mut dm, mut lm) = kernel_pair(&device, &library, cap)?;
            require_stored_page(log_guard.as_ref(), &dm.origin)?;
            let partition = if lm.ticket.is_some() || !matches!(lm.cursor.phase, ScanPhase::Dispositions | ScanPhase::Complete) {
                Partition::Library
            } else { Partition::Device };
            let (db, manifest) = match partition { Partition::Library => (&library, &mut lm), Partition::Device => (&device, &mut dm) };
            if manifest.ticket.is_none() && matches!(manifest.cursor.phase, ScanPhase::Dispositions | ScanPhase::Complete) {
                return Ok(AdvanceResult::Advanced { fence: known_step(&state), state });
            }
            let observation = current_observation(&device, &library)?;
            let gap = partition_gap(partition, manifest, &observation);
            let mut reset = gap.library || gap.device || gap.plugins || !frontier_matches(db, &manifest.cursor)?;
            // A parked flip is checked again before anything is staged; a stale one rescans.
            let mut ready = None;
            if let (false, Some(ticket)) = (reset, manifest.ticket.clone()) {
                match check_ticket(&device, &library, &ticket, &request.generating)? {
                    TicketCheck::Stale => reset = true,
                    TicketCheck::Protected => {
                        defer_ticket(&device, &library, manifest)?;
                        let state = operation_state(&device, &library, &coordinator, cap)?;
                        admission.require_identity()?;
                        library.commit()?;
                        device.commit()?;
                        return Ok(AdvanceResult::Advanced { fence: known_step(&state), state });
                    }
                    TicketCheck::Ready { change, request_id } => ready = Some((ticket, change, request_id)),
                }
            }
            if reset { manifest.ticket = None; }
            if reset && manifest.validation.is_none() { manifest.validated = None; }
            manifest.foreign.merge(&gap);
            if reset && !gap.library && !gap.device && !gap.plugins {
                match partition { Partition::Library => manifest.foreign.library = true, Partition::Device => manifest.foreign.device = true }
            }
            if let Some((ticket, change, request_id)) = ready {
                // The step that parked the row already made its stamp durable; nothing is written here.
                drop(log_guard);
                library.rollback()?;
                device.rollback()?;
                return self.install_native_archive_transition(&admission, request, ticket, change, request_id, log);
            }
            // A resumed validation visits one immutable page, never a full-history collection.
            if !validate_history_step(db, partition, manifest, ValidationPurpose::Seal)? {
                let state = operation_state(&device, &library, &coordinator, cap)?;
                admission.require_identity()?;
                library.commit()?;
                device.commit()?;
                return Ok(AdvanceResult::Advanced { fence: known_step(&state), state });
            }
            if reset {
                let (frontier, sentinel) = source_frontier(db)?;
                manifest.cursor = Cursor { phase: ScanPhase::Archives, epoch: manifest.cursor.epoch.checked_add(1)
                    .ok_or_else(|| invalid("native-receive-epoch-exhausted"))?, frontier, sentinel_digest: sentinel, after_key: None, after_rowid: 0 };
                manifest.observation = observation;
                write_manifest(db, partition, manifest)?;
                let state = operation_state(&device, &library, &coordinator, cap)?;
                admission.require_identity()?;
                library.commit()?;
                device.commit()?;
                return Ok(AdvanceResult::Advanced { state, fence: unknown_step() });
            }
            let prepared = prepare_rows(db, manifest)?;
            // Validation is control work. Publish it before using a new progress token.
            let validated_state = operation_state(&device, &library, &coordinator, cap)?;
            if validated_state.progress_token != request.expected_progress_token {
                admission.require_identity()?;
                library.commit()?;
                device.commit()?;
                return Ok(AdvanceResult::Advanced { fence: known_step(&validated_state), state: validated_state });
            }
            if let Some(stamp) = prepared.rows.iter().map(|row| &row.stamp).max() { lww::observe(&device, stamp)?; }
            admission.require_identity()?;
            device.commit()?;
            // The library guard pins identity until observation is durable; no effect is installed here.
            library.rollback()?;
            (partition, prepared)
        };
        #[cfg(test)]
        if !prepared.rows.is_empty() { kernel_test::after_observe()?; }
        admission.require(self.device_store()?.connection())?;
        let device = kernel_guard(self.device_store()?.connection())?;
        let library = kernel_guard(&self.connection)?;
        let log_guard = reserve_log(log, cap.mode)?;
        let coordinator = read_coordinator(&device)?.ok_or_else(|| invalid("native-receive-coordinator-missing"))?;
        let state = operation_state(&device, &library, &coordinator, cap)?;
        if state.progress_token != request.expected_progress_token || identity_digest(&device, &library)? != cap.identity_digest {
            return Ok(AdvanceResult::Advanced { state, fence: unknown_step() });
        }
        let (_, mut dm, mut lm) = kernel_pair(&device, &library, cap)?;
        require_stored_page(log_guard.as_ref(), &dm.origin)?;
        let observation = current_observation(&device, &library)?;
        let (tx, manifest) = match partition { Partition::Library => (&library, &mut lm), Partition::Device => (&device, &mut dm) };
        let gap = partition_gap(partition, manifest, &observation);
        if gap.library || gap.device || gap.plugins || !frontier_matches(tx, &manifest.cursor)? {
            return Ok(AdvanceResult::Advanced { state, fence: unknown_step() });
        }
        for row in &prepared.rows {
            if row_witness(tx, row.witness.rowid)?.as_ref().map(|value| &value.digest) != Some(&row.witness.digest) {
                return Ok(AdvanceResult::Advanced { state, fence: unknown_step() });
            }
        }
        let generation = if partition == Partition::Library { super::active_generation(&library)? } else { String::new() };
        let mut effects = Vec::new();
        let mut affected = Vec::new();
        let mut fence = StepFence { keys: Vec::new(), unknown_library: state.foreign.library,
            unknown_device: state.foreign.device || state.foreign.plugins };
        let mut revision = None;
        for row in &prepared.rows {
            let (id, value, previous): (String, String, String) = tx.query_row("SELECT request_id,value,status FROM lww_receive_rows WHERE rowid=?1",
                [row.witness.rowid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            let change = lww::Change { key: row.key.clone(), stamp: row.stamp.clone(), value: serde_json::from_str(&value)? };
            let parent = lww::parent_status(tx, &change.key)?;
            let retired = parent == "retired";
            let hard_delete = change.key.components()[0] == "exists" && matches!(change.value, risunest_sync_wire::unit::UnitValue::Deleted);
            let mut status = if retired { "done" } else if partition == Partition::Library && parent == "held" { "held" } else { "done" };
            if status == "done" && (!retired || hard_delete) && partition == Partition::Library &&
                row_generation_protected(&device, &library, &change, &request.generating)? { status = "deferred"; }
            if status == "done" && (!retired || hard_delete) {
                let plan = lww::receive_projection(tx, &change, matches!(previous.as_str(), "held" | "deferred"))?;
                if plan.project && partition == Partition::Library && change.key.components()[0] == "archive" {
                    let residency = receive_row_residency(&library, &generation, &change.key.components()[1])?;
                    let restore = matches!(change.value, risunest_sync_wire::unit::UnitValue::Deleted);
                    if (restore && residency == Residency::Archived) || (!restore && residency == Residency::Active) {
                        manifest.ticket = Some(TransitionTicket { capability: cap.clone(), source: row.witness.clone(), archive_key: change.key.as_str().into(),
                            restore, decision: if plan.update_unit { TransitionDecision::ApplyRemote } else { TransitionDecision::Identical },
                            expected_residency: residency, expected_revision: revision.unwrap_or(super::current_revision(&library)?),
                            expected_unit_digest: lww::read_unit(&library, &change.key)?.as_ref().map(digest).transpose()?,
                            observed_stamp_digest: digest(&change.stamp)?, cas_job: None, legacy: None });
                        break;
                    }
                }
                if plan.project && revision.is_none() {
                    revision = Some(if partition == Partition::Device { super::device_store::begin_mutation_remote(tx)? }
                        else { let next = super::current_revision(tx)?.checked_add(1).ok_or_else(|| invalid("native-receive-revision-exhausted"))?;
                            super::content_change_index::begin_mutation(tx, &generation, next, "server")?; next });
                }
                let changed = lww::apply_receive_projection(tx, &generation, &id, &change, &plan, lww::authority(&device)?)?;
                if plan.project {
                    effects.push(Effect { key: change.key.as_str().into(), affected: true, plugin_storage_changed: changed });
                    affected.push(change.key.clone());
                    if changed { fence.keys.push(change.key.clone()); }
                }
            }
            if status != previous { tx.execute("UPDATE lww_receive_rows SET status=?2 WHERE rowid=?1", params![row.witness.rowid, status])?; }
            manifest.cursor.after_key = Some(change.key.as_str().into());
            manifest.cursor.after_rowid = row.witness.rowid;
        }
        if manifest.ticket.is_none() && prepared.exhausted { advance_phase(&mut manifest.cursor); }
        if let Some(revision) = revision {
            if partition == Partition::Library {
                lww::refresh_received_orders(tx, &generation, &affected)?;
                super::content_change_index::finish_mutation(tx)?;
                super::commit::set_active(tx, revision, &generation)?;
            } else { super::device_store::finish_mutation_remote(tx)?; }
        }
        append_effects(tx, partition, manifest, &effects)?;
        manifest.observation = current_observation(&device, &library)?;
        write_manifest(tx, partition, manifest)?;
        // The committed state is read inside the guards, so a peer lock after commit cannot drop this step's keys.
        let state = operation_state(&device, &library, &coordinator, cap)?;
        #[cfg(test)]
        if !prepared.rows.is_empty() { kernel_test::before_commit()?; }
        admission.require_identity()?;
        library.commit()?;
        #[cfg(test)]
        if !prepared.rows.is_empty() { kernel_test::after_library()?; }
        admission.require_identity()?;
        device.commit()?;
        fence.unknown_library |= state.foreign.library || state.foreign.scope;
        fence.unknown_device |= state.foreign.device || state.foreign.plugins || state.foreign.scope;
        Ok(AdvanceResult::Advanced { state, fence })
    }

    /// Stages the CAS a parked flip needs outside every write guard, then installs it in one
    /// library commit with the row completion and its effect. No Archive intent is reserved.
    fn install_native_archive_transition(&self, admission: &lww::IntentAdmissionGuard, request: &AdvanceRequest,
        ticket: TransitionTicket, change: lww::Change, request_id: String, log: Option<&Connection>) -> StoreResult<AdvanceResult> {
        use crate::asset_repository::job_pins::CasReleaseOutcome;
        let character = change.key.components()[1].clone();
        let cas = crate::asset_repository::PayloadCas::new(&self.repository_root)?;
        let generation = super::active_generation(&self.connection)?;
        let (staged, scratch) = if ticket.restore {
            let archived = super::archive::read_archived_object(&self.connection, &generation, &character)?
                .ok_or_else(|| lww::error("archive-object-missing"))?;
            let file = cas.open_object(&archived.object_hash)?.ok_or_else(|| lww::error("archive-object-missing"))?;
            let scratch = super::archive::create_restore_staging(&self.connection, cas.repository_root())?;
            match super::archive::stage_archive_payload(&self.connection, file, &character, &archived, &|| false) {
                Ok(payload) => (StagedTransition::Restore(archived, payload), Some(scratch)),
                Err(error) => { let _ = super::archive::clear_restore_staging(&self.connection); return Err(error); }
            }
        } else {
            (StagedTransition::Archive(super::archive::stage_incoming_archive(&self.connection, &cas, &generation, &character, &change.value)?), None)
        };
        let mut job = None;
        let result = self.commit_native_archive_transition(admission, request, &ticket, &change, &request_id, &cas, staged, &mut job, log);
        let cleanup = if scratch.is_some() { super::archive::clear_restore_staging(&self.connection) } else { Ok(()) };
        drop(scratch);
        // Every repository and database guard has ended before an abort catalogs published output.
        let released = match job.as_mut() {
            Some(job) if !job.is_released() => job.release(CasReleaseOutcome::Aborted).map_err(StoreError::from),
            _ => Ok(()),
        };
        match result {
            Ok(result) => { cleanup?; released?; Ok(result) }
            Err(error) => { let _ = (cleanup, released); Err(error) }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_native_archive_transition(&self, admission: &lww::IntentAdmissionGuard, request: &AdvanceRequest,
        ticket: &TransitionTicket, change: &lww::Change, request_id: &str, cas: &crate::asset_repository::PayloadCas,
        staged: StagedTransition, job: &mut Option<crate::asset_repository::job_pins::DurableCasJob>,
        log: Option<&Connection>) -> StoreResult<AdvanceResult> {
        use crate::asset_repository::job_pins::{CasJobKind, CasJobOwner, DurableCasJob};
        let cap = &request.capability;
        #[cfg(test)]
        kernel_test::after_stage()?;
        if matches!(staged, StagedTransition::Archive(_)) {
            let id = uuid::Uuid::new_v4().to_string();
            *job = Some(DurableCasJob::begin(&self.repository_root, &id, CasJobKind::DirectAssetOrInlayWrite,
                CasJobOwner::page_write(&id), super::device_store::now_ms()?)?);
        }
        let repository = crate::asset_repository::coordinator::lock_repository_mutation()?;
        admission.require(self.device_store()?.connection())?;
        let device = kernel_guard(self.device_store()?.connection())?;
        let library = kernel_guard(&self.connection)?;
        let log_guard = reserve_log(log, cap.mode)?;
        let coordinator = read_coordinator(&device)?.ok_or_else(|| invalid("native-receive-coordinator-missing"))?;
        let state = operation_state(&device, &library, &coordinator, cap)?;
        if state.progress_token != request.expected_progress_token || identity_digest(&device, &library)? != cap.identity_digest {
            return Ok(AdvanceResult::Advanced { state, fence: unknown_step() });
        }
        let (_, _, mut lm) = kernel_pair(&device, &library, cap)?;
        require_stored_page(log_guard.as_ref(), &lm.origin)?;
        if partition_gap(Partition::Library, &lm, &current_observation(&device, &library)?).library || !frontier_matches(&library, &lm.cursor)? {
            return Ok(AdvanceResult::Advanced { state, fence: unknown_step() });
        }
        match check_ticket(&device, &library, ticket, &request.generating)? {
            TicketCheck::Stale => return Ok(AdvanceResult::Advanced { state, fence: unknown_step() }),
            TicketCheck::Protected => {
                defer_ticket(&device, &library, &mut lm)?;
                let state = operation_state(&device, &library, &coordinator, cap)?;
                admission.require_identity()?;
                library.commit()?;
                device.commit()?;
                return Ok(AdvanceResult::Advanced { fence: known_step(&state), state });
            }
            TicketCheck::Ready { .. } => {}
        }
        let parts = change.key.components();
        let character = &parts[1];
        let generation = super::active_generation(&library)?;
        let revision = super::current_revision(&library)?.checked_add(1).ok_or_else(|| invalid("native-receive-revision-exhausted"))?;
        super::content_change_index::begin_mutation(&library, &generation, revision, "server")?;
        match staged {
            StagedTransition::Archive(staged) => super::archive::install_incoming_archive(&library, cas, &generation, character, staged,
                job.as_mut().ok_or_else(|| invalid("native-receive-ticket-integrity"))?, &repository)?,
            StagedTransition::Restore(archived, payload) => {
                if super::archive::read_archived_object(&library, &generation, character)?.as_ref() != Some(&archived) {
                    return Err(invalid("native-receive-ticket-integrity"));
                }
                super::archive::restore_staged_rows(&library, &generation, character, &payload, &|| false)?;
            }
        }
        let binding = lww::authority(&device)?;
        match ticket.decision {
            // The deterministic transition receipt, unit version and witness of the released route.
            TransitionDecision::ApplyRemote => {
                let header = lww::Header { binding_authority: binding,
                    request_id: format!("archive-transition:{request_id}:{}", risunest_sync_wire::hash(change.key.as_str().as_bytes())) };
                let receipt = digest(&("native-archive-transition", &ticket.source, &ticket.archive_key))?;
                let context = lww::ArchiveContext { header: &header, stamp: &change.stamp, digest: &receipt, incoming: Some(change) };
                lww::record_archive_state(&library, &generation, character, Some(&context), ticket.restore)?;
            }
            // The exact local unit stays; a local_pending outbox entry keeps its witness skip.
            TransitionDecision::Identical => {
                if ticket.restore { lww::reproject_archived_children(&library, &generation, character)?; }
                lww::apply_receive_projection(&library, &generation, request_id, change,
                    &lww::ReceiveProjection { update_unit: false, project: false }, binding)?;
            }
        }
        super::content_change_index::finish_mutation(&library)?;
        super::commit::set_active(&library, revision, &generation)?;
        library.execute("UPDATE lww_receive_rows SET status='done' WHERE rowid=?1", [ticket.source.rowid])?;
        lm.ticket = None;
        lm.cursor.after_key = Some(ticket.archive_key.clone());
        lm.cursor.after_rowid = ticket.source.rowid;
        append_effects(&library, Partition::Library, &mut lm, &[Effect { key: ticket.archive_key.clone(), affected: true, plugin_storage_changed: false }])?;
        lm.observation = current_observation(&device, &library)?;
        write_manifest(&library, Partition::Library, &lm)?;
        let state = operation_state(&device, &library, &coordinator, cap)?;
        #[cfg(test)]
        kernel_test::ticket_before_commit()?;
        admission.require_identity()?;
        library.commit()?;
        #[cfg(test)]
        kernel_test::ticket_after_library()?;
        // The device guard spans the library commit, so no generation can start in between.
        admission.require_identity()?;
        device.commit()?;
        if let Some(job) = job.as_mut() { job.finish_page_catalog_registration(&repository)?; }
        Ok(AdvanceResult::Advanced { fence: known_step(&state), state })
    }
}

impl PersistentStore {
    /// One immutable history page; the caller chains `previous_digest` from the page before it.
    pub(crate) fn read_native_receive_page(&self, request: &ReadPage) -> StoreResult<HistoryPage> {
        let cap = &request.capability;
        let device = Transaction::new_unchecked(self.device_store()?.connection(), TransactionBehavior::Deferred)?;
        let library = Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)?;
        let coordinator = read_coordinator(&device)?.ok_or_else(|| invalid("native-receive-coordinator-missing"))?;
        coordinator.require(cap)?;
        if coordinator.cleanup.iter().any(|proof| proof.capability == *cap) { return Err(invalid("native-receive-cleanup-started")); }
        let db: &Connection = match request.partition { Partition::Library => &library, Partition::Device => &device };
        let manifest = read_manifest(db, request.partition, cap)?.ok_or_else(|| invalid("native-receive-manifest-missing"))?;
        Ok(match request.kind {
            PageKind::Effects => {
                let page = read_effect_page(db, request.partition, &manifest, request.ordinal.0, &request.previous_digest)?;
                HistoryPage::Effects { digest: digest(&page)?, page }
            }
            PageKind::Dispositions => {
                let page = read_disposition_page(db, request.partition, &manifest, request.ordinal.0, &request.previous_digest)?;
                HistoryPage::Dispositions { digest: digest(&page)?, page }
            }
        })
    }

    /// Records that the exact sealed roots were projected. Drain, and a Staged source already finished
    /// here, settle at once; any other source settles through its transport acknowledgment or retirement.
    pub(crate) fn acknowledge_native_receive_delivery(&self, request: &DeliveryRequest) -> StoreResult<OperationState> {
        let cap = &request.capability;
        request.roots.validate()?;
        let admission = self.acquire_intent_admission()?;
        admission.require(self.device_store()?.connection())?;
        let device = kernel_guard(self.device_store()?.connection())?;
        let library = kernel_guard(&self.connection)?;
        let coordinator = read_coordinator(&device)?.ok_or_else(|| invalid("native-receive-coordinator-missing"))?;
        let roots = operation_state(&device, &library, &coordinator, cap)?.roots.ok_or_else(|| invalid("native-receive-seal-required"))?;
        if roots != request.roots { return Err(invalid("native-receive-delivery-changed")); }
        require_projection_identity(&device, &library, &request.projection)?;
        let (mut lm, mut dm) = settlement_manifests(&library, &device, cap)?;
        let settled = cap.mode == Mode::Drain || (cap.mode == Mode::Staged && dm.local_finish_witness);
        settle_manifests(&library, &device, &mut lm, &mut dm, &roots, if settled { Phase::Settled } else { Phase::Delivered })?;
        let state = operation_state(&device, &library, &coordinator, cap)?;
        commit_pair(&admission, library, device, true)?;
        Ok(state)
    }

    /// Runs after the remote acknowledgment and before the transport page is deleted, so a failure
    /// keeps the page and a retry acknowledges again. Any other receive takes no guard.
    pub(crate) fn settle_native_receive_transport(&self, log: &Connection, header: &lww::Header) -> StoreResult<()> {
        {
            let device = Transaction::new_unchecked(self.device_store()?.connection(), TransactionBehavior::Deferred)?;
            if transport_history(&device, header)?.is_none() { return Ok(()); }
        }
        let admission = self.acquire_intent_admission()?;
        admission.require(self.device_store()?.connection())?;
        let device = kernel_guard(self.device_store()?.connection())?;
        let library = kernel_guard(&self.connection)?;
        let log = kernel_guard(log)?;
        let Some((coordinator, cap, origin)) = transport_history(&device, header)? else { return Ok(()) };
        if !stored_page_matches(&log, &origin)? { return Err(invalid("native-receive-stored-page-changed")); }
        let state = operation_state(&device, &library, &coordinator, &cap)?;
        let roots = state.roots.ok_or_else(|| invalid("native-receive-delivery-required"))?;
        let (mut lm, mut dm) = settlement_manifests(&library, &device, &cap)?;
        if state.outcome != Some(Outcome::Processed) || [&lm, &dm].iter().any(|manifest| manifest.delivered.as_ref() != Some(&roots)) {
            return Err(invalid("native-receive-delivery-required"));
        }
        if !dm.local_finish_witness && !retained_source(&device, &dm.origin)?.is_some_and(|status| status.applied && status.finished) {
            return Err(invalid("native-receive-finish-required"));
        }
        settle_manifests(&library, &device, &mut lm, &mut dm, &roots, Phase::Settled)?;
        commit_pair(&admission, library, device, true)?;
        drop(log);
        Ok(())
    }

    /// Local retirement for exact delivered history whose transport acknowledgment cannot be observed.
    /// It changes no cursor and claims no remote acknowledgment; unprovable history stays pending.
    pub(crate) fn retire_native_receive(&self, log: Option<&Connection>, request: &RetireRequest) -> StoreResult<RetireResult> {
        let cap = &request.capability;
        request.roots.validate()?;
        let admission = self.acquire_intent_admission()?;
        admission.require(self.device_store()?.connection())?;
        let device = kernel_guard(self.device_store()?.connection())?;
        let library = kernel_guard(&self.connection)?;
        let log = reserve_log(log, cap.mode)?;
        let coordinator = read_coordinator(&device)?.ok_or_else(|| invalid("native-receive-coordinator-missing"))?;
        coordinator.require(cap)?;
        if let Some(proof) = coordinator.cleanup.iter().find(|proof| proof.capability == *cap) {
            return if proof.roots == request.roots { Ok(RetireResult::Retired) } else { Err(invalid("native-receive-delivery-changed")) };
        }
        let roots = operation_state(&device, &library, &coordinator, cap)?.roots.ok_or_else(|| invalid("native-receive-delivery-required"))?;
        if roots != request.roots { return Err(invalid("native-receive-delivery-changed")); }
        let (mut lm, mut dm) = settlement_manifests(&library, &device, cap)?;
        if [&lm, &dm].iter().any(|manifest| manifest.delivered.as_ref() != Some(&roots)) {
            return Err(invalid("native-receive-delivery-required"));
        }
        if lm.phase == Phase::Settled && dm.phase == Phase::Settled { return Ok(RetireResult::Retired); }
        if ![&lm, &dm].iter().all(|manifest| history_validated(manifest, &[ValidationPurpose::Seal, ValidationPurpose::Cleanup])) {
            return Err(invalid("native-receive-validation-incomplete"));
        }
        let current_identity = identity_digest(&device, &library)? == cap.identity_digest;
        if (current_identity && log.as_ref().map(|log| stored_page_matches(log, &dm.origin)).transpose()?.unwrap_or(false)) ||
            !retirement_proven(&device, &library, cap, &dm)? {
            return Ok(RetireResult::Pending);
        }
        settle_manifests(&library, &device, &mut lm, &mut dm, &roots, Phase::Settled)?;
        commit_pair(&admission, library, device, true)?;
        Ok(RetireResult::Retired)
    }

    /// One bounded cleanup step for settled history; `true` once the capability is closed.
    pub(crate) fn cleanup_native_receive(&self, cap: &Capability) -> StoreResult<bool> {
        let admission = self.acquire_intent_admission()?;
        admission.require(self.device_store()?.connection())?;
        {
            let device = kernel_guard(self.device_store()?.connection())?;
            let library = kernel_guard(&self.connection)?;
            let mut coordinator = read_coordinator(&device)?.ok_or_else(|| invalid("native-receive-coordinator-missing"))?;
            coordinator.require(cap)?;
            if !coordinator.cleanup.iter().any(|proof| proof.capability == *cap) {
                let (mut lm, mut dm) = settlement_manifests(&library, &device, cap)?;
                let roots = Roots { library: lm.root(), device: dm.root() };
                if [&lm, &dm].iter().any(|manifest| manifest.phase != Phase::Settled || manifest.delivered.as_ref() != Some(&roots)) {
                    return Err(invalid("native-receive-settlement-required"));
                }
                // Deletion is authorized only after both partitions revalidate their complete history.
                if validate_history_step(&library, Partition::Library, &mut lm, ValidationPurpose::Cleanup)? &&
                    validate_history_step(&device, Partition::Device, &mut dm, ValidationPurpose::Cleanup)? {
                    begin_cleanup(&device, &library, &mut coordinator, cap, &roots)?;
                }
                commit_pair(&admission, library, device, false)?;
                return Ok(false);
            }
        }
        cleanup_step_holding(&admission, self.device_store()?.connection(), &self.connection, cap, row_limits().0)
    }
}

#[cfg(test)]
pub(super) mod kernel_test {
    use super::*;
    thread_local! {
        static LIMITS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((MAX_STEP_ROWS, MAX_DECODED_BYTES / 2)) };
        static HOOK: std::cell::RefCell<Option<(u8, bool, Box<dyn FnOnce()>)>> = const { std::cell::RefCell::new(None) };
    }
    pub(super) fn limits() -> (usize, usize) { LIMITS.with(std::cell::Cell::get) }
    pub(in crate::persistent_store) struct Limits((usize, usize));
    impl Limits {
        pub(in crate::persistent_store) fn set(rows: usize, bytes: usize) -> Self {
            assert!((1..=MAX_STEP_ROWS).contains(&rows) && (1..=MAX_DECODED_BYTES / 2).contains(&bytes));
            Self(LIMITS.with(|value| value.replace((rows, bytes))))
        }
    }
    impl Drop for Limits { fn drop(&mut self) { LIMITS.with(|value| value.set(self.0)); } }
    pub(in crate::persistent_store) fn hook(phase: u8, hook: impl FnOnce() + 'static) { HOOK.with(|value| *value.borrow_mut() = Some((phase, true, Box::new(hook)))); }
    pub(in crate::persistent_store) fn inspect(phase: u8, hook: impl FnOnce() + 'static) { HOOK.with(|value| *value.borrow_mut() = Some((phase, false, Box::new(hook)))); }
    fn run(phase: u8) -> StoreResult<()> {
        let hook = HOOK.with(|value| { let mut value = value.borrow_mut();
            if value.as_ref().is_some_and(|(at, _, _)| *at == phase) { value.take() } else { None } });
        if let Some((_, fail, hook)) = hook { hook(); if fail { return Err(invalid("synthetic-kernel-interruption")); } }
        Ok(())
    }
    pub(super) fn before_commit() -> StoreResult<()> { run(3) }
    pub(super) fn after_observe() -> StoreResult<()> { run(1) }
    pub(super) fn after_library() -> StoreResult<()> { run(2) }
    pub(super) fn after_stage() -> StoreResult<()> { run(4) }
    pub(super) fn ticket_before_commit() -> StoreResult<()> { run(5) }
    pub(super) fn ticket_after_library() -> StoreResult<()> { run(6) }
    pub(super) fn settle_after_library() -> StoreResult<()> { run(7) }
}


pub(super) fn device_revision(db: &Connection) -> StoreResult<i64> {
    Ok(db.query_row("SELECT revision FROM device_meta WHERE singleton=1", [], |row| row.get(0))?)
}
pub(super) fn is_history_setting(key: &str) -> bool { key.starts_with(SETTING_PREFIX) }

#[cfg(test)]
mod tests {
    use super::*;
    thread_local! {
        static INTERRUPT_CLEANUP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static INTERRUPT_ADMISSION: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
        static INPUT_LIMITS: std::cell::Cell<(usize, u64)> = const { std::cell::Cell::new((MAX_STEP_ROWS, INPUT_STEP_BYTES)) };
    }
    pub(super) fn interrupt_cleanup_after_library() -> bool { INTERRUPT_CLEANUP.with(|flag| flag.replace(false)) }
    pub(super) fn interrupt_admission(phase: u8) -> bool {
        INTERRUPT_ADMISSION.with(|flag| if flag.get() == phase { flag.set(0); true } else { false })
    }
    pub(super) fn input_limits() -> (usize, u64) { INPUT_LIMITS.with(std::cell::Cell::get) }
    struct InputLimits((usize, u64));
    impl InputLimits {
        fn set(rows: usize, bytes: u64) -> Self {
            assert!((1..=MAX_STEP_ROWS).contains(&rows) && (1..=INPUT_STEP_BYTES).contains(&bytes));
            Self(INPUT_LIMITS.with(|limits| limits.replace((rows, bytes))))
        }
    }
    impl Drop for InputLimits { fn drop(&mut self) { INPUT_LIMITS.with(|limits| limits.set(self.0)); } }
    fn coordinator() -> Coordinator { Coordinator::new() }
    fn candidate(c: &Coordinator, mode: Mode) -> Capability {
        c.candidate(mode, "1".repeat(64), "2".repeat(64)).unwrap()
    }
    fn close_clean(c: &mut Coordinator, cap: &Capability) {
        let root = PartitionRoot { pages: 0.into(), root: EMPTY_ROOT.into(), disposition_pages: 0.into(), disposition_root: EMPTY_ROOT.into() };
        c.cleanup.push(CleanupProof { capability: cap.clone(), roots: Roots { library: root.clone(), device: root }, library_done: true, device_done: true });
        c.close(cap).unwrap();
    }
    #[test]
    fn admission_retry_never_allocates_a_consumed_candidate() {
        let mut c = coordinator();
        let first = candidate(&c, Mode::Stored);
        assert!(matches!(c.admit(&first).unwrap(), Admission::Admitted(_)));
        assert!(matches!(c.admit(&first).unwrap(), Admission::Existing(_)));
        c.sealed(&first).unwrap(); close_clean(&mut c, &first);
        assert!(c.admit(&first).is_err());
        let next = candidate(&c, Mode::Stored);
        assert!(next.sequence.0 > first.sequence.0);
        assert!(matches!(c.admit(&next).unwrap(), Admission::Admitted(_)));
    }
    #[test]
    fn old_server_slot_does_not_accumulate_one_tombstone_per_local_operation() {
        let mut c = coordinator();
        let server = candidate(&c, Mode::Stored);
        c.admit(&server).unwrap(); c.sealed(&server).unwrap();
        for _ in 0..100 {
            let local = candidate(&c, Mode::Drain);
            c.admit(&local).unwrap(); c.sealed(&local).unwrap(); close_clean(&mut c, &local);
            assert_eq!(c.closed.len(), 1);
        }
        close_clean(&mut c, &server);
        assert_eq!(c.closed, vec![ClosedRange { first: 1, last: 101 }]);
    }
    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_kv(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE device_settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);").unwrap();
        db
    }
    fn manifest() -> Manifest {
        let c = coordinator();
        let mut value = empty_manifest(candidate(&c, Mode::Stored), Origin {
            request_digest: "3".repeat(64), request_bytes: 1, source_rowid: Some(1), body_digest: "4".repeat(64),
            body_bytes: 0, authority: 0.into(), progress: None,
        }, Observation { library_revision: 1, device_revision: 1,
            plugins: DevicePluginFence { instance_id: uuid::Uuid::new_v4().to_string(), revision: 0.into() } },
            0, None, Foreign::default());
        value.phase = Phase::Producing;
        value
    }
    fn effect(key: &str, affected: bool, plugin_storage_changed: bool) -> Effect {
        Effect { key: UnitKey::new(&["root", key]).unwrap().as_str().to_owned(), affected, plugin_storage_changed }
    }
    #[test]
    fn incomplete_sequence_custody_and_reused_candidate_are_rejected() {
        let mut c = coordinator();
        let first = candidate(&c, Mode::Stored);
        let loser = candidate(&c, Mode::Stored);
        c.admit(&first).unwrap();
        assert!(c.admit(&loser).is_err());
        let mut corrupted = c.clone();
        corrupted.server_history = None;
        corrupted.active_producer = None;
        assert!(corrupted.validate().is_err());
        c.sealed(&first).unwrap();
        let local = candidate(&c, Mode::Drain);
        c.admit(&local).unwrap();
        let mut corrupted = c.clone();
        corrupted.local_history = Some(first);
        assert!(corrupted.validate().is_err());
    }
    #[test]
    fn exact_effect_history_deduplicates_but_retains_a_later_plugin_change() {
        let mut db = database();
        let tx = db.transaction().unwrap();
        let mut m = manifest();
        append_effects(&tx, Partition::Library, &mut m, &[effect("a", true, false), effect("a", true, false)]).unwrap();
        append_effects(&tx, Partition::Library, &mut m, &[effect("a", true, true)]).unwrap();
        append_effects(&tx, Partition::Library, &mut m, &[effect("a", true, true)]).unwrap();
        tx.commit().unwrap();
        assert_eq!((m.effect_pages, m.affected_count, m.plugin_changed_count), (2, 1, 1));
        validate_effect_history(&db, Partition::Library, &m).unwrap();
        let first = read_effect_page(&db, Partition::Library, &m, 0, EMPTY_ROOT).unwrap();
        assert_eq!(first.effects.len(), 1);
        let second = read_effect_page(&db, Partition::Library, &m, 1, &digest(&first).unwrap()).unwrap();
        assert!(!second.effects[0].affected);
        assert!(second.effects[0].plugin_storage_changed);
        {
            let tx = db.transaction().unwrap();
            let phantom = effect("phantom", true, false);
            write(&tx, Partition::Library, &index_key(&m.capability, &phantom.key).unwrap(),
                &EffectIndex { key: phantom.key.clone(), affected_at: Some(0), plugin_changed_at: None }).unwrap();
            assert!(append_effects(&tx, Partition::Library, &mut m, &[phantom]).is_err());
            tx.rollback().unwrap();
        }
        m.phase = Phase::Finalizing;
        {
            let tx = db.transaction().unwrap();
            assert!(!validate_history_step(&tx, Partition::Library, &mut m, ValidationPurpose::Seal).unwrap());
            tx.commit().unwrap();
        }
        let mut m = read_manifest(&db, Partition::Library, &m.capability).unwrap().unwrap();
        assert_eq!(m.validation.as_ref().unwrap().effect_ordinal, 1);
        assert!(m.validated.is_none());
        {
            let tx = db.transaction().unwrap();
            tx.execute("DELETE FROM app_kv WHERE key=?1", [effect_key(&m.capability, 1).unwrap()]).unwrap();
            assert!(validate_history_step(&tx, Partition::Library, &mut m, ValidationPurpose::Seal).is_err());
            tx.rollback().unwrap();
        }
        {
            let tx = db.transaction().unwrap();
            assert!(validate_history_step(&tx, Partition::Library, &mut m, ValidationPurpose::Seal).unwrap());
            tx.commit().unwrap();
        }
        db.execute("DELETE FROM app_kv WHERE key=?1", [effect_key(&m.capability, 0).unwrap()]).unwrap();
        assert!(validate_effect_history(&db, Partition::Library, &m).is_err());
    }
    #[test]
    fn maximum_key_fits_a_page_and_uncommitted_history_cannot_escape() {
        let mut db = database();
        let mut m = manifest();
        let overhead = UnitKey::new(&["root", ""]).unwrap().as_str().len();
        let key = UnitKey::new(&["root", &"x".repeat(MAX_UNIT_KEY_BYTES - overhead)]).unwrap();
        let effect = Effect { key: key.as_str().to_owned(), affected: true, plugin_storage_changed: false };
        assert_eq!(effect.key.len(), MAX_UNIT_KEY_BYTES);
        assert!(append_effects(&db, Partition::Library, &mut m, &[effect.clone()]).is_err());
        {
            let tx = db.transaction().unwrap();
            append_effects(&tx, Partition::Library, &mut m, &[effect]).unwrap();
            tx.rollback().unwrap();
        }
        assert!(read_manifest(&db, Partition::Library, &m.capability).unwrap().is_none());
        assert!(validate_effect_history(&db, Partition::Library, &m).is_err());
    }
    #[test]
    fn dispositions_are_final_unique_status_and_do_not_replace_historical_effects() {
        let mut db = database();
        let tx = db.transaction().unwrap();
        let mut m = manifest();
        append_effects(&tx, Partition::Device, &mut m, &[effect("a", true, false)]).unwrap();
        m.phase = Phase::Finalizing;
        let key = UnitKey::new(&["root", "b"]).unwrap().as_str().to_owned();
        append_dispositions(&tx, Partition::Device, &mut m, &[
            DispositionEntry { key: key.clone(), disposition: Disposition::Deferred },
            DispositionEntry { key: key.clone(), disposition: Disposition::Deferred },
        ]).unwrap();
        append_dispositions(&tx, Partition::Device, &mut m, &[DispositionEntry { key: key.clone(), disposition: Disposition::Deferred }]).unwrap();
        assert_eq!(m.disposition_pages, 1);
        tx.execute_batch("SAVEPOINT phantom_disposition").unwrap();
        let phantom = UnitKey::new(&["root", "phantom"]).unwrap().as_str().to_owned();
        write(&tx, Partition::Device, &disposition_index_key(&m.capability, &phantom).unwrap(),
            &DispositionIndex { key: phantom.clone(), page: 0, disposition: Disposition::Held }).unwrap();
        assert!(append_dispositions(&tx, Partition::Device, &mut m, &[DispositionEntry { key: phantom, disposition: Disposition::Held }]).is_err());
        tx.execute_batch("ROLLBACK TO phantom_disposition; RELEASE phantom_disposition").unwrap();
        assert!(matches!(append_dispositions(&tx, Partition::Device, &mut m, &[DispositionEntry { key, disposition: Disposition::Held }]),
            Err(StoreError::Validation { message }) if message == "native-receive-disposition-changed"));
        tx.commit().unwrap();
        assert_eq!((m.affected_count, m.held_count, m.deferred_count), (1, 0, 1));
        validate_effect_history(&db, Partition::Device, &m).unwrap();
        validate_disposition_history(&db, Partition::Device, &m).unwrap();
        let page = read_disposition_page(&db, Partition::Device, &m, 0, EMPTY_ROOT).unwrap();
        assert_eq!(page.entries[0].disposition, Disposition::Deferred);
    }
    #[test]
    fn cleanup_requires_delivery_and_keeps_capability_custody_across_partition_loss() {
        let mut library = database();
        let mut device = database();
        let mut c = coordinator();
        let cap = candidate(&c, Mode::Stored);
        c.admit(&cap).unwrap(); c.sealed(&cap).unwrap();
        let mut lm = manifest(); lm.capability = cap.clone();
        let mut dm = lm.clone();
        {
            let ltx = library.transaction().unwrap();
            let dtx = device.transaction().unwrap();
            append_effects(&ltx, Partition::Library, &mut lm, &[effect("a", true, false)]).unwrap();
            append_effects(&dtx, Partition::Device, &mut dm, &[effect("b", true, true)]).unwrap();
            write_coordinator(&dtx, &c).unwrap();
            ltx.commit().unwrap(); dtx.commit().unwrap();
        }
        let roots = Roots { library: lm.root(), device: dm.root() };
        {
            let ltx = library.transaction().unwrap();
            let dtx = device.transaction().unwrap();
            assert!(begin_cleanup(&dtx, &ltx, &mut c, &cap, &roots).is_err());
        }
        for m in [&mut lm, &mut dm] {
            m.phase = Phase::Settled; m.outcome = Some(Outcome::Processed); m.delivered = Some(roots.clone());
        }
        for (db, partition, m) in [(&mut library, Partition::Library, &mut lm), (&mut device, Partition::Device, &mut dm)] {
            let tx = db.transaction().unwrap();
            assert!(validate_history_step(&tx, partition, m, ValidationPurpose::Cleanup).unwrap());
            tx.commit().unwrap();
        }
        {
            let ltx = library.transaction().unwrap();
            let dtx = device.transaction().unwrap();
            write_manifest(&ltx, Partition::Library, &lm).unwrap();
            write_manifest(&dtx, Partition::Device, &dm).unwrap();
            begin_cleanup(&dtx, &ltx, &mut c, &cap, &roots).unwrap();
            ltx.commit().unwrap();
            dtx.rollback().unwrap();
        }
        assert!(cleanup_step(&mut device, &mut library, &cap, 1).is_err());
        assert!(read_manifest(&library, Partition::Library, &cap).unwrap().is_some());
        c = read_coordinator(&device).unwrap().unwrap();
        {
            let ltx = library.transaction().unwrap();
            let dtx = device.transaction().unwrap();
            write_manifest(&ltx, Partition::Library, &lm).unwrap();
            write_manifest(&dtx, Partition::Device, &dm).unwrap();
            begin_cleanup(&dtx, &ltx, &mut c, &cap, &roots).unwrap();
            ltx.commit().unwrap(); dtx.commit().unwrap();
        }
        INTERRUPT_CLEANUP.with(|flag| flag.set(true));
        assert!(cleanup_step(&mut device, &mut library, &cap, MAX_STEP_ROWS).is_err());
        assert!(read_manifest(&library, Partition::Library, &cap).unwrap().is_none());
        let recovered = read_coordinator(&device).unwrap().unwrap();
        assert_eq!(recovered.cleanup.len(), 1);
        assert!(!recovered.cleanup[0].library_done);
        assert!(recovered.require(&cap).is_ok());
        loop {
            let complete = cleanup_step(&mut device, &mut library, &cap, 1).unwrap();
            let reopened = read_coordinator(&device).unwrap().unwrap();
            assert_eq!(reopened.require(&cap).is_err(), complete);
            if complete { break; }
        }
        let c = read_coordinator(&device).unwrap().unwrap();
        assert!(c.require(&cap).is_err());
        assert_eq!(c.closed, vec![ClosedRange { first: 1, last: 1 }]);
    }
    #[test]
    fn reopened_store_keeps_namespace_and_prepare_never_allocates() {
        let directory = tempfile::tempdir().unwrap();
        let store = PersistentStore::open(directory.path()).unwrap();
        let source = Source::Drain { binding_authority: store.lww_binding_authority().unwrap() };
        let before = read_coordinator(store.device_store().unwrap().connection()).unwrap().unwrap();
        let first = store.prepare_native_receive(&source).unwrap();
        let second = store.prepare_native_receive(&source).unwrap();
        assert_eq!(first.incarnation, second.incarnation);
        assert_eq!(first.sequence, second.sequence);
        assert_ne!(first.nonce, second.nonce);
        assert_eq!(before.next_sequence, read_coordinator(store.device_store().unwrap().connection()).unwrap().unwrap().next_sequence);
        drop(store);
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let next = store.prepare_native_receive(&source).unwrap();
        assert_eq!(next.incarnation, first.incarnation);
        assert_eq!(next.sequence, first.sequence);
        let lease = store.acquire_revision(store.revision().unwrap()).unwrap().lease;
        let baseline = store.read_native_receive_evidence(&lease).unwrap();
        let request = BeginRequest { candidate: first.clone(), source: source.clone(), native_baseline: Some(baseline) };
        let admitted = match store.begin_local_native_receive(&request).unwrap() {
            BeginResult::Admitted { state } => state, other => panic!("unexpected admission {other:?}"),
        };
        assert_eq!(admitted.phase, Phase::Producing);
        assert_eq!(admitted.foreign, Foreign::default());
        assert!(admitted.roots.is_none());
        let repeated = match store.begin_local_native_receive(&request).unwrap() {
            BeginResult::Existing { state } => state, other => panic!("unexpected retry {other:?}"),
        };
        assert_eq!(repeated.capability, first);
        assert_eq!(repeated.progress_token, admitted.progress_token);
        let competing = BeginRequest { candidate: next, source: source.clone(), native_baseline: None };
        assert!(matches!(store.begin_local_native_receive(&competing),
            Err(StoreError::Validation { message }) if message == "native-receive-candidate-consumed"));
        let waiting = BeginRequest { candidate: store.prepare_native_receive(&source).unwrap(), source, native_baseline: None };
        match store.begin_local_native_receive(&waiting).unwrap() {
            BeginResult::Recovery { state } => assert_eq!(state.capability, first), other => panic!("unexpected occupied slot {other:?}"),
        }
        store.release_revision(&lease).unwrap();
        let mut library_manifest = read_manifest(&store.connection, Partition::Library, &first).unwrap().unwrap();
        {
            let tx = Transaction::new_unchecked(&store.connection, TransactionBehavior::Immediate).unwrap();
            library_manifest.phase = Phase::Admitting;
            write_manifest(&tx, Partition::Library, &library_manifest).unwrap();
            tx.commit().unwrap();
        }
        assert!(matches!(store.read_native_receive_state(&first),
            Err(StoreError::Validation { message }) if message == "native-receive-partition-phase-changed"));
        {
            let tx = Transaction::new_unchecked(&store.connection, TransactionBehavior::Immediate).unwrap();
            library_manifest.phase = Phase::Producing;
            write_manifest(&tx, Partition::Library, &library_manifest).unwrap();
            tx.commit().unwrap();
            let tx = Transaction::new_unchecked(store.device_store().unwrap().connection(), TransactionBehavior::Immediate).unwrap();
            let mut coordinator = read_coordinator(&tx).unwrap().unwrap();
            coordinator.active_producer = None;
            write_coordinator(&tx, &coordinator).unwrap();
            tx.commit().unwrap();
        }
        assert!(matches!(store.read_native_receive_state(&first),
            Err(StoreError::Validation { message }) if message == "native-receive-producer-changed"));
    }
    #[test]
    fn local_admission_recovers_partition_boundaries_without_recreating_producing_history() {
        for (phase, changed_identity) in [(1, false), (2, false), (1, true)] {
            let directory = tempfile::tempdir().unwrap();
            let mut store = PersistentStore::open(directory.path()).unwrap();
            let source = Source::Drain { binding_authority: store.lww_binding_authority().unwrap() };
            let before = store.revision().unwrap();
            let lease = store.acquire_revision(before).unwrap().lease;
            let baseline = store.read_native_receive_evidence(&lease).unwrap();
            store.release_revision(&lease).unwrap();
            let request = BeginRequest { candidate: store.prepare_native_receive(&source).unwrap(), source, native_baseline: Some(baseline) };
            INTERRUPT_ADMISSION.with(|flag| flag.set(phase));
            assert!(matches!(store.begin_local_native_receive(&request),
                Err(StoreError::Validation { message }) if message == "synthetic-admission-interruption"));
            let pending = read_manifest(store.device_store().unwrap().connection(), Partition::Device, &request.candidate).unwrap().unwrap();
            assert_eq!(pending.phase, Phase::Admitting);
            assert_eq!(read_manifest(&store.connection, Partition::Library, &request.candidate).unwrap().is_some(), phase == 2);
            drop(store);
            let mut store = PersistentStore::open(directory.path()).unwrap();
            if changed_identity {
                store.connection.execute("UPDATE library_sync_selection SET selection_epoch='synthetic-replaced-selection' WHERE singleton=1", []).unwrap();
                assert!(matches!(store.begin_local_native_receive(&request),
                    Err(StoreError::Validation { message }) if message == "native-receive-identity-changed"));
                assert_eq!(store.read_native_receive_state(&request.candidate).unwrap().phase, Phase::Admitting);
                assert!(store.read_native_receive_state(&request.candidate).unwrap().foreign.scope);
                assert!(read_manifest(&store.connection, Partition::Library, &request.candidate).unwrap().is_none());
            } else {
                let state = match store.begin_local_native_receive(&request).unwrap() {
                    BeginResult::Existing { state } => state, other => panic!("unexpected resumed admission {other:?}"),
                };
                assert_eq!(state.capability, request.candidate);
                assert_eq!(state.phase, Phase::Producing);
                assert_eq!(state.revision, before);
                assert_eq!(state.roots, None);
                let key = format!("{}manifest", request.candidate.key().unwrap());
                store.connection.execute("DELETE FROM app_kv WHERE key=?1", [key]).unwrap();
                assert!(matches!(store.begin_local_native_receive(&request),
                    Err(StoreError::Validation { message }) if message == "native-receive-library-manifest-missing"));
                assert!(read_manifest(&store.connection, Partition::Library, &request.candidate).unwrap().is_none());
            }
            let coordinator = read_coordinator(store.device_store().unwrap().connection()).unwrap().unwrap();
            coordinator.require(&request.candidate).unwrap();
            assert_eq!(coordinator.next_sequence, 2);
            assert_eq!(store.revision().unwrap(), before);
        }
    }


    #[test]
    fn local_recovery_does_not_stage_a_stored_admission_without_transport_guard() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let staged = staged_input(&mut store, &[input_change(0)], "synthetic-server-owned-admission".into());
        let Source::Staged { header } = staged.source else { unreachable!() };
        let stored_source = Source::Stored { header };
        let cap = store.prepare_native_receive(&stored_source).unwrap();
        {
            let device = Transaction::new_unchecked(store.device_store().unwrap().connection(), TransactionBehavior::Immediate).unwrap();
            let library = Transaction::new_unchecked(&store.connection, TransactionBehavior::Immediate).unwrap();
            let mut coordinator = read_coordinator(&device).unwrap().unwrap();
            assert!(matches!(coordinator.admit(&cap).unwrap(), Admission::Admitted(_)));
            let origin = source_origin(&device, &stored_source, false).unwrap();
            let manifest = empty_manifest(cap.clone(), origin, current_observation(&device, &library).unwrap(), 0, None, Foreign::default());
            write_manifest(&device, Partition::Device, &manifest).unwrap();
            write_coordinator(&device, &coordinator).unwrap();
            library.commit().unwrap();
            device.commit().unwrap();
        }
        let before = store.read_native_receive_state(&cap).unwrap().progress_token;
        let source = Source::Drain { binding_authority: store.lww_binding_authority().unwrap() };
        let request = BeginRequest { candidate: store.prepare_native_receive(&source).unwrap(), source, native_baseline: None };
        let state = match store.begin_local_native_receive(&request).unwrap() {
            BeginResult::Recovery { state } => state, other => panic!("unexpected stored recovery {other:?}"),
        };
        assert_eq!(state.capability, cap);
        assert_eq!(state.phase, Phase::Admitting);
        assert_eq!(state.progress_token, before);
        assert!(read_manifest(&store.connection, Partition::Library, &cap).unwrap().is_none());
        assert_eq!(store.lww_receive_row_counts("synthetic-server-owned-admission").unwrap(), (0, 0));
        assert_eq!(read_coordinator(store.device_store().unwrap().connection()).unwrap().unwrap().next_sequence, 2);
    }

    fn input_change(index: usize) -> lww::Change {
        use risunest_sync_wire::{stamp::Stamp, unit::UnitValue};
        let key = if index % 2 == 0 { UnitKey::new(&["root", &format!("synthetic-{index}")]).unwrap() }
            else { UnitKey::new(&["plugin-local", "synthetic-input", "string", &format!("key-{index}")]).unwrap() };
        lww::Change { key, stamp: Stamp { physical_ms: 7.into(), logical: 0,
            writer_id: "00000000-0000-4000-8000-000000000001".into() },
            value: UnitValue::inline(&serde_json::to_vec(&format!("value-{index}")).unwrap()).unwrap() }
    }
    fn staged_input(store: &mut PersistentStore, changes: &[lww::Change], request_id: String) -> BeginRequest {
        let header = lww::Header { binding_authority: store.lww_binding_authority().unwrap(), request_id };
        store.lww_stage_receive(&lww::StageReceive { header: header.clone(), changes: changes.to_vec(),
            progress: lww::Progress { kind: "server".into(), cursor: 0.into(), writer_id: None },
            admitted_time_upper_ms: 7.into() }).unwrap();
        for db in [&store.connection, store.device_store().unwrap().connection()] {
            db.execute("DELETE FROM lww_receive_rows WHERE request_id=?1", [&header.request_id]).unwrap();
        }
        let source = Source::Staged { header };
        BeginRequest { candidate: store.prepare_native_receive(&source).unwrap(), source, native_baseline: None }
    }
    fn input_state(result: BeginResult) -> OperationState {
        match result { BeginResult::Admitted { state } | BeginResult::Existing { state } => state,
            other => panic!("unexpected input admission {other:?}") }
    }
    #[test]
    fn bounded_input_restarts_each_partition_boundary_without_skipping_or_resetting_rows() {
        for interruption in [2, 3] {
            let _limits = InputLimits::set(2, INPUT_STEP_BYTES);
            let directory = tempfile::tempdir().unwrap();
            let mut store = PersistentStore::open(directory.path()).unwrap();
            let mut changes = (0..5).map(input_change).collect::<Vec<_>>();
            let object_body = serde_json::to_vec(&format!("{}漢字", "x".repeat(
                super::super::device_store::plugin_values::text_value::PART_BYTES + 1))).unwrap();
            let object_hash = risunest_sync_wire::hash(&object_body);
            store.lww_put_object(&object_hash, &object_body).unwrap();
            changes[1].value = risunest_sync_wire::unit::UnitValue::object(
                risunest_sync_wire::descriptor::RecordDescriptor { object_hash: object_hash.clone(),
                    dependency_root: None, relation_root: None, dependencies: Vec::new(), relations: Vec::new(), scopes: Vec::new() }).unwrap();
            let request = staged_input(&mut store, &changes, "synthetic-input-restart".into());
            let device = store.device_store().unwrap().connection();
            device.execute("DELETE FROM message_page_verified_objects WHERE hash=?1", [&object_hash]).unwrap();
            device.execute("DELETE FROM message_page_objects WHERE hash=?1", [&object_hash]).unwrap();
            assert!(!super::super::message_pages::retained_object_present(device, &object_hash).unwrap());
            INTERRUPT_ADMISSION.with(|flag| flag.set(interruption));
            assert!(matches!(store.begin_local_native_receive(&request),
                Err(StoreError::Validation { message }) if message == "synthetic-admission-interruption"));
            let library = read_manifest(&store.connection, Partition::Library, &request.candidate).unwrap().unwrap();
            let device = read_manifest(store.device_store().unwrap().connection(), Partition::Device, &request.candidate).unwrap().unwrap();
            assert_eq!(library.input.as_ref().unwrap().cursor.ordinal, 2);
            assert_eq!(device.input.as_ref().unwrap().cursor.ordinal, if interruption == 2 { 0 } else { 2 });
            assert_eq!(super::super::message_pages::retained_object_present(store.device_store().unwrap().connection(), &object_hash).unwrap(), interruption == 3);
            store.connection.execute("UPDATE lww_receive_rows SET status='held' WHERE key=?1", [changes[0].key.as_str()]).unwrap();
            drop(store);
            let mut store = PersistentStore::open(directory.path()).unwrap();
            for _ in 0..4 {
                if input_state(store.begin_local_native_receive(&request).unwrap()).phase == Phase::Producing { break; }
            }
            let state = store.read_native_receive_state(&request.candidate).unwrap();
            assert_eq!(state.phase, Phase::Producing);
            assert_eq!(state.revision, 0);
            let library = read_manifest(&store.connection, Partition::Library, &request.candidate).unwrap().unwrap();
            let device = read_manifest(store.device_store().unwrap().connection(), Partition::Device, &request.candidate).unwrap().unwrap();
            assert_eq!(library.input, device.input);
            assert_eq!(device.input.as_ref().unwrap().cursor.ordinal, 5);
            assert_eq!(device.input.as_ref().unwrap().completion, Some(InputCompletion::JsonEnd));
            for change in &changes {
                let db = if lww::is_device_unit(&change.key) { store.device_store().unwrap().connection() } else { &store.connection };
                let (stamp, value, status): (String, String, String) = db.query_row(
                    "SELECT stamp,value,status FROM lww_receive_rows WHERE key=?1", [change.key.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
                assert_eq!(serde_json::from_str::<risunest_sync_wire::stamp::Stamp>(&stamp).unwrap(), change.stamp);
                assert_eq!(serde_json::from_str::<risunest_sync_wire::unit::UnitValue>(&value).unwrap(), change.value);
                assert_eq!(status, if change.key == changes[0].key { "held" } else { "staged" });
            }
            let counts = store.lww_receive_row_counts("synthetic-input-restart").unwrap();
            assert_eq!(counts, (3, 2));
            let device_object = super::super::device_store::plugin_values::text_value::inspect_object(
                store.device_store().unwrap().connection(), &object_hash).unwrap();
            assert_eq!(device_object.hash, object_hash);
            assert_eq!(device_object.encoded_length, object_body.len() as u64);
        }
    }
    #[test]
    fn input_reader_preserves_consumed_offsets_across_chunks_escapes_and_empty_tail() {
        let _limits = InputLimits::set(MAX_STEP_ROWS, 64);
        for empty in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let mut store = PersistentStore::open(directory.path()).unwrap();
            let mut changes = if empty { Vec::new() } else { (0..3).map(input_change).collect::<Vec<_>>() };
            if !empty {
                changes[0].key = UnitKey::new(&["root", &format!("{}漢字\"\\", "x".repeat(24_000))]).unwrap();
                changes[0].value = risunest_sync_wire::unit::UnitValue::inline(
                    &serde_json::to_vec(&"escaped \" \\ newline\n 한글".repeat(4_000)).unwrap()).unwrap();
            }
            let request = staged_input(&mut store, &changes, format!("{}漢字\"\\", "h".repeat(INPUT_BUFFER_BYTES - 3)));
            let mut state = input_state(store.begin_local_native_receive(&request).unwrap());
            if !empty {
                assert_eq!(state.phase, Phase::Admitting);
                let first = read_manifest(store.device_store().unwrap().connection(), Partition::Device, &request.candidate).unwrap().unwrap();
                let progress = first.input.as_ref().unwrap();
                assert_eq!(progress.cursor.ordinal, 1);
                assert!(progress.cursor.offset > INPUT_BUFFER_BYTES as u64);
                assert_eq!(progress.completion, None);
            }
            for _ in 0..3 {
                if state.phase == Phase::Producing { break; }
                state = input_state(store.begin_local_native_receive(&request).unwrap());
            }
            assert_eq!(state.phase, Phase::Producing);
            let manifest = read_manifest(store.device_store().unwrap().connection(), Partition::Device, &request.candidate).unwrap().unwrap();
            let input = manifest.input.unwrap();
            assert_eq!(input.cursor.offset, manifest.origin.body_bytes);
            assert_eq!(input.cursor.ordinal, changes.len() as u64);
            assert_eq!(input.completion, Some(InputCompletion::JsonEnd));
            for change in changes {
                let db = if lww::is_device_unit(&change.key) { store.device_store().unwrap().connection() } else { &store.connection };
                let value: String = db.query_row("SELECT value FROM lww_receive_rows WHERE key=?1", [change.key.as_str()], |row| row.get(0)).unwrap();
                assert_eq!(serde_json::from_str::<risunest_sync_wire::unit::UnitValue>(&value).unwrap(), change.value);
            }
        }
    }

    #[test]
    fn input_parser_uses_consumed_offsets_and_rejects_unparsed_trailing_bytes() {
        for padding in [INPUT_BUFFER_BYTES - 1, INPUT_BUFFER_BYTES - 4, INPUT_BUFFER_BYTES - 7] {
            let directory = tempfile::tempdir().unwrap();
            let store = PersistentStore::open(directory.path()).unwrap();
            let body = format!("{}\"漢\\u5b57\\\\\\\"\",42", " ".repeat(padding));
            let device = store.device_store().unwrap().connection();
            device.execute("INSERT INTO lww_receive VALUES('synthetic-reader','0','synthetic',?1,0,0,'server','','0')", [&body]).unwrap();
            let rowid = device.last_insert_rowid();
            let mut reader = SourceReader::new(device, rowid, body.len() as u64, 0).unwrap();
            assert_eq!(reader.value::<String>().unwrap(), "漢字\\\"");
            let comma = body.rfind(',').unwrap() as u64;
            assert_eq!(reader.position, comma);
            assert_eq!(reader.byte().unwrap(), Some(b','));
            assert_eq!(reader.value::<u64>().unwrap(), 42);
            assert_eq!(reader.position, body.len() as u64);
            assert_eq!(reader.byte().unwrap(), None);
        }
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let mut request = staged_input(&mut store, &[input_change(0)], "synthetic-invalid-input-tail".into());
        let device = store.device_store().unwrap().connection();
        let body: String = device.query_row("SELECT body FROM lww_receive", [], |row| row.get(0)).unwrap();
        let changed = format!("{body} false");
        device.execute("UPDATE lww_receive SET body=?1,digest=?2", params![changed, risunest_sync_wire::hash(changed.as_bytes())]).unwrap();
        request.candidate = store.prepare_native_receive(&request.source).unwrap();
        assert!(matches!(store.begin_local_native_receive(&request),
            Err(StoreError::Validation { message }) if message == "native-receive-input-json"));
        assert_eq!(store.lww_receive_row_counts("synthetic-invalid-input-tail").unwrap(), (0, 0));
        let manifest = read_manifest(store.device_store().unwrap().connection(), Partition::Device, &request.candidate).unwrap().unwrap();
        assert_eq!(manifest.input, Some(InputProgress::default()));
        assert_eq!(manifest.phase, Phase::Admitting);
    }

    #[test]
    fn ahead_input_requires_exact_interval_and_rows_while_finished_old_input_never_restages() {
        for damage in ["offset", "missing", "changed", "finished", "already-finished"] {
            let _limits = InputLimits::set(2, INPUT_STEP_BYTES);
            let directory = tempfile::tempdir().unwrap();
            let mut store = PersistentStore::open(directory.path()).unwrap();
            let mut changes = (0..4).map(input_change).collect::<Vec<_>>();
            changes[0].key = UnitKey::new(&["root", "language"]).unwrap();
            changes[2].key = UnitKey::new(&["root", "additionalPrompt"]).unwrap();
            let request = staged_input(&mut store, &changes, "synthetic-ahead-input".into());
            let Source::Staged { header } = &request.source else { unreachable!() };
            let apply = lww::ApplyReceive { header: header.clone(), generating: Vec::new() };
            let rows = |db: &Connection| {
                let mut query = db.prepare("SELECT key,stamp,value,status FROM lww_receive_rows WHERE request_id=?1 ORDER BY key").unwrap();
                let values = query.query_map([&header.request_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?, row.get::<_, String>(3)?))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
                values
            };
            let units = |store: &PersistentStore| changes.iter().map(|change| {
                let db = if lww::is_device_unit(&change.key) { store.device_store().unwrap().connection() } else { &store.connection };
                lww::read_unit(db, &change.key).unwrap()
            }).collect::<Vec<_>>();
            let assert_values = |store: &PersistentStore| {
                let expected = changes.iter().map(|change| Some((change.stamp.clone(), change.value.clone()))).collect::<Vec<_>>();
                assert_eq!(serde_json::to_value(units(store)).unwrap(), serde_json::to_value(expected).unwrap());
                let root = store.read_root(None).unwrap().value;
                assert_eq!(root["language"], serde_json::json!("value-0"));
                assert_eq!(root["additionalPrompt"], serde_json::json!("value-2"));
            };
            let snapshot = |store: &PersistentStore| {
                let device = store.device_store().unwrap().connection();
                let source: (String, bool, bool) = device.query_row(
                    "SELECT body,applied,finished FROM lww_receive WHERE request_id=?1", [&header.request_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
                serde_json::json!({
                    "coordinator": read_coordinator(device).unwrap(),
                    "library": read_manifest(&store.connection, Partition::Library, &request.candidate).unwrap(),
                    "device": read_manifest(device, Partition::Device, &request.candidate).unwrap(),
                    "observation": current_observation(device, &store.connection).unwrap(),
                    "source": source, "rows": [rows(&store.connection), rows(device)],
                    "root": store.read_root(None).unwrap().value, "units": units(store),
                })
            };
            let retained_done = if damage == "finished" {
                store.lww_apply_receive(&apply).unwrap();
                let status: (bool, bool) = store.device_store().unwrap().connection().query_row(
                    "SELECT applied,finished FROM lww_receive WHERE request_id=?1", [&header.request_id],
                    |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
                assert_eq!(status, (true, false));
                assert_values(&store);
                for (device, db) in [(false, &store.connection), (true, store.device_store().unwrap().connection())] {
                    let mut expected = changes.iter().filter(|change| lww::is_device_unit(&change.key) == device)
                        .map(|change| (change.key.as_str().to_owned(), serde_json::to_string(&change.stamp).unwrap(),
                            serde_json::to_string(&change.value).unwrap(), "done".to_owned())).collect::<Vec<_>>();
                    expected.sort();
                    assert_eq!(rows(db), expected);
                }
                Some((rows(&store.connection), rows(store.device_store().unwrap().connection())))
            } else { None };
            if damage != "already-finished" {
                INTERRUPT_ADMISSION.with(|flag| flag.set(2));
                assert!(store.begin_local_native_receive(&request).is_err());
            }
            if damage == "offset" {
                let tx = store.connection.unchecked_transaction().unwrap();
                let mut manifest = read_manifest(&tx, Partition::Library, &request.candidate).unwrap().unwrap();
                manifest.input.as_mut().unwrap().last_batch.as_mut().unwrap().start.offset += 1;
                write_manifest(&tx, Partition::Library, &manifest).unwrap();
                tx.commit().unwrap();
            } else if damage == "missing" {
                store.connection.execute("DELETE FROM lww_receive_rows WHERE key=?1", [changes[0].key.as_str()]).unwrap();
            } else if damage == "changed" {
                store.connection.execute("UPDATE lww_receive_rows SET value='{}' WHERE key=?1", [changes[0].key.as_str()]).unwrap();
            } else {
                if damage == "finished" {
                    let device = store.device_store().unwrap().connection();
                    let device_manifest = read_manifest(device, Partition::Device, &request.candidate).unwrap().unwrap();
                    let library_manifest = read_manifest(&store.connection, Partition::Library, &request.candidate).unwrap().unwrap();
                    assert_eq!(device_manifest.input, Some(InputProgress::default()));
                    let input = library_manifest.input.as_ref().unwrap();
                    let ahead = input.last_batch.as_ref().unwrap();
                    assert_eq!(ahead.start, InputCursor::default());
                    assert_eq!(ahead.end, input.cursor);
                    assert_eq!(ahead.end.ordinal, 2);
                    assert!(ahead.end.offset > ahead.start.offset);
                    assert!(!ahead.ended);
                    assert_eq!(input.completion, None);
                    assert_eq!(ahead.source_binding, digest(&(&request.candidate, &library_manifest.origin)).unwrap());
                    assert_eq!(Some((rows(&store.connection), rows(device))), retained_done);
                    let before_busy = snapshot(&store);
                    assert!(matches!(store.lww_apply_receive(&apply), Err(StoreError::CommitBusy)));
                    assert_eq!(snapshot(&store), before_busy);
                } else {
                    store.lww_apply_receive(&apply).unwrap();
                }
                store.lww_finish_receive(header).unwrap();
                let status: (bool, bool) = store.device_store().unwrap().connection().query_row(
                    "SELECT applied,finished FROM lww_receive WHERE request_id=?1", [&header.request_id],
                    |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
                assert_eq!(status, (true, true));
                assert_values(&store);
                assert_eq!(store.lww_receive_row_counts(&header.request_id).unwrap(), (0, 0));
            }
            let before = store.lww_receive_row_counts("synthetic-ahead-input").unwrap();
            let result = store.begin_local_native_receive(&request);
            if matches!(damage, "finished" | "already-finished") {
                let state = input_state(result.unwrap());
                assert_eq!(state.phase, Phase::Producing);
                assert!(state.foreign.library && state.foreign.device && state.foreign.plugins);
                let manifest = read_manifest(store.device_store().unwrap().connection(), Partition::Device, &request.candidate).unwrap().unwrap();
                assert_eq!(manifest.input.unwrap().completion, Some(InputCompletion::FinishedProof));
                assert_values(&store);
            } else {
                let expected = match damage { "offset" => "native-receive-input-batch-changed",
                    "missing" => "native-receive-input-row-missing", _ => "native-receive-input-row-changed" };
                assert!(matches!(result, Err(StoreError::Validation { message }) if message == expected), "{damage}");
            }
            assert_eq!(store.lww_receive_row_counts("synthetic-ahead-input").unwrap(), before);
        }
    }
    #[test]
    fn input_step_refuses_changed_identity_and_even_quarantined_pending_intents() {
        for changed_identity in [false, true] {
            let _limits = InputLimits::set(1, INPUT_STEP_BYTES);
            let directory = tempfile::tempdir().unwrap();
            let mut store = PersistentStore::open(directory.path()).unwrap();
            let changes = (0..3).map(input_change).collect::<Vec<_>>();
            let request = staged_input(&mut store, &changes, "synthetic-input-admission-fence".into());
            assert_eq!(input_state(store.begin_local_native_receive(&request).unwrap()).phase, Phase::Admitting);
            let before = store.lww_receive_row_counts("synthetic-input-admission-fence").unwrap();
            let before_input = read_manifest(store.device_store().unwrap().connection(), Partition::Device, &request.candidate).unwrap().unwrap().input;
            if changed_identity {
                store.connection.execute("UPDATE library_sync_selection SET selection_epoch='synthetic-input-selection-change' WHERE singleton=1", []).unwrap();
            } else {
                let device = store.device_store().unwrap().connection();
                device.execute("INSERT INTO lww_intents VALUES('synthetic-pending','0','{}','{}','synthetic',0)", []).unwrap();
                device.execute("INSERT INTO lww_intent_failures(request_id,failures,quarantined,error) VALUES('synthetic-pending',3,1,'synthetic')", []).unwrap();
            }
            let result = store.begin_local_native_receive(&request);
            if changed_identity {
                assert!(matches!(result, Err(StoreError::Validation { message }) if message == "native-receive-identity-changed"));
            } else { assert!(matches!(result, Err(StoreError::CommitBusy))); }
            assert_eq!(store.lww_receive_row_counts("synthetic-input-admission-fence").unwrap(), before);
            assert_eq!(read_manifest(store.device_store().unwrap().connection(), Partition::Device, &request.candidate).unwrap().unwrap().input, before_input);
        }
    }

    #[test]
    fn stale_candidate_cannot_restage_missing_rows_after_same_authority_replacement() {
        use risunest_sync_wire::{stamp::Stamp, unit::UnitValue};
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let authority = store.lww_binding_authority().unwrap();
        let header = lww::Header { binding_authority: authority, request_id: "synthetic-stale-admission".into() };
        let change = lww::Change { key: UnitKey::new(&["root", "username"]).unwrap(),
            stamp: Stamp { physical_ms: 7.into(), logical: 0, writer_id: "00000000-0000-4000-8000-000000000001".into() },
            value: UnitValue::inline(br#""old source""#).unwrap() };
        store.lww_stage_receive(&lww::StageReceive { header: header.clone(), changes: vec![change],
            progress: lww::Progress { kind: "server".into(), cursor: 0.into(), writer_id: None }, admitted_time_upper_ms: 7.into() }).unwrap();
        let source = Source::Staged { header: header.clone() };
        let request = BeginRequest { candidate: store.prepare_native_receive(&source).unwrap(), source, native_baseline: None };
        store.connection.execute("DELETE FROM lww_receive_rows WHERE request_id=?1", [&header.request_id]).unwrap();
        let previous = sync_selection::identity(&store.connection).unwrap();
        let staging = store.replace_begin().unwrap().staging_id;
        store.replace_put_root(&staging, &serde_json::json!({"username":"replacement"})).unwrap();
        store.replace_commit(&staging, Some(previous.revision)).unwrap();
        assert_ne!(sync_selection::identity(&store.connection).unwrap().generation, previous.generation);
        assert_eq!(store.lww_binding_authority().unwrap(), authority);
        assert!(store.device_store().unwrap().connection().query_row("SELECT EXISTS(SELECT 1 FROM lww_receive WHERE request_id=?1)",
            [&header.request_id], |row| row.get::<_, bool>(0)).unwrap());
        let before_rows: i64 = store.connection.query_row("SELECT count(*) FROM lww_receive_rows WHERE request_id=?1",
            [&header.request_id], |row| row.get(0)).unwrap();
        assert_eq!(before_rows, 0);
        assert!(matches!(store.begin_local_native_receive(&request),
            Err(StoreError::Validation { message }) if message == "native-receive-identity-changed"));
        assert_eq!(store.connection.query_row("SELECT count(*) FROM lww_receive_rows WHERE request_id=?1", [&header.request_id],
            |row| row.get::<_, i64>(0)).unwrap(), before_rows);
        assert_eq!(store.read_root(None).unwrap().value["username"], "replacement");
        assert_eq!(read_coordinator(store.device_store().unwrap().connection()).unwrap().unwrap().next_sequence, 1);
    }
    #[test]
    fn candidate_source_uses_complete_digest_for_large_legacy_request_identity() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let header = lww::Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: "legacy".repeat(30_000) };
        store.lww_stage_receive(&lww::StageReceive { header: header.clone(), changes: Vec::new(),
            progress: lww::Progress { kind: "server".into(), cursor: 0.into(), writer_id: None }, admitted_time_upper_ms: 0.into() }).unwrap();
        let source = Source::Staged { header: header.clone() };
        let candidate = store.prepare_native_receive(&source).unwrap();
        assert_eq!(candidate.source_digest.len(), 64);
        let changed = Source::Staged { header: lww::Header { request_id: format!("{}x", header.request_id), ..header } };
        assert!(store.prepare_native_receive(&changed).is_err());
        let rowid: i64 = store.device_store().unwrap().connection().query_row("SELECT rowid FROM lww_receive", [], |row| row.get(0)).unwrap();
        store.device_store().unwrap().connection().execute("UPDATE lww_receive SET body=body||' ' WHERE rowid=?1", [rowid]).unwrap();
        assert!(store.prepare_native_receive(&source).is_err());
    }
    #[test]
    fn private_payload_is_text_without_asset_path_forms_and_malformed_hex_is_rejected() {
        let raw = encode(&serde_json::json!({"request":"assets/example", "key":"assets\\\\example"})).unwrap();
        assert!(!raw.contains('/')); assert!(!raw.contains('\\'));
        let decoded: serde_json::Value = decode(&raw).unwrap();
        assert_eq!(decoded["request"], "assets/example");
        for raw in [r#"{"version":1,"data":"AA"}"#, r#"{"version":1,"data":"0"}"#,
            r#"{"version":1,"data":"gg"}"#, r#"{"version":2,"data":"00"}"#] {
            assert!(decode::<serde_json::Value>(raw).is_err());
        }
    }

    #[test]
    fn intent_recovery_respects_active_custody_but_allows_sealed_history() {
        for interruption in [1, 2, 0] {
            let directory = tempfile::tempdir().unwrap();
            let mut store = PersistentStore::open(directory.path()).unwrap();
            let source = Source::Drain { binding_authority: store.lww_binding_authority().unwrap() };
            let request = BeginRequest { candidate: store.prepare_native_receive(&source).unwrap(), source, native_baseline: None };
            if interruption != 0 { INTERRUPT_ADMISSION.with(|flag| flag.set(interruption)); }
            let result = store.begin_local_native_receive(&request);
            if interruption == 0 { result.unwrap(); } else { assert!(result.is_err()); }
            assert!(matches!(store.lww_recover_intents(), Err(StoreError::CommitBusy)));
            assert!(!store.lww_recover_intents_at_open().unwrap());
            let mut reopened = PersistentStore::open(directory.path()).unwrap();
            assert!(matches!(reopened.lww_recover_intents(), Err(StoreError::CommitBusy)));
            store.begin_local_native_receive(&request).unwrap();
            for _ in 0..100 {
                let state = store.read_native_receive_state(&request.candidate).unwrap();
                if state.phase == Phase::Sealed { break; }
                assert!(matches!(reopened.lww_recover_intents(), Err(StoreError::CommitBusy)));
                store.advance_local_native_receive(&AdvanceRequest { capability: request.candidate.clone(),
                    expected_progress_token: state.progress_token, generating: vec![] }).unwrap();
            }
            assert_eq!(store.read_native_receive_state(&request.candidate).unwrap().outcome, Some(Outcome::Processed));
            reopened.lww_recover_intents().unwrap();
            assert_eq!(reopened.read_native_receive_state(&request.candidate).unwrap().phase, Phase::Sealed);
            let db = store.device_store().unwrap().connection();
            db.execute("INSERT INTO lww_intents VALUES('custody-protected-legacy','0','{}','{','synthetic',0)", []).unwrap();
            store.connection.execute("DELETE FROM app_kv WHERE key=?1", [format!("{}manifest", request.candidate.key().unwrap())]).unwrap();
            assert!(matches!(reopened.lww_recover_intents(), Err(StoreError::Validation { message }) if message == "native-receive-library-manifest-missing"));
            assert!(matches!(reopened.lww_recover_intents_at_open(), Err(StoreError::Validation { message }) if message == "native-receive-library-manifest-missing"));
            assert_eq!(db.query_row("SELECT count(*) FROM lww_intent_failures WHERE request_id='custody-protected-legacy'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
            assert!(!db.query_row("SELECT complete FROM lww_intents WHERE request_id='custody-protected-legacy'", [], |row| row.get::<_, bool>(0)).unwrap());
        }
    }

    fn settle_evidence(store: &mut PersistentStore) -> NativeReceiveEvidence {
        let lease = store.acquire_revision(store.revision().unwrap()).unwrap().lease;
        let evidence = store.read_native_receive_evidence(&lease).unwrap();
        store.release_revision(&lease).unwrap();
        evidence
    }
    fn local_step(store: &PersistentStore, cap: &Capability) -> StoreResult<AdvanceResult> {
        let state = store.read_native_receive_state(cap)?;
        store.advance_local_native_receive(&AdvanceRequest { capability: cap.clone(), expected_progress_token: state.progress_token, generating: vec![] })
    }
    fn stored_step(store: &PersistentStore, log: &Connection, cap: &Capability) -> StoreResult<AdvanceResult> {
        let state = store.read_native_receive_state(cap)?;
        store.advance_stored_native_receive(log, &AdvanceRequest { capability: cap.clone(), expected_progress_token: state.progress_token, generating: vec![] })
    }
    fn step_until(store: &PersistentStore, cap: &Capability, done: impl Fn(&PersistentStore) -> bool) {
        for _ in 0..1000 { if done(store) { return; } local_step(store, cap).unwrap(); }
        panic!("the receive must reach the expected settlement state");
    }
    fn sealed(store: &PersistentStore, cap: &Capability) -> bool { store.read_native_receive_state(cap).unwrap().phase == Phase::Sealed }
    fn manifests(store: &PersistentStore, cap: &Capability) -> (Manifest, Manifest) {
        (read_manifest(&store.connection, Partition::Library, cap).unwrap().unwrap(),
            read_manifest(store.device_store().unwrap().connection(), Partition::Device, cap).unwrap().unwrap())
    }
    fn phases(store: &PersistentStore, cap: &Capability) -> (Phase, Phase) {
        let (library, device) = manifests(store, cap);
        (library.phase, device.phase)
    }
    fn active_producer(store: &PersistentStore) -> Option<Capability> {
        read_coordinator(store.device_store().unwrap().connection()).unwrap().unwrap().active_producer
    }
    fn source_flags(store: &PersistentStore, request_id: &str) -> (bool, bool) {
        store.device_store().unwrap().connection().query_row("SELECT applied,finished FROM lww_receive WHERE request_id=?1",
            [request_id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
    }
    fn refused<T: std::fmt::Debug>(result: StoreResult<T>, expected: &str) {
        match result {
            Err(StoreError::Validation { message }) if message == expected => {}
            other => panic!("expected {expected}, got {other:?}"),
        }
    }
    fn first_failure(store: &PersistentStore, cap: &Capability) -> StoreError {
        (0..1000).find_map(|_| local_step(store, cap).err()).expect("the armed commit must fail")
    }
    fn change_identity(store: &PersistentStore) {
        store.connection.execute("UPDATE library_sync_selection SET selection_epoch='synthetic-replaced-selection' WHERE singleton=1", []).unwrap();
    }
    fn staged_header(request: &BeginRequest) -> lww::Header {
        match &request.source { Source::Staged { header } | Source::Stored { header } => header.clone(), Source::Drain { .. } => unreachable!() }
    }
    fn baseline_input(store: &mut PersistentStore, changes: &[lww::Change], request_id: String) -> BeginRequest {
        let mut request = staged_input(store, changes, request_id);
        request.native_baseline = Some(settle_evidence(store));
        request
    }
    fn namespace_rows(db: &Connection, partition: Partition, cap: &Capability) -> i64 {
        let prefix = cap.key().unwrap();
        db.query_row(&format!("SELECT count(*) FROM {} WHERE key>=?1 AND key<?2", partition.table()),
            params![prefix, prefix_end(&prefix).unwrap()], |row| row.get(0)).unwrap()
    }

    #[test]
    fn settlement_completes_each_half_committed_pair_after_reopen() {
        for (stop, changed) in [("enter", false), ("seal", false), ("seal", true), ("committed-seal", true)] {
            let directory = tempfile::tempdir().unwrap();
            let mut store = PersistentStore::open(directory.path()).unwrap();
            let request = baseline_input(&mut store, &[input_change(0), input_change(1)], format!("synthetic-settle-{stop}-{changed}"));
            let id = staged_header(&request).request_id;
            let cap = input_state(store.begin_local_native_receive(&request).unwrap()).capability;
            if stop == "enter" {
                step_until(&store, &cap, |store| { let (library, device) = manifests(store, &cap); scan_done(&library) && scan_done(&device) });
                assert_eq!(phases(&store, &cap), (Phase::Producing, Phase::Producing));
            } else {
                step_until(&store, &cap, |store| phases(store, &cap) == (Phase::Finalizing, Phase::Finalizing));
                if changed && stop != "committed-seal" { change_identity(&store); }
            }
            let effects = manifests(&store, &cap).0.effect_root;
            kernel_test::hook(7, || {});
            let failure = first_failure(&store, &cap);
            assert!(matches!(&failure, StoreError::Validation { message } if message == "synthetic-kernel-interruption"), "{failure:?}");
            let half = if stop == "enter" { (Phase::Finalizing, Phase::Producing) } else { (Phase::Sealed, Phase::Finalizing) };
            assert_eq!(phases(&store, &cap), half);
            if stop == "committed-seal" { change_identity(&store); }
            assert_eq!(active_producer(&store), Some(cap.clone()));
            assert_eq!(source_flags(&store, &id), (false, false));
            drop(store);
            let mut store = PersistentStore::open(directory.path()).unwrap();
            let resumed = input_state(store.begin_local_native_receive(&request).unwrap());
            assert_eq!((resumed.capability, resumed.foreign.scope), (cap.clone(), changed));
            assert_eq!(phases(&store, &cap), half);
            step_until(&store, &cap, |store| sealed(store, &cap));
            let state = store.read_native_receive_state(&cap).unwrap();
            let outcome = if changed && stop != "committed-seal" { Outcome::Superseded } else { Outcome::Processed };
            assert_eq!((state.outcome, state.foreign.scope, state.roots.is_some()), (Some(outcome), changed, true));
            assert_eq!(phases(&store, &cap), (Phase::Sealed, Phase::Sealed));
            assert_eq!(manifests(&store, &cap).0.effect_root, effects);
            assert_eq!(active_producer(&store), None);
            // Only a seal under the current identity marks its retained source applied.
            assert_eq!(source_flags(&store, &id), (!changed, false));
        }
    }

    #[test]
    fn identity_change_supersedes_owned_effects_after_a_half_seal() {
        for start in ["producing", "admitting", "stored"] {
            let directory = tempfile::tempdir().unwrap();
            let logs = tempfile::tempdir().unwrap();
            let log = stored_log(&logs.path().join("synthetic-operation-log.sqlite"));
            let mut store = PersistentStore::open(directory.path()).unwrap();
            let mut request = baseline_input(&mut store, &[input_change(0), input_change(1)], format!("synthetic-supersede-{start}"));
            let header = staged_header(&request);
            if start == "stored" {
                let source = Source::Stored { header: header.clone() };
                request = BeginRequest { candidate: store.prepare_native_receive(&source).unwrap(), source, native_baseline: request.native_baseline.clone() };
                let body: String = store.device_store().unwrap().connection().query_row("SELECT body FROM lww_receive WHERE request_id=?1",
                    [&header.request_id], |row| row.get(0)).unwrap();
                put_stored_page(&log, &serde_json::from_str(&body).unwrap());
            }
            let cap = request.candidate.clone();
            let step = |store: &PersistentStore| if start == "stored" { stored_step(store, &log, &cap) } else { local_step(store, &cap) };
            let begin = |store: &mut PersistentStore| if start == "stored" { store.begin_stored_native_receive(&log, &request) }
                else { store.begin_local_native_receive(&request) };
            if start == "admitting" {
                INTERRUPT_ADMISSION.with(|flag| flag.set(1));
                refused(begin(&mut store), "synthetic-admission-interruption");
                assert!(read_manifest(&store.connection, Partition::Library, &cap).unwrap().is_none());
            } else {
                input_state(begin(&mut store).unwrap());
                for _ in 0..1000 { if manifests(&store, &cap).0.effect_pages > 0 { break; } step(&store).unwrap(); }
            }
            let effects = read_manifest(&store.connection, Partition::Library, &cap).unwrap().map(|manifest| manifest.effect_root);
            change_identity(&store);
            // An unfinished admission cannot resume; an admitted producer is returned with its foreign scope.
            match begin(&mut store) {
                Err(StoreError::Validation { message }) if start == "admitting" && message == "native-receive-identity-changed" => {}
                Ok(BeginResult::Existing { state }) if start != "admitting" => assert!(state.foreign.scope),
                other => panic!("unexpected begin after the identity change {other:?}"),
            }
            kernel_test::hook(7, || {});
            let failure = (0..1000).find_map(|_| step(&store).err()).expect("the armed commit must fail");
            assert!(matches!(&failure, StoreError::Validation { message } if message == "synthetic-kernel-interruption"), "{failure:?}");
            let (library, device) = manifests(&store, &cap);
            assert_eq!((library.phase, library.outcome), (Phase::Sealed, Some(Outcome::Superseded)));
            assert_eq!(device.phase, if start == "admitting" { Phase::Admitting } else { Phase::Producing });
            drop(store);
            let store = PersistentStore::open(directory.path()).unwrap();
            assert_eq!(store.read_native_receive_state(&cap).unwrap().roots, None);
            for _ in 0..1000 { if sealed(&store, &cap) { break; } step(&store).unwrap(); }
            let state = store.read_native_receive_state(&cap).unwrap();
            assert_eq!((state.outcome, state.foreign.scope), (Some(Outcome::Superseded), true));
            let (library, device) = manifests(&store, &cap);
            for manifest in [&library, &device] {
                assert_eq!((manifest.phase, manifest.disposition_pages, manifest.held_count, manifest.deferred_count), (Phase::Sealed, 0, 0, 0));
                assert!(manifest.ticket.is_none());
            }
            assert_eq!(Some(library.effect_root), effects.or(Some(EMPTY_ROOT.into())));
            assert_eq!(active_producer(&store), None);
            assert_eq!(source_flags(&store, &header.request_id), (false, false));
            // Superseded history did not process the transport page, so its acknowledgment is not held back.
            store.settle_native_receive_transport(&log, &header).unwrap();
            assert_eq!(phases(&store, &cap), (Phase::Sealed, Phase::Sealed));
        }
    }

    #[test]
    fn final_dispositions_resume_across_reopen_and_deferred_wins_per_key() {
        for superseded in [false, true] {
        use risunest_sync_wire::{stamp::Stamp, unit::UnitValue};
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let authority = store.lww_binding_authority().unwrap();
        let key = |owner: &str| UnitKey::new(&["character", owner, "name"]).unwrap();
        for (index, (id, owners)) in [("synthetic-held-first", &["ghost-a", "ghost-b", "ghost-c"][..]), ("synthetic-held-second", &["ghost-a"][..])].into_iter().enumerate() {
            let changes = owners.iter().map(|owner| lww::Change { key: key(owner),
                stamp: Stamp { physical_ms: (7 + index as u64).into(), logical: 0, writer_id: "00000000-0000-4000-8000-000000000001".into() },
                value: UnitValue::inline(br#""held name""#).unwrap() }).collect();
            store.lww_stage_receive(&lww::StageReceive { header: lww::Header { binding_authority: authority, request_id: id.into() }, changes,
                progress: lww::Progress { kind: "server".into(), cursor: 0.into(), writer_id: None }, admitted_time_upper_ms: 100.into() }).unwrap();
        }
        let source = Source::Drain { binding_authority: authority };
        let request = BeginRequest { candidate: store.prepare_native_receive(&source).unwrap(), source, native_baseline: Some(settle_evidence(&mut store)) };
        let cap = input_state(store.begin_local_native_receive(&request).unwrap()).capability;
        let _limits = kernel_test::Limits::set(1, 100_000);
        step_until(&store, &cap, |store| phases(store, &cap) == (Phase::Finalizing, Phase::Finalizing));
        assert_eq!(manifests(&store, &cap).0.disposition_pages, 0);
        // The later row of one key is deferred at the frontier; the whole key is decided deferred.
        store.connection.execute("UPDATE lww_receive_rows SET status='deferred' WHERE request_id='synthetic-held-second'", []).unwrap();
        let effects = manifests(&store, &cap).0.effect_root;
        let mut changed = false;
        let mut pages = Vec::new();
        for _ in 0..100 {
            if sealed(&store, &cap) { break; }
            local_step(&store, &cap).unwrap();
            let count = manifests(&store, &cap).0.disposition_pages;
            pages.push(count);
            if superseded && !changed && count == 2 { change_identity(&store); changed = true; }
            drop(store);
            store = PersistentStore::open(directory.path()).unwrap();
        }
        assert!(sealed(&store, &cap));
        assert!(pages.windows(2).all(|pair| pair[1] <= pair[0] + 1), "one bounded disposition page per step");
        let (library, device) = manifests(&store, &cap);
        assert_eq!(library.effect_root, effects);
        if superseded {
            assert!(changed);
            assert!(pages.windows(2).all(|pair| pair[0].abs_diff(pair[1]) <= 1));
            assert_eq!((library.disposition_pages, library.held_count, library.deferred_count), (0, 0, 0));
            assert_eq!(store.read_native_receive_state(&cap).unwrap().outcome, Some(Outcome::Superseded));
            for ordinal in 0..2 {
                let read = ReadPage { capability: cap.clone(), partition: Partition::Library, kind: PageKind::Dispositions,
                    ordinal: ordinal.into(), previous_digest: EMPTY_ROOT.into() };
                assert!(store.read_native_receive_page(&read).is_err());
            }
            continue;
        }
        assert_eq!((library.disposition_pages, library.held_count, library.deferred_count), (3, 2, 1));
        assert_eq!((device.disposition_pages, store.read_native_receive_state(&cap).unwrap().outcome), (0, Some(Outcome::Processed)));
        let mut previous = EMPTY_ROOT.to_string();
        let mut decided = Vec::new();
        for ordinal in 0..library.disposition_pages {
            let read = ReadPage { capability: cap.clone(), partition: Partition::Library, kind: PageKind::Dispositions, ordinal: ordinal.into(), previous_digest: previous.clone() };
            let HistoryPage::Dispositions { page, digest } = store.read_native_receive_page(&read).unwrap() else { panic!("disposition page expected") };
            decided.extend(page.entries.into_iter().map(|entry| (entry.key, entry.disposition)));
            previous = digest;
        }
        assert_eq!(previous, library.disposition_root);
        assert_eq!(decided, vec![(key("ghost-a").as_str().to_owned(), Disposition::Deferred),
            (key("ghost-b").as_str().to_owned(), Disposition::Held), (key("ghost-c").as_str().to_owned(), Disposition::Held)]);
        }
    }

    #[test]
    fn exact_delivery_settles_drain_and_cleanup_resumes_every_boundary_without_reviving_the_capability() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let authority = store.lww_binding_authority().unwrap();
        store.lww_stage_receive(&lww::StageReceive { header: lww::Header { binding_authority: authority, request_id: "synthetic-cleanup".into() },
            changes: (0..6).map(input_change).collect(), progress: lww::Progress { kind: "server".into(), cursor: 0.into(), writer_id: None },
            admitted_time_upper_ms: 7.into() }).unwrap();
        let source = Source::Drain { binding_authority: authority };
        let request = BeginRequest { candidate: store.prepare_native_receive(&source).unwrap(), source: source.clone(),
            native_baseline: Some(settle_evidence(&mut store)) };
        let cap = input_state(store.begin_local_native_receive(&request).unwrap()).capability;
        let _limits = kernel_test::Limits::set(1, 100_000);
        step_until(&store, &cap, |store| sealed(store, &cap));
        let roots = store.read_native_receive_state(&cap).unwrap().roots.unwrap();
        let retire = RetireRequest { capability: cap.clone(), roots: roots.clone() };
        let first_page = ReadPage { capability: cap.clone(), partition: Partition::Library, kind: PageKind::Effects, ordinal: 0.into(), previous_digest: EMPTY_ROOT.into() };
        store.read_native_receive_page(&first_page).unwrap();
        refused(store.cleanup_native_receive(&cap), "native-receive-settlement-required");
        refused(store.retire_native_receive(None, &retire), "native-receive-delivery-required");
        let epoch: String = store.connection.query_row("SELECT selection_epoch FROM library_sync_selection WHERE singleton=1", [], |row| row.get(0)).unwrap();
        let mut delivery = DeliveryRequest { capability: cap.clone(), roots: roots.clone(), projection: settle_evidence(&mut store) };
        for witness in ["library", "device", "plugins", "missing-device"] {
            match witness {
                "library" => { store.connection.execute("UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='currentRevision'", []).unwrap(); }
                "device" => { store.device_store().unwrap().connection().execute("UPDATE device_meta SET revision=revision+1 WHERE singleton=1", []).unwrap(); }
                "plugins" => { store.device_store().unwrap().connection().execute("UPDATE plugin_gc_state SET revision=revision+1 WHERE singleton=1", []).unwrap(); }
                _ => delivery.projection.device_revision = None,
            }
            refused(store.acknowledge_native_receive_delivery(&delivery), "native-receive-projection-changed");
            assert_eq!(phases(&store, &cap), (Phase::Sealed, Phase::Sealed));
            delivery.projection = settle_evidence(&mut store);
        }
        let swapped = DeliveryRequest { capability: cap.clone(), roots: Roots { library: roots.device.clone(), device: roots.library.clone() },
            projection: delivery.projection.clone() };
        refused(store.acknowledge_native_receive_delivery(&swapped), "native-receive-delivery-changed");
        change_identity(&store);
        refused(store.acknowledge_native_receive_delivery(&delivery), "native-receive-projection-identity-changed");
        store.connection.execute("UPDATE library_sync_selection SET selection_epoch=?1 WHERE singleton=1", [&epoch]).unwrap();
        assert_eq!(phases(&store, &cap), (Phase::Sealed, Phase::Sealed));
        kernel_test::hook(7, || {});
        refused(store.acknowledge_native_receive_delivery(&delivery), "synthetic-kernel-interruption");
        assert_eq!(phases(&store, &cap), (Phase::Settled, Phase::Sealed));
        drop(store);
        let mut store = PersistentStore::open(directory.path()).unwrap();
        for _ in 0..2 {
            let state = store.acknowledge_native_receive_delivery(&delivery).unwrap();
            assert_eq!((state.phase, state.roots.as_ref()), (Phase::Settled, Some(&roots)));
        }
        refused(store.acknowledge_native_receive_delivery(&swapped), "native-receive-delivery-changed");
        assert!(matches!(store.retire_native_receive(None, &retire).unwrap(), RetireResult::Retired));
        let library_rows = namespace_rows(&store.connection, Partition::Library, &cap);
        let device_rows = namespace_rows(store.device_store().unwrap().connection(), Partition::Device, &cap);
        assert!(library_rows > 2 && device_rows > 2);
        let (mut validating, mut library_partial, mut device_partial, mut interrupted) = (false, false, false, false);
        for _ in 0..1000 {
            let proof = read_coordinator(store.device_store().unwrap().connection()).unwrap().unwrap().cleanup.iter()
                .find(|proof| proof.capability == cap).map(|proof| (proof.library_done, proof.device_done));
            let library_left = namespace_rows(&store.connection, Partition::Library, &cap);
            let device_left = namespace_rows(store.device_store().unwrap().connection(), Partition::Device, &cap);
            if let Some((library_done, _)) = proof {
                refused(store.read_native_receive_page(&first_page), "native-receive-cleanup-started");
                assert!(matches!(store.retire_native_receive(None, &retire).unwrap(), RetireResult::Retired));
                library_partial |= library_left > 0 && library_left < library_rows;
                device_partial |= library_done && device_left > 0 && device_left < device_rows;
                // Library deletion becomes durable before the coordinator records it.
                assert!(!library_done || library_left == 0);
                if !interrupted && library_left < library_rows {
                    INTERRUPT_CLEANUP.with(|flag| flag.set(true));
                    refused(store.cleanup_native_receive(&cap), "synthetic-cleanup-interruption");
                    assert!(namespace_rows(&store.connection, Partition::Library, &cap) < library_left);
                    interrupted = true;
                    drop(store);
                    store = PersistentStore::open(directory.path()).unwrap();
                    continue;
                }
            } else {
                assert_eq!((library_left, device_left), (library_rows, device_rows));
                validating |= manifests(&store, &cap).0.validation.is_some();
                assert_eq!(phases(&store, &cap), (Phase::Settled, Phase::Settled));
            }
            let closed = store.cleanup_native_receive(&cap).unwrap();
            drop(store);
            store = PersistentStore::open(directory.path()).unwrap();
            if closed { break; }
        }
        assert!(validating && library_partial && device_partial && interrupted);
        assert_eq!((namespace_rows(&store.connection, Partition::Library, &cap),
            namespace_rows(store.device_store().unwrap().connection(), Partition::Device, &cap)), (0, 0));
        // The closed capability never revives, even for an identical source.
        refused(store.read_native_receive_state(&cap), "native-receive-capability-retired");
        refused(store.read_native_receive_page(&first_page), "native-receive-capability-retired");
        refused(store.retire_native_receive(None, &retire), "native-receive-capability-retired");
        refused(store.cleanup_native_receive(&cap), "native-receive-capability-retired");
        refused(store.begin_local_native_receive(&request), "native-receive-candidate-consumed");
        let fresh = store.prepare_native_receive(&source).unwrap();
        assert!(fresh.sequence.0 > cap.sequence.0);
        let state = input_state(store.begin_local_native_receive(&BeginRequest { candidate: fresh.clone(), source, native_baseline: None }).unwrap());
        assert_eq!(state.capability, fresh);
        refused(store.read_native_receive_page(&first_page), "native-receive-capability-retired");
    }

    #[test]
    fn staged_finish_waits_for_exact_delivery_and_retirement_requires_finish_proof() {
        staged_finish_retirement_scenarios(&["witness-retire", "witness-redeliver", "finished-unwitnessed", "unfinished", "pruned", "pruned-without-progress"]);
    }

    #[test]
    fn pruned_receive_retirement_requires_progress_at_the_original_cursor() {
        staged_finish_retirement_scenarios(&["pruned-behind"]);
    }

    #[test]
    fn pruned_receive_retirement_requires_the_source_to_be_prunable() {
        staged_finish_retirement_scenarios(&["pruned-held", "pruned-wrong-channel", "pruned-equal-cursor"]);
    }

    fn staged_finish_retirement_scenarios(scenarios: &[&str]) {
        for &scenario in scenarios {
            let directory = tempfile::tempdir().unwrap();
            let mut store = PersistentStore::open(directory.path()).unwrap();
            let header = lww::Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: format!("synthetic-retire-{scenario}") };
            store.lww_stage_receive(&lww::StageReceive { header: header.clone(), changes: vec![lww::Change { key: UnitKey::new(&["root", "language"]).unwrap(), ..input_change(0) }],
                progress: lww::Progress { kind: "server".into(), cursor: if scenario.starts_with("pruned") { 1.into() } else { 0.into() }, writer_id: None },
                admitted_time_upper_ms: 7.into() }).unwrap();
            let source = Source::Staged { header };
            let request = BeginRequest { candidate: store.prepare_native_receive(&source).unwrap(), source, native_baseline: None };
            let header = staged_header(&request);
            let cap = input_state(store.begin_local_native_receive(&request).unwrap()).capability;
            step_until(&store, &cap, |store| sealed(store, &cap));
            assert_eq!(source_flags(&store, &header.request_id), (true, false));
            assert_eq!(store.read_root(None).unwrap().value["language"], "value-0");
            // The native seal is the applied receive; finishing it waits for the exact delivery.
            refused(store.lww_finish_receive(&header), "native-receive-delivery-required");
            assert_eq!(source_flags(&store, &header.request_id), (true, false));
            let roots = store.read_native_receive_state(&cap).unwrap().roots.unwrap();
            let delivery = DeliveryRequest { capability: cap.clone(), roots: roots.clone(), projection: settle_evidence(&mut store) };
            assert_eq!(store.acknowledge_native_receive_delivery(&delivery).unwrap().phase, Phase::Delivered);
            let retire = RetireRequest { capability: cap.clone(), roots: roots.clone() };
            assert!(matches!(store.retire_native_receive(None, &retire).unwrap(), RetireResult::Pending));
            let mark_finished = |store: &PersistentStore, request_id: &str| {
                store.device_store().unwrap().connection().execute("UPDATE lww_receive SET finished=1 WHERE request_id=?1", [request_id]).unwrap();
            };
            if scenario.starts_with("witness") {
                store.lww_finish_receive(&header).unwrap();
                assert!(manifests(&store, &cap).1.local_finish_witness);
                assert_eq!(source_flags(&store, &header.request_id), (true, true));
            } else if scenario == "finished-unwitnessed" {
                // A finish recorded without this binary's witness is proven by the retained row.
                mark_finished(&store, &header.request_id);
            } else {
                // A later finished receive of the same progress channel, as the prune rule requires.
                let later = lww::Header { request_id: format!("synthetic-later-{scenario}"), ..header.clone() };
                store.lww_stage_receive(&lww::StageReceive { header: later.clone(), changes: Vec::new(),
                    progress: lww::Progress { kind: "server".into(), cursor: 2.into(), writer_id: None }, admitted_time_upper_ms: 7.into() }).unwrap();
                mark_finished(&store, &later.request_id);
                let device = store.device_store().unwrap().connection();
                if scenario != "pruned-without-progress" {
                    device.execute("INSERT INTO lww_progress VALUES('server','','2',?1) ON CONFLICT(kind,writer_id) DO UPDATE SET cursor='2'",
                        [header.binding_authority.0.to_string()]).unwrap();
                }
                match scenario {
                    "pruned-behind" => { device.execute("UPDATE lww_progress SET cursor='0'", []).unwrap(); }
                    "pruned-wrong-channel" => { device.execute("UPDATE lww_receive SET writer_id='another-writer' WHERE request_id=?1", [&later.request_id]).unwrap(); }
                    "pruned-equal-cursor" => { device.execute("UPDATE lww_receive SET cursor='1' WHERE request_id=?1", [&later.request_id]).unwrap(); }
                    "pruned-held" => { store.connection.execute("UPDATE lww_receive_rows SET status='held' WHERE request_id=?1", [&header.request_id]).unwrap(); }
                    _ => {}
                }
                if scenario != "unfinished" {
                    mark_finished(&store, &header.request_id);
                    device.execute("DELETE FROM lww_receive WHERE request_id=?1", [&header.request_id]).unwrap();
                }
            }
            let expected = matches!(scenario, "witness-retire" | "witness-redeliver" | "finished-unwitnessed" | "pruned");
            if scenario == "witness-redeliver" {
                // A repeated exact delivery after the finish settles at once.
                assert_eq!(store.acknowledge_native_receive_delivery(&delivery).unwrap().phase, Phase::Settled);
            } else if scenario == "witness-retire" {
                kernel_test::hook(7, || {});
                refused(store.retire_native_receive(None, &retire), "synthetic-kernel-interruption");
                assert_eq!(phases(&store, &cap), (Phase::Settled, Phase::Delivered));
                drop(store);
                store = PersistentStore::open(directory.path()).unwrap();
            }
            let retired = matches!(store.retire_native_receive(None, &retire).unwrap(), RetireResult::Retired);
            assert_eq!(retired, expected, "{scenario}");
            let settled = if expected { (Phase::Settled, Phase::Settled) } else { (Phase::Delivered, Phase::Delivered) };
            assert_eq!(phases(&store, &cap), settled, "{scenario}");
        }
    }

    fn stored_log(path: &std::path::Path) -> Connection {
        let log = Connection::open(path).unwrap();
        log.busy_timeout(std::time::Duration::ZERO).unwrap();
        log.execute_batch("CREATE TABLE IF NOT EXISTS receive_pages(authority TEXT PRIMARY KEY,body TEXT NOT NULL,next_key TEXT,bootstrap INTEGER NOT NULL,done INTEGER NOT NULL)").unwrap();
        log
    }
    fn put_stored_page(log: &Connection, page: &lww::StageReceive) {
        log.execute("INSERT OR REPLACE INTO receive_pages VALUES(?1,?2,NULL,0,1)",
            params![page.header.binding_authority.0.to_string(), serde_json::to_string(page).unwrap()]).unwrap();
    }

    #[test]
    fn stored_receive_holds_the_transport_log_and_settles_only_on_its_exact_acknowledgment() {
        stored_receive_settlement(false);
    }

    #[test]
    fn obsolete_stored_receive_retires_without_acknowledging_the_retained_transport_page() {
        stored_receive_settlement(true);
    }

    fn stored_receive_settlement(obsolete: bool) {
        let directory = tempfile::tempdir().unwrap();
        let logs = tempfile::tempdir().unwrap();
        let path = logs.path().join("synthetic-operation-log.sqlite");
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let header = lww::Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: "synthetic-stored-page".into() };
        let page = lww::StageReceive { header: header.clone(), changes: vec![lww::Change { key: UnitKey::new(&["root", "language"]).unwrap(), ..input_change(0) }, input_change(1)],
            progress: lww::Progress { kind: "server".into(), cursor: 1.into(), writer_id: None }, admitted_time_upper_ms: 7.into() };
        store.lww_stage_receive(&page).unwrap();
        let log = stored_log(&path);
        put_stored_page(&log, &page);
        let source = Source::Stored { header: header.clone() };
        let native_baseline = obsolete.then(|| {
            let mut evidence = settle_evidence(&mut store);
            evidence.identity.selection_epoch = "synthetic-stale-baseline".into();
            evidence
        });
        let request = BeginRequest { candidate: store.prepare_native_receive(&source).unwrap(), source, native_baseline };
        refused(store.begin_local_native_receive(&request), "native-receive-source-mode");
        // Stored admission reserves the transport log last; a peer holding it keeps the admission out.
        let peer = Connection::open(&path).unwrap();
        peer.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(matches!(store.begin_stored_native_receive(&log, &request), Err(StoreError::CommitBusy)));
        peer.execute_batch("ROLLBACK").unwrap();
        let mut changed = page.clone();
        changed.changes[0] = input_change(2);
        if !obsolete {
            put_stored_page(&log, &changed);
            refused(store.begin_stored_native_receive(&log, &request), "native-receive-stored-page-changed");
            put_stored_page(&log, &page);
        }
        let cap = input_state(store.begin_stored_native_receive(&log, &request).unwrap()).capability;
        if !obsolete {
            for changed in [changed, lww::StageReceive { progress: lww::Progress { cursor: 2.into(), ..page.progress.clone() }, ..page.clone() },
                lww::StageReceive { admitted_time_upper_ms: 8.into(), ..page.clone() }] {
                put_stored_page(&log, &changed);
                refused(stored_step(&store, &log, &cap), "native-receive-stored-page-changed");
            }
        }
        // Old readers may reorder JSON or omit transport-only metadata.
        let reordered = serde_json::json!({"more":false,"admittedTimeUpperMs":page.admitted_time_upper_ms,
            "progress":page.progress,"changes":page.changes,"requestId":page.header.request_id,
            "bindingAuthority":page.header.binding_authority,"receiveFrontier":"2"});
        log.execute("UPDATE receive_pages SET body=?1", [reordered.to_string()]).unwrap();
        stored_step(&store, &log, &cap).unwrap();
        refused(local_step(&store, &cap), "native-receive-stored-guard-required");
        peer.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(matches!(stored_step(&store, &log, &cap), Err(StoreError::CommitBusy)));
        peer.execute_batch("ROLLBACK").unwrap();
        // A replaced or missing originating page is stale.
        put_stored_page(&log, &lww::StageReceive { header: lww::Header { request_id: "synthetic-other-page".into(), ..header.clone() }, ..page.clone() });
        refused(stored_step(&store, &log, &cap), "native-receive-stored-page-changed");
        log.execute("DELETE FROM receive_pages", []).unwrap();
        refused(stored_step(&store, &log, &cap), "native-receive-stored-page-changed");
        put_stored_page(&log, &page);
        for _ in 0..1000 { if sealed(&store, &cap) { break; } stored_step(&store, &log, &cap).unwrap(); }
        let state = store.read_native_receive_state(&cap).unwrap();
        assert_eq!(state.outcome, Some(Outcome::Processed));
        assert!(state.foreign.scope);
        assert_eq!(store.read_root(None).unwrap().value["language"], "value-0");
        assert_eq!(source_flags(&store, &header.request_id), (true, false));
        let roots = state.roots.unwrap();
        let retire = RetireRequest { capability: cap.clone(), roots: roots.clone() };
        // Transport settlement needs the exact delivery and then the finish.
        refused(store.settle_native_receive_transport(&log, &header), "native-receive-delivery-required");
        let delivery = DeliveryRequest { capability: cap.clone(), roots, projection: settle_evidence(&mut store) };
        assert_eq!(store.acknowledge_native_receive_delivery(&delivery).unwrap().phase, Phase::Delivered);
        refused(store.settle_native_receive_transport(&log, &header), "native-receive-finish-required");
        refused(store.retire_native_receive(None, &retire), "native-receive-stored-guard-required");
        if obsolete {
            store.device_store().unwrap().connection().execute("UPDATE lww_clock SET binding_authority='1'", []).unwrap();
            assert!(store.lww_finish_receive(&header).is_err());
            assert!(matches!(store.retire_native_receive(Some(&log), &retire).unwrap(), RetireResult::Retired));
            for _ in 0..1000 { if store.cleanup_native_receive(&cap).unwrap() { break; } }
            refused(store.read_native_receive_state(&cap), "native-receive-capability-retired");
            assert_eq!(log.query_row("SELECT count(*) FROM receive_pages", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
            let next = lww::StageReceive { header: lww::Header { binding_authority: 1.into(), request_id: "synthetic-new-binding".into() }, ..page.clone() };
            store.lww_stage_receive(&next).unwrap();
            put_stored_page(&log, &next);
            let source = Source::Stored { header: next.header };
            let request = BeginRequest { candidate: store.prepare_native_receive(&source).unwrap(), source, native_baseline: None };
            assert!(matches!(store.begin_stored_native_receive(&log, &request).unwrap(), BeginResult::Admitted { .. }));
            return;
        }
        store.lww_finish_receive(&header).unwrap();
        // Local retirement leaves history whose transport page is still pending to the acknowledgment.
        assert!(matches!(store.retire_native_receive(Some(&log), &retire).unwrap(), RetireResult::Pending));
        store.settle_native_receive_transport(&log, &lww::Header { request_id: "synthetic-unrelated".into(), ..header.clone() }).unwrap();
        assert_eq!(phases(&store, &cap), (Phase::Delivered, Phase::Delivered));
        kernel_test::hook(7, || {});
        refused(store.settle_native_receive_transport(&log, &header), "synthetic-kernel-interruption");
        assert_eq!(phases(&store, &cap), (Phase::Settled, Phase::Delivered));
        drop(store);
        let store = PersistentStore::open(directory.path()).unwrap();
        store.settle_native_receive_transport(&log, &header).unwrap();
        assert_eq!(phases(&store, &cap), (Phase::Settled, Phase::Settled));
        assert!(matches!(store.retire_native_receive(Some(&log), &retire).unwrap(), RetireResult::Retired));
    }

    #[test]
    fn seal_revalidates_owned_history_whose_proof_is_missing() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let request = baseline_input(&mut store, &[input_change(0), input_change(1)], "synthetic-seal-validation".into());
        let cap = input_state(store.begin_local_native_receive(&request).unwrap()).capability;
        step_until(&store, &cap, |store| { let (library, device) = manifests(store, &cap);
            library.cursor.phase == ScanPhase::Complete && device.cursor.phase == ScanPhase::Complete });
        let (mut library, _) = manifests(&store, &cap);
        assert!(library.effect_pages > 0 && history_validated(&library, &[ValidationPurpose::Seal]));
        library.validated = None;
        let tx = Transaction::new_unchecked(&store.connection, TransactionBehavior::Immediate).unwrap();
        write_manifest(&tx, Partition::Library, &library).unwrap();
        tx.execute(&format!("DELETE FROM {} WHERE key=?1", Partition::Library.table()), [effect_key(&cap, 0).unwrap()]).unwrap();
        tx.commit().unwrap();
        refused(local_step(&store, &cap), "native-receive-effect-page-missing");
        assert_eq!(phases(&store, &cap), (Phase::Finalizing, Phase::Finalizing));
        assert_eq!(active_producer(&store), Some(cap));
    }
}