use super::{query, snapshot, ConversationMutation, ConversationWindowQuery, PersistentStore, RevisionReadLease,
    StoreError, StoreResult, WorkingSetCommit};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

#[cfg(test)]
thread_local! {
    static BEFORE_SCRIPT_SNAPSHOT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Target {
    pub character_id: String,
    pub conversation_id: String,
    pub library_identity: String,
    pub binding_authority: risunest_sync_wire::stamp::DecimalU64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UnitFence { stamp: String, version: String }

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ExistenceFence {
    character: Option<UnitFence>,
    conversation: Option<UnitFence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct MessageFence { unit: Option<UnitFence>, pub count: i64 }

impl UnitFence {
    fn owned_bytes(&self) -> usize { self.stamp.capacity() + self.version.capacity() }
}

impl ExistenceFence {
    pub(crate) fn owned_bytes(&self) -> usize {
        self.character.as_ref().map_or(0, UnitFence::owned_bytes) + self.conversation.as_ref().map_or(0, UnitFence::owned_bytes)
    }
}

impl MessageFence {
    pub(crate) fn owned_bytes(&self) -> usize { self.unit.as_ref().map_or(0, UnitFence::owned_bytes) }
}

pub(crate) struct ReadView {
    lease: RevisionReadLease,
    pub revision: i64,
    pub metadata: Value,
    pub existence: ExistenceFence,
    pub messages: MessageFence,
}

pub(crate) fn error(code: &str) -> StoreError {
    StoreError::Validation { message: format!("native-script:{code}") }
}

fn fence(db: &Connection, components: &[&str]) -> StoreResult<Option<UnitFence>> {
    let key = risunest_sync_wire::unit::UnitKey::new(components).map_err(|_| error("invalid-target"))?;
    Ok(db.query_row("SELECT stamp,version FROM lww_units WHERE key=?1", [key.as_str()],
        |row| Ok(UnitFence { stamp: row.get(0)?, version: row.get(1)? })).optional()?)
}

impl ReadView {
    pub(crate) fn read(&self, target: &Target, start: i64, count: i64) -> StoreResult<Vec<Value>> {
        let result = query::read_conversation_window(&self.lease.connection, &ConversationWindowQuery {
            character_id: target.character_id.clone(), conversation_id: target.conversation_id.clone(),
            start_index: Some(start), limit: Some(count), anchor_message_id: None,
            anchor_occurrence: None, before: None, after: None, skip_parser_work: None,
        }, &self.lease.target)?.ok_or_else(|| error("missing-conversation"))?;
        if result.value.start_index != start || result.value.messages.len() as i64 != count {
            return Err(error("history-gap"));
        }
        Ok(result.value.messages)
    }

    pub(crate) fn defaults(&self, target: &Target) -> StoreResult<Vec<(String, String)>> {
        let character = query::read_character(&self.lease.connection, &target.character_id, &self.lease.target)?
            .ok_or_else(|| error("missing-character"))?.value;
        let root = query::read_root(&self.lease.connection, &self.lease.target)?.value;
        let mut defaults = Vec::new();
        for source in [character.get("defaultVariables"), root.get("templateDefaultVariables")] {
            if let Some(source) = source.and_then(Value::as_str) {
                for line in source.split('\n') {
                    let mut parts = line.split('=');
                    if let (Some(key), Some(value)) = (parts.next(), parts.next()) {
                        if !key.is_empty() && !value.is_empty() && !defaults.iter().any(|(existing, _)| existing == key) {
                            defaults.push((key.to_owned(), value.to_owned()));
                        }
                    }
                }
            }
        }
        Ok(defaults)
    }

    pub(crate) fn close(self) -> StoreResult<()> { snapshot::close_revision(self.lease) }
}

#[derive(Debug, Clone)]
pub(crate) struct FrozenAttempt {
    pub commit: WorkingSetCommit,
    pub digest: String,
    pub retained_bytes: usize,
    database_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryIssue { Busy, Failed, Quarantined, Integrity, StoreMismatch }

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Resolution { Committed(i64), NotCommitted, Unresolved(RecoveryIssue) }

/// A change of the history's rows: one `ReplaceRange`, in the coordinates the earlier splices leave.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Splice { pub start: i64, pub delete_count: i64, pub messages: Vec<Value> }

pub(crate) struct Prepared {
    pub attempt: Option<FrozenAttempt>,
    pub changed_keys: Vec<String>,
    pub messages_changed: bool,
}

impl PersistentStore {
    pub(crate) fn script_read_view(&self, target: &Target) -> StoreResult<ReadView> {
        if self.library_identity()? != target.library_identity || self.lww_binding_authority()? != target.binding_authority {
            return Err(error("stale-authority"));
        }
        let revision = self.revision()?;
        #[cfg(test)]
        BEFORE_SCRIPT_SNAPSHOT.with(|hook| hook.borrow_mut().take()).map(|hook| hook());
        let (_, lease) = snapshot::acquire_revision(&self.database_path, revision, Arc::clone(&self.active_readers))?;
        if lease.target.generation != target.library_identity { return Err(error("stale-authority")); }
        let db = &lease.connection;
        let metadata = query::read_conversation_metadata(db, &target.character_id, &target.conversation_id, &lease.target)?
            .ok_or_else(|| error("missing-conversation"))?.value;
        let character = query::read_character_summary(db, &target.character_id, &lease.target)?
            .ok_or_else(|| error("missing-character"))?;
        if character.r#type != "character" { return Err(error("unsupported-character")); }
        let existence = ExistenceFence {
            character: fence(db, &["exists", "character", &target.character_id])?,
            conversation: fence(db, &["exists", "conversation", &target.character_id, &target.conversation_id])?,
        };
        let messages = MessageFence { unit: fence(db, &["messages", &target.character_id, &target.conversation_id])?, count: metadata.total_messages };
        Ok(ReadView { lease, revision, metadata: metadata.conversation, existence, messages })
    }

    pub(crate) fn script_prepare(&self, target: &Target, existence: &ExistenceFence,
        messages: Option<&MessageFence>, edits: &BTreeMap<i64, Value>, splices: &[Splice], variables: &[(String, String)]) -> StoreResult<Prepared> {
        let view = self.script_read_view(target)?;
        if &view.existence != existence { return Err(error("stale-owner")); }
        if messages.is_some_and(|expected| expected != &view.messages) { return Err(error("stale-messages")); }
        let mut scriptstate = view.metadata.get("scriptstate").and_then(Value::as_object).cloned().unwrap_or_default();
        let mut changed_keys = Vec::new();
        for (key, value) in variables {
            let stored_key = format!("${key}");
            if scriptstate.get(&stored_key) != Some(&Value::String(value.clone())) {
                scriptstate.insert(stored_key, Value::String(value.clone()));
                changed_keys.push(key.clone());
            }
        }
        let mut ranges = Vec::new();
        for (&index, value) in edits {
            if index < 0 || index >= view.messages.count { return Err(error("stale-messages")); }
            if view.read(target, index, 1)?.first() == Some(value) { continue; }
            ranges.push(ConversationMutation::ReplaceRange {
                character_id: target.character_id.clone(), conversation_id: target.conversation_id.clone(),
                start: index, delete_count: 1, messages: vec![value.clone()], conversation: None, configured_index: None,
            });
        }
        // The edits address stored rows; the splices follow them.
        let mut count = view.messages.count;
        for splice in splices {
            let end = splice.start.checked_add(splice.delete_count).ok_or_else(|| error("invalid-splice"))?;
            if splice.start < 0 || splice.delete_count < 0 || end > count { return Err(error("invalid-splice")); }
            count = count - splice.delete_count + splice.messages.len() as i64;
            ranges.push(ConversationMutation::ReplaceRange {
                character_id: target.character_id.clone(), conversation_id: target.conversation_id.clone(),
                start: splice.start, delete_count: splice.delete_count, messages: splice.messages.clone(), conversation: None, configured_index: None,
            });
        }
        let messages_changed = !ranges.is_empty();
        let mutations = if changed_keys.is_empty() { None } else {
            Some(vec![super::lww::UnitMutation::Set {
                key: risunest_sync_wire::unit::UnitKey::new(&["conversation", &target.character_id, &target.conversation_id, "scriptstate"])
                    .map_err(|_| error("invalid-target"))?, value: Value::Object(scriptstate),
            }])
        };
        let attempt = if !messages_changed && mutations.is_none() { None } else {
            let commit = WorkingSetCommit {
                expected_revision: view.revision, request_id: Some(uuid::Uuid::new_v4().to_string()),
                binding_authority: Some(target.binding_authority), unit_mutations: mutations,
                conversations: (!ranges.is_empty()).then_some(ranges), ..Default::default()
            };
            let digest = super::lww::commit_intent_digest(&commit)?;
            let database_path = self.database_path.clone();
            let retained_bytes = commit_bytes(&commit).saturating_add(digest.capacity()).saturating_add(database_path.capacity());
            Some(FrozenAttempt { digest, commit, retained_bytes, database_path })
        };
        view.close()?;
        Ok(Prepared { attempt, changed_keys, messages_changed })
    }

    pub(crate) fn script_issue(&mut self, attempt: &FrozenAttempt) -> StoreResult<Resolution> {
        if self.database_path != attempt.database_path { return Ok(Resolution::Unresolved(RecoveryIssue::StoreMismatch)); }
        if super::lww::commit_intent_digest(&attempt.commit)? != attempt.digest { return Err(error("attempt-integrity")); }
        // The caller holds its owner admission guard before entering this reservation path.
        let result = self.commit_holding(&attempt.commit, |_| Ok(()));
        let resolved = self.script_resolve(attempt)?;
        if resolved == Resolution::NotCommitted { result?; }
        Ok(resolved)
    }

    pub(crate) fn script_resolve(&mut self, attempt: &FrozenAttempt) -> StoreResult<Resolution> {
        if self.database_path != attempt.database_path { return Ok(Resolution::Unresolved(RecoveryIssue::StoreMismatch)); }
        if super::lww::commit_intent_digest(&attempt.commit)? != attempt.digest { return Err(error("attempt-integrity")); }
        let recovered = self.lww_recover_intents();
        let request = attempt.commit.request_id.as_deref().ok_or_else(|| error("missing-request"))?;
        let receipt: Option<(String, i64)> = self.connection.query_row(
            "SELECT digest,revision FROM lww_requests WHERE request_id=?1", [request], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
        let device = self.device_store()?.connection();
        let pending: bool = device.query_row("SELECT EXISTS(SELECT 1 FROM lww_intents WHERE request_id=?1)", [request], |row| row.get(0))?;
        if let Some((digest, revision)) = receipt {
            if digest != attempt.digest || attempt.commit.expected_revision.checked_add(1) != Some(revision) {
                return Ok(Resolution::Unresolved(RecoveryIssue::Integrity));
            }
            if !pending { return Ok(Resolution::Committed(revision)); }
        }
        if pending {
            let quarantined: bool = device.query_row(
                "SELECT EXISTS(SELECT 1 FROM lww_intent_failures WHERE request_id=?1 AND quarantined=1)", [request], |row| row.get(0))?;
            return Ok(Resolution::Unresolved(if quarantined { RecoveryIssue::Quarantined }
                else if matches!(recovered, Err(StoreError::CommitBusy | StoreError::SnapshotReleased)) { RecoveryIssue::Busy }
                else { RecoveryIssue::Failed }));
        }
        if self.lww_request_unreserved(request)? { return Ok(Resolution::NotCommitted); }
        Ok(Resolution::Unresolved(RecoveryIssue::Integrity))
    }
}

pub(crate) fn stored_variable(metadata: &Value, key: &str) -> Option<Value> {
    metadata.get("scriptstate")?.get(format!("${key}")).cloned()
}

pub(crate) fn owned_value_bytes(value: &Value) -> usize {
    use std::mem::size_of;
    size_of::<Value>().saturating_add(match value {
        Value::String(value) => value.capacity(),
        Value::Array(values) => values.capacity().saturating_mul(size_of::<Value>())
            .saturating_add(values.iter().map(owned_value_bytes).sum::<usize>()),
        Value::Object(values) => values.iter().map(|(key, value)| key.capacity()
            .saturating_add(size_of::<String>()).saturating_add(owned_value_bytes(value))
            .saturating_add(8 * size_of::<usize>())).sum(),
        _ => 0,
    })
}

fn commit_bytes(commit: &WorkingSetCommit) -> usize {
    use std::mem::size_of;
    let mut bytes = size_of::<FrozenAttempt>() + commit.request_id.as_ref().map_or(0, String::capacity);
    if let Some(ranges) = &commit.conversations {
        bytes += ranges.capacity() * size_of::<ConversationMutation>();
        for range in ranges {
            if let ConversationMutation::ReplaceRange { character_id, conversation_id, messages, .. } = range {
                bytes += character_id.capacity() + conversation_id.capacity()
                    + messages.capacity() * size_of::<Value>() + messages.iter().map(owned_value_bytes).sum::<usize>();
            }
        }
    }
    if let Some(mutations) = &commit.unit_mutations {
        bytes += mutations.capacity() * size_of::<super::lww::UnitMutation>();
        for mutation in mutations {
            if let super::lww::UnitMutation::Set { key, value } = mutation {
                bytes += key.as_str().len() * 2 + owned_value_bytes(value);
            }
        }
    }
    bytes
}

pub(crate) fn object_mut(value: &mut Value) -> StoreResult<&mut Map<String, Value>> {
    value.as_object_mut().ok_or_else(|| error("invalid-message"))
}

#[cfg(test)]
#[path = "../native_script/store_tests.rs"]
mod tests;
