//! Strict continuation of a previously acknowledged selected-conversation copy.
use super::*;
use super::super::{archive, AssetOwnerLocator, ConversationMutation, ReadTarget};
use std::collections::HashMap;

#[cfg(test)]
#[path = "continuation_tests.rs"]
mod tests;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CopyOrigin {
    pub request_id: String,
    pub digest: String,
    pub committed_revision: i64,
    pub source: CopySource,
    pub target_character_id: String,
    pub target_conversation_id: String,
    pub binding_authority: risunest_sync_wire::stamp::DecimalU64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CopyContinuationRequest {
    pub header: lww::Header,
    pub origin: CopyOrigin,
    pub context_id: String,
    pub basis_version: u64,
    pub current_lease: String,
    pub expected_revision: i64,
    pub capture_id: String,
    pub character_fields: Vec<CopyField>,
    pub conversation_fields: Vec<CopyField>,
    pub ranges: Vec<CopyRange>,
    pub introduced_asset_bindings: Vec<IntroducedAssetBinding>,
    pub introduced_owner_head: Option<IntroducedOwnerHead>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ContinuationConflict {
    Identity, TargetMissing, TargetArchived, History, Field, AssetBinding, Range,
    UnsupportedField, Budget, EvidenceExpired, Cancelled, BasisVersion,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub(crate) enum OpenContinuationResult {
    Ready { context_id: String, origin: CopyOrigin, basis_version: u64,
        covered_through_request_id: String, required_lease_ids: Vec<String> },
    ReceiptNotObserved,
    HistoricalStatusUnavailable,
    CommittedRefreshRequired,
    OutcomeUnresolved { candidate_request_id: String },
    Conflict { kind: ContinuationConflict },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub(crate) enum CopyContinuationResult {
    Committed { revision: i64, source_character_id: String, source_conversation_id: String,
        target_character_id: String, target_conversation_id: String, capture_id: String, next_basis_version: u64 },
    Replayed { committed_revision: i64, source_character_id: String, source_conversation_id: String,
        target_character_id: String, target_conversation_id: String, capture_id: String,
        requires_current_recovery: bool, current_identity_matches: bool },
    HistoricalStatusUnavailable,
    OutcomeUnresolved { candidate_request_id: String },
    Conflict { kind: ContinuationConflict },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub(crate) enum CloseContinuationResult {
    Closed,
    BasisVersionConflict,
    CloseBlocked { reason: CloseBlockedReason, candidate_request_id: String },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CloseBlockedReason { OutcomeUnresolved }

#[derive(Clone)]
pub(super) struct ContinuationLimits {
    contexts: usize, request_bytes: usize, basis_bytes: usize, store_basis_bytes: usize,
    inflight_bytes: usize, metadata_bytes: usize, store_metadata_bytes: usize,
    inflight_metadata_bytes: usize, leases: usize, store_leases: usize,
}

impl Default for ContinuationLimits {
    fn default() -> Self {
        const MIB: usize = 1024 * 1024;
        Self { contexts: 4, request_bytes: 64 * MIB, basis_bytes: 64 * MIB,
            store_basis_bytes: 128 * MIB, inflight_bytes: 64 * MIB, metadata_bytes: 8 * MIB,
            store_metadata_bytes: 16 * MIB, inflight_metadata_bytes: 8 * MIB, leases: 32, store_leases: 64 }
    }
}

#[derive(Clone, Serialize)]
enum Basis { Original(Box<ConflictCopyRequest>), Patched { lease: String, ranges: Vec<CopyRange> } }

#[derive(Clone, Serialize)]
struct Metadata { character: Value, conversation: Value }

#[derive(Clone, Serialize)]
struct Head {
    version: u64,
    covered_request_id: String,
    basis: Basis,
    raw: Metadata,
    canonical: Metadata,
    history_hash: String,
    history_count: usize,
    aliases: Vec<AssetAlias>,
    owner_head: Option<AssetOwnerHead>,
    leases: BTreeSet<String>,
}

#[derive(Serialize)]
struct Candidate { request: CopyContinuationRequest, digest: String, next: Head, leases: BTreeSet<String> }

struct Context { origin: CopyOrigin, head: Head, candidate: Option<Candidate> }

#[derive(Default)]
pub(crate) struct ContinuationState { contexts: HashMap<String, Context>, limits: ContinuationLimits }

impl ContinuationState {
    pub(crate) fn pins(&self, lease: &str) -> bool {
        self.contexts.values().any(|context| context.head.leases.contains(lease)
            || context.candidate.as_ref().is_some_and(|candidate| candidate.leases.contains(lease)))
    }

    pub(crate) fn clear(&mut self) { self.contexts.clear(); }

    fn admit(&self, new_context: Option<&Context>, candidate: Option<(&str, &Candidate)>) -> StoreResult<()> {
        let limits = &self.limits;
        let mut settled_basis = 0usize;
        let mut settled_metadata = 0usize;
        let mut handles = BTreeSet::new();
        let mut inflight = 0usize;
        let mut inflight_metadata = 0usize;
        let mut candidate_count = 0usize;
        let mut promotion = None;
        if self.contexts.len() + usize::from(new_context.is_some()) > limits.contexts { return Err(fail(ContinuationConflict::Budget)); }
        for (id, context) in self.contexts.iter().map(|(id, c)| (id.as_str(), c)).chain(new_context.map(|c| ("", c))) {
            let basis = encoded_len(&context.head.basis)?;
            let metadata = head_metadata_size(&context.origin, &context.head)?;
            if basis > limits.basis_bytes || metadata > limits.metadata_bytes { return Err(fail(ContinuationConflict::Budget)); }
            settled_basis = checked_sum(settled_basis, basis)?;
            settled_metadata = checked_sum(settled_metadata, metadata)?;
            let mut local = context.head.leases.clone();
            let pending = candidate.filter(|(target, _)| *target == id).map(|(_, c)| c).or(context.candidate.as_ref());
            if let Some(pending) = pending {
                candidate_count += 1;
                // Count the entire frozen candidate, including proof handles and both metadata
                // views. The separate metadata bound intentionally counts those values again.
                inflight = checked_sum(inflight, encoded_len(pending)?)?;
                let metadata = head_metadata_size(&context.origin, &pending.next)?;
                if encoded_len(&pending.next.basis)? > limits.basis_bytes || metadata > limits.metadata_bytes { return Err(fail(ContinuationConflict::Budget)); }
                inflight_metadata = checked_sum(inflight_metadata, checked_sum(metadata, encoded_len(&pending.leases)?)?)?;
                local.extend(pending.leases.iter().cloned());
                promotion = Some((basis, head_metadata_size(&context.origin, &context.head)?, encoded_len(&pending.next.basis)?, metadata));
            }
            if local.len() > limits.leases { return Err(fail(ContinuationConflict::Budget)); }
            handles.extend(local);
        }
        if settled_basis > limits.store_basis_bytes || settled_metadata > limits.store_metadata_bytes
            || inflight > limits.inflight_bytes || inflight_metadata > limits.inflight_metadata_bytes
            || handles.len() > limits.store_leases || candidate_count > 1 { return Err(fail(ContinuationConflict::Budget)); }
        if let Some((old_basis, old_metadata, next_basis, next_metadata)) = promotion {
            if checked_sum(settled_basis - old_basis, next_basis)? > limits.store_basis_bytes
                || checked_sum(settled_metadata - old_metadata, next_metadata)? > limits.store_metadata_bytes { return Err(fail(ContinuationConflict::Budget)); }
        }
        Ok(())
    }
}

fn checked_sum(a: usize, b: usize) -> StoreResult<usize> { a.checked_add(b).ok_or_else(|| fail(ContinuationConflict::Budget)) }
fn encoded_len<T: Serialize>(value: &T) -> StoreResult<usize> { Ok(serde_json::to_vec(value)?.len()) }
fn head_metadata_size(origin: &CopyOrigin, head: &Head) -> StoreResult<usize> {
    // Include serialized field names and framing, with room for the context's fixed-width ID.
    checked_sum(encoded_len(&(origin, head))? - encoded_len(&head.basis)?, 128)
}
fn fail(kind: ContinuationConflict) -> StoreError {
    StoreError::Validation { message: format!("copy-continuation:{}", serde_json::to_string(&kind).expect("enum serializes")) }
}
fn classify(error: &StoreError) -> Option<ContinuationConflict> {
    if *error == StoreError::SnapshotReleased { return Some(ContinuationConflict::EvidenceExpired); }
    let StoreError::Validation { message } = error else { return None; };
    if let Some(kind) = message.strip_prefix("copy-continuation:") { return serde_json::from_str(kind).ok(); }
    message.strip_prefix("conflict-copy:").and_then(|kind| serde_json::from_str::<CopyConflict>(kind).ok()).map(|kind| match kind {
        CopyConflict::Identity => ContinuationConflict::Identity, CopyConflict::SourceHistory => ContinuationConflict::EvidenceExpired,
        CopyConflict::Range => ContinuationConflict::Range, CopyConflict::AssetBinding => ContinuationConflict::AssetBinding,
        CopyConflict::TargetCollision => ContinuationConflict::TargetMissing, CopyConflict::UnsupportedField => ContinuationConflict::UnsupportedField,
        CopyConflict::Cancelled => ContinuationConflict::Cancelled,
    })
}
fn continuation_digest(request: &CopyContinuationRequest) -> StoreResult<String> {
    exact_hash(&serde_json::to_value(("conflict-copy-continuation-v1", request))?)
}
fn check_cancelled(check: &dyn Fn() -> bool) -> StoreResult<()> { if check() { Err(fail(ContinuationConflict::Cancelled)) } else { Ok(()) } }
fn proof_leases(request: &ConflictCopyRequest) -> BTreeSet<String> {
    let mut leases = BTreeSet::from([request.source.lease.clone()]);
    leases.extend(request.introduced_asset_bindings.iter().map(|proof| proof.witness_lease.clone()));
    leases.extend(request.introduced_owner_head.iter().map(|proof| proof.witness_lease.clone()));
    leases
}
fn metadata(mut character: Value, mut conversation: Value) -> StoreResult<Metadata> {
    character.as_object_mut().ok_or_else(|| fail(ContinuationConflict::History))?.remove("chats");
    conversation.as_object_mut().ok_or_else(|| fail(ContinuationConflict::History))?.remove("message");
    Ok(Metadata { character, conversation })
}
fn messages(conversation: &Value) -> StoreResult<Vec<Value>> {
    conversation.get("message").and_then(Value::as_array).cloned().ok_or_else(|| fail(ContinuationConflict::History))
}
fn apply_ranges(history: &mut Vec<Value>, ranges: &[CopyRange]) -> StoreResult<()> {
    for range in ranges {
        let start = usize::try_from(range.start).map_err(|_| fail(ContinuationConflict::Range))?;
        let delete = usize::try_from(range.delete_count).map_err(|_| fail(ContinuationConflict::Range))?;
        let end = start.checked_add(delete).ok_or_else(|| fail(ContinuationConflict::Range))?;
        if start > history.len() || end > history.len() || range.messages.iter().any(|row| !row.is_object()) { return Err(fail(ContinuationConflict::Range)); }
        history.splice(start..end, range.messages.iter().cloned());
    }
    Ok(())
}
fn field_name(field: &CopyField) -> &str { match field { CopyField::Set { field, .. } | CopyField::Delete { field } => field } }
fn validate_continuation_fields(fields: &[CopyField], character: bool) -> StoreResult<()> {
    validate_fields(fields, character)?;
    if fields.iter().any(|field| matches!(field_name(field), "chatFolders" | "folderId" | "ownerId" | "characterId" | "type")) {
        return Err(fail(ContinuationConflict::UnsupportedField));
    }
    Ok(())
}
fn compare_fields(current: &Value, expected: &Value, fields: &[CopyField]) -> StoreResult<()> {
    for field in fields {
        // Option preserves the difference between a missing property and JSON null.
        if current.get(field_name(field)) != expected.get(field_name(field)) { return Err(fail(ContinuationConflict::Field)); }
    }
    Ok(())
}

enum ReceiptState { Unavailable, Missing { matches: bool }, Found { revision: i64, matches: bool } }

impl PersistentStore {
    fn continuation_receipt(&self, origin: &CopyOrigin, request_id: &str, digest: &str,
        binding: &risunest_sync_wire::stamp::DecimalU64) -> StoreResult<ReceiptState> {
        let device = Transaction::new_unchecked(self.device_store()?.connection(), TransactionBehavior::Immediate)?;
        let library = Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)?;
        let current = sync_selection::identity(&library)?;
        if !same_library(&origin.source.identity, &current) { return Ok(ReceiptState::Unavailable); }
        let matches = same_identity(&origin.source.identity, &current) && lww::authority(&device)? == *binding;
        Ok(match receipt_by_id(&library, request_id, digest)? {
            Some(revision) => ReceiptState::Found { revision, matches }, None => ReceiptState::Missing { matches },
        })
    }

    pub(crate) fn open_conflict_copy_continuation(&mut self, original: &ConflictCopyRequest) -> StoreResult<OpenContinuationResult> {
        let result = self.open_copy_continuation_inner(original);
        match result { Err(error) if classify(&error).is_some() => Ok(OpenContinuationResult::Conflict { kind: classify(&error).unwrap() }), result => result }
    }

    fn open_copy_continuation_inner(&mut self, original: &ConflictCopyRequest) -> StoreResult<OpenContinuationResult> {
        let digest = digest(original)?;
        let mut origin = CopyOrigin { request_id: original.header.request_id.clone(), digest,
            committed_revision: 0, source: original.source.clone(), target_character_id: original.target_character_id.clone(),
            target_conversation_id: original.target_conversation_id.clone(), binding_authority: original.header.binding_authority.clone() };
        let receipt = self.continuation_receipt(&origin, &origin.request_id, &origin.digest, &origin.binding_authority);
        if receipt.is_err() {
            if let Some(candidate) = self.copy_continuations.contexts.values().find(|context| context.origin.request_id == origin.request_id
                && context.origin.digest == origin.digest).and_then(|context| context.candidate.as_ref()) {
                return Ok(OpenContinuationResult::OutcomeUnresolved { candidate_request_id: candidate.request.header.request_id.clone() });
            }
        }
        match receipt? {
            ReceiptState::Unavailable => return Ok(OpenContinuationResult::HistoricalStatusUnavailable),
            ReceiptState::Missing { .. } => return Ok(OpenContinuationResult::ReceiptNotObserved),
            ReceiptState::Found { matches: false, .. } => return Ok(OpenContinuationResult::CommittedRefreshRequired),
            ReceiptState::Found { revision, .. } => origin.committed_revision = revision,
        }
        if let Some((id, context)) = self.copy_continuations.contexts.iter().find(|(_, context)| context.origin == origin) {
            return Ok(open_result(id, context));
        }
        if encoded_len(original)? > self.copy_continuations.limits.request_bytes { return Err(fail(ContinuationConflict::Budget)); }
        let leases = proof_leases(original);
        for lease in &leases { self.read_view(Some(lease))?; }
        let prepared = self.prepare_conflict_copy(original, &|| false)?;
        let character = prepared.input.add_character.ok_or_else(|| fail(ContinuationConflict::History))?;
        let conversation = character.get("chats").and_then(Value::as_array).and_then(|chats| chats.first()).cloned().ok_or_else(|| fail(ContinuationConflict::History))?;
        let history = messages(&conversation)?;
        let mut raw_character = self.read_character(&origin.source.character_id, Some(&origin.source.lease))?.ok_or_else(|| fail(ContinuationConflict::EvidenceExpired))?.value;
        let mut raw_conversation = self.read_conversation(&origin.source.character_id, &origin.source.conversation_id, Some(&origin.source.lease))?.ok_or_else(|| fail(ContinuationConflict::EvidenceExpired))?.value;
        apply_fields(&mut raw_character, &original.character_fields)?;
        apply_fields(&mut raw_conversation, &original.conversation_fields)?;
        let head = Head { version: 0, covered_request_id: origin.request_id.clone(), basis: Basis::Original(Box::new(original.clone())),
            raw: metadata(raw_character, raw_conversation)?, canonical: metadata(character, conversation)?,
            history_hash: exact_hash(&json!(history))?, history_count: history.len(), aliases: prepared.aliases,
            owner_head: prepared.input.asset_owner_heads.and_then(|mut heads| heads.pop()), leases };
        let context = Context { origin, head, candidate: None };
        self.copy_continuations.admit(Some(&context), None)?;
        let id = format!("copy-continuation-{}", uuid::Uuid::new_v4());
        let result = open_result(&id, &context);
        self.copy_continuations.contexts.insert(id, context);
        Ok(result)
    }

    pub(crate) fn close_copy_continuation(&mut self, id: &str, expected_basis_version: u64) -> CloseContinuationResult {
        let Some(context) = self.copy_continuations.contexts.get(id) else { return CloseContinuationResult::Closed; };
        if let Some(candidate) = &context.candidate { return CloseContinuationResult::CloseBlocked {
            reason: CloseBlockedReason::OutcomeUnresolved, candidate_request_id: candidate.request.header.request_id.clone() }; }
        if context.head.version != expected_basis_version { return CloseContinuationResult::BasisVersionConflict; }
        self.copy_continuations.contexts.remove(id);
        CloseContinuationResult::Closed
    }
}

fn open_result(id: &str, context: &Context) -> OpenContinuationResult {
    if let Some(candidate) = &context.candidate { return OpenContinuationResult::OutcomeUnresolved { candidate_request_id: candidate.request.header.request_id.clone() }; }
    OpenContinuationResult::Ready { context_id: id.into(), origin: context.origin.clone(), basis_version: context.head.version,
        covered_through_request_id: context.head.covered_request_id.clone(), required_lease_ids: context.head.leases.iter().cloned().collect() }
}

struct PreparedContinuation { input: WorkingSetCommit, aliases: Vec<AssetAlias>, manifest: Option<Vec<u8>>, candidate: Candidate }

fn replay_continuation(request: &CopyContinuationRequest, revision: i64, matches: bool) -> CopyContinuationResult {
    CopyContinuationResult::Replayed { committed_revision: revision,
        source_character_id: request.origin.source.character_id.clone(), source_conversation_id: request.origin.source.conversation_id.clone(),
        target_character_id: request.origin.target_character_id.clone(), target_conversation_id: request.origin.target_conversation_id.clone(),
        capture_id: request.capture_id.clone(), requires_current_recovery: true, current_identity_matches: matches }
}

fn scoped_request(request: &CopyContinuationRequest) -> ConflictCopyRequest {
    // Keep the first copy's alias namespace stable. Only submitted occurrences use witnesses;
    // the current lease is used to classify external references, never as their binding proof.
    let mut source = request.origin.source.clone();
    source.lease = request.current_lease.clone();
    ConflictCopyRequest { header: lww::Header { request_id: request.origin.request_id.clone(), binding_authority: request.origin.binding_authority.clone() },
        expected_revision: request.expected_revision, source, target_character_id: request.origin.target_character_id.clone(),
        target_conversation_id: request.origin.target_conversation_id.clone(), character_fields: request.character_fields.clone(),
        conversation_fields: request.conversation_fields.clone(), ranges: request.ranges.clone(),
        introduced_asset_bindings: request.introduced_asset_bindings.clone(), introduced_owner_head: request.introduced_owner_head.clone() }
}

fn target_values(db: &Connection, target: &ReadTarget, origin: &CopyOrigin) -> StoreResult<(Value, Value)> {
    if archive::is_archived(db, &target.generation, &origin.target_character_id)? { return Err(fail(ContinuationConflict::TargetArchived)); }
    let character = query::read_character(db, &origin.target_character_id, target)?.ok_or_else(|| fail(ContinuationConflict::TargetMissing))?.value;
    let conversation = query::read_conversation(db, &origin.target_character_id, &origin.target_conversation_id, target)?
        .ok_or_else(|| fail(ContinuationConflict::TargetMissing))?.value;
    Ok((character, conversation))
}

impl PersistentStore {
    fn continuation_basis(&self, context: &Context) -> StoreResult<Vec<Value>> {
        let history = match &context.head.basis {
            Basis::Original(original) => {
                let prepared = self.prepare_conflict_copy(original, &|| false)?;
                let character = prepared.input.add_character.ok_or_else(|| fail(ContinuationConflict::History))?;
                let conversation = character.get("chats").and_then(Value::as_array).and_then(|chats| chats.first()).ok_or_else(|| fail(ContinuationConflict::History))?;
                messages(conversation)?
            },
            Basis::Patched { lease, ranges } => {
                let (db, target) = self.read_view(Some(lease))?;
                let (_, conversation) = target_values(db, &target, &context.origin)?;
                let mut history = messages(&conversation)?;
                apply_ranges(&mut history, ranges)?;
                history
            },
        };
        if history.len() != context.head.history_count || exact_hash(&json!(history))? != context.head.history_hash { return Err(fail(ContinuationConflict::History)); }
        Ok(history)
    }

    fn prepare_copy_continuation(&self, request: &CopyContinuationRequest, digest: &str, check: &dyn Fn() -> bool) -> StoreResult<PreparedContinuation> {
        let context = self.copy_continuations.contexts.get(&request.context_id).ok_or_else(|| fail(ContinuationConflict::EvidenceExpired))?;
        if context.origin != request.origin || request.header.binding_authority != request.origin.binding_authority { return Err(fail(ContinuationConflict::Identity)); }
        if request.basis_version != context.head.version { return Err(fail(ContinuationConflict::BasisVersion)); }
        if request.header.request_id.is_empty() || request.capture_id.is_empty() { return Err(fail(ContinuationConflict::Identity)); }
        validate_continuation_fields(&request.character_fields, true)?;
        validate_continuation_fields(&request.conversation_fields, false)?;
        let (db, target) = self.read_view(Some(&request.current_lease))?;
        if !same_identity(&context.origin.source.identity, &sync_selection::identity(db)?) { return Err(fail(ContinuationConflict::Identity)); }
        if target.revision != request.expected_revision { return Err(StoreError::RevisionConflict { expected: request.expected_revision, actual: target.revision }); }
        let (character, conversation) = target_values(db, &target, &context.origin)?;
        let mut history = messages(&conversation)?;
        if exact_hash(&json!(history))? != context.head.history_hash || history != self.continuation_basis(context)? { return Err(fail(ContinuationConflict::History)); }
        let mut current = metadata(character, conversation)?;
        compare_fields(&current.character, &context.head.canonical.character, &request.character_fields)?;
        compare_fields(&current.conversation, &context.head.canonical.conversation, &request.conversation_fields)?;
        for alias in &context.head.aliases {
            if query::read_asset_alias(db, &alias.kind, &alias.key, &target)?.map(|row| row.value).as_ref() != Some(alias) { return Err(fail(ContinuationConflict::AssetBinding)); }
        }
        let changes_owner = request.character_fields.iter().any(|field| field_name(field) == "additionalAssets");
        if changes_owner && query::read_asset_owner_head(db, &AssetOwnerLocator::CharacterAdditionalAssets {
            character_id: request.origin.target_character_id.clone() }, &target)?.map(|row| row.value) != context.head.owner_head {
            return Err(fail(ContinuationConflict::AssetBinding));
        }
        // Validate raw ranges before asset preparation, including intermediate local insertions.
        let mut range_check = history.clone();
        apply_ranges(&mut range_check, &request.ranges)?;
        drop(range_check);
        let scoped = scoped_request(request);
        let mut fields = request.character_fields.clone();
        let mut chat_fields = request.conversation_fields.clone();
        let mut ranges = request.ranges.clone();
        let introduced = assets::prepare_scoped(self, &scoped, &mut fields, &mut chat_fields, &mut ranges, check)?;
        apply_ranges(&mut history, &ranges)?;
        let mut raw = context.head.raw.clone();
        let mut canonical = context.head.canonical.clone();
        apply_fields(&mut raw.character, &request.character_fields)?;
        apply_fields(&mut raw.conversation, &request.conversation_fields)?;
        apply_fields(&mut canonical.character, &fields)?;
        apply_fields(&mut canonical.conversation, &chat_fields)?;
        apply_fields(&mut current.character, &fields)?;
        apply_fields(&mut current.conversation, &chat_fields)?;
        let mut known = context.head.aliases.clone();
        known.extend(introduced.iter().cloned());
        let aliases = assets::retain_referenced(&canonical.character, &canonical.conversation, &history, &known)?;
        let introduced = introduced.into_iter().filter(|alias| aliases.contains(alias)).collect::<Vec<_>>();
        let (owner_head, manifest) = if changes_owner {
            let (head, manifest) = assets::manifest(&canonical.character, &request.origin.target_character_id, &aliases)?;
            (Some(head), manifest)
        } else { (context.head.owner_head.clone(), None) };
        let mut mutations = ranges.iter().map(|range| ConversationMutation::ReplaceRange {
            character_id: request.origin.target_character_id.clone(), conversation_id: request.origin.target_conversation_id.clone(),
            start: range.start, delete_count: range.delete_count, messages: range.messages.clone(), conversation: None, configured_index: None,
        }).collect::<Vec<_>>();
        if !chat_fields.is_empty() {
            if mutations.is_empty() { mutations.push(ConversationMutation::ReplaceRange { character_id: request.origin.target_character_id.clone(),
                conversation_id: request.origin.target_conversation_id.clone(), start: 0, delete_count: 0, messages: vec![], conversation: None, configured_index: None }); }
            if let ConversationMutation::ReplaceRange { conversation, .. } = &mut mutations[0] { *conversation = Some(current.conversation); }
        }
        let next = Head { version: context.head.version.checked_add(1).ok_or_else(|| fail(ContinuationConflict::Budget))?,
            covered_request_id: request.header.request_id.clone(), basis: Basis::Patched { lease: request.current_lease.clone(), ranges },
            raw, canonical, history_hash: exact_hash(&json!(history))?, history_count: history.len(), aliases, owner_head: owner_head.clone(),
            // Newly generated aliases do not exist in the precommit reader. Their historical
            // witnesses keep payloads rooted until a later reader captures the canonical aliases.
            leases: proof_leases(&scoped) };
        let candidate_leases = next.leases.clone();
        for lease in &candidate_leases { self.read_view(Some(lease))?; }
        let candidate = Candidate { request: request.clone(), digest: digest.into(), next, leases: candidate_leases };
        self.copy_continuations.admit(None, Some((&request.context_id, &candidate)))?;
        Ok(PreparedContinuation { input: WorkingSetCommit { expected_revision: request.expected_revision,
            character_details: (!fields.is_empty()).then(|| vec![current.character]), conversations: (!mutations.is_empty()).then_some(mutations),
            asset_owner_heads: changes_owner.then(|| owner_head.into_iter().collect()), ..Default::default() }, aliases: introduced, manifest, candidate })
    }

    fn promote_copy_continuation(&mut self, request: &CopyContinuationRequest, digest: &str) -> StoreResult<()> {
        if let Some(context) = self.copy_continuations.contexts.get_mut(&request.context_id) {
            if let Some(candidate) = &context.candidate {
                if candidate.request.header.request_id == request.header.request_id {
                    if candidate.digest != digest { return Err(StoreError::Validation { message: "request-id-integrity".into() }); }
                    context.head = context.candidate.take().unwrap().next;
                }
            }
        }
        Ok(())
    }

    fn unknown_copy_candidate(&self, request: &CopyContinuationRequest) -> Option<String> {
        self.copy_continuations.contexts.get(&request.context_id).and_then(|context| context.candidate.as_ref())
            .map(|candidate| candidate.request.header.request_id.clone())
    }

    pub(crate) fn continue_conflict_copy(&mut self, request: &CopyContinuationRequest, check: &dyn Fn() -> bool) -> StoreResult<CopyContinuationResult> {
        let digest = continuation_digest(request)?;
        let result = self.continue_copy_inner(request, &digest, check);
        if result.is_err() {
            // Only a successful lookup can establish precommit failure after a candidate exists.
            match self.continuation_receipt(&request.origin, &request.header.request_id, &digest, &request.header.binding_authority) {
                Ok(ReceiptState::Found { revision, matches }) => {
                    self.promote_copy_continuation(request, &digest)?;
                    return Ok(replay_continuation(request, revision, matches));
                },
                Ok(ReceiptState::Missing { .. }) => {
                    if let Some(context) = self.copy_continuations.contexts.get_mut(&request.context_id) {
                        if context.candidate.as_ref().is_some_and(|candidate| candidate.digest == digest) { context.candidate = None; }
                    }
                },
                unresolved => {
                    if let Some(candidate) = self.copy_continuations.contexts.get(&request.context_id).and_then(|context| context.candidate.as_ref()) {
                        if candidate.request.header.request_id == request.header.request_id && candidate.digest != digest {
                            return Err(StoreError::Validation { message: "request-id-integrity".into() });
                        }
                    }
                    if let Some(candidate_request_id) = self.unknown_copy_candidate(request) {
                        if matches!(&unresolved, Err(StoreError::Validation { message }) if message == "request-id-integrity") { return unresolved.map(|_| unreachable!()); }
                        return Ok(CopyContinuationResult::OutcomeUnresolved { candidate_request_id });
                    }
                    if matches!(unresolved, Ok(ReceiptState::Unavailable)) { return Ok(CopyContinuationResult::HistoricalStatusUnavailable); }
                },
            }
        }
        match result { Err(error) if classify(&error).is_some() => Ok(CopyContinuationResult::Conflict { kind: classify(&error).unwrap() }), result => result }
    }

    fn continue_copy_inner(&mut self, request: &CopyContinuationRequest, digest: &str, check: &dyn Fn() -> bool) -> StoreResult<CopyContinuationResult> {
        let identity_matches = match self.continuation_receipt(&request.origin, &request.header.request_id, digest, &request.header.binding_authority)? {
            ReceiptState::Unavailable => return Ok(if let Some(candidate_request_id) = self.unknown_copy_candidate(request) {
                CopyContinuationResult::OutcomeUnresolved { candidate_request_id }
            } else { CopyContinuationResult::HistoricalStatusUnavailable }),
            ReceiptState::Found { revision, matches } => {
                self.promote_copy_continuation(request, digest)?;
                return Ok(replay_continuation(request, revision, matches));
            },
            ReceiptState::Missing { matches } => matches,
        };
        if let Some(context) = self.copy_continuations.contexts.get(&request.context_id) {
            if let Some(candidate) = &context.candidate {
                if candidate.request.header.request_id != request.header.request_id { return Ok(CopyContinuationResult::OutcomeUnresolved { candidate_request_id: candidate.request.header.request_id.clone() }); }
                if candidate.digest != digest { return Err(StoreError::Validation { message: "request-id-integrity".into() }); }
                // The exact candidate's receipt is absent in its captured library. Its transaction
                // did not commit, so this same request may prepare again without changing its basis.
            }
        }
        if !identity_matches { return Err(fail(ContinuationConflict::Identity)); }
        if encoded_len(request)? > self.copy_continuations.limits.request_bytes { return Err(fail(ContinuationConflict::Budget)); }
        check_cancelled(check)?;
        self.lww_recover_intents()?;
        let prepared = self.prepare_copy_continuation(request, digest, check)?;
        let PreparedContinuation { input, aliases, manifest, candidate } = prepared;
        self.copy_continuations.contexts.get_mut(&request.context_id).unwrap().candidate = Some(candidate);
        let cas = PayloadCas::new(&self.repository_root)?;
        let staged: Option<StagedPayload> = manifest.as_ref().map(|bytes| cas.stage_reader_expected(
            &mut std::io::Cursor::new(bytes), &risunest_sync_wire::hash(bytes), bytes.len() as u64)).transpose()?;
        let job_id = uuid::Uuid::new_v4().to_string();
        let mut publication = DurableCasJob::begin(&self.repository_root, &job_id, CasJobKind::DirectAssetOrInlayWrite,
            CasJobOwner::page_write(&job_id), super::super::device_store::now_ms()?)?;
        let result = (|| {
            let stamp = self.reserve_commit_only_stamp(&request.header)?;
            let repository = crate::asset_repository::coordinator::lock_repository_mutation()?;
            let device = Transaction::new_unchecked(self.device_store()?.connection(), TransactionBehavior::Immediate)?;
            let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
            let current = sync_selection::identity(&tx)?;
            if !same_library(&current, &request.origin.source.identity) { return Err(fail(ContinuationConflict::Identity)); }
            let matches = same_identity(&request.origin.source.identity, &current) && lww::authority(&device)? == request.header.binding_authority;
            if let Some(revision) = receipt_by_id(&tx, &request.header.request_id, digest)? { return Ok(replay_continuation(request, revision, matches)); }
            if !matches { return Err(fail(ContinuationConflict::Identity)); }
            let target = ReadTarget { generation: current.generation.clone(), revision: current.revision };
            target_values(&tx, &target, &request.origin)?;
            for alias in &aliases {
                if let Some(previous) = query::read_asset_alias(&tx, &alias.kind, &alias.key, &target)? {
                    if previous.value != *alias { return Err(fail(ContinuationConflict::AssetBinding)); }
                }
            }
            check_cancelled(check)?;
            let committed = commit::apply_incremental(&tx, request.expected_revision,
                |tx, generation| commit::prepare_working_set(tx, generation, &input, &input, &aliases),
                |tx, generation, capture| {
                    if let Some(staged) = staged {
                        publication.record_page_publication(std::slice::from_ref(&staged), &repository)?;
                        check_cancelled(check)?;
                        let object = cas.publish_staged(staged)?;
                        check_cancelled(check)?;
                        asset_object_catalog::register_in_transaction(tx, &[asset_object_catalog::AssetObjectRegistration {
                            object_hash: object.content_hash, byte_size: object.byte_size }], super::super::device_store::now_ms()?)?;
                    }
                    commit::apply_working_set(tx, generation, &input, &aliases, capture, Some((&request.header, &stamp)))?;
                    tx.execute("INSERT INTO lww_requests(request_id,digest,revision) VALUES(?1,?2,?3)",
                        params![request.header.request_id, digest, current_revision(tx)? + 1])?;
                    check_cancelled(check)
                })?;
            tx.commit()?;
            drop(device);
            check_cancelled(check)?;
            publication.finish_page_catalog_registration(&repository)?;
            Ok(CopyContinuationResult::Committed { revision: committed.revision,
                source_character_id: request.origin.source.character_id.clone(), source_conversation_id: request.origin.source.conversation_id.clone(),
                target_character_id: request.origin.target_character_id.clone(), target_conversation_id: request.origin.target_conversation_id.clone(),
                capture_id: request.capture_id.clone(), next_basis_version: request.basis_version + 1 })
        })();
        let released = if publication.is_released() { Ok(()) } else { publication.release(CasReleaseOutcome::Aborted).map_err(StoreError::from) };
        let result = match result { Ok(result) => { released?; result }, Err(error) => { let _ = released; return Err(error); } };
        self.promote_copy_continuation(request, digest)?;
        Ok(result)
    }

    pub(crate) fn hydrate_copy_continuation(&self, request: &CopyContinuationRequest, check: &dyn Fn() -> bool,
        cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) -> StoreResult<()> {
        let context = self.copy_continuations.contexts.get(&request.context_id).ok_or_else(|| fail(ContinuationConflict::EvidenceExpired))?;
        if context.origin != request.origin || context.head.version != request.basis_version { return Err(fail(ContinuationConflict::Identity)); }
        if context.candidate.is_some() { return Err(StoreError::Validation { message: "copy continuation outcome must be resolved before hydration".into() }); }
        if encoded_len(request)? > self.copy_continuations.limits.request_bytes { return Err(fail(ContinuationConflict::Budget)); }
        let (db, target) = self.read_view(Some(&request.current_lease))?;
        if !same_identity(&request.origin.source.identity, &sync_selection::identity(db)?) || target.revision != request.expected_revision {
            return Err(fail(ContinuationConflict::Identity));
        }
        validate_continuation_fields(&request.character_fields, true)?;
        validate_continuation_fields(&request.conversation_fields, false)?;
        assets::hydrate_scoped(self, &scoped_request(request), check, cancellation)
    }
}
