//! Save a witnessed owner edit while retaining its archived residency.
use super::{active_generation, archive, asset_object_catalog, commit, current_revision, lww,
    AssetOwnerHead, AssetOwnerLocator, ConversationMutation, PersistentStore, StoreError,
    StoreResult, WorkingSetCommit};
use crate::asset_repository::PayloadCas;
use crate::asset_repository::job_pins::{CasJobKind, CasJobOwner, CasReleaseOutcome, DurableCasJob};
use risunest_sync_wire::payload_value;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HistoryWitness {
    pub character_id: String,
    pub conversation_id: String,
    pub message_count: Option<i64>,
    pub history_hash: Option<String>,
    pub detail_hash: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArchiveReconcileRequest {
    pub header: lww::Header,
    pub expected_revision: i64,
    pub selection_epoch: String,
    pub character_id: String,
    pub histories: Vec<HistoryWitness>,
    pub unit_mutations: Vec<lww::UnitMutation>,
    pub conversations: Vec<ConversationMutation>,
    pub asset_owner_heads: Vec<AssetOwnerHead>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub(crate) enum ArchiveReconcileResult {
    Committed { revision: i64, archive: archive::ArchivePreview, histories: Vec<HistoryWitness> },
    // A receipt acknowledges this request only. The owner may since have been restored or deleted.
    Replayed { committed_revision: i64 },
    HistoryConflict { character_id: String, conversation_id: String },
}

fn invalid(message: &str) -> StoreError { StoreError::Validation { message: message.into() } }
fn cancelled(check: &dyn Fn() -> bool) -> StoreResult<()> {
    if check() { Err(invalid(archive::ARCHIVE_CANCELLED_MESSAGE)) } else { Ok(()) }
}
fn frame(hash: &mut Sha256, value: &Value) -> StoreResult<()> {
    let bytes = payload_value::encode(value).map_err(lww::error)?;
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
    Ok(())
}

fn history_witness(db: &Connection, generation: &str, owner: &str, conversation: &str,
    check: &dyn Fn() -> bool) -> StoreResult<HistoryWitness> {
    cancelled(check)?;
    let stored: Option<(i64, String)> = db.query_row(
        "SELECT message_count,detail FROM conversations WHERE generation=?1 AND character_id=?2 AND conversation_id=?3",
        params![generation, owner, conversation], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
    let mut witness = HistoryWitness { character_id: owner.into(), conversation_id: conversation.into(),
        message_count: None, history_hash: None, detail_hash: None };
    let Some((expected_count, detail)) = stored else { return Ok(witness) };
    let mut hash = Sha256::new();
    hash.update(b"risunest-archive-history-v1\0");
    frame(&mut hash, &serde_json::json!([owner, conversation]))?;
    let mut statement = db.prepare("SELECT message_index,message_id,value FROM messages WHERE generation=?1 AND character_id=?2 AND conversation_id=?3 ORDER BY message_index")?;
    let mut rows = statement.query(params![generation, owner, conversation])?;
    let mut count = 0i64;
    while let Some(row) = rows.next()? {
        cancelled(check)?;
        let index: i64 = row.get(0)?;
        if index != count { return Err(invalid("archive history indexes are not contiguous")); }
        let id: Option<String> = row.get(1)?;
        let value: Value = serde_json::from_str(&row.get::<_, String>(2)?)?;
        frame(&mut hash, &serde_json::json!([index, id, value]))?;
        count = count.checked_add(1).ok_or_else(|| invalid("archive history count overflow"))?;
    }
    if count != expected_count { return Err(invalid("archive history count differs")); }
    frame(&mut hash, &serde_json::json!(count))?;
    let detail: Value = serde_json::from_str(&detail)?;
    witness.message_count = Some(count);
    witness.history_hash = Some(hex::encode(hash.finalize()));
    witness.detail_hash = Some(risunest_sync_wire::hash(&payload_value::encode(&detail).map_err(lww::error)?));
    Ok(witness)
}

fn validate_request(request: &ArchiveReconcileRequest) -> StoreResult<WorkingSetCommit> {
    if request.header.request_id.is_empty() || request.character_id.is_empty() || request.selection_epoch.is_empty() {
        return Err(invalid("archive reconciliation identity is missing"));
    }
    let owner = &request.character_id;
    let mut witnesses = BTreeMap::new();
    for witness in &request.histories {
        if witness.character_id != *owner || witness.conversation_id.is_empty()
            || witnesses.insert(witness.conversation_id.as_str(), witness).is_some() {
            return Err(invalid("invalid archive history witness target"));
        }
        match (witness.message_count, &witness.history_hash, &witness.detail_hash) {
            (None, None, None) => (),
            (Some(count), Some(history), Some(detail)) if count >= 0 && super::is_lowercase_sha256_hex(history)
                && super::is_lowercase_sha256_hex(detail) => (),
            _ => return Err(invalid("invalid archive history witness")),
        }
    }
    let mut touched = BTreeSet::new();
    let mut counts = witnesses.iter().map(|(id, witness)| (*id, witness.message_count)).collect::<BTreeMap<_, _>>();
    for mutation in &request.unit_mutations {
        let p = mutation.key().components();
        let allowed = match p.as_slice() {
            [kind, id, field] if kind == "character" && id == owner => !matches!(field.as_str(), "chaId" | "type" | "chats"),
            [kind, id, _, field] if kind == "conversation" && id == owner => !matches!(field.as_str(), "id" | "message"),
            [kind, scope, id] => kind == "order" && scope == "conversations" && id == owner,
            _ => false,
        };
        if !allowed { return Err(invalid("archive reconciliation mutation is outside its owner")); }
    }
    for mutation in &request.conversations {
        let (character, conversation) = match mutation {
            ConversationMutation::ReplaceRange { character_id, conversation_id, .. }
            | ConversationMutation::Delete { character_id, conversation_id } => (character_id, Some(conversation_id)),
            ConversationMutation::Reorder { character_id, .. } => (character_id, None),
        };
        if character != owner { return Err(invalid("archive reconciliation mutation is outside its owner")); }
        let Some(id) = conversation else { continue };
        touched.insert(id.as_str());
        let count = counts.get_mut(id.as_str()).ok_or_else(|| invalid("archive history witness is missing"))?;
        match mutation {
            ConversationMutation::ReplaceRange { start, delete_count, messages, conversation, configured_index, .. } => {
                match *count {
                    Some(old) => {
                        if conversation.is_some() || configured_index.is_some() || *start < 0 || *delete_count < 0
                            || *start > old || *delete_count > old - *start {
                            return Err(invalid("archive range does not match its witnessed history"));
                        }
                        *count = Some((old - *delete_count).checked_add(i64::try_from(messages.len()).map_err(|_| invalid("archive range size overflow"))?)
                            .ok_or_else(|| invalid("archive range size overflow"))?);
                    },
                    None => {
                        if *start != 0 || *delete_count != 0 || conversation.is_none() {
                            return Err(invalid("new archive conversation requires its complete initial detail"));
                        }
                        *count = Some(i64::try_from(messages.len()).map_err(|_| invalid("archive range size overflow"))?);
                    },
                }
            },
            ConversationMutation::Delete { .. } => { *count = None; },
            ConversationMutation::Reorder { .. } => unreachable!(),
        }
    }
    if touched.len() != witnesses.len() { return Err(invalid("archive history witness has no mutation")); }
    for head in &request.asset_owner_heads {
        if !matches!(&head.owner, AssetOwnerLocator::CharacterAdditionalAssets { character_id } if character_id == owner) {
            return Err(invalid("archive asset owner is outside its owner"));
        }
    }
    Ok(WorkingSetCommit { expected_revision: request.expected_revision,
        unit_mutations: Some(request.unit_mutations.clone()), conversations: Some(request.conversations.clone()),
        asset_owner_heads: Some(request.asset_owner_heads.clone()), ..Default::default() })
}

fn receipt(db: &Connection, request: &ArchiveReconcileRequest, digest: &str) -> StoreResult<Option<i64>> {
    let prior: Option<(String, i64)> = db.query_row("SELECT digest,revision FROM lww_requests WHERE request_id=?1",
        [&request.header.request_id], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
    prior.map(|(prior, revision)| if prior == digest { Ok(revision) } else { Err(invalid("request-id-integrity")) }).transpose()
}

impl PersistentStore {
    // The caller keeps the original lease until these witnesses and the submitted journal agree.
    // A later hash never authorizes an old positional range without a rebase.
    pub(crate) fn archive_reconcile_histories(&self, lease: &str, owner: &str, conversations: &[String]) -> StoreResult<Vec<HistoryWitness>> {
        let (db, target) = self.read_view(Some(lease))?;
        if archive::is_archived(db, &target.generation, owner)? { return Err(archive::archived_error(owner)); }
        let mut unique = BTreeSet::new();
        conversations.iter().map(|id| {
            if id.is_empty() || !unique.insert(id) { return Err(invalid("duplicate archive history target")); }
            history_witness(db, &target.generation, owner, id, &|| false)
        }).collect()
    }

    // Run before pausing frontend persistence. The commit path below performs only local I/O.
    pub(crate) fn hydrate_archive_reconcile(&self, owner: &str, check: &dyn Fn() -> bool,
        cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) -> StoreResult<()> {
        archive::fetch_operation_body(&self.connection, &self.repository_root, owner, true, check, cancellation.clone())?;
        archive::fetch_operation_body(&self.connection, &self.repository_root, owner, false, check, cancellation)
    }

    pub(crate) fn reconcile_archived_character(&mut self, request: &ArchiveReconcileRequest,
        check: &dyn Fn() -> bool) -> StoreResult<ArchiveReconcileResult> {
        let input = validate_request(request)?;
        let digest = risunest_sync_wire::hash(&serde_json::to_vec(&("archive-reconcile-v1", request))?);
        if let Some(committed_revision) = receipt(&self.connection, request, &digest)? {
            return Ok(ArchiveReconcileResult::Replayed { committed_revision });
        }
        self.lww_recover_intents()?;
        cancelled(check)?;
        let generation = active_generation(&self.connection)?;
        let actual = current_revision(&self.connection)?;
        if actual != request.expected_revision { return Err(StoreError::RevisionConflict { expected: request.expected_revision, actual }); }
        let archived = archive::read_archived_object(&self.connection, &generation, &request.character_id)?
            .ok_or_else(|| invalid("archive reconciliation owner is not archived"))?;
        let cas = PayloadCas::new(&self.repository_root)?;
        let file = cas.open_object(&archived.object_hash)?.ok_or_else(|| invalid(archive::ARCHIVE_DATA_MISSING_MESSAGE))?;
        let _scratch = archive::create_restore_staging(&self.connection, cas.repository_root())?;
        let mut publication = None;
        let result = (|| {
            let payload = archive::stage_archive_payload(&self.connection, file, &request.character_id, &archived, check)?;
            let job_id = uuid::Uuid::new_v4().to_string();
            publication = Some(DurableCasJob::begin(&self.repository_root, &job_id, CasJobKind::DirectAssetOrInlayWrite,
                CasJobOwner::page_write(&job_id), super::device_store::now_ms()?)?);
            // No new replayable intent is persisted. Only the device clock advances before the library commit.
            let stamp = match self.reserve_commit_only_stamp(&request.header) {
                Ok(stamp) => stamp,
                Err(error) => {
                    if let Some(committed_revision) = receipt(&self.connection, request, &digest)? {
                        return Ok(ArchiveReconcileResult::Replayed { committed_revision });
                    }
                    return Err(error);
                }
            };
            let repository = crate::asset_repository::coordinator::lock_repository_mutation()?;
            let device = Transaction::new_unchecked(self.device_store()?.connection(), TransactionBehavior::Immediate)?;
            let transaction = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
            if let Some(committed_revision) = receipt(&transaction, request, &digest)? {
                return Ok(ArchiveReconcileResult::Replayed { committed_revision });
            }
            if lww::authority(&device)? != request.header.binding_authority {
                return Err(invalid("binding-authority-changed"));
            }
            if super::sync_selection::read(&transaction)?.epoch != request.selection_epoch {
                return Err(invalid("Sync selection changed"));
            }
            let context = lww::ArchiveContext { header: &request.header, stamp: &stamp, digest: &digest, incoming: None };
            let mut histories = Vec::new();
            let mut conflict = None;
            let mut preview = None;
            let committed = commit::apply_incremental(&transaction, request.expected_revision,
                |tx, active| {
                    let current = archive::read_archived_object(tx, active, &request.character_id)?;
                    if current.as_ref() != Some(&archived) { return Err(invalid("archive reconciliation source changed")); }
                    Ok(())
                },
                |tx, active, ()| {
                    cancelled(check)?;
                    archive::restore_staged_rows(tx, active, &request.character_id, &payload, check)?;
                    // The existing overlay may temporarily retain this owner's history in native memory.
                    lww::reproject_archived_children(tx, active, &request.character_id)?;
                    for expected in &request.histories {
                        let actual = history_witness(tx, active, &request.character_id, &expected.conversation_id, check)?;
                        let deletes = request.conversations.iter().any(|mutation| matches!(mutation,
                            ConversationMutation::Delete { conversation_id, .. } if conversation_id == &expected.conversation_id));
                        if actual.message_count != expected.message_count || actual.history_hash != expected.history_hash
                            || (deletes && actual.detail_hash != expected.detail_hash) {
                            conflict = Some(expected.conversation_id.clone());
                            return Err(invalid("archive history changed"));
                        }
                    }
                    let prepared = commit::prepare_working_set(tx, active, &input, &input, &[])?;
                    commit::apply_working_set(tx, active, &input, &[], prepared, Some((&request.header, &stamp)))?;
                    for expected in &request.histories {
                        histories.push(history_witness(tx, active, &request.character_id, &expected.conversation_id, check)?);
                    }
                    let asset_hashes = super::snapshot::collect_character_asset_hashes(tx, &cas, active, &request.character_id)?;
                    let shared_asset_hashes = super::snapshot::collect_shared_character_asset_hashes(tx, &cas, active, &request.character_id)?;
                    let (local, metadata) = archive::stage_archive_object(tx, &cas, active, &request.character_id, check, false)?;
                    let (shared, _) = archive::stage_archive_object(tx, &cas, active, &request.character_id, check, true)?;
                    let staged = [local, shared];
                    publication.as_mut().unwrap().record_page_publication(&staged, &repository)?;
                    let [local, shared] = staged;
                    let local = cas.publish_staged(local)?;
                    cancelled(check)?;
                    let shared = cas.publish_staged(shared)?;
                    asset_object_catalog::register_in_transaction(tx, &[
                        asset_object_catalog::AssetObjectRegistration { object_hash: local.content_hash.clone(), byte_size: local.byte_size },
                        asset_object_catalog::AssetObjectRegistration { object_hash: shared.content_hash.clone(), byte_size: shared.byte_size },
                    ], super::device_store::now_ms()?)?;
                    let updated = archive::ArchivedObject { object_hash: local.content_hash, shared_object_hash: shared.content_hash,
                        archived_at: archived.archived_at, conversation_count: metadata.conversation_count,
                        message_count: metadata.message_count, asset_hashes, shared_asset_hashes, identity_remap: Vec::new() };
                    let prepared = archive::prepared_archive_character(&request.character_id, updated, &metadata)?;
                    archive::install_archived_character(tx, active, &request.character_id, &prepared, check)?;
                    lww::record_archive_state(tx, active, &request.character_id, Some(&context), false)?;
                    preview = Some(archive::preview(tx, active, &request.character_id)?);
                    cancelled(check)
                });
            if let Some(conversation_id) = conflict {
                return Ok(ArchiveReconcileResult::HistoryConflict { character_id: request.character_id.clone(), conversation_id });
            }
            let committed = committed?;
            transaction.commit()?;
            drop(device);
            publication.as_mut().unwrap().finish_page_catalog_registration(&repository)?;
            Ok(ArchiveReconcileResult::Committed { revision: committed.revision,
                archive: preview.ok_or_else(|| invalid("archive reconciliation result is missing"))?, histories })
        })();
        let cleanup = archive::clear_restore_staging(&self.connection);
        // All repository and database guards have ended before abort catalogs any published output.
        let released = if let Some(publication) = publication.as_mut() {
            if publication.is_released() { Ok(()) } else { publication.release(CasReleaseOutcome::Aborted).map_err(StoreError::from) }
        } else { Ok(()) };
        match result {
            Ok(result) => { cleanup?; released?; Ok(result) },
            Err(error) => { let _ = cleanup; let _ = released; Err(error) },
        }
    }
}
