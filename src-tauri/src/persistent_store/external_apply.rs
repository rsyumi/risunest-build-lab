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
    pub(crate) fn external_active_generation(&self) -> StoreResult<String> {
        active_generation(&self.connection)
    }

    pub(crate) fn external_revision_at_root(root: &Path) -> StoreResult<i64> {
        let path = root.join("persistent").join(super::DATABASE_FILE);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() || crate::trust_boundary::is_link_like(&metadata) {
            return invalid("Persistent database must be a regular file");
        }
        crate::trust_boundary::open_regular_source(&path)?;
        let connection = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        current_revision(&connection)
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
        let staged = (|| {
            let object_sizes = stage_objects(
                &self.repository_root,
                &root,
                content.as_ref(),
                objects,
                application.probe,
            )?;
            let record_hashes = stage_records(
                &mut self.connection,
                &self.repository_root,
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

/// One arriving record of a difference, read, checked against its catalog
/// entry and decoded before any write transaction.
pub(crate) struct DecodedRecord {
    key: String,
    content_hash: String,
    locator: LogicalRecordLocator,
    envelope: LogicalRecordEnvelope,
}

/// What applying a difference costs the active writer beyond its own
/// envelopes: the page and manifest bytes behind them, and the rows written,
/// rows deleted and bodies confirmed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DependentCost {
    pub bytes: u64,
    pub work: u64,
}

/// The records a difference moves, decoded, and what they would cost to apply
/// against the library as it stands.
pub(crate) struct DecodedDifference {
    records: Vec<DecodedRecord>,
    removed: Vec<(String, LogicalRecordLocator)>,
    pub cost: DependentCost,
}

struct PreparedRecord {
    locator: LogicalRecordLocator,
    envelope: LogicalRecordEnvelope,
    messages: Option<Vec<rows::SerializedMessage>>,
    /// Bodies the written rows refer to, with the length each must have.
    requires: std::collections::BTreeSet<(String, Option<u64>)>,
}

/// A receive small enough to change the active generation in place, with
/// every body it needs already read and decoded, so applying it only writes.
pub(crate) struct PreparedExternalDifference {
    /// Conversations follow every other record, as a complete stage defers
    /// one behind its parent.
    records: Vec<PreparedRecord>,
    removed: Vec<LogicalRecordLocator>,
    removed_keys: Vec<String>,
    arrived: Vec<(String, String)>,
    object_sizes: BTreeMap<String, u64>,
}

impl PersistentStore {
    /// Places the bodies a difference's records refer to and reports what every
    /// object in the snapshot is long, which is what resolving a record's
    /// dependencies needs. Writes into the content store rather than the
    /// library, so it belongs outside the apply transaction.
    pub(crate) fn stage_external_snapshot_objects<O>(
        &self,
        staging_root: &Path,
        objects: O,
        probe: &dyn CancellationProbe,
    ) -> StoreResult<BTreeMap<String, u64>>
    where
        O: IntoIterator<Item = StoreResult<ExternalSnapshotObject>>,
    {
        let root = verified_staging_root(staging_root)?;
        let content = ContentStore::open_existing(&root.join("external-storage"))?;
        stage_objects(&self.repository_root, &root, content.as_ref(), objects, probe)
    }

    /// Reads the records a difference moves and measures what applying them
    /// would cost, from the snapshot's catalog lengths and the rows the
    /// library holds under each key. No page or manifest is read. The counts
    /// describe the library as it is now; applying refuses any other revision.
    pub(crate) fn decode_external_snapshot_difference(
        &self,
        staging_root: &Path,
        records: &[ExternalSnapshotRecord],
        removed: &[String],
        catalog: &BTreeMap<String, u64>,
        probe: &dyn CancellationProbe,
    ) -> StoreResult<DecodedDifference> {
        let root = verified_staging_root(staging_root)?;
        let content = ContentStore::open_existing(&root.join("external-storage"))?;
        let generation = active_generation(&self.connection)?;
        let mut cost = DependentCost::default();
        let mut removals = Vec::with_capacity(removed.len());
        for key in removed {
            let locator = decode_logical_record_key(key)
                .map_err(|_| validation("External snapshot logical key is invalid"))?;
            if matches!(locator, LogicalRecordLocator::Root) {
                return invalid("External snapshot difference cannot remove the root record");
            }
            removals.push((key.clone(), locator));
        }
        let removed_characters: std::collections::BTreeSet<&str> = removals
            .iter()
            .filter_map(|(_, locator)| match locator {
                LogicalRecordLocator::Character { character_id } => Some(character_id.as_str()),
                _ => None,
            })
            .collect();
        for (_, locator) in &removals {
            check(probe)?;
            // A removed character's rows are counted once, with the character.
            let beneath = match locator {
                LogicalRecordLocator::Conversation { character_id, .. }
                    if removed_characters.contains(character_id.as_str()) =>
                {
                    0
                }
                _ => rows_beneath(&self.connection, &generation, locator, true)?,
            };
            cost.work = cost.work.saturating_add(1 + beneath);
        }
        let mut decoded = Vec::with_capacity(records.len());
        for record in records {
            check(probe)?;
            let locator = decode_logical_record_key(&record.key)
                .map_err(|_| validation("External snapshot logical key is invalid"))?;
            let envelope = read_record(&root, content.as_ref(), record, &locator, probe)?;
            let whole_character =
                matches!(envelope, LogicalRecordEnvelope::ArchivedCharacter { .. });
            let (bytes, work) = dependent_cost(catalog, &envelope)?;
            cost.bytes = cost.bytes.saturating_add(bytes);
            cost.work = cost.work.saturating_add(1 + work).saturating_add(rows_beneath(
                &self.connection,
                &generation,
                &locator,
                whole_character,
            )?);
            decoded.push(DecodedRecord {
                key: record.key.clone(),
                content_hash: record.content_hash.clone(),
                locator,
                envelope,
            });
        }
        Ok(DecodedDifference {
            records: decoded,
            removed: removals,
            cost,
        })
    }

    /// Resolves everything a decoded difference's rows are made from once its
    /// objects are staged: owner manifests, message pages and the fields the
    /// library keeps for itself. What it reads of the library is at the
    /// revision the apply will require.
    pub(crate) fn prepare_external_snapshot_difference(
        &self,
        decoded: DecodedDifference,
        object_sizes: BTreeMap<String, u64>,
        probe: &dyn CancellationProbe,
    ) -> StoreResult<PreparedExternalDifference> {
        let cas = PayloadCas::new(&self.repository_root)?;
        let generation = active_generation(&self.connection)?;
        let local_root = local_root(&self.connection, &generation)?;
        let arrived = decoded
            .records
            .iter()
            .map(|record| (record.key.clone(), record.content_hash.clone()))
            .collect();
        let (conversations, others): (Vec<_>, Vec<_>) = decoded
            .records
            .into_iter()
            .partition(|record| matches!(record.locator, LogicalRecordLocator::Conversation { .. }));
        let records = others
            .into_iter()
            .chain(conversations)
            .map(|record| {
                check(probe)?;
                prepare_record(
                    &self.connection,
                    &cas,
                    &generation,
                    &object_sizes,
                    record.locator,
                    record.envelope,
                    local_root.as_ref(),
                    probe,
                )
            })
            .collect::<StoreResult<Vec<_>>>()?;
        let (removed_keys, removed) = decoded.removed.into_iter().unzip();
        Ok(PreparedExternalDifference {
            records,
            removed,
            removed_keys,
            arrived,
            object_sizes,
        })
    }

    /// Applies what the snapshot changed to the generation the library is
    /// already using, so the records it left alone are neither rewritten nor
    /// read. Everything the receive owes - rows, revision, the change index the
    /// other backup consumers read, the connection's base and its record map,
    /// and the job's completion - is one transaction, and nothing in it reads
    /// or decodes a snapshot body.
    pub(crate) fn apply_external_snapshot_difference(
        &mut self,
        expected_revision: i64,
        difference: &PreparedExternalDifference,
        job: &str,
    ) -> StoreResult<super::RevisionResult> {
        let cas = PayloadCas::new(&self.repository_root)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actual_revision = current_revision(&tx)?;
        if actual_revision != expected_revision {
            return Err(StoreError::RevisionConflict {
                expected: expected_revision,
                actual: actual_revision,
            });
        }
        super::external_storage_state::begin_receive_activation(&tx, job)?;
        let generation = active_generation(&tx)?;
        let revision = actual_revision + 1;
        super::server_sync_outbox::begin_mutation(&tx, &generation, revision)?;
        super::content_change_index::begin_mutation(&tx, &generation, revision, "external")?;

        let mut touched = std::collections::BTreeSet::new();
        let mut confirmed = std::collections::BTreeSet::new();
        // A character is removed with everything under it, so its conversations
        // need no separate removal and the order between them does not matter.
        for locator in &difference.removed {
            note_touched_character(&mut touched, locator);
            rows::apply_delete(&tx, &generation, locator)?;
        }
        for record in &difference.records {
            note_touched_character(&mut touched, &record.locator);
            // Staged bodies are confirmed rather than trusted: nothing may
            // name one that is no longer there.
            for required in &record.requires {
                if confirmed.insert(required) {
                    require_object(&cas, &difference.object_sizes, &required.0, required.1)?;
                }
            }
            rows::apply_serialized_record(
                &tx,
                &generation,
                &record.locator,
                &record.envelope,
                record.messages.as_deref(),
            )?;
        }
        for character in touched {
            tx.execute("UPDATE characters SET conversation_count=(SELECT count(*) FROM conversations WHERE generation=?1 AND character_id=?2) WHERE generation=?1 AND character_id=?2",params![generation,character])?;
        }
        rows::validate_configured_index_uniqueness(&tx, &generation)?;
        let duplicate_plugin_ordinals: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM plugin_storage WHERE generation=?1 GROUP BY ordinal HAVING count(*)>1)",
            [&generation], |row| row.get(0),
        )?;
        if duplicate_plugin_ordinals {
            return invalid("External snapshot plugin records contain duplicate positions");
        }
        super::content_change_index::finish_mutation(&tx)?;
        super::server_sync_outbox::finish_mutation(&tx)?;
        super::commit::set_active(&tx, revision, &generation)?;
        // Read after the revision moves, so the base names what the library
        // now holds. The record map already describes the base this snapshot
        // follows, so only the keys that moved are written. The selected
        // adapter is deliberately not marked dirty: this content came from it.
        super::external_storage_state::finish_receive_activation(
            &tx,
            job,
            super::external_storage_state::BaseRecords::Moved {
                removed: &difference.removed_keys,
                arrived: &difference.arrived,
            },
        )?;
        tx.commit()?;
        Ok(super::RevisionResult { revision })
    }
}

/// The rows an apply deletes under a key besides the key's own: a
/// conversation's messages, or, when the whole character goes, every message
/// and conversation under it.
fn rows_beneath(
    connection: &rusqlite::Connection,
    generation: &str,
    locator: &LogicalRecordLocator,
    whole_character: bool,
) -> StoreResult<u64> {
    let count: i64 = match locator {
        LogicalRecordLocator::Character { character_id } if whole_character => connection
            .query_row(
                "SELECT (SELECT count(*) FROM messages WHERE generation=?1 AND character_id=?2)
                      + (SELECT count(*) FROM conversations WHERE generation=?1 AND character_id=?2)",
                params![generation, character_id],
                |row| row.get(0),
            )?,
        LogicalRecordLocator::Conversation {
            character_id,
            conversation_id,
        } => connection.query_row(
            "SELECT count(*) FROM messages
             WHERE generation=?1 AND character_id=?2 AND conversation_id=?3",
            params![generation, character_id, conversation_id],
            |row| row.get(0),
        )?,
        _ => 0,
    };
    Ok(count as u64)
}

/// What confirming one body inside the apply costs, in message rows: a checked
/// `stat` of a library object against a row insert.
pub(crate) const PRESENCE_CHECK_WORK: u64 = 128;

/// The page and manifest bytes an envelope's rows are made from, and the
/// message rows and body checks they add, from catalog lengths alone. A page
/// holds at most `LOGICAL_MESSAGE_PAGE_SIZE` messages and a manifest names at
/// most `entry_count` bodies, so both are counted at that bound.
fn dependent_cost(
    catalog: &BTreeMap<String, u64>,
    envelope: &LogicalRecordEnvelope,
) -> StoreResult<(u64, u64)> {
    let length = |hash: &str| {
        catalog
            .get(hash)
            .copied()
            .ok_or_else(|| validation("External snapshot has an incomplete payload reference"))
    };
    let checks = |count: u64| count.saturating_mul(PRESENCE_CHECK_WORK);
    let owners = |heads: &[crate::logical_records::LogicalOwnerHead]| -> StoreResult<(u64, u64)> {
        let mut cost = (0u64, 0u64);
        for head in heads {
            if let Some(hash) = &head.manifest_hash {
                cost.0 = cost.0.saturating_add(length(hash)?);
                cost.1 = cost.1.saturating_add(checks(head.entry_count.saturating_add(1)));
            }
        }
        Ok(cost)
    };
    match envelope {
        LogicalRecordEnvelope::Root { owner_heads, .. }
        | LogicalRecordEnvelope::Character { owner_heads, .. } => owners(owner_heads),
        LogicalRecordEnvelope::ArchivedCharacter {
            asset_hashes,
            owner_heads,
            ..
        } => {
            let (bytes, work) = owners(owner_heads)?;
            Ok((bytes, work.saturating_add(checks(asset_hashes.len() as u64 + 1))))
        }
        LogicalRecordEnvelope::Conversation {
            message_page_hashes,
            ..
        } => {
            let mut bytes = 0u64;
            for hash in message_page_hashes {
                bytes = bytes.saturating_add(length(hash)?);
            }
            Ok((
                bytes,
                (message_page_hashes.len() as u64)
                    .saturating_mul(crate::logical_records::LOGICAL_MESSAGE_PAGE_SIZE as u64),
            ))
        }
        LogicalRecordEnvelope::Asset { .. } | LogicalRecordEnvelope::Inlay { .. } => {
            Ok((0, checks(1)))
        }
        _ => Ok((0, 0)),
    }
}

fn note_touched_character(
    touched: &mut std::collections::BTreeSet<String>,
    locator: &LogicalRecordLocator,
) {
    if let LogicalRecordLocator::Character { character_id }
    | LogicalRecordLocator::Conversation { character_id, .. } = locator
    {
        touched.insert(character_id.clone());
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
        ObjectSource::Unchanged => {
            return invalid("External snapshot record body was not fetched");
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
            let path = cas
                .object_path(&object.content_hash)?
                .ok_or_else(|| validation("External snapshot library body is missing"))?;
            let file = crate::trust_boundary::open_regular_source(&path)?;
            if file.metadata()?.len() != object.byte_length {
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
    /// batch rather than at activation. Cancellation is only asked before the
    /// transaction begins, never while it commits.
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
        for write in self.writes.drain(..) {
            match write {
                StageWrite::Record {
                    record,
                    message_count,
                } => rows::apply_serialized_record_rows(
                    &transaction,
                    generation,
                    &record.locator,
                    &record.envelope,
                    message_count,
                )?,
                StageWrite::Messages {
                    character_id,
                    conversation_id,
                    first,
                    messages,
                } => rows::insert_serialized_messages(
                    &transaction,
                    generation,
                    &character_id,
                    &conversation_id,
                    first,
                    &messages,
                )?,
            }
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
    let mut saw_root = false;
    let mut batch = StageBatch {
        writes: Vec::new(),
        records: 0,
        work: 0,
        bytes: 0,
        written: 0,
    };
    let stage = |connection: &mut rusqlite::Connection,
                     batch: &mut StageBatch,
                     record: ExternalSnapshotRecord,
                     locator: LogicalRecordLocator|
     -> StoreResult<()> {
        check(probe)?;
        let envelope = read_record(staging_root, content, &record, &locator, probe)?;
        let (mut bytes, _) = dependent_cost(objects, &envelope)?;
        // A conversation's pages are counted message by message as they join
        // a batch.
        if matches!(locator, LogicalRecordLocator::Conversation { .. }) {
            bytes = 0;
        }
        let prepared = prepare_record(
            connection,
            &cas,
            &active,
            objects,
            locator,
            envelope,
            local_root.as_ref(),
            probe,
        )?;
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
fn prepare_record(
    connection: &rusqlite::Connection,
    cas: &PayloadCas,
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
    let messages = resolve_dependencies(cas, objects, &mut envelope, &mut requires, probe)?
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
        requires,
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
    objects: &BTreeMap<String, u64>,
    envelope: &mut LogicalRecordEnvelope,
    requires: &mut std::collections::BTreeSet<(String, Option<u64>)>,
    probe: &dyn CancellationProbe,
) -> StoreResult<Option<Vec<serde_json::Value>>> {
    match envelope {
        LogicalRecordEnvelope::Root { value, owner_heads } => {
            let resolved = resolve_owner_heads(cas, objects, owner_heads, requires, probe)?;
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
            let resolved = resolve_owner_heads(cas, objects, owner_heads, requires, probe)?;
            rows::rehydrate_character_owner(detail, &character_id, &resolved)?;
            Ok(None)
        }
        LogicalRecordEnvelope::ArchivedCharacter {
            archive_object_hash,
            archive_object_size,
            asset_hashes,
            owner_heads,
            ..
        } => {
            require(
                cas,
                objects,
                requires,
                archive_object_hash,
                Some(*archive_object_size),
            )?;
            for hash in asset_hashes {
                check(probe)?;
                require(cas, objects, requires, hash, None)?;
            }
            resolve_owner_heads(cas, objects, owner_heads, requires, probe)?;
            Ok(None)
        }
        LogicalRecordEnvelope::Conversation {
            message_page_hashes,
            ..
        } => {
            let mut messages = Vec::new();
            for hash in message_page_hashes.iter() {
                check(probe)?;
                require_object(cas, objects, hash, None)?;
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
            require(cas, objects, requires, hash, Some(*size))?;
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
    objects: &BTreeMap<String, u64>,
    heads: &[crate::logical_records::LogicalOwnerHead],
    requires: &mut std::collections::BTreeSet<(String, Option<u64>)>,
    probe: &dyn CancellationProbe,
) -> StoreResult<Vec<rows::ResolvedOwnerHead>> {
    let mut resolved = Vec::with_capacity(heads.len());
    for head in heads {
        check(probe)?;
        let tuples = if let Some(hash) = &head.manifest_hash {
            require(cas, objects, requires, hash, None)?;
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
                    require(cas, objects, requires, &hex::encode(hash), None)?;
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
    objects: &BTreeMap<String, u64>,
    requires: &mut std::collections::BTreeSet<(String, Option<u64>)>,
    hash: &str,
    expected_size: Option<u64>,
) -> StoreResult<()> {
    // Owner entries often share a payload; each body is confirmed once.
    let required = (hash.to_owned(), expected_size);
    if !requires.contains(&required) {
        require_object(cas, objects, hash, expected_size)?;
        requires.insert(required);
    }
    Ok(())
}

fn require_object(
    cas: &PayloadCas,
    objects: &BTreeMap<String, u64>,
    hash: &str,
    expected_size: Option<u64>,
) -> StoreResult<()> {
    let catalog_size = objects
        .get(hash)
        .copied()
        .ok_or_else(|| validation("External snapshot has an incomplete payload reference"))?;
    if expected_size.is_some_and(|expected| expected != catalog_size)
        || cas.stat_object(hash)? != Some(catalog_size)
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
