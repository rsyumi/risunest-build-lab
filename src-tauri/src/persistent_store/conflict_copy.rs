//! Copy a captured local conversation without changing its received owner.
mod assets;
pub(crate) mod continuation;
#[cfg(test)]
mod tests;

use super::{asset_object_catalog, commit, current_revision, lww, query, sync_selection,
    AssetAlias, AssetOwnerHead, PersistentStore, StoreError, StoreResult, WorkingSetCommit};
use crate::asset_repository::{PayloadCas, StagedPayload};
use crate::asset_repository::job_pins::{CasJobKind, CasJobOwner, CasReleaseOutcome, DurableCasJob};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CopySource {
    pub lease: String,
    pub identity: sync_selection::CaptureIdentity,
    pub character_id: String,
    pub conversation_id: String,
    pub message_count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub(crate) enum CopyField {
    Set { field: String, value: Value },
    Delete { field: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CopyRange {
    pub start: i64,
    pub delete_count: i64,
    pub messages: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "type", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub(crate) enum AssetScope {
    CharacterField { mutation: usize },
    ConversationField { mutation: usize },
    Message { range: usize, message: usize },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AssetLocation {
    // RFC 6901 pointer relative to the submitted field value or message.
    pub pointer: String,
    pub token_index: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IntroducedAssetBinding {
    pub scope: AssetScope,
    pub location: AssetLocation,
    pub alias: AssetAlias,
    pub witness_lease: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IntroducedOwnerHead {
    pub mutation: usize,
    pub head: AssetOwnerHead,
    pub witness_lease: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConflictCopyRequest {
    pub header: lww::Header,
    pub expected_revision: i64,
    pub source: CopySource,
    pub target_character_id: String,
    pub target_conversation_id: String,
    pub character_fields: Vec<CopyField>,
    pub conversation_fields: Vec<CopyField>,
    pub ranges: Vec<CopyRange>,
    pub introduced_asset_bindings: Vec<IntroducedAssetBinding>,
    pub introduced_owner_head: Option<IntroducedOwnerHead>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CopyConflict { Identity, SourceHistory, Range, AssetBinding, TargetCollision, UnsupportedField, Cancelled }

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub(crate) enum ConflictCopyResult {
    Committed { revision: i64, source_character_id: String, source_conversation_id: String,
        target_character_id: String, target_conversation_id: String },
    Replayed { committed_revision: i64, target_character_id: String, target_conversation_id: String,
        source_character_id: String, source_conversation_id: String,
        requires_current_recovery: bool, current_identity_matches: bool },
    HistoricalStatusUnavailable,
    Conflict { kind: CopyConflict },
}

fn conflict(kind: CopyConflict) -> StoreError {
    StoreError::Validation { message: format!("conflict-copy:{}", serde_json::to_string(&kind).expect("enum serializes")) }
}

fn cancelled(check: &dyn Fn() -> bool) -> StoreResult<()> {
    if check() { Err(conflict(CopyConflict::Cancelled)) } else { Ok(()) }
}

fn same_library(a: &sync_selection::CaptureIdentity, b: &sync_selection::CaptureIdentity) -> bool {
    a.store_id == b.store_id && a.library_epoch == b.library_epoch
}

fn same_identity(a: &sync_selection::CaptureIdentity, b: &sync_selection::CaptureIdentity) -> bool {
    same_library(a, b) && a.generation == b.generation && a.selection_epoch == b.selection_epoch && a.revision <= b.revision
}

fn digest(request: &ConflictCopyRequest) -> StoreResult<String> {
    let value = serde_json::to_value(("conflict-copy-v1", request))?;
    exact_hash(&value)
}

fn exact_hash(value: &Value) -> StoreResult<String> {
    fn ordered(value: &Value, depth: usize) -> StoreResult<Value> {
        if depth >= 127 { return Err(StoreError::Validation { message: "copy input nesting limit".into() }); }
        Ok(match value {
            Value::Object(values) => {
                let sorted = values.iter().collect::<std::collections::BTreeMap<_, _>>();
                Value::Object(sorted.into_iter().map(|(key, value)| Ok((key.clone(), ordered(value, depth + 1)?)))
                    .collect::<StoreResult<serde_json::Map<_, _>>>()?)
            },
            Value::Array(values) => Value::Array(values.iter().map(|value| ordered(value, depth + 1)).collect::<StoreResult<_>>()?),
            value => value.clone(),
        })
    }
    // Receipt inputs include native i64 identities and sizes. JavaScript numeric comparison
    // encoding would merge distinct integers above its exact range.
    Ok(risunest_sync_wire::hash(&serde_json::to_vec(&ordered(value, 0)?)?))
}

fn receipt(db: &Connection, request: &ConflictCopyRequest, digest: &str) -> StoreResult<Option<i64>> {
    receipt_by_id(db, &request.header.request_id, digest)
}

fn receipt_by_id(db: &Connection, request_id: &str, digest: &str) -> StoreResult<Option<i64>> {
    let prior: Option<(String, i64)> = db.query_row("SELECT digest,revision FROM lww_requests WHERE request_id=?1",
        [request_id], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
    prior.map(|(prior, revision)| if prior == digest { Ok(revision) } else {
        Err(StoreError::Validation { message: "request-id-integrity".into() })
    }).transpose()
}

fn replay(request: &ConflictCopyRequest, revision: i64, matches: bool) -> ConflictCopyResult {
    ConflictCopyResult::Replayed { committed_revision: revision,
        source_character_id: request.source.character_id.clone(), source_conversation_id: request.source.conversation_id.clone(),
        target_character_id: request.target_character_id.clone(), target_conversation_id: request.target_conversation_id.clone(),
        requires_current_recovery: true, current_identity_matches: matches }
}

fn validate_fields(fields: &[CopyField], character: bool) -> StoreResult<()> {
    let mut seen = BTreeSet::new();
    for field in fields {
        let name = match field { CopyField::Set { field, .. } | CopyField::Delete { field } => field };
        let reserved = if character { matches!(name.as_str(), "chaId" | "chats" | "chatPage" | "coldstorage" | "coldStoragedChats" | "archived") }
            else { matches!(name.as_str(), "id" | "message") };
        if name.is_empty() || reserved || !seen.insert(name) { return Err(conflict(CopyConflict::UnsupportedField)); }
    }
    Ok(())
}

fn apply_fields(value: &mut Value, fields: &[CopyField]) -> StoreResult<()> {
    let object = value.as_object_mut().ok_or_else(|| conflict(CopyConflict::SourceHistory))?;
    for field in fields {
        match field {
            CopyField::Set { field, value } => { object.insert(field.clone(), value.clone()); },
            CopyField::Delete { field } => { object.remove(field); },
        }
    }
    Ok(())
}

fn normalize(character: &mut Value, conversation: &mut Value, request: &ConflictCopyRequest) -> StoreResult<()> {
    let chat = conversation.as_object_mut().ok_or_else(|| conflict(CopyConflict::SourceHistory))?;
    let owner = character.as_object_mut().ok_or_else(|| conflict(CopyConflict::SourceHistory))?;
    chat.insert("id".into(), json!(request.target_conversation_id));
    let folder = chat.get("folderId").and_then(Value::as_str).and_then(|id| owner.get("chatFolders")
        .and_then(Value::as_array).and_then(|folders| folders.iter().find(|folder| folder.get("id").and_then(Value::as_str) == Some(id)))).cloned();
    if folder.is_none() && chat.contains_key("folderId") { chat.insert("folderId".into(), Value::Null); }
    owner.insert("chatFolders".into(), Value::Array(folder.into_iter().collect()));
    owner.insert("chaId".into(), json!(request.target_character_id));
    owner.insert("chatPage".into(), json!(0));
    owner.remove("coldstorage");
    owner.remove("coldStoragedChats");
    owner.insert("chats".into(), Value::Array(vec![conversation.clone()]));
    Ok(())
}

struct PreparedCopy { input: WorkingSetCommit, aliases: Vec<AssetAlias>, manifest: Option<Vec<u8>> }

impl PersistentStore {
    pub(crate) fn read_conflict_copy_source(&self, lease: &str, character_id: &str, conversation_id: &str) -> StoreResult<CopySource> {
        let (db, target) = self.read_view(Some(lease))?;
        if super::archive::is_archived(db, &target.generation, character_id)? { return Err(conflict(CopyConflict::SourceHistory)); }
        let message_count = db.query_row("SELECT message_count FROM conversations WHERE generation=?1 AND character_id=?2 AND conversation_id=?3",
            params![target.generation, character_id, conversation_id], |row| row.get(0)).optional()?
            .ok_or_else(|| conflict(CopyConflict::SourceHistory))?;
        Ok(CopySource { lease: lease.into(), identity: sync_selection::identity(db)?,
            character_id: character_id.into(), conversation_id: conversation_id.into(), message_count })
    }

    fn prepare_conflict_copy(&self, request: &ConflictCopyRequest, check: &dyn Fn() -> bool) -> StoreResult<PreparedCopy> {
        validate_fields(&request.character_fields, true)?;
        validate_fields(&request.conversation_fields, false)?;
        if request.header.request_id.is_empty() || request.target_character_id.is_empty() || request.target_conversation_id.is_empty()
            || request.target_character_id == request.source.character_id {
            return Err(conflict(CopyConflict::TargetCollision));
        }
        let source = self.read_conflict_copy_source(&request.source.lease, &request.source.character_id, &request.source.conversation_id)
            .map_err(|error| if error == StoreError::SnapshotReleased { conflict(CopyConflict::SourceHistory) } else { error })?;
        if source != request.source { return Err(conflict(CopyConflict::SourceHistory)); }
        let (db, target) = self.read_view(Some(&source.lease))?;
        let mut character = query::read_character(db, &source.character_id, &target)?
            .ok_or_else(|| conflict(CopyConflict::SourceHistory))?.value;
        let mut conversation = query::read_conversation(db, &source.character_id, &source.conversation_id, &target)?
            .ok_or_else(|| conflict(CopyConflict::SourceHistory))?.value;
        character.as_object_mut().ok_or_else(|| conflict(CopyConflict::SourceHistory))?.remove("coldstorage");
        character.as_object_mut().unwrap().remove("coldStoragedChats");
        let mut fields = request.character_fields.clone();
        let mut chat_fields = request.conversation_fields.clone();
        let mut ranges = request.ranges.clone();
        let prepared_assets = assets::prepare(self, request, &mut character, &mut conversation, &mut fields, &mut chat_fields, &mut ranges, check)?;
        apply_fields(&mut character, &fields)?;
        apply_fields(&mut conversation, &chat_fields)?;
        normalize(&mut character, &mut conversation, request)?;
        let (head, manifest) = assets::manifest(&character, &request.target_character_id, &prepared_assets)?;
        Ok(PreparedCopy { input: WorkingSetCommit { expected_revision: request.expected_revision, add_character: Some(character),
            asset_owner_heads: Some(vec![head]), ..Default::default() }, aliases: prepared_assets, manifest })
    }

    pub(crate) fn copy_conflicted_conversation(&mut self, request: &ConflictCopyRequest, check: &dyn Fn() -> bool) -> StoreResult<ConflictCopyResult> {
        let result = self.copy_conflicted_conversation_inner(request, check);
        if result.is_err() {
            // A competing invocation or a postcommit cleanup error may have produced a receipt
            // after the initial lookup. Never report that historical commit as a fresh conflict.
            if !same_library(&sync_selection::identity(&self.connection)?, &request.source.identity) {
                return Ok(ConflictCopyResult::HistoricalStatusUnavailable);
            }
            let device = Transaction::new_unchecked(self.device_store()?.connection(), TransactionBehavior::Immediate)?;
            let library = Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)?;
            let current = sync_selection::identity(&library)?;
            if !same_library(&current, &request.source.identity) { return Ok(ConflictCopyResult::HistoricalStatusUnavailable); }
            if let Some(revision) = receipt(&library, request, &digest(request)?)? {
                return Ok(replay(request, revision, same_identity(&request.source.identity, &current)
                    && lww::authority(&device)? == request.header.binding_authority));
            }
        }
        match result {
            Err(StoreError::Validation { message }) if message.starts_with("conflict-copy:") => {
                Ok(ConflictCopyResult::Conflict { kind: serde_json::from_str(&message[14..])? })
            },
            result => result,
        }
    }

    fn copy_conflicted_conversation_inner(&mut self, request: &ConflictCopyRequest, check: &dyn Fn() -> bool) -> StoreResult<ConflictCopyResult> {
        let digest = digest(request)?;
        // Only this captured library can establish whether the operation committed. A different
        // active library cannot turn an uncertain historical result into a rejected request.
        if !same_library(&sync_selection::identity(&self.connection)?, &request.source.identity) {
            return Ok(ConflictCopyResult::HistoricalStatusUnavailable);
        }
        {
            let device = Transaction::new_unchecked(self.device_store()?.connection(), TransactionBehavior::Immediate)?;
            let library = Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)?;
            let current = sync_selection::identity(&library)?;
            if !same_library(&current, &request.source.identity) { return Ok(ConflictCopyResult::HistoricalStatusUnavailable); }
            if let Some(revision) = receipt(&library, request, &digest)? {
                return Ok(replay(request, revision, same_identity(&request.source.identity, &current)
                    && lww::authority(&device)? == request.header.binding_authority));
            }
            if !same_identity(&request.source.identity, &current) || lww::authority(&device)? != request.header.binding_authority {
                return Err(conflict(CopyConflict::Identity));
            }
        }
        cancelled(check)?;
        self.lww_recover_intents()?;
        let prepared = self.prepare_conflict_copy(request, check)?;
        let cas = PayloadCas::new(&self.repository_root)?;
        let staged: Option<StagedPayload> = prepared.manifest.as_ref().map(|bytes| cas.stage_reader_expected(
            &mut std::io::Cursor::new(bytes), &risunest_sync_wire::hash(bytes), bytes.len() as u64)).transpose()?;
        let job_id = uuid::Uuid::new_v4().to_string();
        let mut publication = DurableCasJob::begin(&self.repository_root, &job_id, CasJobKind::DirectAssetOrInlayWrite,
            CasJobOwner::page_write(&job_id), super::device_store::now_ms()?)?;
        let result = (|| {
            let stamp = self.reserve_commit_only_stamp(&request.header)?;
            let repository = crate::asset_repository::coordinator::lock_repository_mutation()?;
            let device = Transaction::new_unchecked(self.device_store()?.connection(), TransactionBehavior::Immediate)?;
            let transaction = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
            let current = sync_selection::identity(&transaction)?;
            if !same_library(&current, &request.source.identity) { return Ok(ConflictCopyResult::HistoricalStatusUnavailable); }
            let matches = same_identity(&request.source.identity, &current) && lww::authority(&device)? == request.header.binding_authority;
            if let Some(revision) = receipt(&transaction, request, &digest)? { return Ok(replay(request, revision, matches)); }
            if !matches { return Err(conflict(CopyConflict::Identity)); }
            let collision: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM characters WHERE generation=?1 AND character_id=?2)",
                params![current.generation, request.target_character_id], |row| row.get(0))?;
            if collision { return Err(conflict(CopyConflict::TargetCollision)); }
            for alias in &prepared.aliases {
                if let Some(previous) = query::read_asset_alias(&transaction, &alias.kind, &alias.key,
                    &super::ReadTarget { generation: current.generation.clone(), revision: current.revision })? {
                    if previous.value != *alias { return Err(conflict(CopyConflict::AssetBinding)); }
                }
            }
            cancelled(check)?;
            let committed = commit::apply_incremental(&transaction, request.expected_revision,
                |tx, generation| commit::prepare_working_set(tx, generation, &prepared.input, &prepared.input, &prepared.aliases),
                |tx, generation, capture| {
                    if let Some(staged) = staged {
                        publication.record_page_publication(std::slice::from_ref(&staged), &repository)?;
                        cancelled(check)?;
                        let object = cas.publish_staged(staged)?;
                        cancelled(check)?;
                        asset_object_catalog::register_in_transaction(tx, &[asset_object_catalog::AssetObjectRegistration {
                            object_hash: object.content_hash, byte_size: object.byte_size }], super::device_store::now_ms()?)?;
                    }
                    commit::apply_working_set(tx, generation, &prepared.input, &prepared.aliases, capture, Some((&request.header, &stamp)))?;
                    tx.execute("INSERT INTO lww_requests(request_id,digest,revision) VALUES(?1,?2,?3)",
                        params![request.header.request_id, digest, current_revision(tx)? + 1])?;
                    cancelled(check)
                })?;
            transaction.commit()?;
            drop(device);
            publication.finish_page_catalog_registration(&repository)?;
            Ok(ConflictCopyResult::Committed { revision: committed.revision, target_character_id: request.target_character_id.clone(),
                source_character_id: request.source.character_id.clone(), source_conversation_id: request.source.conversation_id.clone(),
                target_conversation_id: request.target_conversation_id.clone() })
        })();
        // Release may catalog partially published objects, so it must run after all database locks.
        let released = if publication.is_released() { Ok(()) } else { publication.release(CasReleaseOutcome::Aborted).map_err(StoreError::from) };
        match result { Ok(result) => { released?; Ok(result) }, Err(error) => { let _ = released; Err(error) } }
    }
}
