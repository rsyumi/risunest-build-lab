use super::{lww::UnitMutation, ConversationMutation, ConversationWindowQuery, PersistentStore, StoreError, StoreResult, WorkingSetCommit};
use crate::native_generation::operation::{runtime_session_id, ActiveGeneration, Checkpoint, Owner, Phase, Start, Status, StoredHistory, Target, Ticket};
use crate::native_generation::operation::{GenerationLease, ProjectionIdentity, OriginalIdState, OriginalRowWitness,
    NormalizationWitness, NormalizationReceipt, NormalizationEvidence, NormalizationResolution,
    ProjectionUnavailable, NormalizedResidentRow, NormalizationProjection, NormalizationMapping, NormalizationProjectionResult};
use crate::native_generation::inputs::{OllamaIntent, PreparationError, PreparationSnapshot};
use crate::native_generation::preparation_effects::PreparedVariablePlan;
use crate::native_generation::{cbs::{Entropy, HostCapture}, prepare::{admit_ollama, next_statics}, service::GenerationError};
use risunest_sync_wire::unit::UnitKey;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, value::RawValue, Value};
use std::{collections::{BTreeMap, BTreeSet}, sync::{Arc, Mutex, OnceLock, Weak}, time::{Duration, SystemTime, UNIX_EPOCH}};

// The key stays fixed; the version lives in the stored envelope so prior experimental readers fail closed.
pub(super) const JOURNAL_KEY: &str = "native-generation-journal-v1";
const JOURNAL_VERSION: u64 = 3;
const MAX_DESCRIPTOR_BYTES: usize = 64;
const MAX_RESERVED: usize = 64;
const MAX_ACTIVE: usize = 16;
const MAX_TERMINAL: usize = 64;
const MAX_HISTORY_MESSAGES: i64 = 4096;
const MAX_HISTORY_BYTES: usize = 8 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PROTECTION_JOURNAL_BYTES: usize = 32 * 1024 * 1024;
const MAX_PROJECTION_ROWS: usize = 128;
// Definite revision conflicts that left the target unchanged; each retry is a new frozen request.
const MAX_REVISION_RETRIES: usize = 8;

#[cfg(test)]
std::thread_local! {
    static FINISH_WAITING_FOR_SEGMENT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    pub(crate) static BEFORE_CHECKPOINT_COMMIT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, target_os = "windows"))]
pub(crate) fn with_finish_waiting_for_segment<T>(hook: Box<dyn FnOnce()>, work: impl FnOnce() -> T) -> T {
    struct Restore(Option<Box<dyn FnOnce()>>, std::marker::PhantomData<std::rc::Rc<()>>);
    impl Drop for Restore {
        fn drop(&mut self) { FINISH_WAITING_FOR_SEGMENT.with(|slot| slot.replace(self.0.take())); }
    }
    let _restore = Restore(FINISH_WAITING_FOR_SEGMENT.with(|slot| slot.replace(Some(hook))), std::marker::PhantomData);
    work()
}

#[cfg(all(test, target_os = "windows"))]
#[test]
fn scoped_waiting_hook_restores_after_unwind_and_allows_callback_reentry() {
    use std::{cell::Cell, rc::Rc};
    let called = Rc::new(Cell::new(false));
    let observed = called.clone();
    with_finish_waiting_for_segment(Box::new(move || {
        with_finish_waiting_for_segment(Box::new(|| {}), || observed.set(true));
    }), || {
        assert!(std::panic::catch_unwind(|| with_finish_waiting_for_segment(
            Box::new(|| panic!("inner hook must be removed")), || panic!("synthetic unwind"))).is_err());
        FINISH_WAITING_FOR_SEGMENT.with(|slot| slot.borrow_mut().take()).unwrap()();
    });
    assert!(called.get());
}

fn invalid(code: &str) -> StoreError {
    StoreError::Validation { message: format!("native-generation:{code}") }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct UnitFence { stamp: String, version: String }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Fence {
    character: Option<UnitFence>,
    conversation: Option<UnitFence>,
    messages: Option<UnitFence>,
    count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Attempt {
    request_id: String,
    intent_digest: String,
    expected_revision: i64,
    kind: AttemptKind,
    // A Variables write that also stores the start trigger's message edits: the history's row count after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    edited_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum AttemptKind {
    Output { sequence: u64, output_hash: String },
    HistoryIds,
    Effect { effect: SendEffect },
}

// The send settles interaction, the prepared chat variables and author note, and memory before dispatch.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
enum SendEffect { Interaction, Variables, Memory }

// A settled effect keeps its receipt; one that needed no write settles without one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum EffectState { Pending, Settled { receipt: Option<(String, String, i64)> } }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SendEffects { interaction: EffectState, variables: EffectState, memory: EffectState }

impl SendEffects {
    fn pending() -> Self { Self { interaction: EffectState::Pending, variables: EffectState::Pending, memory: EffectState::Pending } }
    // A record written before these effects existed never runs them on resume.
    fn absent() -> Self { Self { interaction: EffectState::Settled { receipt: None }, variables: EffectState::Settled { receipt: None }, memory: EffectState::Settled { receipt: None } } }
    fn get(&self, effect: SendEffect) -> &EffectState {
        match effect { SendEffect::Interaction => &self.interaction, SendEffect::Variables => &self.variables, SendEffect::Memory => &self.memory }
    }
    fn get_mut(&mut self, effect: SendEffect) -> &mut EffectState {
        match effect { SendEffect::Interaction => &mut self.interaction, SendEffect::Variables => &mut self.variables, SendEffect::Memory => &mut self.memory }
    }
}

fn effect_settled(record: &Record, effect: SendEffect) -> bool {
    record.preparation.as_ref().is_some_and(|value| matches!(value.effects.get(effect), EffectState::Settled { .. }))
}

// Each effect names only the units its receipt may have written; a Variables write that stored message edits also wrote
// `messages`.
#[derive(Debug)]
pub(crate) struct SendEffectKeys { statics: UnitKey, interaction: UnitKey, variables: UnitKey, note: UnitKey, memory: UnitKey, messages: UnitKey }

impl SendEffectKeys {
    fn for_effect(&self, effect: SendEffect) -> Vec<&UnitKey> {
        match effect {
            SendEffect::Interaction => vec![&self.statics, &self.interaction],
            SendEffect::Variables => vec![&self.variables, &self.note],
            SendEffect::Memory => vec![&self.memory],
        }
    }
}

pub(crate) fn send_effect_unit_keys(target: &Target) -> StoreResult<SendEffectKeys> {
    let key = |parts: &[&str]| UnitKey::new(parts).map_err(|_| invalid("invalid-target"));
    Ok(SendEffectKeys {
        statics: key(&["root", "statics"])?, interaction: key(&["character", &target.character_id, "lastInteraction"])?,
        variables: key(&["conversation", &target.character_id, &target.conversation_id, "scriptstate"])?,
        note: key(&["conversation", &target.character_id, &target.conversation_id, "note"])?,
        memory: key(&["conversation", &target.character_id, &target.conversation_id, "lastMemory"])?,
        messages: key(&["messages", &target.character_id, &target.conversation_id])?,
    })
}

/// Whether the store reads `messages` back as a whole history, as a reservation requires of the stored one.
pub(crate) fn generation_history_fits(messages: &[Value]) -> bool {
    i64::try_from(messages.len()).is_ok_and(|count| count <= MAX_HISTORY_MESSAGES) && normalized_history_fits(messages, MAX_HISTORY_BYTES).is_ok()
}

// `Edits` also replaces a range of the history's rows, leaving `count` rows.
enum EffectPlan { Settled, Mutations(Vec<UnitMutation>), Edits { units: Vec<UnitMutation>, messages: ConversationMutation, count: i64 }, Stale }

fn effect_error(error: GenerationError) -> StoreError {
    match error { GenerationError::Store(error) => error, GenerationError::Preparation(_) => invalid("send-effects") }
}

// The chat variables, the author note and the message edits in one write, over only the values the preparation read. The
// edits address the `count` rows the preparation read, which the effect's fence holds unchanged until this write.
fn variable_mutation(history: &StoredHistory, plan: &PreparedVariablePlan, keys: &SendEffectKeys, target: &Target, count: i64) -> StoreResult<EffectPlan> {
    if plan.is_empty() { return Ok(EffectPlan::Settled); }
    let written: BTreeSet<_> = plan.assignments.iter().map(|(name, _)| format!("${name}")).collect();
    if written.len() != plan.original.len() || written.iter().any(|key| !plan.original.contains_key(key)) { return Err(invalid("variable-plan")); }
    let note = history.conversation.get("note");
    if plan.note.as_ref().is_some_and(|planned| note != planned.original.as_ref()) { return Ok(EffectPlan::Stale); }
    let mut mutations = Vec::new();
    if !plan.assignments.is_empty() {
        let mut current = match history.conversation.get("scriptstate") {
            None | Some(Value::Null) => serde_json::Map::new(),
            Some(Value::Object(map)) => map.clone(),
            _ => return Ok(EffectPlan::Stale),
        };
        if plan.original.iter().any(|(key, expected)| current.get(key) != expected.as_ref()) { return Ok(EffectPlan::Stale); }
        let before = current.clone();
        for (name, value) in &plan.assignments { current.insert(format!("${name}"), value.clone()); }
        if current != before { mutations.push(UnitMutation::Set { key: keys.variables.clone(), value: Value::Object(current) }); }
    }
    if let Some(planned) = plan.note.as_ref().filter(|planned| note != Some(&planned.value)) {
        mutations.push(UnitMutation::Set { key: keys.note.clone(), value: planned.value.clone() });
    }
    if let Some(edits) = &plan.messages {
        let integer = |value: usize| i64::try_from(value).map_err(|_| invalid("variable-plan"));
        if integer(edits.base_count)? != count || edits.start + edits.delete_count > edits.base_count { return Err(invalid("variable-plan")); }
        let messages = ConversationMutation::ReplaceRange { character_id: target.character_id.clone(), conversation_id: target.conversation_id.clone(),
            start: integer(edits.start)?, delete_count: integer(edits.delete_count)?, messages: edits.rows.clone(), conversation: None, configured_index: None };
        return Ok(EffectPlan::Edits { units: mutations, messages, count: integer(edits.count())? });
    }
    Ok(if mutations.is_empty() { EffectPlan::Settled } else { EffectPlan::Mutations(mutations) })
}

fn pin_preparation_failure(record: &mut Record, error: &PreparationError) -> StoreResult<()> {
    let preparation = record.preparation.as_mut().ok_or_else(|| invalid("preparation-required"))?;
    if preparation.ready || record.execution.is_some() || preparation.error.as_ref().is_some_and(|pinned| pinned != error) {
        return Err(invalid("preparation-error-pinned"));
    }
    preparation.error = Some(error.clone());
    preparation.entropy = None;
    preparation.host = None;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectReceipt {
    pub(crate) request_id: String,
    pub(crate) digest: String,
    pub(crate) revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectsEvidence {
    pub(crate) affected_unit_keys: Vec<String>,
    pub(crate) receipts: Vec<EffectReceipt>,
}

// The send metadata the first output row carries, as TS records it; the store adds the generation ID and stage 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutputInfo {
    pub(crate) model: String,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) max_context: u64,
    // Whole milliseconds from the dispatch claim to the output offer.
    pub(crate) stage3: u64,
    // Completion starts after response application and ends at the final ownership check, before the row freezes.
    pub(crate) stage4_started: Option<std::time::Instant>,
}

fn now_millis() -> StoreResult<u64> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| invalid("invalid-clock"))?.as_millis();
    u64::try_from(now).map_err(|_| invalid("invalid-clock"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct IdAssignment { index: usize, original_hash: String, id: String }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum Normalization {
    Pending { assignments: Option<Vec<IdAssignment>> },
    Settled { receipt: Option<(String, String, i64)> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Preparation {
    preset_id: String,
    normalization: Normalization,
    ready: bool,
    error: Option<PreparationError>,
    effects: SendEffects,
    // Wall-clock milliseconds bounding prompt assembly, the TS stage 1 timing.
    #[serde(default)]
    normalized_at: Option<u64>,
    #[serde(default)]
    ready_at: Option<u64>,
    // The clock reading and random draws of the reservation's admission, which later admissions and the preparation
    // replay; a record written before they were kept has none and draws afresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    entropy: Option<Entropy>,
    // The WebView's host capture of the reservation, which the preparation reads; a record written before it was kept
    // has none and declines what reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host: Option<HostCapture>,
    // The history's row count once the settled Variables write stored the start trigger's message edits, where the output
    // row goes; a record without edits, or written before they were stored, has none and keeps `initial_count`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    edited_count: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CancelReason { User, Edit, Plugin, Authority, Lifecycle }

// A compact typed classification; live transport details are not journaled.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct FailureDescriptor {
    pub(crate) category: String,
    pub(crate) detail: Option<String>,
    pub(crate) stage: Option<String>,
    pub(crate) http_status: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum TerminalDecision {
    Normal { sequence: u64, output_hash: String },
    Cancelled { reason: CancelReason },
    Failed { failure: FailureDescriptor },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct OutputReceipt {
    pub(crate) request_id: String,
    pub(crate) intent_digest: String,
    pub(crate) receipt_revision: i64,
    pub(crate) source_sequence: u64,
    pub(crate) output_hash: String,
}

// Members whose loss would widen authority are required; lost evidence only reads as unavailable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Execution {
    pub(crate) lifecycle_id: String,
    #[serde(deserialize_with = "required")]
    pub(crate) dispatch_id: Option<String>,
    #[serde(deserialize_with = "required")]
    pub(crate) cancel: Option<CancelReason>,
    #[serde(default)]
    pub(crate) terminal: Option<TerminalDecision>,
    #[serde(default)]
    pub(crate) output_receipt: Option<OutputReceipt>,
}

#[derive(Debug)]
pub(crate) enum DispatchClaim {
    Issued { dispatch_id: String, status: Status },
    Refused(Status),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutputResolution {
    Committed(OutputReceipt),
    Uncommitted,
    Unresolved,
}

#[derive(Clone, Copy)]
enum Settlement { Owner, Recovery { foreign: bool } }

enum ReceiptCheck { Verified(OutputReceipt), Missing, Mismatch }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Record {
    target: Target,
    phase: Phase,
    session: Option<String>,
    owner: Option<String>,
    message_id: String,
    message_time: u64,
    fence: Fence,
    initial_count: i64,
    checkpoint: u64,
    output_hash: Option<String>,
    committed_revision: Option<i64>,
    #[serde(deserialize_with = "required")]
    attempt: Option<Attempt>,
    #[serde(deserialize_with = "required")]
    preparation: Option<Preparation>,
    #[serde(deserialize_with = "required")]
    execution: Option<Execution>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    epoch: String,
    high_water: u64,
    #[serde(deserialize_with = "unique_records")]
    records: BTreeMap<u64, Record>,
}

fn required<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}

fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

fn unique_records<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<BTreeMap<u64, T>, D::Error> {
    struct Records<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Records<T> {
        type Value = BTreeMap<u64, T>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("unique generation ticket sequences")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut records = BTreeMap::new();
            while let Some((key, record)) = map.next_entry::<String, T>()? {
                let sequence = key.parse::<u64>().map_err(serde::de::Error::custom)?;
                if records.insert(sequence, record).is_some() {
                    return Err(serde::de::Error::custom("duplicate generation ticket sequence"));
                }
                if records.len() > MAX_RESERVED + MAX_ACTIVE + MAX_TERMINAL {
                    return Err(serde::de::Error::custom("generation journal record limit"));
                }
            }
            Ok(records)
        }
    }
    deserializer.deserialize_map(Records(std::marker::PhantomData))
}

// Version 2 had no variable effect. These readers keep its enum and required members independent of version 3.
#[derive(Deserialize)]
enum V2SendEffect { Interaction, Memory }

#[derive(Deserialize)]
enum V2AttemptKind {
    Output { sequence: u64, output_hash: String },
    HistoryIds,
    Effect { effect: V2SendEffect },
}

impl V2AttemptKind {
    fn into_current(self) -> AttemptKind {
        match self {
            Self::Output { sequence, output_hash } => AttemptKind::Output { sequence, output_hash },
            Self::HistoryIds => AttemptKind::HistoryIds,
            Self::Effect { effect } => AttemptKind::Effect { effect: match effect {
                V2SendEffect::Interaction => SendEffect::Interaction, V2SendEffect::Memory => SendEffect::Memory,
            } },
        }
    }
}

#[derive(Deserialize)]
struct V2Attempt { request_id: String, intent_digest: String, expected_revision: i64, kind: V2AttemptKind }

impl V2Attempt {
    fn into_current(self) -> Attempt {
        Attempt { request_id: self.request_id, intent_digest: self.intent_digest, expected_revision: self.expected_revision, kind: self.kind.into_current(),
            edited_count: None }
    }
}

#[derive(Deserialize)]
struct V2SendEffects { interaction: EffectState, memory: EffectState }

impl V2SendEffects {
    fn absent() -> Self { Self { interaction: EffectState::Settled { receipt: None }, memory: EffectState::Settled { receipt: None } } }
}

#[derive(Deserialize)]
struct V2Preparation {
    preset_id: String,
    normalization: Normalization,
    ready: bool,
    error: Option<PreparationError>,
    #[serde(default = "V2SendEffects::absent")]
    effects: V2SendEffects,
    #[serde(default)]
    normalized_at: Option<u64>,
    #[serde(default)]
    ready_at: Option<u64>,
    #[serde(default)]
    entropy: Option<Entropy>,
    #[serde(default)]
    host: Option<HostCapture>,
}

impl V2Preparation {
    fn into_current(self) -> Preparation {
        Preparation { preset_id: self.preset_id, normalization: self.normalization, ready: self.ready, error: self.error,
            effects: SendEffects { interaction: self.effects.interaction, memory: self.effects.memory, ..SendEffects::absent() },
            normalized_at: self.normalized_at, ready_at: self.ready_at, entropy: self.entropy, host: self.host, edited_count: None }
    }
}

#[derive(Deserialize)]
struct V2Record {
    target: Target,
    phase: Phase,
    session: Option<String>,
    owner: Option<String>,
    message_id: String,
    message_time: u64,
    fence: Fence,
    initial_count: i64,
    checkpoint: u64,
    output_hash: Option<String>,
    committed_revision: Option<i64>,
    #[serde(deserialize_with = "required")]
    attempt: Option<V2Attempt>,
    #[serde(deserialize_with = "required")]
    preparation: Option<V2Preparation>,
    #[serde(deserialize_with = "required")]
    execution: Option<Execution>,
}

impl V2Record {
    fn into_current(self) -> Record {
        Record { target: self.target, phase: self.phase, session: self.session, owner: self.owner, message_id: self.message_id,
            message_time: self.message_time, fence: self.fence, initial_count: self.initial_count, checkpoint: self.checkpoint,
            output_hash: self.output_hash, committed_revision: self.committed_revision, attempt: self.attempt.map(V2Attempt::into_current),
            preparation: self.preparation.map(V2Preparation::into_current), execution: self.execution }
    }
}

#[derive(Deserialize)]
struct V2Journal {
    epoch: String,
    high_water: u64,
    #[serde(deserialize_with = "unique_records")]
    records: BTreeMap<u64, V2Record>,
}

impl V2Journal {
    fn into_current(self) -> Journal {
        Journal { epoch: self.epoch, high_water: self.high_water, records: self.records.into_iter().map(|(id, record)| (id, record.into_current())).collect() }
    }
}

// The two experimental shapes written before the envelope: 3c31b2fa8 had no preparation member and
// direct output attempts; c0d0490dd added preparation and kinded attempts.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyAttempt {
    request_id: String,
    intent_digest: String,
    expected_revision: i64,
    #[serde(default, deserialize_with = "present")]
    kind: Option<V2AttemptKind>,
    #[serde(default, deserialize_with = "present")]
    sequence: Option<u64>,
    #[serde(default, deserialize_with = "present")]
    output_hash: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyRecord {
    target: Target,
    phase: Phase,
    session: Option<String>,
    owner: Option<String>,
    message_id: String,
    message_time: u64,
    fence: Fence,
    initial_count: i64,
    checkpoint: u64,
    output_hash: Option<String>,
    committed_revision: Option<i64>,
    #[serde(deserialize_with = "required")]
    attempt: Option<LegacyAttempt>,
    #[serde(default, deserialize_with = "present")]
    preparation: Option<Option<V2Preparation>>,
}

#[derive(Deserialize)]
struct LegacyRecords(#[serde(deserialize_with = "unique_records")] BTreeMap<u64, LegacyRecord>);

#[derive(Default)]
struct JournalMembers<'a> {
    version: Option<&'a RawValue>,
    journal: Option<&'a RawValue>,
    epoch: Option<&'a RawValue>,
    high_water: Option<&'a RawValue>,
    records: Option<&'a RawValue>,
    unknown: bool,
}

impl<'de> Deserialize<'de> for JournalMembers<'de> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Members;
        impl<'de> serde::de::Visitor<'de> for Members {
            type Value = JournalMembers<'de>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a native generation journal object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut seen = BTreeSet::new();
                let mut members = JournalMembers::default();
                while let Some(key) = map.next_key::<String>()? {
                    if !seen.insert(key.clone()) { return Err(serde::de::Error::custom("duplicate generation journal member")); }
                    let slot = match key.as_str() {
                        "version" => &mut members.version,
                        "journal" => &mut members.journal,
                        "epoch" => &mut members.epoch,
                        "high_water" => &mut members.high_water,
                        "records" => &mut members.records,
                        _ => { map.next_value::<serde::de::IgnoredAny>()?; members.unknown = true; continue; }
                    };
                    *slot = Some(map.next_value::<&'de RawValue>()?);
                }
                Ok(members)
            }
        }
        deserializer.deserialize_map(Members)
    }
}

struct DecodedJournal { journal: Journal, conversion_needed: bool }

// Every reader decodes through here. An explicit version never falls back to legacy decoding.
fn decode_journal(body: &str) -> StoreResult<DecodedJournal> {
    let format = |_: serde_json::Error| invalid("journal-format");
    let members: JournalMembers = serde_json::from_str(body).map_err(format)?;
    let envelope = members.version.is_some() || members.journal.is_some();
    let legacy = members.epoch.is_some() || members.high_water.is_some() || members.records.is_some();
    if envelope == legacy { return Err(invalid("journal-shape")); }
    if envelope {
        let version = members.version.ok_or_else(|| invalid("journal-shape"))?;
        let version = serde_json::from_str::<u64>(version.get()).map_err(|_| invalid("journal-version"))?;
        if !matches!(version, 2 | JOURNAL_VERSION) { return Err(invalid("journal-version")); }
        let body = members.journal.ok_or_else(|| invalid("journal-shape"))?.get();
        return match version {
            2 => Ok(DecodedJournal { journal: serde_json::from_str::<V2Journal>(body).map_err(format)?.into_current(), conversion_needed: true }),
            _ => Ok(DecodedJournal { journal: serde_json::from_str(body).map_err(format)?, conversion_needed: false }),
        };
    }
    let (Some(epoch), Some(high_water), Some(records), false) = (members.epoch, members.high_water, members.records, members.unknown) else {
        return Err(invalid("journal-shape"));
    };
    let LegacyRecords(legacy_records) = serde_json::from_str(records.get()).map_err(format)?;
    let prepared = legacy_records.values().next().map(|record| record.preparation.is_some());
    let mut records = BTreeMap::new();
    for (sequence, record) in legacy_records {
        if Some(record.preparation.is_some()) != prepared { return Err(invalid("journal-shape")); }
        let attempt = match record.attempt {
            None => None,
            Some(attempt) => Some(Attempt {
                kind: match (record.preparation.is_some(), attempt.kind, attempt.sequence, attempt.output_hash) {
                    (true, Some(kind), None, None) => kind.into_current(),
                    (false, None, Some(sequence), Some(output_hash)) => AttemptKind::Output { sequence, output_hash },
                    _ => return Err(invalid("journal-shape")),
                },
                request_id: attempt.request_id, intent_digest: attempt.intent_digest, expected_revision: attempt.expected_revision,
                edited_count: None,
            }),
        };
        records.insert(sequence, Record {
            target: record.target, phase: record.phase, session: record.session, owner: record.owner,
            message_id: record.message_id, message_time: record.message_time, fence: record.fence,
            initial_count: record.initial_count, checkpoint: record.checkpoint, output_hash: record.output_hash,
            committed_revision: record.committed_revision, attempt, preparation: record.preparation.flatten().map(V2Preparation::into_current), execution: None,
        });
    }
    let journal = Journal {
        epoch: serde_json::from_str(epoch.get()).map_err(format)?,
        high_water: serde_json::from_str(high_water.get()).map_err(format)?,
        records,
    };
    Ok(DecodedJournal { journal, conversion_needed: true })
}

fn encode_journal(journal: &Journal) -> StoreResult<String> {
    #[derive(Serialize)]
    struct Envelope<'a> { version: u64, journal: &'a Journal }
    Ok(serde_json::to_string(&Envelope { version: JOURNAL_VERSION, journal })?)
}

fn descriptor_token(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_DESCRIPTOR_BYTES
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_decision(decision: &TerminalDecision) -> bool {
    match decision {
        TerminalDecision::Normal { sequence, output_hash } => *sequence > 0 && digest(output_hash),
        TerminalDecision::Cancelled { .. } => true,
        TerminalDecision::Failed { failure } => descriptor_token(&failure.category)
            && failure.detail.as_deref().is_none_or(descriptor_token) && failure.stage.as_deref().is_none_or(descriptor_token)
            && failure.http_status.is_none_or(|code| (100..=599).contains(&code)),
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum NativeGenerationTarget<'a> {
    Character { character_id: &'a str },
    Conversation { character_id: &'a str, conversation_id: &'a str },
}

// Callers own the paired store admission and device-before-library write guards through commit.
pub(crate) fn has_protected_target(device: &Connection, library: &Connection, target: NativeGenerationTarget<'_>) -> StoreResult<bool> {
    has_protected_target_limited(device, library, target, MAX_PROTECTION_JOURNAL_BYTES)
}

fn has_protected_target_limited(device: &Connection, library: &Connection, target: NativeGenerationTarget<'_>, max_bytes: usize) -> StoreResult<bool> {
    for connection in [device, library] {
        if connection.transaction_state(Some(rusqlite::MAIN_DB))? != rusqlite::TransactionState::Write {
            return Err(invalid("protection-write-guard"));
        }
    }
    let (character_id, conversation_id) = match target {
        NativeGenerationTarget::Character { character_id } => (character_id, None),
        NativeGenerationTarget::Conversation { character_id, conversation_id } => (character_id, Some(conversation_id)),
    };
    if character_id.is_empty() || conversation_id.is_some_and(str::is_empty) { return Err(invalid("invalid-target")); }
    let library_identity = super::active_generation(library)?;
    if library_identity.is_empty() { return Err(invalid("protection-identity")); }
    let authority = super::lww::authority(device)?;
    // This bounds the read, not all historical LWW request IDs captured in a fence.
    let Some(DecodedJournal { journal, .. }) = read_journal_limited(device, max_bytes)? else { return Ok(false) };
    validate_protection_journal(&journal)?;
    Ok(journal.records.values().any(|record| {
        record.target.library_identity == library_identity && record.target.binding_authority == authority
            && record.target.character_id == character_id
            && conversation_id.is_none_or(|id| record.target.conversation_id == id)
            && (record.phase == Phase::Running || record.attempt.is_some())
    }))
}

fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}

fn digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_protection_journal(journal: &Journal) -> StoreResult<()> {
    if !canonical_uuid(&journal.epoch) { return Err(invalid("protection-journal")); }
    let (mut reserved, mut active, mut terminal) = (0, 0, 0);
    for (sequence, record) in &journal.records {
        let target = &record.target;
        if *sequence == 0 || *sequence > journal.high_water || target.library_identity.is_empty()
            || target.character_id.is_empty() || target.character_id.len() > 512
            || target.conversation_id.is_empty() || target.conversation_id.len() > 512
            || target.expected_revision < 0 || record.initial_count < 0 {
            return Err(invalid("protection-journal"));
        }
        if record.phase == Phase::Running || record.attempt.is_some() {
            active += 1;
            if !record.session.as_deref().is_some_and(canonical_uuid) || !record.owner.as_deref().is_some_and(canonical_uuid) {
                return Err(invalid("protection-owner"));
            }
        } else if record.phase == Phase::Reserved { reserved += 1; } else { terminal += 1; }
        if let Some(attempt) = &record.attempt {
            if !canonical_uuid(&attempt.request_id) || !digest(&attempt.intent_digest) || attempt.expected_revision < 0
                || attempt.edited_count.is_some_and(|count| count < 0 || count > MAX_HISTORY_MESSAGES
                    || attempt.kind != AttemptKind::Effect { effect: SendEffect::Variables }) {
                return Err(invalid("protection-attempt"));
            }
            match &attempt.kind {
                AttemptKind::Output { sequence, output_hash } => {
                    if *sequence == 0 || !digest(output_hash) { return Err(invalid("protection-attempt")); }
                }
                AttemptKind::Effect { effect } => {
                    let Some(preparation) = &record.preparation else { return Err(invalid("protection-attempt")) };
                    let phase = match effect {
                        SendEffect::Interaction => !preparation.ready && matches!(preparation.normalization, Normalization::Pending { assignments: None }),
                        SendEffect::Variables => matches!(preparation.normalization, Normalization::Settled { .. })
                            && effect_settled(record, SendEffect::Interaction) && !effect_settled(record, SendEffect::Memory)
                            && if preparation.ready { preparation.error.is_none() && record.execution.as_ref().is_some_and(|execution| execution.dispatch_id.is_none()) }
                                else { preparation.error.is_some() && record.execution.is_none() },
                        SendEffect::Memory => preparation.ready && matches!(preparation.normalization, Normalization::Settled { .. })
                            && effect_settled(record, SendEffect::Variables)
                            && record.execution.as_ref().is_some_and(|execution| execution.dispatch_id.is_none()),
                    };
                    if !phase || *preparation.effects.get(*effect) != EffectState::Pending { return Err(invalid("protection-attempt")); }
                }
                AttemptKind::HistoryIds => {
                    let Some(Preparation { normalization: Normalization::Pending { assignments: Some(assignments) }, .. }) = &record.preparation else {
                        return Err(invalid("protection-attempt"));
                    };
                    if assignments.is_empty() || assignments.len() > MAX_HISTORY_MESSAGES as usize { return Err(invalid("protection-attempt")); }
                    let mut previous = None;
                    for assignment in assignments {
                        if assignment.index as u64 >= record.initial_count as u64
                            || previous.is_some_and(|index| index >= assignment.index)
                            || !canonical_uuid(&assignment.id) || !digest(&assignment.original_hash) {
                            return Err(invalid("protection-attempt"));
                        }
                        previous = Some(assignment.index);
                    }
                }
            }
        }
        if let Some(preparation) = &record.preparation {
            if !effect_settled(record, SendEffect::Interaction)
                && (preparation.ready || !matches!(preparation.normalization, Normalization::Pending { assignments: None })) {
                return Err(invalid("protection-effects"));
            }
            // Only a receipt-backed Variables write stores message edits.
            if preparation.edited_count.is_some_and(|count| count < 0 || count > MAX_HISTORY_MESSAGES
                || !matches!(preparation.effects.variables, EffectState::Settled { receipt: Some(_) })) {
                return Err(invalid("protection-effects"));
            }
            if preparation.error.is_some() && (preparation.ready || record.execution.is_some() || record.phase == Phase::Reserved
                || (record.phase == Phase::Running && !matches!(preparation.normalization, Normalization::Settled { .. }))
                || record.attempt.as_ref().is_some_and(|attempt| !matches!(attempt.kind, AttemptKind::Effect { effect: SendEffect::Variables }))) {
                return Err(invalid("protection-preparation"));
            }
            if record.execution.as_ref().is_some_and(|execution| execution.dispatch_id.is_some())
                && [SendEffect::Interaction, SendEffect::Variables, SendEffect::Memory].into_iter().any(|effect| !effect_settled(record, effect)) {
                return Err(invalid("protection-effects"));
            }
        }
        if let Some(execution) = &record.execution {
            let ready = record.preparation.as_ref().is_some_and(|value| value.ready && matches!(value.normalization, Normalization::Settled { .. }));
            if record.phase == Phase::Reserved || !ready || !canonical_uuid(&execution.lifecycle_id)
                || execution.dispatch_id.as_deref().is_some_and(|id| !canonical_uuid(id))
                || execution.terminal.as_ref().is_some_and(|decision| !valid_decision(decision)
                    || (matches!(decision, TerminalDecision::Normal { .. }) && execution.dispatch_id.is_none())) {
                return Err(invalid("protection-execution"));
            }
            if let Some(receipt) = &execution.output_receipt {
                if execution.dispatch_id.is_none() || !canonical_uuid(&receipt.request_id) || !digest(&receipt.intent_digest)
                    || receipt.receipt_revision <= target.expected_revision || receipt.source_sequence == 0
                    || receipt.source_sequence > record.checkpoint || !digest(&receipt.output_hash)
                    || record.output_hash.as_deref() != Some(receipt.output_hash.as_str()) {
                    return Err(invalid("protection-execution"));
                }
            }
        }
    }
    if reserved > MAX_RESERVED || active > MAX_ACTIVE || terminal > MAX_TERMINAL { return Err(invalid("protection-journal-limit")); }
    Ok(())
}

impl Journal {
    fn new() -> Self {
        Self { epoch: uuid::Uuid::new_v4().to_string(), high_water: 0, records: BTreeMap::new() }
    }

    fn record(&self, ticket: &Ticket) -> StoreResult<&Record> {
        if ticket.epoch != self.epoch { return Err(invalid("expired-ticket")); }
        if ticket.sequence.0 == 0 || ticket.sequence.0 > self.high_water { return Err(invalid("unknown-ticket")); }
        self.records.get(&ticket.sequence.0).ok_or_else(|| invalid("expired-ticket"))
    }

    fn prune(&mut self) {
        for (maximum, reserved) in [(MAX_RESERVED, true), (MAX_TERMINAL, false)] {
            let eligible: Vec<_> = self.records.iter().filter(|(_, record)| {
                record.attempt.is_none() && if reserved { record.phase == Phase::Reserved } else { record.phase.terminal() }
            }).map(|(sequence, _)| *sequence).collect();
            for sequence in eligible.iter().take(eligible.len().saturating_sub(maximum)) {
                self.records.remove(sequence);
            }
        }
    }
}

// The byte cap applies in SQL before the body is materialized.
fn read_journal_limited(db: &Connection, max_bytes: usize) -> StoreResult<Option<DecodedJournal>> {
    let body: Option<Option<String>> = db.query_row(
        "SELECT CASE WHEN length(CAST(value AS BLOB))<=?2 THEN value ELSE NULL END FROM device_settings WHERE key=?1",
        params![JOURNAL_KEY, i64::try_from(max_bytes).unwrap_or(i64::MAX)], |row| row.get(0)).optional()?;
    body.map(|body| decode_journal(&body.ok_or_else(|| invalid("protection-journal-limit"))?)).transpose()
}

fn read_journal(db: &Connection) -> StoreResult<Option<Journal>> {
    Ok(read_journal_limited(db, MAX_PROTECTION_JOURNAL_BYTES)?.map(|decoded| decoded.journal))
}

// Writers replace only the current envelope; recognized older formats convert in their own transaction first.
fn read_current_journal(db: &Connection) -> StoreResult<Option<Journal>> {
    match read_journal_limited(db, MAX_PROTECTION_JOURNAL_BYTES)? {
        Some(DecodedJournal { conversion_needed: true, .. }) => Err(invalid("journal-conversion-required")),
        decoded => Ok(decoded.map(|decoded| decoded.journal)),
    }
}

fn write_journal(db: &Connection, journal: &Journal) -> StoreResult<()> {
    db.execute("INSERT INTO device_settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![JOURNAL_KEY, encode_journal(journal)?])?;
    Ok(())
}

fn status(ticket: &Ticket, record: &Record) -> Status {
    Status { ticket: ticket.clone(), phase: record.phase.clone(), message_id: record.message_id.clone(),
        checkpoint: record.checkpoint.into(), committed_revision: record.committed_revision, pending_commit: record.attempt.is_some(),
        preparation_ready: record.preparation.as_ref().is_some_and(|value| value.ready),
        preparation_error: record.preparation.as_ref().and_then(|value| value.error.clone()) }
}

// The output row follows the history the send stored before dispatch, its message edits included.
fn output_index(record: &Record) -> i64 {
    record.preparation.as_ref().and_then(|preparation| preparation.edited_count).unwrap_or(record.initial_count)
}

fn require_owner(record: &Record, owner: &Owner) -> StoreResult<()> {
    if record.session.as_deref() != Some(owner.session.as_str()) || record.owner.as_deref() != Some(owner.token.as_str()) {
        return Err(invalid("stale-owner"));
    }
    Ok(())
}

// Provider work and renderer callbacks never hold this gate. It covers only one issued store segment.
fn segment_gate(ticket: &Ticket) -> StoreResult<Arc<Mutex<()>>> {
    static GATES: OnceLock<Mutex<BTreeMap<String, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut gates = GATES.get_or_init(|| Mutex::new(BTreeMap::new())).lock().map_err(|_| invalid("segment-poisoned"))?;
    gates.retain(|_, gate| gate.strong_count() > 0);
    let key = format!("{}:{}", ticket.epoch, ticket.sequence.0);
    if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) { return Ok(gate); }
    let gate = Arc::new(Mutex::new(()));
    gates.insert(key, Arc::downgrade(&gate));
    Ok(gate)
}

struct DeviceBarrier { _connection: Connection }

fn row_hash(row: &Value) -> StoreResult<String> {
    Ok(risunest_sync_wire::hash(&serde_json::to_vec(row)?))
}

fn normalized_history_fits(messages: &[Value], max_bytes: usize) -> StoreResult<()> {
    let mut bytes = 0usize;
    for row in messages {
        let mut normalized;
        let value = if row["chatId"].is_null() {
            normalized = row.clone();
            normalized.as_object_mut().ok_or_else(|| invalid("invalid-message"))?
                .insert("chatId".into(), json!("00000000-0000-4000-8000-000000000000"));
            &normalized
        } else { row };
        bytes = bytes.checked_add(serde_json::to_vec(value)?.len()).ok_or_else(|| invalid("history-limit"))?;
        if bytes > max_bytes { return Err(invalid("history-limit")); }
    }
    Ok(())
}

fn preparation_ready(record: &Record) -> bool {
    record.preparation.as_ref().is_none_or(|value| value.ready && matches!(value.normalization, Normalization::Settled { .. }))
}

fn unit_fence(db: &Connection, parts: &[&str]) -> StoreResult<Option<UnitFence>> {
    let key = UnitKey::new(parts).map_err(|_| invalid("invalid-target"))?;
    Ok(db.query_row("SELECT stamp,version FROM lww_units WHERE key=?1", [key.as_str()],
        |row| Ok(UnitFence { stamp: row.get(0)?, version: row.get(1)? })).optional()?)
}

fn capture_fence(db: &Connection, target: &Target, count: i64) -> StoreResult<Fence> {
    Ok(Fence {
        character: unit_fence(db, &["exists", "character", &target.character_id])?,
        conversation: unit_fence(db, &["exists", "conversation", &target.character_id, &target.conversation_id])?,
        messages: unit_fence(db, &["messages", &target.character_id, &target.conversation_id])?, count,
    })
}

fn normalization_commit(target: &Target, messages: &[Value], assignments: &[IdAssignment], expected_revision: i64, request_id: &str) -> StoreResult<WorkingSetCommit> {
    let expected_indices: Vec<_> = messages.iter().enumerate().filter_map(|(index, row)| row["chatId"].is_null().then_some(index)).collect();
    if assignments.iter().map(|entry| entry.index).collect::<Vec<_>>() != expected_indices { return Err(invalid("assignment-integrity")); }
    let mut ranges: Vec<(usize, Vec<Value>)> = Vec::new();
    for assignment in assignments {
        let row = &messages[assignment.index];
        if row_hash(row)? != assignment.original_hash || !canonical_uuid(&assignment.id) { return Err(invalid("assignment-integrity")); }
        let mut row = row.clone();
        row.as_object_mut().ok_or_else(|| invalid("invalid-message"))?.insert("chatId".into(), json!(assignment.id));
        match ranges.last_mut() {
            Some((start, rows)) if *start + rows.len() == assignment.index => rows.push(row),
            _ => ranges.push((assignment.index, vec![row])),
        }
    }
    Ok(WorkingSetCommit {
        expected_revision, request_id: Some(request_id.into()), binding_authority: Some(target.binding_authority),
        conversations: Some(ranges.into_iter().map(|(start, messages)| ConversationMutation::ReplaceRange {
            character_id: target.character_id.clone(), conversation_id: target.conversation_id.clone(),
            start: start as i64, delete_count: messages.len() as i64, messages, conversation: None, configured_index: None,
        }).collect()), ..Default::default()
    })
}

fn projection_identity(db: &Connection) -> StoreResult<ProjectionIdentity> {
    let identity = super::sync_selection::identity(db)?;
    Ok(ProjectionIdentity { store_id: identity.store_id, library_epoch: identity.library_epoch,
        generation: identity.generation, selection_epoch: identity.selection_epoch })
}

fn original_id_state(row: &Value) -> OriginalIdState {
    match row.get("chatId") { None => OriginalIdState::Missing, Some(Value::Null) => OriginalIdState::Null, _ => OriginalIdState::Present }
}

fn normalization_evidence(record: &Record) -> StoreResult<NormalizationEvidence> {
    Ok(match &record.preparation.as_ref().ok_or_else(|| invalid("preparation-required"))?.normalization {
        Normalization::Pending { .. } if record.phase.terminal() && record.attempt.is_none() => NormalizationEvidence::NoIdEffects,
        Normalization::Pending { .. } => NormalizationEvidence::Pending,
        Normalization::Settled { receipt: None } => NormalizationEvidence::NoIdEffects,
        Normalization::Settled { receipt: Some((request_id, intent_digest, receipt_revision)) } => {
            if !canonical_uuid(request_id) || !digest(intent_digest) || *receipt_revision <= record.target.expected_revision {
                return Err(invalid("receipt-integrity"));
            }
            NormalizationEvidence::Recorded(NormalizationReceipt { target: record.target.clone(), request_id: request_id.clone(),
                intent_digest: intent_digest.clone(), receipt_revision: *receipt_revision })
        }
    })
}

fn normalization_receipt_matches(db: &Connection, receipt: &NormalizationReceipt) -> StoreResult<bool> {
    let actual: Option<(String, i64)> = db.query_row("SELECT digest,revision FROM lww_requests WHERE request_id=?1",
        [&receipt.request_id], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
    Ok(actual == Some((receipt.intent_digest.clone(), receipt.receipt_revision)))
}

fn projection_count(db: &Connection, target: &Target) -> StoreResult<Option<i64>> {
    Ok(db.query_row("SELECT message_count FROM conversations WHERE generation=?1 AND character_id=?2 AND conversation_id=?3",
        params![target.library_identity, target.character_id, target.conversation_id], |row| row.get(0)).optional()?)
}

fn unavailable(reason: ProjectionUnavailable) -> NormalizationMapping { NormalizationMapping::Unavailable(reason) }

#[derive(Clone, Copy)]
struct ProjectionLimits { history_rows: usize, history_bytes: usize, resident_rows: usize }

const PROJECTION_LIMITS: ProjectionLimits = ProjectionLimits {
    history_rows: MAX_HISTORY_MESSAGES as usize, history_bytes: MAX_HISTORY_BYTES, resident_rows: MAX_PROJECTION_ROWS,
};

impl PersistentStore {
    fn generation_projection_record(&self, ticket: &Ticket) -> StoreResult<Record> {
        let journal = read_journal(self.device_store()?.connection())?.ok_or_else(|| invalid("expired-ticket"))?;
        validate_protection_journal(&journal)?;
        Ok(journal.record(ticket)?.clone())
    }

    fn generation_projection_rows(&self, target: &Target, lease: &GenerationLease, start: usize, count: usize,
        max_rows: usize, max_bytes: usize) -> StoreResult<Result<Vec<Value>, ProjectionUnavailable>> {
        if count > max_rows { return Ok(Err(ProjectionUnavailable::Limit)); }
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        while rows.len() < count {
            let Some(page) = self.read_conversation_window(&ConversationWindowQuery {
                character_id: target.character_id.clone(), conversation_id: target.conversation_id.clone(),
                start_index: Some((start + rows.len()) as i64), limit: Some((count - rows.len()).min(128) as i64),
                anchor_message_id: None, anchor_occurrence: None, before: None, after: None, skip_parser_work: None,
            }, Some(&lease.token))? else { return Ok(Err(ProjectionUnavailable::HistoryChanged)) };
            if page.revision != lease.revision || page.value.messages.is_empty() { return Ok(Err(ProjectionUnavailable::HistoryChanged)); }
            for row in &page.value.messages {
                let Some(next) = bytes.checked_add(serde_json::to_vec(row)?.len()) else { return Ok(Err(ProjectionUnavailable::Limit)) };
                bytes = next;
                if bytes > max_bytes { return Ok(Err(ProjectionUnavailable::Limit)); }
            }
            rows.extend(page.value.messages);
        }
        Ok(Ok(rows))
    }

    pub(crate) fn generation_capture_normalization_witness(&self, ticket: &Ticket, original: &GenerationLease, indices: &[usize]) -> StoreResult<NormalizationWitness> {
        self.generation_capture_normalization_witness_limited(ticket, original, indices, PROJECTION_LIMITS)
    }

    fn generation_capture_normalization_witness_limited(&self, ticket: &Ticket, original: &GenerationLease, indices: &[usize], limits: ProjectionLimits) -> StoreResult<NormalizationWitness> {
        let gate = segment_gate(ticket)?;
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let record = self.generation_projection_record(ticket)?;
        if record.preparation.is_none() { return Err(invalid("preparation-required")); }
        if indices.len() > limits.resident_rows { return Err(invalid("projection-limit")); }
        if !self.revision_leases.contains_key(&original.token) { return Err(invalid("projection-lease-unavailable")); }
        let (db, read) = self.read_view(Some(&original.token))?;
        if read.revision != original.revision || original.revision != record.target.expected_revision { return Err(invalid("projection-stale-lease")); }
        let identity = projection_identity(db)?;
        if identity != projection_identity(&self.connection)? || identity.generation != record.target.library_identity
            || self.lww_binding_authority()? != record.target.binding_authority { return Err(invalid("projection-identity")); }
        let count = projection_count(db, &record.target)?.ok_or_else(|| invalid("projection-history"))?;
        if count != record.initial_count || count < 0 { return Err(invalid("projection-history")); }
        if count > MAX_HISTORY_MESSAGES || usize::try_from(count).ok().is_none_or(|count| count > limits.history_rows) { return Err(invalid("projection-limit")); }
        let count = count as usize;
        if indices.iter().any(|index| *index >= count) || indices.windows(2).any(|pair| pair[0] >= pair[1]) { return Err(invalid("projection-witness")); }
        let rows = self.generation_projection_rows(&record.target, original, 0, count, limits.history_rows, limits.history_bytes)?
            .map_err(|reason| invalid(if reason == ProjectionUnavailable::Limit { "projection-limit" } else { "projection-history" }))?;
        let rows = indices.iter().map(|index| Ok(OriginalRowWitness { original_index: *index,
            row_hash: row_hash(&rows[*index])?, id_state: original_id_state(&rows[*index]) })).collect::<StoreResult<Vec<_>>>()?;
        if identity != projection_identity(&self.connection)? || self.lww_binding_authority()? != record.target.binding_authority { return Err(invalid("projection-identity")); }
        Ok(NormalizationWitness { ticket: ticket.clone(), target: record.target, identity,
            original_revision: original.revision, original_count: count, rows })
    }

    pub(crate) fn generation_resolve_normalization(&mut self, ticket: &Ticket) -> StoreResult<NormalizationResolution> {
        let previous = self.generation_projection_record(ticket)?;
        if self.library_identity()? != previous.target.library_identity || self.lww_binding_authority()? != previous.target.binding_authority {
            return Ok(NormalizationResolution { status: status(ticket, &previous), evidence: normalization_evidence(&previous)? });
        }
        self.generation_recover(ticket)?;
        let gate = segment_gate(ticket)?;
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let record = self.generation_projection_record(ticket)?;
        let mut evidence = normalization_evidence(&record)?;
        if let NormalizationEvidence::Recorded(receipt) = &evidence {
            if self.library_identity()? == record.target.library_identity && self.lww_binding_authority()? == record.target.binding_authority {
                if self.revision()? < receipt.receipt_revision || !normalization_receipt_matches(&self.connection, receipt)? { return Err(invalid("receipt-integrity")); }
                evidence = NormalizationEvidence::Verified(receipt.clone());
            }
        }
        if self.library_identity()? != record.target.library_identity || self.lww_binding_authority()? != record.target.binding_authority {
            evidence = normalization_evidence(&record)?;
        }
        Ok(NormalizationResolution { status: status(ticket, &record), evidence })
    }

    pub(crate) fn generation_project_normalization(&self, ticket: &Ticket, witness: &NormalizationWitness,
        original: &GenerationLease, current: &GenerationLease) -> StoreResult<NormalizationProjectionResult> {
        self.generation_project_normalization_limited(ticket, witness, original, current, PROJECTION_LIMITS)
    }

    fn generation_project_normalization_limited(&self, ticket: &Ticket, witness: &NormalizationWitness,
        original: &GenerationLease, current: &GenerationLease, limits: ProjectionLimits) -> StoreResult<NormalizationProjectionResult> {
        let gate = segment_gate(ticket)?;
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let record = match self.generation_projection_record(ticket) {
            Ok(record) => record,
            Err(StoreError::Validation { message }) if message == "native-generation:expired-ticket" => return Ok(NormalizationProjectionResult {
                status: None, evidence: NormalizationEvidence::Unknown, mapping: unavailable(ProjectionUnavailable::ExpiredTicket),
            }),
            Err(error) => return Err(error),
        };
        let mut evidence = normalization_evidence(&record)?;
        let mut mapping = self.generation_normalization_mapping(ticket, &record, witness, original, current, limits, &mut evidence)?;
        if witness.identity != projection_identity(&self.connection)? || self.lww_binding_authority()? != record.target.binding_authority {
            evidence = normalization_evidence(&record)?;
            mapping = unavailable(ProjectionUnavailable::IdentityChanged);
        }
        Ok(NormalizationProjectionResult { status: Some(status(ticket, &record)), evidence, mapping })
    }

    fn generation_normalization_mapping(&self, ticket: &Ticket, record: &Record, witness: &NormalizationWitness,
        original: &GenerationLease, current: &GenerationLease, limits: ProjectionLimits, evidence: &mut NormalizationEvidence) -> StoreResult<NormalizationMapping> {
        use ProjectionUnavailable as U;
        if record.attempt.is_some() || matches!(evidence, NormalizationEvidence::Pending) { return Ok(unavailable(U::Pending)); }
        if witness.ticket != *ticket || witness.target != record.target || witness.original_revision != record.target.expected_revision
            || i64::try_from(witness.original_count).ok() != Some(record.initial_count) { return Ok(unavailable(U::InvalidWitness)); }
        if witness.rows.len() > limits.resident_rows { return Ok(unavailable(U::Limit)); }
        if witness.rows.iter().any(|row| row.original_index >= witness.original_count || !digest(&row.row_hash))
            || witness.rows.windows(2).any(|pair| pair[0].original_index >= pair[1].original_index) { return Ok(unavailable(U::InvalidWitness)); }
        if !self.revision_leases.contains_key(&original.token) || !self.revision_leases.contains_key(&current.token) { return Ok(unavailable(U::LeaseUnavailable)); }
        let (old_db, old_read) = self.read_view(Some(&original.token))?;
        let (new_db, new_read) = self.read_view(Some(&current.token))?;
        if old_read.revision != original.revision || original.revision != record.target.expected_revision || new_read.revision != current.revision
            || current.revision < original.revision { return Ok(unavailable(U::StaleLease)); }
        let identity = projection_identity(old_db)?;
        if identity != witness.identity || identity != projection_identity(new_db)? || identity != projection_identity(&self.connection)?
            || identity.generation != record.target.library_identity || self.lww_binding_authority()? != record.target.binding_authority { return Ok(unavailable(U::IdentityChanged)); }
        if record.initial_count < 0 || record.initial_count > MAX_HISTORY_MESSAGES || witness.original_count > limits.history_rows { return Ok(unavailable(U::Limit)); }
        let receipt = match evidence {
            NormalizationEvidence::NoIdEffects => {
                let rows = match self.generation_projection_rows(&record.target, original, 0, witness.original_count, limits.history_rows, limits.history_bytes)? {
                    Ok(rows) => rows, Err(reason) => return Ok(unavailable(reason)),
                };
                for row in &witness.rows {
                    if row.row_hash != row_hash(&rows[row.original_index])? || row.id_state != original_id_state(&rows[row.original_index]) { return Ok(unavailable(U::InvalidWitness)); }
                }
                return Ok(NormalizationMapping::NoIdEffects);
            }
            NormalizationEvidence::Recorded(receipt) => receipt.clone(),
            _ => return Ok(unavailable(U::Pending)),
        };
        if current.revision < receipt.receipt_revision { return Ok(unavailable(U::StaleLease)); }
        if !normalization_receipt_matches(new_db, &receipt)? { return Ok(unavailable(U::ReceiptIntegrity)); }
        *evidence = NormalizationEvidence::Verified(receipt.clone());
        let count = record.initial_count;
        let Some(current_count) = projection_count(new_db, &record.target)? else { return Ok(unavailable(U::HistoryChanged)) };
        if count < 0 || projection_count(old_db, &record.target)? != Some(count) { return Ok(unavailable(U::HistoryChanged)); }
        let current_fence = capture_fence(new_db, &record.target, current_count)?;
        if current_fence != record.fence || current_fence.character.is_none() || current_fence.conversation.is_none() { return Ok(unavailable(U::MessageFenceChanged)); }
        let Some(message_fence) = &current_fence.messages else { return Ok(unavailable(U::MessageFenceChanged)) };
        // Variables and Memory can commit after normalization without changing any message row.
        let latest_effect = record.preparation.as_ref().and_then(|preparation| [SendEffect::Variables, SendEffect::Memory].into_iter()
            .filter_map(|effect| match preparation.effects.get(effect) {
                EffectState::Settled { receipt: Some((_, _, revision)) } => Some(*revision),
                _ => None,
            }).max());
        if record.checkpoint == 0 {
            if message_fence.version != receipt.request_id || current_count != count
                || record.committed_revision != Some(latest_effect.unwrap_or(receipt.receipt_revision)) { return Ok(unavailable(U::MessageFenceChanged)); }
        } else {
            let Some(revision) = record.committed_revision else { return Ok(unavailable(U::ReceiptIntegrity)) };
            if current.revision < revision { return Ok(unavailable(U::StaleLease)); }
            if current_count != count.checked_add(1).ok_or_else(|| invalid("history-limit"))? || message_fence.version == receipt.request_id
                || !canonical_uuid(&message_fence.version) || revision <= receipt.receipt_revision { return Ok(unavailable(U::MessageFenceChanged)); }
            let output_receipt: Option<(String, i64)> = new_db.query_row("SELECT digest,revision FROM lww_requests WHERE request_id=?1",
                [&message_fence.version], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
            if !output_receipt.is_some_and(|(hash, actual)| digest(&hash) && actual == revision) { return Ok(unavailable(U::ReceiptIntegrity)); }
            let output = match self.generation_projection_rows(&record.target, current, count as usize, 1, 1, MAX_OUTPUT_BYTES + 4096)? {
                Ok(rows) => rows, Err(reason) => return Ok(unavailable(reason)),
            };
            if output[0]["chatId"].as_str() != Some(record.message_id.as_str())
                || output[0]["data"].as_str().map(|text| risunest_sync_wire::hash(text.as_bytes())) != record.output_hash { return Ok(unavailable(U::HistoryChanged)); }
        }
        let old = match self.generation_projection_rows(&record.target, original, 0, count as usize, limits.history_rows, limits.history_bytes)? {
            Ok(rows) => rows, Err(reason) => return Ok(unavailable(reason)),
        };
        let new = match self.generation_projection_rows(&record.target, current, 0, count as usize, limits.history_rows, limits.history_bytes)? {
            Ok(rows) => rows, Err(reason) => return Ok(unavailable(reason)),
        };
        for row in &witness.rows {
            if row.row_hash != row_hash(&old[row.original_index])? || row.id_state != original_id_state(&old[row.original_index]) { return Ok(unavailable(U::InvalidWitness)); }
        }
        let mut ids: std::collections::BTreeSet<String> = old.iter().filter_map(|row| row["chatId"].as_str().map(str::to_owned)).collect();
        ids.insert(record.message_id.clone());
        let mut assignments = Vec::new();
        for (index, (old, new)) in old.iter().zip(&new).enumerate() {
            let mut expected = old.clone();
            if old["chatId"].is_null() {
                let Some(id) = new["chatId"].as_str() else { return Ok(unavailable(U::HistoryChanged)) };
                if !canonical_uuid(id) || !ids.insert(id.to_owned()) { return Ok(unavailable(U::HistoryChanged)); }
                let Some(object) = expected.as_object_mut() else { return Ok(unavailable(U::HistoryChanged)) };
                object.insert("chatId".into(), json!(id));
                assignments.push(IdAssignment { index, original_hash: row_hash(old)?, id: id.into() });
            }
            if expected != *new { return Ok(unavailable(U::HistoryChanged)); }
        }
        if assignments.is_empty() { return Ok(unavailable(U::ReceiptIntegrity)); }
        let commit = normalization_commit(&record.target, &old, &assignments, receipt.receipt_revision.checked_sub(1).ok_or_else(|| invalid("receipt-integrity"))?, &receipt.request_id)?;
        if super::lww::commit_intent_digest(&commit)? != receipt.intent_digest { return Ok(unavailable(U::ReceiptIntegrity)); }
        let rows = witness.rows.iter().filter_map(|row| assignments.binary_search_by_key(&row.original_index, |entry| entry.index).ok().map(|index| {
            NormalizedResidentRow { original_index: row.original_index, current_index: row.original_index,
                original_row_hash: row.row_hash.clone(), original_id_state: row.id_state.clone(), assigned_id: assignments[index].id.clone() }
        })).collect();
        Ok(NormalizationMapping::Mapped(NormalizationProjection { baseline_revision: original.revision, receipt_revision: receipt.receipt_revision,
            projected_revision: current.revision, original_count: count as usize, current_count: current_count as usize, prefix_count: count as usize, rows }))
    }

    fn generation_record(&self, ticket: &Ticket) -> StoreResult<Record> {
        let journal = read_journal(self.device_store()?.connection())?.ok_or_else(|| invalid("expired-ticket"))?;
        Ok(journal.record(ticket)?.clone())
    }

    // Converts recognized older records in one device transaction, preserving evidence and avoiding pruning.
    fn generation_convert_journal(&mut self) -> StoreResult<()> {
        self.generation_convert_journal_limited(MAX_PROTECTION_JOURNAL_BYTES)
    }

    fn generation_convert_journal_limited(&mut self, max_bytes: usize) -> StoreResult<()> {
        if !read_journal_limited(self.device_store()?.connection(), max_bytes)?.is_some_and(|decoded| decoded.conversion_needed) { return Ok(()); }
        let tx = self.device_store_mut()?.transaction()?;
        let Some(DecodedJournal { journal, conversion_needed: true }) = read_journal_limited(&tx, max_bytes)? else { return Ok(()) };
        validate_protection_journal(&journal)?;
        let body = encode_journal(&journal)?;
        if body.len() > max_bytes { return Err(invalid("protection-journal-limit")); }
        tx.execute("UPDATE device_settings SET value=?2 WHERE key=?1", params![JOURNAL_KEY, body])?;
        tx.commit()?;
        Ok(())
    }

    fn generation_replace_record(&mut self, ticket: &Ticket, previous: &Record, next: &Record) -> StoreResult<()> {
        self.generation_convert_journal()?;
        let tx = self.device_store_mut()?.transaction()?;
        let mut journal = read_current_journal(&tx)?.ok_or_else(|| invalid("expired-ticket"))?;
        if journal.record(ticket)? != previous { return Err(StoreError::CommitBusy); }
        let mut stored = next.clone();
        if stored.phase.terminal() && stored.attempt.is_none() {
            if let Some(Preparation { normalization: Normalization::Pending { assignments }, .. }) = &mut stored.preparation {
                *assignments = None;
            }
        }
        journal.records.insert(ticket.sequence.0, stored);
        journal.prune();
        write_journal(&tx, &journal)?;
        tx.commit()?;
        Ok(())
    }

    fn generation_snapshot(&mut self, target: &Target, history: bool) -> StoreResult<(StoredHistory, Fence)> {
        self.generation_snapshot_limited(target, history, MAX_HISTORY_MESSAGES, MAX_HISTORY_BYTES)
    }

    fn generation_snapshot_limited(&mut self, target: &Target, history: bool, max_messages: i64, max_bytes: usize) -> StoreResult<(StoredHistory, Fence)> {
        if self.lww_binding_authority()? != target.binding_authority || self.library_identity()? != target.library_identity {
            return Err(invalid("stale-target"));
        }
        let revision = self.revision()?;
        let lease = self.acquire_revision(revision)?.lease;
        let result = self.generation_snapshot_at_lease(target, history, max_messages, max_bytes, &lease);
        let released = self.release_revision(&lease);
        match result { Ok(value) => { released?; Ok(value) }, Err(error) => { let _ = released; Err(error) } }
    }

    fn generation_snapshot_at_lease(&self, target: &Target, history: bool, max_messages: i64, max_bytes: usize, lease: &str) -> StoreResult<(StoredHistory, Fence)> {
        let (db, read) = self.read_view(Some(lease))?;
        if read.generation != target.library_identity { return Err(invalid("stale-target")); }
        let character = self.read_character(&target.character_id, Some(lease))?.ok_or_else(|| invalid("missing-character"))?.value;
        if character.get("type").and_then(Value::as_str) != Some("character") { return Err(invalid("unsupported-character")); }
        let metadata = self.read_conversation_metadata(&target.character_id, &target.conversation_id, Some(lease))?
            .ok_or_else(|| invalid("missing-conversation"))?.value;
        let fence = capture_fence(db, target, metadata.total_messages)?;
        let mut messages = Vec::new();
        if history {
            if metadata.total_messages > max_messages { return Err(invalid("history-limit")); }
            let mut bytes = 0usize;
            while (messages.len() as i64) < metadata.total_messages {
                let page = self.read_conversation_window(&ConversationWindowQuery {
                    character_id: target.character_id.clone(), conversation_id: target.conversation_id.clone(),
                    start_index: Some(messages.len() as i64), limit: Some(128), anchor_message_id: None,
                    anchor_occurrence: None, before: None, after: None, skip_parser_work: None,
                }, Some(lease))?.ok_or_else(|| invalid("missing-conversation"))?.value;
                if page.messages.is_empty() { return Err(invalid("history-gap")); }
                for message in &page.messages {
                    bytes = bytes.checked_add(serde_json::to_vec(message)?.len()).ok_or_else(|| invalid("history-limit"))?;
                    if bytes > max_bytes { return Err(invalid("history-limit")); }
                }
                messages.extend(page.messages);
            }
        }
        Ok((StoredHistory { revision: read.revision, character, conversation: metadata.conversation, messages }, fence))
    }

    pub(crate) fn generation_preparation_snapshot(&mut self, intent: &OllamaIntent) -> StoreResult<PreparationSnapshot> {
        self.generation_preparation_snapshot_fenced(intent).map(|(snapshot, _)| snapshot)
    }

    fn generation_preparation_snapshot_fenced(&mut self, intent: &OllamaIntent) -> StoreResult<(PreparationSnapshot, Fence)> {
        let target = &intent.target;
        if target.character_id.is_empty() || target.conversation_id.is_empty() || target.character_id.len() > 512 || target.conversation_id.len() > 512 {
            return Err(invalid("invalid-target"));
        }
        if intent.effective_preset_id.is_empty() || intent.effective_preset_id.len() > 512 {
            return Err(invalid("invalid-preset"));
        }
        if self.lww_binding_authority()? != target.binding_authority || self.library_identity()? != target.library_identity {
            return Err(invalid("stale-target"));
        }
        let lease = self.acquire_revision(target.expected_revision)?.lease;
        let result = self.generation_preparation_with_fence(intent, &lease);
        let released = self.release_revision(&lease);
        match result { Ok(value) => { released?; Ok(value) }, Err(error) => { let _ = released; Err(error) } }
    }

    fn generation_preparation_at_lease(&self, intent: &OllamaIntent, lease: &str) -> StoreResult<PreparationSnapshot> {
        self.generation_preparation_with_fence(intent, lease).map(|(snapshot, _)| snapshot)
    }

    fn generation_preparation_with_fence(&self, intent: &OllamaIntent, lease: &str) -> StoreResult<(PreparationSnapshot, Fence)> {
        let (history, fence) = self.generation_snapshot_at_lease(&intent.target, true, MAX_HISTORY_MESSAGES, MAX_HISTORY_BYTES, lease)?;
        let root = self.read_root(Some(lease))?.value;
        let preset = self.read_preset(&intent.effective_preset_id, Some(lease))?
            .ok_or_else(|| invalid("missing-preset"))?.value;
        let selected_preset_id = self.generation_selected_preset_at_lease(&root, lease)?;
        Ok((PreparationSnapshot { history, root, preset, selected_preset_id, entropy: None, host: intent.host.clone() }, fence))
    }

    fn generation_selected_preset_at_lease(&self, root: &Value, lease: &str) -> StoreResult<Option<String>> {
        // Resolve the GUI selection using the same ID/index/default rules as working-set export.
        let presets = self.query_presets(Some(lease))?.items;
        let selected = match root.get("botPresetsId") {
            Some(Value::String(id)) => presets.iter().position(|preset| preset.id == *id).unwrap_or(0),
            selection => selection.and_then(Value::as_u64).and_then(|index| usize::try_from(index).ok()).unwrap_or(0),
        };
        Ok(presets.get(selected).map(|preset| preset.id.clone()))
    }

    pub(crate) fn generation_reserve(&mut self, target: Target) -> StoreResult<Ticket> {
        if target.character_id.is_empty() || target.conversation_id.is_empty() || target.character_id.len() > 512 || target.conversation_id.len() > 512 {
            return Err(invalid("invalid-target"));
        }
        let (snapshot, fence) = self.generation_snapshot(&target, false)?;
        if snapshot.revision != target.expected_revision {
            return Err(StoreError::RevisionConflict { expected: target.expected_revision, actual: snapshot.revision });
        }
        self.generation_reserve_record(target, fence, None)
    }

    fn generation_reserve_record(&mut self, target: Target, fence: Fence, preparation: Option<Preparation>) -> StoreResult<Ticket> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| invalid("invalid-clock"))?.as_millis();
        let now = u64::try_from(now).map_err(|_| invalid("invalid-clock"))?;
        self.generation_convert_journal()?;
        let tx = self.device_store_mut()?.transaction()?;
        let mut journal = read_current_journal(&tx)?.unwrap_or_else(Journal::new);
        journal.high_water = journal.high_water.checked_add(1).ok_or_else(|| invalid("ticket-overflow"))?;
        let ticket = Ticket { epoch: journal.epoch.clone(), sequence: journal.high_water.into() };
        journal.records.insert(journal.high_water, Record {
            target, phase: Phase::Reserved, session: None, owner: None, message_id: uuid::Uuid::new_v4().to_string(),
            message_time: now, initial_count: fence.count, fence, checkpoint: 0, output_hash: None,
            committed_revision: None, attempt: None, preparation, execution: None,
        });
        journal.prune();
        write_journal(&tx, &journal)?;
        tx.commit()?;
        Ok(ticket)
    }

    pub(crate) fn generation_reserve_ollama(&mut self, intent: &OllamaIntent) -> Result<Ticket, GenerationError> {
        self.generation_reserve_ollama_limited(intent, MAX_HISTORY_BYTES)
    }

    fn generation_reserve_ollama_limited(&mut self, intent: &OllamaIntent, max_bytes: usize) -> Result<Ticket, GenerationError> {
        let (snapshot, fence) = self.generation_preparation_snapshot_fenced(intent)?;
        let entropy = admit_ollama(&snapshot)?;
        normalized_history_fits(&snapshot.history.messages, max_bytes)?;
        Ok(self.generation_reserve_record(intent.target.clone(), fence, Some(Preparation {
            preset_id: intent.effective_preset_id.clone(), normalization: Normalization::Pending { assignments: None },
            ready: false, error: None, effects: SendEffects::pending(), normalized_at: None, ready_at: None, entropy: Some(entropy),
            host: intent.host.clone(), edited_count: None,
        }))?)
    }

    pub(crate) fn generation_start_preparation(&mut self, ticket: &Ticket) -> StoreResult<Start> {
        if self.generation_record(ticket)?.preparation.is_none() { return Err(invalid("preparation-required")); }
        self.generation_start(ticket)
    }

    // The native service holds its execution gate while recovering this existing owner.
    pub(crate) fn generation_resume_preparation(&mut self, ticket: &Ticket) -> StoreResult<Start> {
        self.generation_reconcile_orphans()?;
        let gate = segment_gate(ticket)?;
        let segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let previous = self.generation_record(ticket)?;
        if previous.preparation.is_none() { return Err(invalid("preparation-required")); }
        let previous = self.generation_resolve_preparation(ticket, &previous)?;
        if previous.phase == Phase::Reserved {
            drop(segment);
            return self.generation_start_preparation(ticket);
        }
        if previous.phase != Phase::Running || previous.session.as_deref() != Some(runtime_session_id())
            || previous.preparation.as_ref().unwrap().ready {
            return Ok(Start::Existing(status(ticket, &previous)));
        }
        let (history, fence) = match self.generation_snapshot(&previous.target, true) {
            Ok(value) => value,
            Err(StoreError::Validation { .. }) => return Ok(Start::Existing(self.generation_stale(ticket, &previous)?)),
            Err(error) => return Err(error),
        };
        if fence != previous.fence { return Ok(Start::Existing(self.generation_stale(ticket, &previous)?)); }
        let owner = Owner { ticket: ticket.clone(), session: runtime_session_id().to_owned(),
            token: previous.owner.clone().ok_or_else(|| invalid("stale-owner"))? };
        Ok(Start::Started { owner, history, status: status(ticket, &previous) })
    }

    fn generation_preparation_for_record(&mut self, record: &Record) -> StoreResult<(PreparationSnapshot, Fence)> {
        let preparation = record.preparation.as_ref().ok_or_else(|| invalid("preparation-required"))?;
        if self.lww_binding_authority()? != record.target.binding_authority || self.library_identity()? != record.target.library_identity {
            return Err(invalid("stale-target"));
        }
        let intent = OllamaIntent { target: record.target.clone(), effective_preset_id: preparation.preset_id.clone(), host: preparation.host.clone() };
        let lease = self.acquire_revision(self.revision()?)?.lease;
        let result = self.generation_preparation_with_fence(&intent, &lease)
            .map(|(snapshot, fence)| (PreparationSnapshot { entropy: preparation.entropy.clone(), ..snapshot }, fence));
        let released = self.release_revision(&lease);
        match result { Ok(value) => { released?; Ok(value) }, Err(error) => { let _ = released; Err(error) } }
    }

    fn generation_stale(&mut self, ticket: &Ticket, previous: &Record) -> StoreResult<Status> {
        Ok(status(ticket, &self.generation_stale_record(ticket, previous)?))
    }

    fn generation_stale_record(&mut self, ticket: &Ticket, previous: &Record) -> StoreResult<Record> {
        let mut next = previous.clone();
        next.phase = Phase::Stale;
        self.generation_replace_record(ticket, previous, &next)?;
        Ok(next)
    }

    // The current preparation view under the record's fence, admitted for this route; a changed target ends Stale.
    fn generation_admitted_snapshot(&mut self, ticket: &Ticket, previous: &Record) -> Result<Result<PreparationSnapshot, Status>, GenerationError> {
        let (snapshot, fence) = match self.generation_preparation_for_record(previous) {
            Ok(value) => value,
            Err(StoreError::Validation { .. }) => return Ok(Err(self.generation_stale(ticket, previous)?)),
            Err(error) => return Err(error.into()),
        };
        if fence != previous.fence { return Ok(Err(self.generation_stale(ticket, previous)?)); }
        admit_ollama(&snapshot)?;
        normalized_history_fits(&snapshot.history.messages, MAX_HISTORY_BYTES)?;
        Ok(Ok(snapshot))
    }

    // The root, character and conversation metadata at one current revision, without history.
    fn generation_effect_view(&mut self, target: &Target, preset_id: Option<&str>) -> StoreResult<(StoredHistory, Value, Fence, Option<String>)> {
        if self.lww_binding_authority()? != target.binding_authority || self.library_identity()? != target.library_identity {
            return Err(invalid("stale-target"));
        }
        let lease = self.acquire_revision(self.revision()?)?.lease;
        let result = self.generation_snapshot_at_lease(target, false, MAX_HISTORY_MESSAGES, MAX_HISTORY_BYTES, &lease)
            .and_then(|(history, fence)| {
                let root = self.read_root(Some(&lease))?.value;
                let selected = if let Some(preset_id) = preset_id {
                    self.read_preset(preset_id, Some(&lease))?.ok_or_else(|| invalid("missing-preset"))?;
                    self.generation_selected_preset_at_lease(&root, &lease)?
                } else { None };
                Ok((history, root, fence, selected))
            });
        let released = self.release_revision(&lease);
        match result { Ok(value) => { released?; Ok(value) }, Err(error) => { let _ = released; Err(error) } }
    }

    // One pre-dispatch effect: a fresh view under the record's fence, a frozen receipt-backed commit, and a new
    // request after a definite revision conflict that left the target unchanged. A failed preparation pins its
    // original error in the same replacement as the attempt; only this live call may retry its retained plan.
    fn generation_commit_effect(&mut self, owner: &Owner, mut previous: Record, effect: SendEffect,
        original_error: Option<&PreparationError>, expected_selection: Option<Option<&str>>,
        plan: &mut dyn FnMut(&StoredHistory, &Value) -> Result<EffectPlan, GenerationError>) -> Result<Record, GenerationError> {
        let mut retries = 0;
        loop {
            let preset = expected_selection.map(|_| previous.preparation.as_ref().ok_or_else(|| invalid("preparation-required")))
                .transpose()?.map(|preparation| preparation.preset_id.as_str());
            let (history, root, fence, selected) = match self.generation_effect_view(&previous.target, preset) {
                Ok(view) => view,
                Err(StoreError::Validation { .. }) => return Ok(self.generation_stale_record(&owner.ticket, &previous)?),
                Err(error) => return Err(error.into()),
            };
            if fence != previous.fence || expected_selection.is_some_and(|expected| selected.as_deref() != expected) {
                return Ok(self.generation_stale_record(&owner.ticket, &previous)?);
            }
            let mutations = match plan(&history, &root)? {
                EffectPlan::Stale => return Ok(self.generation_stale_record(&owner.ticket, &previous)?),
                EffectPlan::Settled => {
                    let mut next = previous.clone();
                    if let Some(error) = original_error { pin_preparation_failure(&mut next, error)?; next.phase = Phase::Failed; }
                    *next.preparation.as_mut().ok_or_else(|| invalid("preparation-required"))?.effects.get_mut(effect) = EffectState::Settled { receipt: None };
                    self.generation_replace_record(&owner.ticket, &previous, &next)?;
                    return Ok(next);
                }
                EffectPlan::Mutations(mutations) => (Some(mutations), None),
                // The edits and the unit writes land in one revision under one receipt.
                EffectPlan::Edits { units, messages, count } => ((!units.is_empty()).then_some(units), Some((messages, count))),
            };
            let (mutations, messages) = mutations;
            let request_id = uuid::Uuid::new_v4().to_string();
            let commit = WorkingSetCommit { expected_revision: history.revision, request_id: Some(request_id.clone()),
                binding_authority: Some(previous.target.binding_authority), unit_mutations: mutations,
                conversations: messages.as_ref().map(|(messages, _)| vec![messages.clone()]), ..Default::default() };
            let attempt = Attempt { request_id, intent_digest: super::lww::commit_intent_digest(&commit)?,
                expected_revision: commit.expected_revision, kind: AttemptKind::Effect { effect }, edited_count: messages.map(|(_, count)| count) };
            let mut issued = previous.clone();
            if let Some(error) = original_error { pin_preparation_failure(&mut issued, error)?; }
            issued.attempt = Some(attempt.clone());
            self.generation_replace_record(&owner.ticket, &previous, &issued)?;
            let entered = std::cell::Cell::new(false);
            let result = self.commit_holding(&commit, |store| {
                let barrier = store.generation_barrier(owner, &attempt)?;
                entered.set(true);
                Ok(barrier)
            });
            let resolved = self.generation_resolve_attempt_after(&owner.ticket, &issued,
                !entered.get() || matches!(&result, Err(StoreError::RevisionConflict { .. })))?;
            if resolved.phase != Phase::Running || effect_settled(&resolved, effect) { return Ok(resolved); }
            match result {
                Err(StoreError::RevisionConflict { .. }) if retries < MAX_REVISION_RETRIES => { retries += 1; previous = resolved; }
                Err(error) => return Err(error.into()),
                Ok(_) => return Err(invalid("receipt-missing").into()),
            }
        }
    }

    // TS increments the root statics counter and stamps the character's interaction time in one write.
    fn generation_send_interaction(&mut self, owner: &Owner, previous: Record) -> Result<Record, GenerationError> {
        let now = now_millis()?;
        let keys = send_effect_unit_keys(&previous.target)?;
        self.generation_commit_effect(owner, previous, SendEffect::Interaction, None, None, &mut |_, root| {
            let value = next_statics(root).ok_or_else(|| PreparationError::Unsupported("statics".into()))?;
            Ok(EffectPlan::Mutations(vec![UnitMutation::Set { key: keys.statics.clone(), value },
                UnitMutation::Set { key: keys.interaction.clone(), value: json!(now) }]))
        })
    }

    pub(crate) fn generation_send_variables(&mut self, owner: &Owner, lifecycle_id: &str, plan: &PreparedVariablePlan) -> StoreResult<Status> {
        let gate = segment_gate(&owner.ticket)?;
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let previous = self.generation_record(&owner.ticket)?;
        require_owner(&previous, owner)?;
        if owner.session != runtime_session_id() { return Err(invalid("stale-owner")); }
        let execution = previous.execution.as_ref().ok_or_else(|| invalid("execution-evidence-lost"))?;
        if execution.lifecycle_id != lifecycle_id { return Err(invalid("stale-lifecycle")); }
        if execution.dispatch_id.is_some() { return Err(invalid("dispatch-claimed")); }
        let previous = self.generation_resolve_attempt(&owner.ticket, &previous)?;
        let execution = previous.execution.as_ref().ok_or_else(|| invalid("execution-evidence-lost"))?;
        if previous.phase != Phase::Running || execution.cancel.is_some() || execution.terminal.is_some() || effect_settled(&previous, SendEffect::Variables) {
            return Ok(status(&owner.ticket, &previous));
        }
        if !previous.preparation.as_ref().is_some_and(|value| value.ready && value.error.is_none() && matches!(value.normalization, Normalization::Settled { .. }))
            || !effect_settled(&previous, SendEffect::Interaction) { return Err(invalid("preparation-pending")); }
        let keys = send_effect_unit_keys(&previous.target)?;
        let selection = (!plan.is_empty()).then_some(plan.selected_preset_id.as_deref());
        let (target, count) = (previous.target.clone(), previous.fence.count);
        let record = self.generation_commit_effect(owner, previous, SendEffect::Variables, None, selection,
            &mut |history, _| variable_mutation(history, plan, &keys, &target, count).map_err(Into::into)).map_err(effect_error)?;
        Ok(status(&owner.ticket, &record))
    }

    pub(crate) fn generation_fail_preparation_with_variables(&mut self, owner: &Owner, error: PreparationError, plan: &PreparedVariablePlan) -> StoreResult<Status> {
        if plan.is_empty() { return self.generation_fail_preparation(owner, error); }
        let gate = segment_gate(&owner.ticket)?;
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let previous = self.generation_record(&owner.ticket)?;
        require_owner(&previous, owner)?;
        if owner.session != runtime_session_id() { return Err(invalid("stale-owner")); }
        let previous = self.generation_resolve_preparation(&owner.ticket, &previous)?;
        if previous.phase != Phase::Running { return Ok(status(&owner.ticket, &previous)); }
        let preparation = previous.preparation.as_ref().ok_or_else(|| invalid("preparation-required"))?;
        if previous.execution.is_some() || preparation.ready { return Err(invalid("execution-owned")); }
        if !matches!(preparation.normalization, Normalization::Settled { .. }) || !effect_settled(&previous, SendEffect::Interaction) {
            return Err(invalid("normalization-pending"));
        }
        // Converted older records already settled Variables and must never acquire retroactive writes.
        if effect_settled(&previous, SendEffect::Variables) {
            match self.generation_snapshot(&previous.target, false) {
                Ok((_, fence)) if fence == previous.fence => {},
                Ok(_) | Err(StoreError::Validation { .. }) => return self.generation_stale(&owner.ticket, &previous),
                Err(error) => return Err(error),
            }
            let mut next = previous.clone();
            pin_preparation_failure(&mut next, &error)?;
            next.phase = Phase::Failed;
            self.generation_replace_record(&owner.ticket, &previous, &next)?;
            return Ok(status(&owner.ticket, &next));
        }
        let keys = send_effect_unit_keys(&previous.target)?;
        let (target, count) = (previous.target.clone(), previous.fence.count);
        let record = self.generation_commit_effect(owner, previous, SendEffect::Variables, Some(&error), Some(plan.selected_preset_id.as_deref()),
            &mut |history, _| variable_mutation(history, plan, &keys, &target, count).map_err(Into::into)).map_err(effect_error)?;
        Ok(status(&owner.ticket, &self.generation_finish_preparation_failure(&owner.ticket, &record)?))
    }

    // Memory follows Variables. A cancellation admitted between their segments prevents this later write.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn generation_send_memory(&mut self, owner: &Owner, lifecycle_id: &str, last_memory: Option<&str>) -> StoreResult<Status> {
        let gate = segment_gate(&owner.ticket)?;
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let previous = self.generation_record(&owner.ticket)?;
        require_owner(&previous, owner)?;
        let execution = previous.execution.as_ref().ok_or_else(|| invalid("execution-evidence-lost"))?;
        if execution.lifecycle_id != lifecycle_id { return Err(invalid("stale-lifecycle")); }
        if execution.dispatch_id.is_some() { return Err(invalid("dispatch-claimed")); }
        let previous = self.generation_resolve_attempt(&owner.ticket, &previous)?;
        let execution = previous.execution.as_ref().ok_or_else(|| invalid("execution-evidence-lost"))?;
        if previous.phase != Phase::Running || execution.cancel.is_some() || execution.terminal.is_some() || effect_settled(&previous, SendEffect::Memory) {
            return Ok(status(&owner.ticket, &previous));
        }
        if !effect_settled(&previous, SendEffect::Variables) { return Err(invalid("effects-pending")); }
        let key = send_effect_unit_keys(&previous.target)?.memory;
        let record = self.generation_commit_effect(owner, previous, SendEffect::Memory, None, None, &mut |history, _| {
            Ok(match (last_memory, history.conversation.get("lastMemory")) {
                (Some(memory), Some(Value::String(current))) if current == memory => EffectPlan::Settled,
                (None, None) => EffectPlan::Settled,
                (Some(memory), _) => EffectPlan::Mutations(vec![UnitMutation::Set { key: key.clone(), value: json!(memory) }]),
                (None, Some(_)) => EffectPlan::Mutations(vec![UnitMutation::Delete { key: key.clone() }]),
            })
        }).map_err(effect_error)?;
        Ok(status(&owner.ticket, &record))
    }

    pub(crate) fn generation_ensure_history_ids(&mut self, owner: &Owner) -> Result<Status, GenerationError> {
        let gate = segment_gate(&owner.ticket)?;
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let previous = self.generation_record(&owner.ticket)?;
        require_owner(&previous, owner)?;
        let mut previous = self.generation_resolve_preparation(&owner.ticket, &previous)?;
        if previous.phase != Phase::Running { return Ok(status(&owner.ticket, &previous)); }
        let preparation = previous.preparation.as_ref().ok_or_else(|| invalid("preparation-required"))?;
        if matches!(preparation.normalization, Normalization::Settled { .. }) { return Ok(status(&owner.ticket, &previous)); }
        let interaction = matches!(preparation.normalization, Normalization::Pending { assignments: None })
            && !effect_settled(&previous, SendEffect::Interaction);
        let mut snapshot = match self.generation_admitted_snapshot(&owner.ticket, &previous)? {
            Ok(snapshot) => snapshot,
            Err(status) => return Ok(status),
        };
        // TS counts the send and stamps the interaction time once, before it normalizes history IDs.
        if interaction {
            previous = self.generation_send_interaction(owner, previous)?;
            if previous.phase != Phase::Running { return Ok(status(&owner.ticket, &previous)); }
            snapshot = match self.generation_admitted_snapshot(&owner.ticket, &previous)? {
                Ok(snapshot) => snapshot,
                Err(status) => return Ok(status),
            };
        }
        let preparation = previous.preparation.as_ref().ok_or_else(|| invalid("preparation-required"))?;
        let assignments = match &preparation.normalization {
            Normalization::Pending { assignments: Some(assignments) } => assignments.clone(),
            Normalization::Pending { assignments: None } => {
                let mut assignments = Vec::new();
                let mut ids: std::collections::BTreeSet<String> = snapshot.history.messages.iter()
                    .filter_map(|row| row["chatId"].as_str().map(str::to_owned)).collect();
                ids.insert(previous.message_id.clone());
                for (index, row) in snapshot.history.messages.iter().enumerate().filter(|(_, row)| row["chatId"].is_null()) {
                    let id = loop {
                        let id = uuid::Uuid::new_v4().to_string();
                        if ids.insert(id.clone()) { break id; }
                    };
                    assignments.push(IdAssignment { index, original_hash: row_hash(row)?, id });
                }
                let mut planned = previous.clone();
                let plan = planned.preparation.as_mut().unwrap();
                plan.normalization = if assignments.is_empty() {
                    plan.normalized_at = Some(now_millis()?);
                    Normalization::Settled { receipt: None }
                } else { Normalization::Pending { assignments: Some(assignments.clone()) } };
                self.generation_replace_record(&owner.ticket, &previous, &planned)?;
                previous = planned;
                assignments
            }
            Normalization::Settled { .. } => unreachable!(),
        };
        if assignments.is_empty() { return Ok(status(&owner.ticket, &previous)); }
        let request_id = uuid::Uuid::new_v4().to_string();
        let commit = normalization_commit(&previous.target, &snapshot.history.messages, &assignments, snapshot.history.revision, &request_id)?;
        let attempt = Attempt { request_id, expected_revision: commit.expected_revision,
            intent_digest: super::lww::commit_intent_digest(&commit)?, kind: AttemptKind::HistoryIds, edited_count: None };
        let mut issued = previous.clone();
        issued.attempt = Some(attempt.clone());
        self.generation_replace_record(&owner.ticket, &previous, &issued)?;
        let result = self.commit_holding(&commit, |store| store.generation_barrier(owner, &attempt));
        let resolved = self.generation_resolve_attempt_after(&owner.ticket, &issued,
            matches!(&result, Err(StoreError::RevisionConflict { .. } | StoreError::Validation { .. })))?;
        if resolved.preparation.as_ref().is_some_and(|value| matches!(value.normalization, Normalization::Settled { .. })) {
            return Ok(status(&owner.ticket, &resolved));
        }
        result?;
        Err(invalid("receipt-missing").into())
    }

    pub(crate) fn generation_owned_preparation_snapshot(&mut self, owner: &Owner) -> StoreResult<Option<PreparationSnapshot>> {
        let gate = segment_gate(&owner.ticket)?;
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let previous = self.generation_record(&owner.ticket)?;
        require_owner(&previous, owner)?;
        let previous = self.generation_resolve_preparation(&owner.ticket, &previous)?;
        if previous.phase != Phase::Running { return Ok(None); }
        if !previous.preparation.as_ref().is_some_and(|value| matches!(value.normalization, Normalization::Settled { .. })) {
            return Err(invalid("normalization-pending"));
        }
        match self.generation_preparation_for_record(&previous) {
            Ok((snapshot, fence)) if fence == previous.fence => Ok(Some(snapshot)),
            Ok(_) | Err(StoreError::Validation { .. }) => { self.generation_stale(&owner.ticket, &previous)?; Ok(None) }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn generation_mark_prepared(&mut self, owner: &Owner) -> StoreResult<Status> {
        self.generation_preparation_result(owner, None, None)
    }

    pub(crate) fn generation_fail_preparation(&mut self, owner: &Owner, error: PreparationError) -> StoreResult<Status> {
        self.generation_preparation_result(owner, Some(error), None)
    }

    // Persists the ready mark and the owned lifecycle in one record replacement.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn generation_mark_ready_owned(&mut self, owner: &Owner, lifecycle_id: &str) -> StoreResult<Status> {
        if !canonical_uuid(lifecycle_id) { return Err(invalid("invalid-lifecycle")); }
        self.generation_preparation_result(owner, None, Some(lifecycle_id))
    }

    fn generation_preparation_result(&mut self, owner: &Owner, error: Option<PreparationError>, lifecycle: Option<&str>) -> StoreResult<Status> {
        let gate = segment_gate(&owner.ticket)?;
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let previous = self.generation_record(&owner.ticket)?;
        require_owner(&previous, owner)?;
        if let Some(execution) = &previous.execution {
            if lifecycle == Some(execution.lifecycle_id.as_str()) { return Ok(status(&owner.ticket, &previous)); }
            return Err(invalid("execution-owned"));
        }
        let previous = self.generation_resolve_preparation(&owner.ticket, &previous)?;
        if previous.phase != Phase::Running { return Ok(status(&owner.ticket, &previous)); }
        match self.generation_snapshot(&previous.target, false) {
            Ok((_, fence)) if fence == previous.fence => {},
            Ok(_) | Err(StoreError::Validation { .. }) => return self.generation_stale(&owner.ticket, &previous),
            Err(error) => return Err(error),
        }
        let mut next = previous.clone();
        let preparation = next.preparation.as_mut().ok_or_else(|| invalid("preparation-required"))?;
        if error.is_none() && !matches!(preparation.normalization, Normalization::Settled { .. }) { return Err(invalid("normalization-pending")); }
        if lifecycle.is_some() && preparation.ready { return Err(invalid("ready-unowned")); }
        preparation.ready = error.is_none();
        preparation.entropy = None;
        preparation.host = None;
        if preparation.ready { preparation.ready_at = Some(now_millis()?); }
        preparation.error = error;
        if preparation.error.is_some() { next.phase = Phase::Failed; }
        next.execution = lifecycle.map(|lifecycle_id| Execution { lifecycle_id: lifecycle_id.into(), dispatch_id: None,
            cancel: None, terminal: None, output_receipt: None });
        self.generation_replace_record(&owner.ticket, &previous, &next)?;
        Ok(status(&owner.ticket, &next))
    }

    pub(crate) fn generation_cancel(&mut self, ticket: &Ticket) -> StoreResult<Status> {
        self.generation_cancel_for(ticket, CancelReason::User)
    }

    // An owned operation only records the decision; its owner or recovery settles the phase.
    pub(crate) fn generation_cancel_for(&mut self, ticket: &Ticket, reason: CancelReason) -> StoreResult<Status> {
        let gate = segment_gate(ticket)?;
        #[cfg(test)]
        if matches!(gate.try_lock(), Err(std::sync::TryLockError::WouldBlock)) {
            FINISH_WAITING_FOR_SEGMENT.with(|hook| hook.borrow_mut().take()).map(|hook| hook());
        }
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let mut previous = self.generation_record(ticket)?;
        if previous.preparation.as_ref().is_some_and(|preparation| preparation.error.is_some()) {
            return Ok(status(ticket, &self.generation_resolve_preparation(ticket, &previous)?));
        }
        if let Some(execution) = &previous.execution {
            if !previous.phase.terminal() && execution.cancel.is_none() && execution.terminal.is_none() {
                let mut next = previous.clone();
                if let Some(execution) = next.execution.as_mut() { execution.cancel = Some(reason); }
                self.generation_replace_record(ticket, &previous, &next)?;
                previous = next;
            }
            return Ok(status(ticket, &self.generation_resolve_attempt(ticket, &previous)?));
        }
        let previous = self.generation_resolve_attempt(ticket, &previous)?;
        if previous.phase.terminal() { return Ok(status(ticket, &previous)); }
        match self.generation_snapshot(&previous.target, false) {
            Ok((_, fence)) if fence == previous.fence => {},
            Ok(_) | Err(StoreError::Validation { .. }) => return self.generation_stale(ticket, &previous),
            Err(error) => return Err(error),
        }
        let mut next = previous.clone();
        next.phase = if previous.phase == Phase::Running && previous.session.as_deref() != Some(runtime_session_id()) { Phase::Interrupted } else { Phase::Cancelled };
        self.generation_replace_record(ticket, &previous, &next)?;
        Ok(status(ticket, &next))
    }

    pub(crate) fn generation_status(&self, ticket: &Ticket) -> StoreResult<Status> {
        Ok(status(ticket, &self.generation_record(ticket)?))
    }

    pub(crate) fn generation_start(&mut self, ticket: &Ticket) -> StoreResult<Start> {
        self.generation_convert_journal()?;
        self.generation_reconcile_orphans()?;
        let gate = segment_gate(ticket)?;
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let previous = self.generation_record(ticket)?;
        if previous.phase != Phase::Reserved { return Ok(Start::Existing(status(ticket, &previous))); }
        let (history, fence) = self.generation_snapshot(&previous.target, true)?;
        let mut next = previous.clone();
        if fence != previous.fence {
            next.phase = Phase::Stale;
            self.generation_replace_record(ticket, &previous, &next)?;
            return Ok(Start::Existing(status(ticket, &next)));
        }
        next.phase = Phase::Running;
        next.session = Some(runtime_session_id().to_owned());
        next.owner = Some(uuid::Uuid::new_v4().to_string());
        let tx = self.device_store_mut()?.transaction()?;
        let mut journal = read_current_journal(&tx)?.ok_or_else(|| invalid("expired-ticket"))?;
        if journal.record(ticket)? != &previous { return Err(StoreError::CommitBusy); }
        if journal.records.values().filter(|record| record.phase == Phase::Running || record.attempt.is_some()).count() >= MAX_ACTIVE {
            return Err(invalid("active-limit"));
        }
        if journal.records.values().any(|record| record.phase == Phase::Running
            && record.target.library_identity == next.target.library_identity
            && record.target.character_id == next.target.character_id
            && record.target.conversation_id == next.target.conversation_id) {
            return Err(invalid("target-active"));
        }
        journal.records.insert(ticket.sequence.0, next.clone());
        write_journal(&tx, &journal)?;
        tx.commit()?;
        let owner = Owner { ticket: ticket.clone(), session: runtime_session_id().to_owned(), token: next.owner.clone().unwrap() };
        Ok(Start::Started { owner, history, status: status(ticket, &next) })
    }

    fn generation_barrier(&self, owner: &Owner, attempt: &Attempt) -> StoreResult<DeviceBarrier> {
        let path = self.device_store()?.connection().path().filter(|path| !path.is_empty()).ok_or_else(|| invalid("device-path"))?;
        let db = crate::sqlite_open::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        db.busy_timeout(Duration::ZERO)?;
        db.execute_batch("BEGIN IMMEDIATE").map_err(|error| match error {
            rusqlite::Error::SqliteFailure(ref code, _) if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => StoreError::CommitBusy,
            error => error.into(),
        })?;
        let journal = read_journal(&db)?.ok_or_else(|| invalid("expired-ticket"))?;
        let record = journal.record(&owner.ticket)?;
        require_owner(record, owner)?;
        if record.phase != Phase::Running || record.attempt.as_ref() != Some(attempt) { return Err(invalid("stale-owner")); }
        if super::lww::authority(&db)? != record.target.binding_authority { return Err(invalid("stale-target")); }
        let digest: String = db.query_row("SELECT digest FROM lww_intents WHERE request_id=?1", [&attempt.request_id], |row| row.get(0))?;
        if digest != attempt.intent_digest { return Err(invalid("intent-integrity")); }
        Ok(DeviceBarrier { _connection: db })
    }

    fn generation_resolve_attempt(&mut self, ticket: &Ticket, previous: &Record) -> StoreResult<Record> {
        self.generation_resolve_attempt_after(ticket, previous, false)
    }

    fn generation_resolve_preparation(&mut self, ticket: &Ticket, previous: &Record) -> StoreResult<Record> {
        let resolved = self.generation_resolve_attempt(ticket, previous)?;
        self.generation_finish_preparation_failure(ticket, &resolved)
    }

    fn generation_finish_preparation_failure(&mut self, ticket: &Ticket, previous: &Record) -> StoreResult<Record> {
        if previous.phase != Phase::Running || previous.attempt.is_some()
            || !previous.preparation.as_ref().is_some_and(|preparation| preparation.error.is_some()) { return Ok(previous.clone()); }
        match self.generation_snapshot(&previous.target, false) {
            Ok((_, fence)) if fence == previous.fence => {},
            Ok(_) | Err(StoreError::Validation { .. }) => return self.generation_stale_record(ticket, previous),
            Err(error) => return Err(error),
        }
        let mut next = previous.clone();
        next.phase = Phase::Failed;
        self.generation_replace_record(ticket, previous, &next)?;
        Ok(next)
    }

    fn generation_resolve_attempt_after(&mut self, ticket: &Ticket, previous: &Record, known_unapplied: bool) -> StoreResult<Record> {
        let Some(attempt) = &previous.attempt else { return Ok(previous.clone()) };
        let mut known_unapplied = known_unapplied;
        // Receipts outlive a binding or library switch, so the attempt still resolves exactly; the record then ends Stale.
        let retargeted = self.lww_binding_authority()? != previous.target.binding_authority
            || self.library_identity()? != previous.target.library_identity;
        if !known_unapplied { self.lww_recover_intents()?; }
        let receipt: Option<(String, i64)> = self.connection.query_row(
            "SELECT digest,revision FROM lww_requests WHERE request_id=?1", [&attempt.request_id], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
        let mut pending: bool = self.device_store()?.connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM lww_intents WHERE request_id=?1)", [&attempt.request_id], |row| row.get(0))?;
        if let Some((digest, revision)) = &receipt {
            if digest != &attempt.intent_digest || attempt.expected_revision.checked_add(1) != Some(*revision) { return Err(invalid("receipt-integrity")); }
        }
        if pending && retargeted {
            // Only the original library can prove that discarding a quarantined intent loses no uncertain write.
            if self.library_identity()? == previous.target.library_identity {
                if let Some(intent) = self.lww_quarantined_intents()?.into_iter().find(|intent| intent.request_id == attempt.request_id && intent.discardable) {
                    let digest: String = self.device_store()?.connection().query_row(
                        "SELECT digest FROM lww_intents WHERE request_id=?1", [&attempt.request_id], |row| row.get(0))?;
                    if digest != attempt.intent_digest { return Err(invalid("intent-integrity")); }
                    if self.lww_discard_quarantined_intent(&attempt.request_id, &intent.token, self.revision()?)? && receipt.is_none() {
                        known_unapplied = true;
                    }
                    pending = self.device_store()?.connection().query_row(
                        "SELECT EXISTS(SELECT 1 FROM lww_intents WHERE request_id=?1)", [&attempt.request_id], |row| row.get(0))?;
                }
            }
            // Keep the exact attempt and its capacity obligation when proof is unavailable. Never replay to the new target.
            if pending { return if previous.phase.terminal() { Ok(previous.clone()) } else { self.generation_stale_record(ticket, previous) }; }
        }
        let mut next = previous.clone();
        if let Some((digest, revision)) = receipt {
            if digest != attempt.intent_digest || attempt.expected_revision.checked_add(1) != Some(revision) { return Err(invalid("receipt-integrity")); }
            if pending { return Err(StoreError::CommitBusy); }
            next.committed_revision = Some(revision);
            let count = match &attempt.kind {
                AttemptKind::Output { sequence, output_hash } => {
                    next.checkpoint = *sequence;
                    next.output_hash = Some(output_hash.clone());
                    if let Some(execution) = next.execution.as_mut() {
                        execution.output_receipt = Some(OutputReceipt { request_id: attempt.request_id.clone(),
                            intent_digest: attempt.intent_digest.clone(), receipt_revision: revision,
                            source_sequence: *sequence, output_hash: output_hash.clone() });
                    }
                    output_index(previous) + 1
                }
                AttemptKind::HistoryIds => {
                    let preparation = next.preparation.as_mut().ok_or_else(|| invalid("preparation-required"))?;
                    preparation.normalization = Normalization::Settled {
                        receipt: Some((attempt.request_id.clone(), attempt.intent_digest.clone(), revision)),
                    };
                    preparation.normalized_at = Some(now_millis()?);
                    previous.initial_count
                }
                AttemptKind::Effect { effect } => {
                    let preparation = next.preparation.as_mut().ok_or_else(|| invalid("preparation-required"))?;
                    *preparation.effects.get_mut(*effect) = EffectState::Settled {
                        receipt: Some((attempt.request_id.clone(), attempt.intent_digest.clone(), revision)),
                    };
                    // The receipt proves the rows were written, whatever the target holds now.
                    if attempt.edited_count.is_some() { preparation.edited_count = attempt.edited_count; }
                    attempt.edited_count.unwrap_or(previous.initial_count)
                }
            };
            next.attempt = None;
            // An effect without message edits writes no message, so the target's fence stays exactly as it was.
            let unchanged = matches!(attempt.kind, AttemptKind::Effect { .. }) && attempt.edited_count.is_none();
            if retargeted {
                if !next.phase.terminal() { next.phase = Phase::Stale; }
            } else {
                match self.generation_snapshot(&previous.target, false) {
                    Ok((_, fence)) if unchanged && fence == previous.fence => {}
                    Ok((_, fence)) if !unchanged && fence.count == count
                        && fence.messages.as_ref().is_some_and(|unit| unit.version == attempt.request_id)
                        && [&fence.character, &fence.conversation].into_iter().zip([&previous.fence.character, &previous.fence.conversation])
                            .all(|(current, prior)| current == prior || current.as_ref().is_some_and(|unit| unit.version == attempt.request_id)) => {
                        next.fence = fence;
                    }
                    Ok(_) => next.phase = Phase::Stale,
                    Err(StoreError::Validation { .. }) => next.phase = Phase::Stale,
                    Err(error) => return Err(error),
                }
            }
        } else if pending {
            return Err(StoreError::CommitBusy);
        } else {
            if !known_unapplied && (matches!(attempt.kind, AttemptKind::Effect { .. })
                || self.library_identity()? != previous.target.library_identity) {
                if retargeted { return if previous.phase.terminal() { Ok(previous.clone()) } else { self.generation_stale_record(ticket, previous) }; }
                return Err(invalid("receipt-missing"));
            }
            if !retargeted && !known_unapplied && matches!(attempt.kind, AttemptKind::HistoryIds)
                && self.generation_snapshot(&previous.target, false)?.1 != previous.fence {
                return Err(invalid("receipt-missing"));
            }
            next.attempt = None;
            if retargeted && !next.phase.terminal() { next.phase = Phase::Stale; }
        }
        self.generation_replace_record(ticket, previous, &next)?;
        Ok(next)
    }

    pub(crate) fn generation_checkpoint(&mut self, owner: &Owner, checkpoint: &Checkpoint) -> StoreResult<Status> {
        self.generation_checkpoint_with(owner, checkpoint, None)
    }

    // The first output row carries the send's generation metadata. A definite revision conflict that left the
    // target unchanged retries with a new frozen request, as the TS save path resends its commit.
    pub(crate) fn generation_checkpoint_with(&mut self, owner: &Owner, checkpoint: &Checkpoint, info: Option<&OutputInfo>) -> StoreResult<Status> {
        if checkpoint.text.len() > MAX_OUTPUT_BYTES || checkpoint.sequence == 0 { return Err(invalid("checkpoint-limit")); }
        let gate = segment_gate(&owner.ticket)?;
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let previous = self.generation_record(&owner.ticket)?;
        require_owner(&previous, owner)?;
        let mut previous = self.generation_resolve_attempt(&owner.ticket, &previous)?;
        if !preparation_ready(&previous) { return Err(invalid("preparation-pending")); }
        let output_hash = risunest_sync_wire::hash(checkpoint.text.as_bytes());
        if checkpoint.sequence == previous.checkpoint {
            if previous.output_hash.as_deref() != Some(output_hash.as_str()) { return Err(invalid("checkpoint-integrity")); }
            return Ok(status(&owner.ticket, &previous));
        }
        if previous.phase != Phase::Running { return Err(invalid("stale-owner")); }
        if let Some(execution) = &previous.execution {
            if execution.dispatch_id.is_none() { return Err(invalid("dispatch-required")); }
            // A decision admitted before this segment closes output; one admitted after it retains this output.
            if execution.cancel.is_some() || execution.terminal.is_some() { return Err(invalid("execution-closed")); }
        }
        if previous.checkpoint.checked_add(1) != Some(checkpoint.sequence) { return Err(invalid("checkpoint-sequence")); }
        let mut retries = 0;
        loop {
            let (snapshot, fence) = match self.generation_snapshot(&previous.target, false) {
                Ok(snapshot) => snapshot,
                Err(StoreError::Validation { .. }) => return self.generation_stale(&owner.ticket, &previous),
                Err(error) => return Err(error),
            };
            if fence != previous.fence { return self.generation_stale(&owner.ticket, &previous); }
            if previous.output_hash.as_deref() == Some(output_hash.as_str()) {
                let mut next = previous.clone();
                next.checkpoint = checkpoint.sequence;
                self.generation_replace_record(&owner.ticket, &previous, &next)?;
                return Ok(status(&owner.ticket, &next));
            }
            let message = if previous.checkpoint == 0 {
                let mut message = json!({ "role": "char", "data": checkpoint.text, "chatId": previous.message_id,
                    "saying": previous.target.character_id, "time": previous.message_time });
                if let Some(info) = info {
                    let stage1 = previous.preparation.as_ref().and_then(|value| Some(value.ready_at?.saturating_sub(value.normalized_at?))).unwrap_or(0);
                    message["generationInfo"] = json!({ "model": info.model, "generationId": previous.message_id,
                        "inputTokens": info.input_tokens, "outputTokens": info.output_tokens, "maxContext": info.max_context,
                        "stageTiming": { "stage1": stage1, "stage2": 0, "stage3": info.stage3, "stage4": info.stage4_started.map(|start| u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)).unwrap_or(0) } });
                    message["promptInfo"] = json!({});
                }
                message
            } else {
                let page = self.read_conversation_window(&ConversationWindowQuery {
                    character_id: previous.target.character_id.clone(), conversation_id: previous.target.conversation_id.clone(),
                    start_index: Some(output_index(&previous)), limit: Some(1), anchor_message_id: None,
                    anchor_occurrence: None, before: None, after: None, skip_parser_work: None,
                }, None)?.ok_or_else(|| invalid("missing-output"))?;
                if page.revision != snapshot.revision { return Err(StoreError::RevisionConflict { expected: snapshot.revision, actual: page.revision }); }
                let mut message = page.value.messages.into_iter().next().ok_or_else(|| invalid("missing-output"))?;
                if message.get("chatId").and_then(Value::as_str) != Some(previous.message_id.as_str()) { return Err(invalid("stale-output")); }
                message.as_object_mut().ok_or_else(|| invalid("invalid-output"))?.insert("data".into(), Value::String(checkpoint.text.clone()));
                message
            };
            let request_id = uuid::Uuid::new_v4().to_string();
            let commit = WorkingSetCommit {
                expected_revision: snapshot.revision, request_id: Some(request_id.clone()), binding_authority: Some(previous.target.binding_authority),
                conversations: Some(vec![ConversationMutation::ReplaceRange {
                    character_id: previous.target.character_id.clone(), conversation_id: previous.target.conversation_id.clone(),
                    start: output_index(&previous), delete_count: i64::from(previous.checkpoint > 0), messages: vec![message],
                    conversation: None, configured_index: None,
                }]), ..Default::default()
            };
            let attempt = Attempt { request_id, intent_digest: super::lww::commit_intent_digest(&commit)?,
                kind: AttemptKind::Output { sequence: checkpoint.sequence, output_hash: output_hash.clone() }, expected_revision: snapshot.revision,
                edited_count: None };
            let mut issued = previous.clone();
            issued.attempt = Some(attempt.clone());
            self.generation_replace_record(&owner.ticket, &previous, &issued)?;
            #[cfg(test)]
            BEFORE_CHECKPOINT_COMMIT.with(|hook| hook.borrow_mut().take()).map(|hook| hook());
            let result = self.commit_holding(&commit, |store| store.generation_barrier(owner, &attempt));
            let resolved = self.generation_resolve_attempt(&owner.ticket, &issued)?;
            if resolved.checkpoint == checkpoint.sequence { return Ok(status(&owner.ticket, &resolved)); }
            // A commit that landed after the snapshot decides by the fence: a changed target ends Stale,
            // and an unchanged one takes a new frozen request at the current revision.
            if matches!(result, Err(StoreError::RevisionConflict { .. })) && resolved.phase == Phase::Running {
                match self.generation_snapshot(&resolved.target, false) {
                    Ok((_, fence)) if fence == resolved.fence => {}
                    Ok(_) | Err(StoreError::Validation { .. }) => return self.generation_stale(&owner.ticket, &resolved),
                    Err(error) => return Err(error),
                }
                if retries < MAX_REVISION_RETRIES {
                    retries += 1;
                    previous = resolved;
                    continue;
                }
            }
            result?;
            return Err(invalid("receipt-missing"));
        }
    }

    pub(crate) fn generation_finish(&mut self, owner: &Owner, cancelled: bool) -> StoreResult<Status> {
        let gate = segment_gate(&owner.ticket)?;
        #[cfg(test)]
        if matches!(gate.try_lock(), Err(std::sync::TryLockError::WouldBlock)) {
            FINISH_WAITING_FOR_SEGMENT.with(|hook| hook.borrow_mut().take()).map(|hook| hook());
        }
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let previous = self.generation_record(&owner.ticket)?;
        require_owner(&previous, owner)?;
        if previous.execution.is_some() { return Err(invalid("execution-owned")); }
        let previous = self.generation_resolve_preparation(&owner.ticket, &previous)?;
        if previous.phase.terminal() { return Ok(status(&owner.ticket, &previous)); }
        if !cancelled && !preparation_ready(&previous) { return Err(invalid("preparation-pending")); }
        let mut next = previous.clone();
        next.phase = match self.generation_snapshot(&previous.target, false) {
            Ok((_, fence)) if fence == previous.fence => if cancelled { Phase::Cancelled } else { Phase::Completed },
            Ok(_) | Err(StoreError::Validation { .. }) => Phase::Stale,
            Err(error) => return Err(error),
        };
        self.generation_replace_record(&owner.ticket, &previous, &next)?;
        Ok(status(&owner.ticket, &next))
    }

    // An owner lost in this process settles by the foreign-recovery rule: an issued request without a decision is Interrupted.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn generation_settle_lost(&mut self, owner: &Owner) -> StoreResult<Status> {
        let gate = segment_gate(&owner.ticket)?;
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let previous = self.generation_record(&owner.ticket)?;
        require_owner(&previous, owner)?;
        if previous.execution.is_none() { return Err(invalid("execution-evidence-lost")); }
        let previous = self.generation_resolve_attempt(&owner.ticket, &previous)?;
        Ok(status(&owner.ticket, &self.generation_finalize_execution(&owner.ticket, &previous, Settlement::Recovery { foreign: true })?))
    }

    // Verify completed effects; terminal records without an attempt may also have skipped effects.
    pub(crate) fn generation_effects_evidence(&self, ticket: &Ticket) -> StoreResult<Option<EffectsEvidence>> {
        let record = self.generation_projection_record(ticket)?;
        if record.attempt.is_some() { return Ok(None); }
        let read = self.connection.unchecked_transaction()?;
        if super::active_generation(&read)? != record.target.library_identity { return Ok(None); }
        let mut evidence = EffectsEvidence { affected_unit_keys: Vec::new(), receipts: Vec::new() };
        let Some(preparation) = &record.preparation else { return Ok(Some(evidence)); };
        let keys = send_effect_unit_keys(&record.target)?;
        for effect in [SendEffect::Interaction, SendEffect::Variables, SendEffect::Memory] {
            let receipt = match preparation.effects.get(effect) {
                EffectState::Settled { receipt } => receipt,
                // Terminalization cannot leave a possibly written effect without its frozen attempt.
                EffectState::Pending if record.phase.terminal() => continue,
                EffectState::Pending => return Ok(None),
            };
            let Some((request_id, digest, revision)) = receipt else { continue };
            if !canonical_uuid(request_id) || !self::digest(digest) || *revision <= record.target.expected_revision {
                return Err(invalid("effect-receipt-integrity"));
            }
            let stored: Option<(String, i64)> = read.query_row(
                "SELECT digest,revision FROM lww_requests WHERE request_id=?1", [request_id], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
            if stored.as_ref() != Some(&(digest.clone(), *revision)) { return Ok(None); }
            evidence.receipts.push(EffectReceipt { request_id: request_id.clone(), digest: digest.clone(), revision: *revision });
            let mut effect_keys = keys.for_effect(effect);
            if effect == SendEffect::Variables && preparation.edited_count.is_some() { effect_keys.push(&keys.messages); }
            evidence.affected_unit_keys.extend(effect_keys.iter().map(|key| key.as_str().to_owned()));
        }
        Ok(Some(evidence))
    }

    // Read-only origin, including terminal records retained for receipt adoption.
    pub(crate) fn generation_target(&self, ticket: &Ticket) -> StoreResult<Target> {
        Ok(self.generation_projection_record(ticket)?.target)
    }

    // Read-only: reserved, running and unresolved records within the journal's own bounds.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn generation_active(&self) -> StoreResult<Vec<ActiveGeneration>> {
        let Some(journal) = read_journal(self.device_store()?.connection())? else { return Ok(Vec::new()) };
        validate_protection_journal(&journal)?;
        Ok(journal.records.iter().filter(|(_, record)| !record.phase.terminal() || record.attempt.is_some()).take(MAX_RESERVED + MAX_ACTIVE)
            .map(|(sequence, record)| ActiveGeneration {
                ticket: Ticket { epoch: journal.epoch.clone(), sequence: (*sequence).into() },
                character_id: record.target.character_id.clone(), conversation_id: record.target.conversation_id.clone(),
                phase: record.phase.clone(), library_identity: record.target.library_identity.clone(),
            }).collect())
    }

    pub(crate) fn generation_recover(&mut self, ticket: &Ticket) -> StoreResult<Status> {
        let gate = segment_gate(ticket)?;
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let previous = self.generation_record(ticket)?;
        let previous = self.generation_resolve_preparation(ticket, &previous)?;
        let foreign = previous.session.as_deref() != Some(runtime_session_id());
        if previous.phase == Phase::Running && previous.execution.is_some() {
            return Ok(status(ticket, &self.generation_finalize_execution(ticket, &previous, Settlement::Recovery { foreign })?));
        }
        if previous.phase != Phase::Running || !foreign {
            return Ok(status(ticket, &previous));
        }
        let mut next = previous.clone();
        next.phase = Phase::Interrupted;
        self.generation_replace_record(ticket, &previous, &next)?;
        Ok(status(ticket, &next))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn generation_execution(&self, ticket: &Ticket) -> StoreResult<Option<Execution>> {
        Ok(self.generation_record(ticket)?.execution)
    }

    // Compare-and-replace from ReadyOwned to Issued. Only a call that returns Issued may send a request.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn generation_claim_dispatch(&mut self, owner: &Owner, lifecycle_id: &str) -> StoreResult<DispatchClaim> {
        let gate = segment_gate(&owner.ticket)?;
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let previous = self.generation_record(&owner.ticket)?;
        require_owner(&previous, owner)?;
        let execution = previous.execution.as_ref().ok_or_else(|| invalid("execution-evidence-lost"))?;
        if execution.lifecycle_id != lifecycle_id { return Err(invalid("stale-lifecycle")); }
        if execution.dispatch_id.is_some() { return Err(invalid("dispatch-claimed")); }
        let previous = self.generation_resolve_attempt(&owner.ticket, &previous)?;
        let execution = previous.execution.as_ref().ok_or_else(|| invalid("execution-evidence-lost"))?;
        if previous.phase != Phase::Running || execution.cancel.is_some() || execution.terminal.is_some() {
            return Ok(DispatchClaim::Refused(status(&owner.ticket, &previous)));
        }
        if !previous.preparation.as_ref().is_some_and(|value| value.ready && matches!(value.normalization, Normalization::Settled { .. })) {
            return Err(invalid("preparation-pending"));
        }
        if [SendEffect::Interaction, SendEffect::Variables, SendEffect::Memory].into_iter().any(|effect| !effect_settled(&previous, effect)) { return Err(invalid("effects-pending")); }
        match self.generation_snapshot(&previous.target, false) {
            Ok((_, fence)) if fence == previous.fence => {},
            Ok(_) | Err(StoreError::Validation { .. }) => return Ok(DispatchClaim::Refused(self.generation_stale(&owner.ticket, &previous)?)),
            Err(error) => return Err(error),
        }
        let dispatch_id = uuid::Uuid::new_v4().to_string();
        let mut next = previous.clone();
        if let Some(execution) = next.execution.as_mut() { execution.dispatch_id = Some(dispatch_id.clone()); }
        self.generation_replace_record(&owner.ticket, &previous, &next)?;
        Ok(DispatchClaim::Issued { dispatch_id, status: status(&owner.ticket, &next) })
    }

    // Accepted only for the exact offered sequence and hash with its retained receipt; uncommitted only once nothing is pending.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn generation_resolve_output(&mut self, owner: &Owner, sequence: u64, output_hash: &str) -> StoreResult<OutputResolution> {
        let gate = segment_gate(&owner.ticket)?;
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let previous = self.generation_record(&owner.ticket)?;
        require_owner(&previous, owner)?;
        let execution = previous.execution.as_ref().ok_or_else(|| invalid("execution-evidence-lost"))?;
        if execution.dispatch_id.is_none() { return Err(invalid("dispatch-required")); }
        let record = match self.generation_resolve_attempt(&owner.ticket, &previous) {
            Ok(record) => record,
            Err(StoreError::CommitBusy) => return Ok(OutputResolution::Unresolved),
            Err(error) => return Err(error),
        };
        if record.attempt.is_some() { return Ok(OutputResolution::Unresolved); }
        if record.checkpoint < sequence { return Ok(OutputResolution::Uncommitted); }
        if record.checkpoint > sequence { return Err(invalid("checkpoint-sequence")); }
        if record.output_hash.as_deref() != Some(output_hash) { return Err(invalid("checkpoint-integrity")); }
        if self.library_identity()? != record.target.library_identity || self.lww_binding_authority()? != record.target.binding_authority {
            return Err(invalid("stale-target"));
        }
        match self.generation_output_receipt(&record, sequence, output_hash)? {
            ReceiptCheck::Verified(receipt) => Ok(OutputResolution::Committed(receipt)),
            ReceiptCheck::Missing => Err(invalid("output-evidence-unavailable")),
            ReceiptCheck::Mismatch => Err(invalid("receipt-integrity")),
        }
    }

    // The first persisted decision is final. It is durable before attempt resolution so recovery can finish it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn generation_settle_owned(&mut self, owner: &Owner, decision: TerminalDecision) -> StoreResult<Status> {
        if !valid_decision(&decision) { return Err(invalid("invalid-decision")); }
        let gate = segment_gate(&owner.ticket)?;
        let _segment = gate.lock().map_err(|_| invalid("segment-poisoned"))?;
        let mut previous = self.generation_record(&owner.ticket)?;
        require_owner(&previous, owner)?;
        let execution = previous.execution.as_ref().ok_or_else(|| invalid("execution-evidence-lost"))?;
        if previous.phase.terminal() { return Ok(status(&owner.ticket, &previous)); }
        if execution.terminal.is_none() {
            let decision = match (decision, execution.cancel) {
                (TerminalDecision::Normal { .. }, Some(reason)) => TerminalDecision::Cancelled { reason },
                (decision, _) => decision,
            };
            if let TerminalDecision::Normal { sequence, output_hash } = &decision {
                if execution.dispatch_id.is_none() { return Err(invalid("dispatch-required")); }
                // With no offer left to resolve, an unproven normal decision could never settle, so it is refused unwritten.
                if previous.attempt.is_none() && (previous.checkpoint != *sequence || previous.output_hash.as_deref() != Some(output_hash.as_str())
                    || execution.output_receipt.is_none()) {
                    return Err(invalid("output-evidence-unavailable"));
                }
            }
            let mut next = previous.clone();
            if let Some(execution) = next.execution.as_mut() { execution.terminal = Some(decision); }
            self.generation_replace_record(&owner.ticket, &previous, &next)?;
            previous = next;
        }
        let previous = self.generation_resolve_attempt(&owner.ticket, &previous)?;
        Ok(status(&owner.ticket, &self.generation_finalize_execution(&owner.ticket, &previous, Settlement::Owner)?))
    }

    fn generation_output_receipt(&self, record: &Record, sequence: u64, output_hash: &str) -> StoreResult<ReceiptCheck> {
        let Some(receipt) = record.execution.as_ref().and_then(|execution| execution.output_receipt.as_ref()) else { return Ok(ReceiptCheck::Missing) };
        if record.attempt.is_some() || sequence == 0 || record.checkpoint != sequence || record.output_hash.as_deref() != Some(output_hash)
            || receipt.output_hash != output_hash || receipt.source_sequence == 0 || receipt.source_sequence > sequence {
            return Ok(ReceiptCheck::Missing);
        }
        let actual: Option<(String, i64)> = self.connection.query_row("SELECT digest,revision FROM lww_requests WHERE request_id=?1",
            [&receipt.request_id], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
        Ok(if actual == Some((receipt.intent_digest.clone(), receipt.receipt_revision)) { ReceiptCheck::Verified(receipt.clone()) } else { ReceiptCheck::Mismatch })
    }

    // Completed needs the decision, the dispatch and the exact verified terminal output receipt to agree.
    fn generation_finalize_execution(&mut self, ticket: &Ticket, previous: &Record, settlement: Settlement) -> StoreResult<Record> {
        let execution = previous.execution.as_ref().ok_or_else(|| invalid("execution-evidence-lost"))?;
        if previous.phase != Phase::Running { return Ok(previous.clone()); }
        let foreign = matches!(settlement, Settlement::Recovery { foreign: true });
        let phase = match &execution.terminal {
            Some(TerminalDecision::Normal { .. }) if execution.cancel.is_some() => Phase::Cancelled,
            Some(TerminalDecision::Normal { sequence, output_hash }) => {
                // A receipt is never reconciled against another library or binding.
                if self.lww_binding_authority()? != previous.target.binding_authority || self.library_identity()? != previous.target.library_identity {
                    Phase::Stale
                } else if execution.dispatch_id.is_some()
                    && matches!(self.generation_output_receipt(previous, *sequence, output_hash)?, ReceiptCheck::Verified(_)) {
                    Phase::Completed
                } else {
                    match settlement {
                        Settlement::Owner => return Err(invalid("output-evidence-unavailable")),
                        Settlement::Recovery { foreign: true } => Phase::Interrupted,
                        Settlement::Recovery { foreign: false } => return Ok(previous.clone()),
                    }
                }
            }
            Some(TerminalDecision::Cancelled { .. }) => Phase::Cancelled,
            Some(TerminalDecision::Failed { .. }) => Phase::Failed,
            None if matches!(settlement, Settlement::Owner) => return Err(invalid("terminal-required")),
            None if execution.dispatch_id.is_none() && execution.cancel.is_some() => Phase::Cancelled,
            None if foreign => Phase::Interrupted,
            None => return Ok(previous.clone()),
        };
        let phase = if phase == Phase::Interrupted { phase } else {
            match self.generation_snapshot(&previous.target, false) {
                Ok((_, fence)) if fence == previous.fence => phase,
                Ok(_) | Err(StoreError::Validation { .. }) => Phase::Stale,
                Err(error) => return Err(error),
            }
        };
        let mut next = previous.clone();
        next.phase = phase;
        self.generation_replace_record(ticket, previous, &next)?;
        Ok(next)
    }

    pub(crate) fn generation_reconcile_orphans(&mut self) -> StoreResult<Vec<Status>> {
        let Some(journal) = read_journal(self.device_store()?.connection())? else { return Ok(Vec::new()) };
        let tickets: Vec<_> = journal.records.iter().filter(|(_, record)| {
            record.phase == Phase::Running && record.session.as_deref() != Some(runtime_session_id())
        }).map(|(sequence, _)| Ticket { epoch: journal.epoch.clone(), sequence: (*sequence).into() }).collect();
        let mut recovered = Vec::new();
        let mut failure = None;
        for ticket in tickets {
            match self.generation_recover(&ticket) {
                Ok(status) => recovered.push(status),
                Err(StoreError::Validation { message }) if message == "native-generation:expired-ticket" => {},
                Err(error) => if failure.is_none() { failure = Some(error); },
            }
        }
        if let Some(error) = failure { return Err(error); }
        Ok(recovered)
    }
}

#[cfg(test)]
#[path = "../native_generation/store_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "../native_generation/prepare_tests.rs"]
pub(crate) mod preparation_tests;
