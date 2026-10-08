//! Stages a complete, authenticated external logical snapshot for activation.
//!
//! The transport owns download and decryption. This boundary accepts only local
//! regular files, revalidates their identities, and never performs network I/O.
use super::{
    active_generation, current_revision, record_apply as rows,
    server_sync_projection::{preserve_local_view, ServerPayload},
    PersistentStore,
    PreparedReplaceCommit, StoreError, StoreResult,
};
use crate::{
    asset_repository::{
        owner_manifest_codec::{decode_owner_manifest, encode_owner_manifest},
        PayloadCas,
    },
    logical_records::{
        decode_logical_record, decode_logical_record_key, decode_message_page,
        encode_logical_record_key, LogicalRecordEnvelope, LogicalRecordLocator,
    },
};
use crate::external_storage::content_store::{Body, CancellableRead, ContentStore, ObjectSource};
use crate::external_storage::lww_residency::RemoteBodies;
use crate::local_backup::CancellationProbe;
use risunest_external_storage_format::format::{fingerprint, library_fingerprint_domain};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub(crate) struct ExternalSnapshotRecord {
    pub key: String,
    pub content_hash: String,
    pub byte_length: u64,
    pub source: ObjectSource,
}

#[derive(Clone, Debug)]
pub(crate) struct ExternalSnapshotObject {
    pub content_hash: String,
    pub byte_length: u64,
    pub source: ObjectSource,
}

pub(crate) struct ExternalSnapshotApplication<'a> {
    pub expected_revision: i64,
    pub staging_root: &'a Path,
    pub scope_id: &'a [u8; 32],
    pub fingerprint: &'a [u8; 32],
    /// Asked between objects, records, message runs and read chunks. A
    /// cancelled stage is removed like any other rejected one.
    pub probe: &'a dyn CancellationProbe,
}

impl PreparedReplaceCommit {
    pub(crate) fn external_staging_id(&self) -> &str {
        &self.staging_id
    }
}

impl PersistentStore {
    /// Prepare the library portion of a fully downloaded external snapshot.
    /// Device sections, when present in the repository scope, remain under the
    /// existing device-maintenance restore coordinator and are not read here.
    pub(crate) fn prepare_external_snapshot_application<R, O>(
        &mut self,
        application: &ExternalSnapshotApplication<'_>,
        records: R,
        objects: O,
    ) -> StoreResult<PreparedReplaceCommit>
    where
        R: IntoIterator<Item = StoreResult<ExternalSnapshotRecord>>,
        O: IntoIterator<Item = StoreResult<ExternalSnapshotObject>>,
    {
        validate_application(application)?;
        let actual_revision = self.revision()?;
        if application.expected_revision != actual_revision {
            return Err(StoreError::RevisionConflict {
                expected: application.expected_revision,
                actual: actual_revision,
            });
        }

        let root = verified_staging_root(application.staging_root)?;
        // Record bodies are asked for by identity, whether a local capture
        // named them or a receive decoded them into the job's own store. Only
        // a caller that offers neither leaves this directory absent.
        let content = ContentStore::open_existing(&root.join("external-storage"))?;
        let stage = self.replace_begin()?;
        let mut remote = RemoteBodies::deferred(&self.repository_root);
        let staged = (|| {
            let object_sizes = stage_objects(
                &self.repository_root,
                &mut remote,
                &root,
                content.as_ref(),
                objects,
                application.probe,
            )?;
            let record_hashes = stage_records(
                &mut self.connection,
                &self.repository_root,
                &mut remote,
                &stage.staging_id,
                application.expected_revision,
                &root,
                content.as_ref(),
                &object_sizes,
                records,
                application.probe,
            )?;
            if fingerprint(application.scope_id, &record_hashes) != *application.fingerprint {
                return invalid("External snapshot logical fingerprint differs from its catalog");
            }
            check(application.probe)?;

            self.prepare_replace_commit(&stage.staging_id, Some(application.expected_revision))
        })();

        match staged {
            Ok(prepared) => Ok(prepared),
            Err(error) => {
                if let Err(cleanup) = self.replace_abort(&stage.staging_id) {
                    return Err(StoreError::Store {
                        message: format!(
                            "{error}; failed to remove rejected external snapshot stage: {cleanup}"
                        ),
                    });
                }
                Err(error)
            }
        }
    }
}

struct PreparedRecord {
    locator: LogicalRecordLocator,
    envelope: LogicalRecordEnvelope,
    messages: Option<Vec<rows::SerializedMessage>>,
}

/// The page and manifest bytes an envelope's rows are made from, from catalog
/// lengths alone.
fn dependent_bytes(
    catalog: &BTreeMap<String, u64>,
    envelope: &LogicalRecordEnvelope,
) -> StoreResult<u64> {
    let length = |hash: &str| {
        catalog
            .get(hash)
            .copied()
            .ok_or_else(|| validation("External snapshot has an incomplete payload reference"))
    };
    let owners = |heads: &[crate::logical_records::LogicalOwnerHead]| -> StoreResult<u64> {
        let mut bytes = 0u64;
        for head in heads {
            if let Some(hash) = &head.manifest_hash {
                bytes = bytes.saturating_add(length(hash)?);
            }
        }
        Ok(bytes)
    };
    match envelope {
        LogicalRecordEnvelope::Root { owner_heads, .. }
        | LogicalRecordEnvelope::Character { owner_heads, .. }
        | LogicalRecordEnvelope::ArchivedCharacter { owner_heads, .. } => owners(owner_heads),
        LogicalRecordEnvelope::Conversation {
            message_page_hashes,
            ..
        } => {
            let mut bytes = 0u64;
            for hash in message_page_hashes {
                bytes = bytes.saturating_add(length(hash)?);
            }
            Ok(bytes)
        }
        _ => Ok(0),
    }
}

fn validate_application(application: &ExternalSnapshotApplication<'_>) -> StoreResult<()> {
    if *application.scope_id != library_fingerprint_domain() {
        return invalid("External snapshot fingerprint domain differs");
    }
    Ok(())
}

fn verified_staging_root(root: &Path) -> StoreResult<PathBuf> {
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() || crate::trust_boundary::is_link_like(&metadata) {
        return invalid("External snapshot staging root must be a real directory");
    }
    Ok(fs::canonicalize(root)?)
}

/// One snapshot input. A downloaded asset is a file confined to the staging
/// directory it arrived in; a record body is asked for by identity, wherever
/// the content store beside it put it.
fn open_snapshot_input(
    root: &Path,
    content: Option<&ContentStore>,
    source: &ObjectSource,
    expected_size: u64,
) -> StoreResult<Body> {
    let path = match source {
        ObjectSource::Captured(digest) => {
            let content = content
                .ok_or_else(|| validation("External snapshot capture store is unavailable"))?;
            if content.stat(digest)? != Some(expected_size) {
                return invalid("External snapshot input size differs from its catalog");
            }
            return Ok(content.open_body(digest)?);
        }
        ObjectSource::File(path) => path,
        // The library already holds this body; it is confirmed in place rather
        // than read through here.
        ObjectSource::Library(_) => {
            return invalid("External snapshot library body is not an input");
        }
    };
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || crate::trust_boundary::is_link_like(&metadata) {
        return invalid("External snapshot input must be a regular file");
    }
    let canonical = fs::canonicalize(path)?;
    if canonical == root || !canonical.starts_with(root) {
        return invalid("External snapshot input escaped its staging directory");
    }
    let file = crate::trust_boundary::open_regular_source(&canonical)?;
    if file.metadata()?.len() != expected_size {
        return invalid("External snapshot input size differs from its catalog");
    }
    Ok(Body::File(file))
}

fn validate_hash(hash: &str, subject: &str) -> StoreResult<[u8; 32]> {
    if !crate::trust_boundary::is_lower_hex_256(hash) {
        return invalid(format!(
            "{subject} hash must be 64 lowercase hexadecimal characters"
        ));
    }
    let decoded = hex::decode(hash).map_err(|_| validation("External snapshot hash is invalid"))?;
    decoded
        .try_into()
        .map_err(|_| validation("External snapshot hash is invalid"))
}

fn stage_objects<O>(
    repository_root: &Path,
    remote: &mut RemoteBodies,
    staging_root: &Path,
    content: Option<&ContentStore>,
    objects: O,
    probe: &dyn CancellationProbe,
) -> StoreResult<BTreeMap<String, u64>>
where
    O: IntoIterator<Item = StoreResult<ExternalSnapshotObject>>,
{
    let cas = PayloadCas::new(repository_root)?;
    let cancelled = || probe.is_cancelled();
    let mut sizes = BTreeMap::new();
    for object in objects {
        check(probe)?;
        let object = object?;
        validate_hash(&object.content_hash, "External snapshot object")?;
        if sizes
            .insert(object.content_hash.clone(), object.byte_length)
            .is_some()
        {
            return invalid("External snapshot contains a duplicate object hash");
        }
        if let ObjectSource::Library(digest) = &object.source {
            // The repository already holds this body. Confirming that it is
            // there under the identity and length the catalog names is the
            // whole of the work; reading it back to write it again is not.
            if digest != &object.content_hash {
                return invalid("External snapshot library body is not the object it names");
            }
            let size = match cas.stat_object(&object.content_hash)? {
                Some(size) => Some(size),
                None => remote.stat(&object.content_hash)?,
            };
            if size != Some(object.byte_length) {
                return invalid("External snapshot input size differs from its catalog");
            }
            continue;
        }
        let mut source = CancellableRead::new(
            open_snapshot_input(staging_root, content, &object.source, object.byte_length)?,
            &cancelled,
        );
        let prepared =
            cas.prepare_reader_expected(&mut source, &object.content_hash, object.byte_length)?;
        if prepared.content_hash != object.content_hash || prepared.byte_size != object.byte_length
        {
            return invalid("External snapshot object identity changed while staging");
        }
    }
    Ok(sizes)
}

/// A stage is written a batch at a time, each batch its own transaction, so a
/// database-sized snapshot never holds the writer for the whole library. A
/// batch closes at whichever bound it reaches first. Staging 40,001 records
/// of a 20,000-character library took 79 batches, the longest holding the
/// writer 58 ms, where one transaction had held it for 14 s.
const STAGE_BATCH_RECORDS: usize = 512;
const STAGE_BATCH_WORK: u64 = 8_192;
const STAGE_BATCH_BYTES: u64 = 8 * 1024 * 1024;

#[cfg(test)]
thread_local! {
    /// Runs after each stage batch commits, with the number of batches so far
    /// and how long that batch held the writer.
    static AFTER_STAGE_BATCH: std::cell::RefCell<Option<Box<dyn FnMut(usize, std::time::Duration)>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn after_stage_batch(hook: Option<Box<dyn FnMut(usize, std::time::Duration)>>) {
    AFTER_STAGE_BATCH.with(|slot| *slot.borrow_mut() = hook);
}

/// One write a stage batch holds: a record's own rows, declaring how many
/// messages it has, or a run of those messages.
enum StageWrite {
    Record {
        record: PreparedRecord,
        message_count: u64,
    },
    Messages {
        character_id: String,
        conversation_id: String,
        first: usize,
        messages: Vec<rows::SerializedMessage>,
    },
}

/// Records prepared outside any transaction and waiting to be written into
/// the stage together.
struct StageBatch {
    writes: Vec<StageWrite>,
    records: usize,
    work: u64,
    bytes: u64,
    written: usize,
}

impl StageBatch {
    /// Adds one record, writing the batch whenever it fills. A conversation's
    /// messages are spread over as many batches as the bounds need, since the
    /// stage is invisible until activation and one long conversation would
    /// otherwise be one transaction.
    fn push(
        &mut self,
        connection: &mut rusqlite::Connection,
        generation: &str,
        expected_revision: i64,
        mut record: PreparedRecord,
        bytes: u64,
        probe: &dyn CancellationProbe,
    ) -> StoreResult<()> {
        let messages = record.messages.take();
        let conversation = match &record.locator {
            LogicalRecordLocator::Conversation {
                character_id,
                conversation_id,
            } => Some((character_id.clone(), conversation_id.clone())),
            _ => None,
        };
        self.records += 1;
        self.work = self.work.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes);
        self.writes.push(StageWrite::Record {
            record,
            message_count: messages.as_ref().map_or(0, |messages| messages.len() as u64),
        });
        if let (Some((character_id, conversation_id)), Some(messages)) = (conversation, messages) {
            let mut first = 0;
            let mut messages = messages.into_iter().peekable();
            while messages.peek().is_some() {
                if self.full() {
                    self.write(connection, generation, expected_revision, probe)?;
                }
                let mut run = Vec::new();
                while !self.full() {
                    let Some(message) = messages.next() else {
                        break;
                    };
                    self.work = self.work.saturating_add(1);
                    self.bytes = self.bytes.saturating_add(message.byte_len());
                    run.push(message);
                }
                let count = run.len();
                self.writes.push(StageWrite::Messages {
                    character_id: character_id.clone(),
                    conversation_id: conversation_id.clone(),
                    first,
                    messages: run,
                });
                first += count;
            }
        }
        if self.full() {
            self.write(connection, generation, expected_revision, probe)?;
        }
        Ok(())
    }

    fn full(&self) -> bool {
        self.records >= STAGE_BATCH_RECORDS
            || self.work >= STAGE_BATCH_WORK
            || self.bytes >= STAGE_BATCH_BYTES
    }

    /// Writes the batch, provided the library is still at the revision the
    /// stage was prepared against: a local edit ends the stage at the next
    /// batch rather than at activation. Each batch extends conversation pages,
    /// so the final batch and activation never read the whole conversation.
    /// Cancellation can roll back page construction, never a committed batch.
    fn write(
        &mut self,
        connection: &mut rusqlite::Connection,
        generation: &str,
        expected_revision: i64,
        probe: &dyn CancellationProbe,
    ) -> StoreResult<()> {
        if self.writes.is_empty() {
            return Ok(());
        }
        check(probe)?;
        #[cfg(test)]
        let started = std::time::Instant::now();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actual_revision = current_revision(&transaction)?;
        if actual_revision != expected_revision {
            return Err(StoreError::RevisionConflict {
                expected: expected_revision,
                actual: actual_revision,
            });
        }
        let mut complete = Vec::new();
        for write in self.writes.drain(..) {
            match write {
                StageWrite::Record {
                    record,
                    message_count,
                } => {
                    rows::apply_serialized_record_rows(
                        &transaction,
                        generation,
                        &record.locator,
                        &record.envelope,
                        message_count,
                    )?;
                    if let LogicalRecordLocator::Conversation {
                        character_id,
                        conversation_id,
                    } = record.locator
                    {
                        if message_count == 0 {
                            complete.push((character_id, conversation_id));
                        }
                    }
                }
                StageWrite::Messages {
                    character_id,
                    conversation_id,
                    first,
                    messages,
                } => {
                    rows::insert_serialized_messages(
                        &transaction,
                        generation,
                        &character_id,
                        &conversation_id,
                        first,
                        &messages,
                    )?;
                    complete.push((character_id, conversation_id));
                }
            }
        }
        for (character_id, conversation_id) in complete {
            super::message_pages::stage_conversation_pages(
                &transaction,
                generation,
                &character_id,
                &conversation_id,
                &|| check(probe),
            )?;
        }
        transaction.commit()?;
        self.records = 0;
        self.work = 0;
        self.bytes = 0;
        self.written += 1;
        #[cfg(test)]
        AFTER_STAGE_BATCH.with(|slot| {
            if let Some(hook) = slot.borrow_mut().as_mut() {
                hook(self.written, started.elapsed());
            }
        });
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn stage_records<R>(
    connection: &mut rusqlite::Connection,
    repository_root: &Path,
    remote: &mut RemoteBodies,
    generation: &str,
    expected_revision: i64,
    staging_root: &Path,
    content: Option<&ContentStore>,
    objects: &BTreeMap<String, u64>,
    records: R,
    probe: &dyn CancellationProbe,
) -> StoreResult<BTreeMap<String, [u8; 32]>>
where
    R: IntoIterator<Item = StoreResult<ExternalSnapshotRecord>>,
{
    let cas = PayloadCas::new(repository_root)?;
    let actual_revision = current_revision(connection)?;
    if actual_revision != expected_revision {
        return Err(StoreError::RevisionConflict {
            expected: expected_revision,
            actual: actual_revision,
        });
    }
    // Read once for the whole stage; every batch confirms the revision it
    // was read at.
    let active = active_generation(connection)?;
    let local_root = local_root(connection, &active)?;

    let mut hashes = BTreeMap::new();
    let mut deferred_conversations = Vec::new();
    let mut selections = Vec::new();
    let mut saw_root = false;
    let mut batch = StageBatch {
        writes: Vec::new(),
        records: 0,
        work: 0,
        bytes: 0,
        written: 0,
    };
    let mut stage = |connection: &mut rusqlite::Connection,
                     batch: &mut StageBatch,
                     record: ExternalSnapshotRecord,
                     locator: LogicalRecordLocator|
     -> StoreResult<()> {
        check(probe)?;
        let envelope = read_record(staging_root, content, &record, &locator, probe)?;
        let mut bytes = dependent_bytes(objects, &envelope)?;
        // A conversation's pages are counted message by message as they join
        // a batch.
        if matches!(locator, LogicalRecordLocator::Conversation { .. }) {
            bytes = 0;
        }
        let prepared = prepare_record(
            connection,
            &cas,
            remote,
            &active,
            objects,
            locator,
            envelope,
            local_root.as_ref(),
            probe,
        )?;
        if let (
            LogicalRecordLocator::Character { character_id },
            LogicalRecordEnvelope::Character { detail, .. },
        ) = (&prepared.locator, &prepared.envelope)
        {
            selections.push((
                character_id.clone(),
                detail.get("chatPage").and_then(serde_json::Value::as_u64),
            ));
        }
        batch.push(
            connection,
            generation,
            expected_revision,
            prepared,
            record.byte_length.saturating_add(bytes),
            probe,
        )
    };
    for record in records {
        check(probe)?;
        let record = record?;
        let hash = validate_hash(&record.content_hash, "External logical record")?;
        if hashes.insert(record.key.clone(), hash).is_some() {
            return invalid("External snapshot contains a duplicate logical key");
        }
        let locator = decode_logical_record_key(&record.key)
            .map_err(|_| validation("External snapshot logical key is invalid"))?;
        if matches!(locator, LogicalRecordLocator::Root) {
            saw_root = true;
        }
        if matches!(locator, LogicalRecordLocator::Conversation { .. }) {
            deferred_conversations.push((record, locator));
        } else {
            stage(connection, &mut batch, record, locator)?;
        }
    }
    if !saw_root {
        return invalid("External snapshot does not contain its root logical record");
    }
    for (record, locator) in deferred_conversations {
        stage(connection, &mut batch, record, locator)?;
    }
    batch.write(connection, generation, expected_revision, probe)?;
    check(probe)?;

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let actual_revision = current_revision(&transaction)?;
    if actual_revision != expected_revision {
        return Err(StoreError::RevisionConflict {
            expected: expected_revision,
            actual: actual_revision,
        });
    }
    transaction.execute(
        "UPDATE characters SET conversation_count=(
            SELECT count(*) FROM conversations
            WHERE conversations.generation=?1
              AND conversations.character_id=characters.character_id
         ) WHERE generation=?1",
        [generation],
    )?;
    // A chat page kept from this device can name a conversation the snapshot does not have.
    for (character_id, selected) in &selections {
        let count: Option<i64> = transaction
            .query_row(
                "SELECT conversation_count FROM characters
                 WHERE generation=?1 AND character_id=?2 AND archived_object IS NULL",
                params![generation, character_id],
                |row| row.get(0),
            )
            .optional()?;
        if count.is_some_and(|count| {
            count > 0 && !selected.is_some_and(|index| index < count as u64)
        }) {
            super::commit::reset_stale_chat_page(&transaction, generation, character_id)?;
        }
    }
    rows::validate_configured_index_uniqueness(&transaction, generation)?;
    let duplicate_plugin_ordinals: bool = transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM plugin_storage WHERE generation=?1
            GROUP BY ordinal HAVING count(*)>1
         )",
        [generation],
        |row| row.get(0),
    )?;
    if duplicate_plugin_ordinals {
        return invalid("External snapshot plugin records contain duplicate positions");
    }
    transaction.commit()?;
    Ok(hashes)
}

fn local_root(
    connection: &rusqlite::Connection,
    generation: &str,
) -> StoreResult<Option<serde_json::Value>> {
    Ok(connection
        .query_row(
            "SELECT value FROM root WHERE generation=?1",
            [generation],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| serde_json::from_str(&value))
        .transpose()?)
}

/// Reads one record body, checks it against its catalog entry and key, and
/// decodes it.
fn read_record(
    staging_root: &Path,
    content: Option<&ContentStore>,
    record: &ExternalSnapshotRecord,
    locator: &LogicalRecordLocator,
    probe: &dyn CancellationProbe,
) -> StoreResult<LogicalRecordEnvelope> {
    if encode_logical_record_key(locator)
        .map_err(|_| validation("External snapshot logical key is invalid"))?
        != record.key
    {
        return invalid("External snapshot logical key is not canonical");
    }
    let bytes = read_verified_record(staging_root, content, record, probe)?;
    let envelope = decode_logical_record(&bytes)
        .map_err(|_| validation("External snapshot logical record is invalid"))?;
    rows::validate_locator_envelope(locator, &envelope)?;
    Ok(envelope)
}

/// Turns a decoded record into the rows it becomes: the fields this library
/// keeps for itself carried over from `active`, owner data rehydrated, and
/// message pages read and encoded as rows.
#[allow(clippy::too_many_arguments)]
fn prepare_record(
    connection: &rusqlite::Connection,
    cas: &PayloadCas,
    remote: &mut RemoteBodies,
    active: &str,
    objects: &BTreeMap<String, u64>,
    locator: LogicalRecordLocator,
    envelope: LogicalRecordEnvelope,
    local_root: Option<&serde_json::Value>,
    probe: &dyn CancellationProbe,
) -> StoreResult<PreparedRecord> {
    let local_character = if let LogicalRecordLocator::Character { character_id } = &locator {
        connection
            .query_row(
                "SELECT detail FROM characters WHERE generation=?1 AND character_id=?2",
                params![active, character_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|value| serde_json::from_str(&value))
            .transpose()?
    } else {
        None
    };
    let mut payload = ServerPayload {
        record: envelope,
        messages: None,
        derived_objects: BTreeMap::new(),
    };
    preserve_local_view(&mut payload, local_character.as_ref(), local_root);
    let mut envelope = payload.record;
    let mut requires = std::collections::BTreeSet::new();
    let messages = resolve_dependencies(cas, remote, objects, &mut envelope, &mut requires, probe)?
        .map(|messages| {
            messages
                .iter()
                .map(|message| {
                    check(probe)?;
                    rows::SerializedMessage::of(message)
                })
                .collect::<StoreResult<Vec<_>>>()
        })
        .transpose()?;
    Ok(PreparedRecord {
        locator,
        envelope,
        messages,
    })
}

fn read_verified_record(
    root: &Path,
    content: Option<&ContentStore>,
    record: &ExternalSnapshotRecord,
    probe: &dyn CancellationProbe,
) -> StoreResult<Vec<u8>> {
    let mut source = open_snapshot_input(root, content, &record.source, record.byte_length)?;
    let cancelled = || probe.is_cancelled();
    let capacity = usize::try_from(record.byte_length)
        .map_err(|_| validation("External logical record is too large for this platform"))?;
    let mut bytes = Vec::with_capacity(capacity.min(1024 * 1024));
    CancellableRead::new(&mut source, &cancelled)
        .take(record.byte_length.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != record.byte_length
        || hex::encode(Sha256::digest(&bytes)) != record.content_hash
    {
        return invalid("External logical record differs from its catalog hash and size");
    }
    if source.len()? != record.byte_length {
        return invalid("External logical record changed while staging");
    }
    Ok(bytes)
}

/// Reads and checks the bodies an envelope's rows are made from, and lists in
/// `requires` the ones those rows go on referring to.
fn resolve_dependencies(
    cas: &PayloadCas,
    remote: &mut RemoteBodies,
    objects: &BTreeMap<String, u64>,
    envelope: &mut LogicalRecordEnvelope,
    requires: &mut std::collections::BTreeSet<(String, Option<u64>)>,
    probe: &dyn CancellationProbe,
) -> StoreResult<Option<Vec<serde_json::Value>>> {
    match envelope {
        LogicalRecordEnvelope::Root { value, owner_heads } => {
            let resolved = resolve_owner_heads(cas, remote, objects, owner_heads, requires, probe)?;
            rows::rehydrate_root_owners(value, &resolved)?;
            Ok(None)
        }
        LogicalRecordEnvelope::Character {
            detail,
            owner_heads,
            ..
        } => {
            let character_id = detail
                .get("chaId")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| validation("External character ID is missing"))?
                .to_owned();
            let resolved = resolve_owner_heads(cas, remote, objects, owner_heads, requires, probe)?;
            rows::rehydrate_character_owner(detail, &character_id, &resolved)?;
            Ok(None)
        }
        LogicalRecordEnvelope::ArchivedCharacter {
            archive_object_hash,
            shared_archive_object_hash,
            archive_object_size,
            asset_hashes,
            shared_asset_hashes,
            owner_heads,
            ..
        } => {
            require(
                cas,
                remote,
                objects,
                requires,
                archive_object_hash,
                Some(*archive_object_size),
            )?;
            require(cas,remote,objects,requires,shared_archive_object_hash,None)?;
            for hash in asset_hashes.iter().chain(shared_asset_hashes.iter()) {
                check(probe)?;
                require(cas, remote, objects, requires, hash, None)?;
            }
            resolve_owner_heads(cas, remote, objects, owner_heads, requires, probe)?;
            Ok(None)
        }
        LogicalRecordEnvelope::Conversation {
            message_page_hashes,
            ..
        } => {
            let mut messages = Vec::new();
            for hash in message_page_hashes.iter() {
                check(probe)?;
                require_object(cas, remote, objects, hash, None)?;
                let bytes = cas
                    .read_object(hash)?
                    .ok_or_else(|| validation("External message page is missing from local CAS"))?;
                let mut page = decode_message_page(&bytes)
                    .map_err(|_| validation("External message page is invalid"))?;
                messages.append(&mut page);
            }
            Ok(Some(messages))
        }
        LogicalRecordEnvelope::Asset {
            object_hash, size, ..
        }
        | LogicalRecordEnvelope::Inlay {
            object_hash, size, ..
        } => {
            let hash = object_hash
                .as_deref()
                .ok_or_else(|| validation("External alias is missing its complete payload"))?;
            require(cas, remote, objects, requires, hash, Some(*size))?;
            Ok(None)
        }
        // The store no longer holds cold payloads; the shared record format still carries the variant.
        LogicalRecordEnvelope::Cold { .. } => {
            Err(validation("External cold records are unsupported"))
        }
        LogicalRecordEnvelope::Preset { .. } | LogicalRecordEnvelope::Plugin { .. } => Ok(None),
    }
}

fn resolve_owner_heads(
    cas: &PayloadCas,
    remote: &mut RemoteBodies,
    objects: &BTreeMap<String, u64>,
    heads: &[crate::logical_records::LogicalOwnerHead],
    requires: &mut std::collections::BTreeSet<(String, Option<u64>)>,
    probe: &dyn CancellationProbe,
) -> StoreResult<Vec<rows::ResolvedOwnerHead>> {
    let mut resolved = Vec::with_capacity(heads.len());
    for head in heads {
        check(probe)?;
        let tuples = if let Some(hash) = &head.manifest_hash {
            require(cas, remote, objects, requires, hash, None)?;
            let bytes = cas
                .read_object(hash)?
                .ok_or_else(|| validation("External owner manifest is missing from local CAS"))?;
            let entries = decode_owner_manifest(&bytes)
                .map_err(|_| validation("External owner manifest is invalid"))?;
            if entries.len() as u64 != head.entry_count
                || encode_owner_manifest(&entries)
                    .map_err(|_| validation("External owner manifest is invalid"))?
                    != bytes
            {
                return invalid("External owner manifest differs from its logical head");
            }
            let mut tuples = Vec::with_capacity(entries.len());
            for entry in entries {
                check(probe)?;
                if let Some(hash) = entry.payload_hash {
                    require(cas, remote, objects, requires, &hex::encode(hash), None)?;
                }
                tuples.push(serde_json::Value::Array(
                    entry
                        .tuple
                        .into_iter()
                        .map(serde_json::Value::String)
                        .collect(),
                ));
            }
            Some(tuples)
        } else {
            None
        };
        resolved.push(rows::ResolvedOwnerHead {
            head: head.clone(),
            tuples,
        });
    }
    Ok(resolved)
}

fn require(
    cas: &PayloadCas,
    remote: &mut RemoteBodies,
    objects: &BTreeMap<String, u64>,
    requires: &mut std::collections::BTreeSet<(String, Option<u64>)>,
    hash: &str,
    expected_size: Option<u64>,
) -> StoreResult<()> {
    // Owner entries often share a payload; each body is confirmed once.
    let required = (hash.to_owned(), expected_size);
    if !requires.contains(&required) {
        require_object(cas, remote, objects, hash, expected_size)?;
        requires.insert(required);
    }
    Ok(())
}

fn require_object(
    cas: &PayloadCas,
    remote: &mut RemoteBodies,
    objects: &BTreeMap<String, u64>,
    hash: &str,
    expected_size: Option<u64>,
) -> StoreResult<()> {
    let catalog_size = objects
        .get(hash)
        .copied()
        .ok_or_else(|| validation("External snapshot has an incomplete payload reference"))?;
    if expected_size.is_some_and(|expected| expected != catalog_size)
        || match cas.stat_object(hash)? {
            Some(size) => Some(size),
            None => remote.stat(hash)?,
        } != Some(catalog_size)
    {
        return invalid("External snapshot payload size differs from its logical reference");
    }
    Ok(())
}

fn check(probe: &dyn CancellationProbe) -> StoreResult<()> {
    if probe.is_cancelled() {
        return invalid("External snapshot staging cancelled");
    }
    Ok(())
}

fn validation(message: impl Into<String>) -> StoreError {
    StoreError::Validation {
        message: message.into(),
    }
}

fn invalid<T>(message: impl Into<String>) -> StoreResult<T> {
    Err(validation(message))
}

#[cfg(test)]
#[path = "tests/external_apply_tests.rs"]
mod tests;
