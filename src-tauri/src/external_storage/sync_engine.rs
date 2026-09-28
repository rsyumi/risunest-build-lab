//! Bounded sync decision and publication orchestration.
//!
//! The decision is pure: callers obtain one authenticated remote head and one
//! short-lived PDS snapshot, then perform the selected network or activation
//! stage. No branch silently changes a repository's fixed publication strategy.
use super::{
    connection_commands::ConnectedRepository,
    contract::{Cancellation, ErrorKind, ProviderError, Result, VersionToken},
    control::{self, HeadDocument, ObservedHead, PublicationResult},
    job_store::{DurableJob, JobKind, JobStore},
    journal::{JobIdentity, TransferJournal},
    packaging::{
        CatalogRoot, CompletedSnapshot, PackageLimits, SnapshotMetadata, SnapshotPurpose,
    },
    publication::HeadObservation,
};
use crate::persistent_store::{
    device_store::Section as PdsSection,
    external_apply::{ExternalSnapshotApplication, ExternalSnapshotObject, ExternalSnapshotRecord},
    external_conflicts::{
        self, ExternalConflictRecord, PreservedHeadObservation, PreservedRemoteState,
    },
    external_runtime::ExternalBase,
    sync_selection::CaptureIdentity,
    PersistentStore, PreparedReplaceCommit,
};
use std::collections::BTreeMap;

use risunest_external_storage_format::{
    format::{fingerprint, library_fingerprint_domain, Descriptor},
    section::SectionKind,
};
use risunest_sync_wire::head::Sequence;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

type ReceiveParticipation = Vec<(PdsSection, bool, Sequence)>;

pub(crate) struct PreparedReceive {
    job_id: String,
    connection_id: String,
    snapshot_id: String,
    expected: CaptureIdentity,
    authenticated_head: String,
    apply: PreparedApply,
    /// What the snapshot names under each logical key, checked against the
    /// catalog's own fingerprint before either branch is prepared.
    records: std::collections::BTreeMap<String, String>,
    participation: ReceiveParticipation,
    sections: Vec<super::sections::PreparedSectionInput>,
}

/// How the receive will reach the library.
enum PreparedApply {
    /// The whole library was rebuilt in an inactive generation, which is what a
    /// receive too large to hold the active database open has to do.
    Replace(PreparedReplaceCommit),
    /// Only what the snapshot moved reaches the generation already in use.
    Difference(PreparedDifference),
}

struct PreparedDifference {
    expected_revision: i64,
    prepared: crate::persistent_store::external_apply::PreparedExternalDifference,
}

impl PreparedApply {
    fn staging_id(&self) -> Option<&str> {
        match self {
            Self::Replace(commit) => Some(commit.external_staging_id()),
            Self::Difference(_) => None,
        }
    }
}
impl PreparedReceive {
    fn ready_result(&self) -> Value {
        json!({"receiveReady":true,"snapshotId":self.snapshot_id,
            "expectedRevision":self.expected.revision.to_string()})
    }
}

pub(crate) fn prepared_receive_result(app: &AppHandle, job: &str) -> Result<Option<Value>> {
    let state = app.state::<super::job_store::JobCommandState>();
    let prepared = state.prepared_receives.lock().map_err(local_error)?;
    Ok(prepared.get(job).map(PreparedReceive::ready_result))
}

pub(crate) fn discard_receive_preparation(app: &AppHandle, id: &str) -> Result<()> {
    let state = app.state::<super::job_store::JobCommandState>();
    let prepared = state.prepared_receives.lock().map_err(local_error)?.remove(id);
    let jobs = JobStore::open(&super::runtime::root(app)?)?;
    let mut job = jobs.read(id)?;
    let staging_id = prepared.as_ref().and_then(|entry| entry.apply.staging_id())
        .or(job.receive_staging_id.as_deref());
    if let Some(staging_id) = staging_id {
        pds(app)?.replace_abort(staging_id).map_err(local_error)?;
    }
    if job.receive_staging_id.take().is_some() {
        jobs.put(&job)?;
    }
    Ok(())
}

fn require_exact_receive_identity(expected: &CaptureIdentity, current: &CaptureIdentity) -> Result<()> {
    if expected != current {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    Ok(())
}

fn receive_participation(store: &mut PersistentStore) -> Result<ReceiveParticipation> {
    let device = store.device_store_mut().map_err(local_error)?;
    [PdsSection::Hypa, PdsSection::LocalPlugins].into_iter().map(|section| {
        let state = device.section_state(section).map_err(local_error)?;
        Ok((section, state.participating, state.participation_generation))
    }).collect()
}

fn require_receive_participation(store: &mut PersistentStore, expected: &ReceiveParticipation) -> Result<()> {
    if receive_participation(store)? != *expected {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    Ok(())
}

fn wanted_receive_sections(participation: &ReceiveParticipation) -> std::collections::BTreeSet<String> {
    participation.iter().filter_map(|(section, enabled, _)| {
        if !enabled { return None; }
        match section {
            PdsSection::Hypa => Some(SectionKind::Hypa.id().to_owned()),
            PdsSection::LocalPlugins => Some(SectionKind::LocalPlugins.id().to_owned()),
        }
    }).collect()
}

fn prepare_receive_sections(
    connection_id: &str,
    library_lineage: &str,
    participation: &ReceiveParticipation,
    sections: Vec<super::sections::CapturedSection>,
    cancel: &Cancellation,
) -> Result<Vec<super::sections::PreparedSectionInput>> {
    let mut seen = std::collections::BTreeSet::new();
    sections.into_iter().map(|section| {
        cancel.check()?;
        if !seen.insert(section.kind.id().to_owned()) {
            return Err(corrupt("received section is duplicated"));
        }
        let device_section = super::sections::section_of(section.kind)
            .ok_or_else(|| corrupt("device-fixed section in a synchronized state"))?;
        let (_, participating, generation) = participation.iter()
            .find(|(candidate, _, _)| *candidate == device_section)
            .ok_or_else(|| corrupt("received section has no participation identity"))?;
        if !participating {
            return Err(corrupt("received section is not participating"));
        }
        super::sections::prepare_received_section(
            connection_id,
            library_lineage,
            super::sections::SectionArrival::Continuing,
            generation,
            &section,
            cancel,
        )
    }).collect()
}

/// Past this, a receive stops being the normal one-message case and holds the
/// active database longer than a user would sit through, so it is staged in
/// bounded batches and activated instead. Measured on a library of 20,000
/// characters, the in-place hold is about 7 ms for one changed record and
/// 74 ms for 1,000, about 7.5 us per message row written or deleted (247 ms
/// for 32,768), and about 1 ms per body confirmed (`PRESENCE_CHECK_WORK` rows
/// each). A work unit is one row, so 65,536 units and 1,024 records bound the
/// hold near 0.6 s. The byte figures bound what preparation reads and keeps
/// in memory until the apply.
const RECEIVE_DIFFERENCE_BUDGET: super::receive_difference::DifferenceBudget =
    super::receive_difference::DifferenceBudget {
        records: 1024,
        bytes: 16 * 1024 * 1024,
        dependent_bytes: 32 * 1024 * 1024,
        work: 65_536,
    };

/// What the incoming snapshot moves against what the library already holds,
/// when the base still describes it and the move is small enough to apply in
/// place. `None` is the answer whenever there is any doubt about either.
fn receive_difference_plan(
    store: &PersistentStore,
    connection: &str,
    downloaded: &super::snapshot_restore::PreparedRemoteSnapshot,
    budget: super::receive_difference::DifferenceBudget,
) -> Result<Option<super::receive_difference::ReceiveDifference>> {
    use super::receive_difference::{difference, LocalRecord, RemoteRecord};
    let Some(view) = store.external_base_records(connection).map_err(local_error)? else {
        return Ok(None);
    };
    let computed = difference(
        downloaded.records.iter().map(|record| RemoteRecord {
            key: record.key.clone(),
            content_hash: record.content_hash.clone(),
            byte_length: record.byte_length,
        }),
        view.into_iter()
            .map(|(key, content_hash)| LocalRecord { key, content_hash }),
    )?;
    Ok(computed
        .within(budget)
        .then_some(computed))
}

#[allow(clippy::too_many_arguments)]
fn prepare_receive_input(
    store: &mut PersistentStore,
    job: &DurableJob,
    expected: CaptureIdentity,
    authenticated_head: String,
    downloaded: super::snapshot_restore::PreparedRemoteSnapshot,
    sections: Vec<super::sections::CapturedSection>,
    participation: ReceiveParticipation,
    prepared: &super::phase_progress::PhaseProgress,
    cancel: &Cancellation,
) -> Result<Preparation> {
    prepare_receive_within(store, job, expected, authenticated_head, downloaded, sections,
        participation, prepared, cancel, RECEIVE_DIFFERENCE_BUDGET)
}

fn unfetched(source: &super::content_store::ObjectSource) -> bool {
    matches!(source, super::content_store::ObjectSource::Unchanged)
}

/// What preparing a downloaded receive produced.
enum Preparation {
    Ready(PreparedReceive),
    /// The download left the records its base already holds unfetched, and
    /// the receive turned out to need them. Nothing was staged; a download
    /// that fetches every record has to come first.
    NeedsRecords,
}

#[cfg(test)]
impl Preparation {
    fn ready(self) -> PreparedReceive {
        match self {
            Preparation::Ready(prepared) => prepared,
            Preparation::NeedsRecords => panic!("the receive needs records it did not fetch"),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_receive_within(
    store: &mut PersistentStore,
    job: &DurableJob,
    expected: CaptureIdentity,
    authenticated_head: String,
    downloaded: super::snapshot_restore::PreparedRemoteSnapshot,
    sections: Vec<super::sections::CapturedSection>,
    participation: ReceiveParticipation,
    prepared: &super::phase_progress::PhaseProgress,
    cancel: &Cancellation,
    budget: super::receive_difference::DifferenceBudget,
) -> Result<Preparation> {
    require_exact_receive_identity(&expected, &store.external_identity().map_err(local_error)?)?;
    store.external_validate_receive(&job.id, &job.request.connection_id, &expected, &authenticated_head)
        .map_err(|_| ProviderError::new(ErrorKind::PreconditionFailed))?;
    require_receive_participation(store, &participation)?;
    let intent = store.external_job(&job.id).map_err(local_error)?
        .ok_or_else(|| ProviderError::new(ErrorKind::PreconditionFailed))?;
    if intent.capture_id != downloaded.snapshot_id || intent.repository_id != downloaded.repository_id {
        return Err(corrupt("received snapshot binding differs"));
    }
    let wanted = wanted_receive_sections(&participation);
    if sections.iter().any(|section| !wanted.contains(section.kind.id())) {
        return Err(corrupt("received section is not participating"));
    }
    let scope_id = library_fingerprint_domain();
    let expected_fingerprint: [u8; 32] = hex::decode(&downloaded.library_fingerprint)
        .ok().and_then(|value| value.try_into().ok())
        .ok_or_else(|| corrupt("invalid received fingerprint"))?;
    let probe = super::runtime::CancelProbe(cancel.clone());
    // A store helper stopped by the probe reports a validation failure; the
    // job reports it as the cancellation it was.
    let stopped = |error: ProviderError| {
        if cancel.check().is_err() { ProviderError::new(ErrorKind::Cancelled) } else { error }
    };
    let mut hashes = BTreeMap::new();
    let mut record_map = BTreeMap::new();
    for record in &downloaded.records {
        let hash: [u8; 32] = hex::decode(&record.content_hash)
            .ok().and_then(|value| value.try_into().ok())
            .ok_or_else(|| corrupt("invalid received record hash"))?;
        if hashes.insert(record.key.clone(), hash).is_some() {
            return Err(corrupt("received snapshot names a record twice"));
        }
        record_map.insert(record.key.clone(), record.content_hash.clone());
    }
    // A difference is only as good as the catalog it is taken against, and a
    // complete stage would not have accepted this one either.
    if fingerprint(&scope_id, &hashes) != expected_fingerprint {
        return Err(corrupt("received snapshot differs from its catalog fingerprint"));
    }
    cancel.check()?;
    let difference = receive_difference_plan(store, &job.request.connection_id, &downloaded, budget)?;
    // A few small keys can stand for a great deal: a conversation's pages, an
    // owner's manifest, a character's whole history. What they bring is
    // measured from the catalog before any of it is staged or read.
    let decoded = match difference {
        Some(difference) => {
            let arriving: std::collections::BTreeSet<&str> = difference.added.iter()
                .chain(difference.changed.iter()).map(String::as_str).collect();
            let records: Vec<ExternalSnapshotRecord> = downloaded.records.iter()
                .filter(|record| arriving.contains(record.key.as_str()))
                .map(|record| ExternalSnapshotRecord {
                    key: record.key.clone(), content_hash: record.content_hash.clone(),
                    byte_length: record.byte_length, source: record.source.clone(),
                })
                .collect();
            // A record the download left unfetched cannot arrive in place.
            if records.iter().any(|record| unfetched(&record.source)) {
                None
            } else {
                let catalog: BTreeMap<String, u64> = downloaded.objects.iter()
                    .map(|object| (object.content_hash.clone(), object.byte_length))
                    .collect();
                let decoded = store
                    .decode_external_snapshot_difference(
                        &downloaded.staging_root, &records, &difference.removed, &catalog, &probe,
                    )
                    .map_err(|error| stopped(receive_validation_error(error)))?;
                budget
                    .admits_dependents(decoded.cost.bytes, decoded.cost.work)
                    .then_some((difference, records, decoded))
            }
        }
        None => None,
    };
    // A replacement stages every record, including the ones the download
    // skipped because a difference would not have needed them.
    if decoded.is_none()
        && downloaded.records.iter().any(|record| unfetched(&record.source))
    {
        return Ok(Preparation::NeedsRecords);
    }
    let sections = prepare_receive_sections(
        &job.request.connection_id,
        &expected.library_epoch,
        &participation,
        sections,
        cancel,
    )?;
    let apply = match decoded {
        Some((difference, records, decoded)) => {
            let object_items = downloaded.objects.len() as u64;
            let object_bytes: u64 =
                downloaded.objects.iter().map(|object| object.byte_length).sum();
            // What the receive moves: every body it has to stage, and the
            // record rows the apply will write or drop. A body the library
            // already holds is confirmed rather than written, which is why a
            // difference plans so much less than a replacement.
            prepared.plan(
                object_items + records.len() as u64 + difference.removed.len() as u64,
                object_bytes + records.iter().map(|record| record.byte_length).sum::<u64>(),
            );
            for record in &records {
                prepared.completed(record.byte_length);
            }
            for _ in &difference.removed {
                prepared.completed(0);
            }
            let objects = downloaded.objects.into_iter().map(|object| {
                prepared.completed(object.byte_length);
                Ok(ExternalSnapshotObject {
                    content_hash: object.content_hash, byte_length: object.byte_length,
                    source: object.source,
                })
            });
            let object_sizes = store
                .stage_external_snapshot_objects(&downloaded.staging_root, objects, &probe)
                .map_err(|error| stopped(local_error(error)))?;
            PreparedApply::Difference(PreparedDifference {
                expected_revision: expected.revision,
                prepared: store
                    .prepare_external_snapshot_difference(decoded, object_sizes, &probe)
                    .map_err(|error| stopped(receive_validation_error(error)))?,
            })
        }
        None => {
            let application = ExternalSnapshotApplication {
                expected_revision: expected.revision,
                staging_root: &downloaded.staging_root,
                scope_id: &scope_id,
                fingerprint: &expected_fingerprint,
                probe: &probe,
            };
            // A replacement stages every record as well, which is the whole
            // reason it costs what it does.
            prepared.plan(
                (downloaded.records.len() + downloaded.objects.len()) as u64,
                downloaded.records.iter().map(|record| record.byte_length).sum::<u64>()
                    + downloaded.objects.iter().map(|object| object.byte_length).sum::<u64>(),
            );
            let records = downloaded.records.into_iter().map(|record| {
                prepared.completed(record.byte_length);
                Ok(ExternalSnapshotRecord {
                    key: record.key, content_hash: record.content_hash,
                    byte_length: record.byte_length, source: record.source,
                })
            });
            let objects = downloaded.objects.into_iter().map(|object| {
                prepared.completed(object.byte_length);
                Ok(ExternalSnapshotObject {
                    content_hash: object.content_hash, byte_length: object.byte_length,
                    source: object.source,
                })
            });
            PreparedApply::Replace(
                store.prepare_external_snapshot_application(&application, records, objects)
                    .map_err(|error| stopped(local_error(error)))?,
            )
        }
    };
    prepared.flush();
    Ok(Preparation::Ready(PreparedReceive {
        job_id: job.id.clone(), connection_id: job.request.connection_id.clone(),
        snapshot_id: downloaded.snapshot_id, expected, authenticated_head, apply,
        records: record_map, participation, sections,
    }))
}

fn receive_validation_error(error: crate::persistent_store::StoreError) -> ProviderError {
    match error {
        crate::persistent_store::StoreError::Store { .. } => local_error(error),
        _ => ProviderError::new(ErrorKind::PreconditionFailed),
    }
}

// No transport or credentials enter the activation boundary. A failed attempt
// retains the original staged generation and section inputs for the same job.
fn activate_prepared_receive(
    store: &mut PersistentStore,
    prepared: &PreparedReceive,
    current: &CaptureIdentity,
) -> Result<i64> {
    let result = (|| {
        require_exact_receive_identity(&prepared.expected, current)?;
        store.external_validate_receive(&prepared.job_id, &prepared.connection_id,
            &prepared.expected, &prepared.authenticated_head)
            .map_err(receive_validation_error)?;
        require_receive_participation(store, &prepared.participation)?;
        match &prepared.apply {
            PreparedApply::Replace(commit) => {
                // This only revalidates the existing SQL stage; it neither
                // downloads nor materializes the library and never rebases the
                // expected revision.
                let commit = store
                    .prepare_replace_commit(commit.external_staging_id(),
                        Some(prepared.expected.revision))
                    .map_err(receive_validation_error)?;
                for section in &prepared.sections {
                    super::sections::apply_prepared_section(store, section)?;
                }
                store.finish_external_receive(commit, &prepared.job_id, &prepared.records)
                    .map(|result| result.revision).map_err(local_error)
            }
            PreparedApply::Difference(difference) => {
                for section in &prepared.sections {
                    super::sections::apply_prepared_section(store, section)?;
                }
                store.apply_external_snapshot_difference(
                    difference.expected_revision,
                    &difference.prepared,
                    &prepared.job_id,
                )
                .map(|result| result.revision)
                .map_err(receive_validation_error)
            }
        }
    })();
    if let Err(error) = &result {
        if let Some(completed) = store.external_receive_completion(&prepared.job_id, &prepared.connection_id)
            .map_err(local_error)?
        {
            return Ok(completed.revision);
        }
        if matches!(error.kind, ErrorKind::PreconditionFailed | ErrorKind::Corrupt) {
            // A difference leaves nothing inactive behind, so there is nothing
            // to remove and its records stay ready for the next attempt.
            if prepared.apply.staging_id().is_some_and(|staging_id| {
                store.replace_abort(staging_id).is_err()
            }) {
                crate::nlog!("error", "Rejected external receive stage could not be removed");
            }
        }
    }
    result
}

fn corrupt(_: impl std::fmt::Display) -> ProviderError {
    ProviderError::new(ErrorKind::Corrupt)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredHeadObservation {
    commit_id: String,
    authenticated_body_hash: String,
    version: Option<VersionToken>,
}
impl From<&HeadObservation> for StoredHeadObservation {
    fn from(value: &HeadObservation) -> Self {
        Self {
            commit_id: value.commit_id.clone(),
            authenticated_body_hash: value.authenticated_body_hash.clone(),
            version: value.version.clone(),
        }
    }
}

pub(crate) fn observation_json(value: &HeadObservation) -> Result<String> {
    if value.commit_id.is_empty()
        || !crate::trust_boundary::is_lower_hex_256(&value.authenticated_body_hash)
        || value
            .version
            .as_ref()
            .is_some_and(|version| version.0.is_empty())
    {
        return Err(corrupt("invalid authenticated head observation"));
    }
    serde_json::to_string(&StoredHeadObservation::from(value)).map_err(corrupt)
}
fn parse_observation(value: &str) -> Result<StoredHeadObservation> {
    if value.is_empty() || value.len() > 16 * 1024 {
        return Err(corrupt("invalid stored head observation"));
    }
    let parsed: StoredHeadObservation = serde_json::from_str(value).map_err(corrupt)?;
    if parsed.commit_id.is_empty()
        || !crate::trust_boundary::is_lower_hex_256(&parsed.authenticated_body_hash)
        || parsed
            .version
            .as_ref()
            .is_some_and(|version| version.0.is_empty())
        || serde_json::to_string(&parsed).map_err(corrupt)? != value
    {
        return Err(corrupt("invalid stored head observation"));
    }
    Ok(parsed)
}

pub(crate) struct SyncInputs<'a> {
    pub descriptor: &'a Descriptor,
    pub connection_id: &'a str,
    pub current_identity: &'a CaptureIdentity,
    pub local_pristine: bool,
    /// Required once the local side is known to differ from its base.
    pub local_fingerprint: Option<&'a str>,
    /// A participating section holds a write this remote has not seen. Device
    /// values move without the library moving, so they need their own signal.
    pub local_sections_changed: bool,
    pub base: Option<&'a ExternalBase>,
    pub remote: Option<&'a ObservedHead>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SyncAction {
    UpToDate,
    PublishLocal {
        expected: Option<ObservedHead>,
    },
    ReceiveRemote {
        remote: ObservedHead,
    },
    /// The local contents already equal the new remote snapshot. Only the PDS
    /// base and current provider version need to advance.
    AcceptEquivalent {
        remote: ObservedHead,
    },
    PreserveConflict {
        remote: ObservedHead,
    },
    FirstAttachDecision {
        remote: ObservedHead,
    },
    DecisionRequired,
    RecoveryRequired,
}

/// The head this device agreed to, checked against the base row carrying it.
fn stored_observation(base: &ExternalBase) -> Result<StoredHeadObservation> {
    let stored = parse_observation(&base.head_observation)?;
    if stored.commit_id != base.commit_id {
        return Err(corrupt("stored base and head observation differ"));
    }
    Ok(stored)
}

/// Whether the remote carries other content than the base. A version token can
/// move without the content moving, so only the commit and the body it
/// authenticates count.
fn remote_content_differs(stored: &StoredHeadObservation, remote: &ObservedHead) -> bool {
    stored.commit_id != remote.observation.commit_id
        || stored.authenticated_body_hash != remote.observation.authenticated_body_hash
}

fn same_local_lineage(a: &CaptureIdentity, b: &CaptureIdentity) -> bool {
    a.store_id == b.store_id
        && a.library_epoch == b.library_epoch
        && a.generation == b.generation
        && a.selection_epoch == b.selection_epoch
}

pub(crate) fn decide_sync(input: SyncInputs<'_>) -> Result<SyncAction> {
    input.descriptor.validate().map_err(corrupt)?;
    if input.connection_id.is_empty() {
        return Err(corrupt("missing sync connection"));
    }
    if let Some(remote) = input.remote {
        if remote.document.repository_id != input.descriptor.repository_id {
            return Err(corrupt("remote head belongs to another repository"));
        }
    }
    let Some(base) = input.base else {
        return Ok(match input.remote {
            Some(remote) if input.local_pristine => SyncAction::ReceiveRemote {
                remote: remote.clone(),
            },
            Some(remote) => SyncAction::FirstAttachDecision {
                remote: remote.clone(),
            },
            None => SyncAction::PublishLocal { expected: None },
        });
    };
    if base.repository_id != input.descriptor.repository_id {
        return Ok(SyncAction::DecisionRequired);
    }
    if !same_local_lineage(input.current_identity, &base.identity)
        || input.current_identity.revision < base.identity.revision
    {
        return Ok(SyncAction::DecisionRequired);
    }
    let Some(remote) = input.remote else {
        return Ok(SyncAction::RecoveryRequired);
    };
    let stored = stored_observation(base)?;
    let local_changed = input.current_identity.revision != base.identity.revision;
    let remote_content_changed = remote_content_differs(&stored, remote);
    let remote_version_changed = stored.version != remote.observation.version;

    match (local_changed, remote_content_changed) {
        (false, false) if input.local_sections_changed => Ok(SyncAction::PublishLocal {
            expected: Some(remote.clone()),
        }),
        (false, false) if remote_version_changed => Ok(SyncAction::AcceptEquivalent {
            remote: remote.clone(),
        }),
        (false, false) => Ok(SyncAction::UpToDate),
        (false, true) => Ok(SyncAction::ReceiveRemote {
            remote: remote.clone(),
        }),
        (true, false) => Ok(SyncAction::PublishLocal {
            expected: Some(remote.clone()),
        }),
        (true, true) => {
            let local = input
                .local_fingerprint
                .ok_or_else(|| corrupt("local fingerprint was not captured"))?;
            if !crate::trust_boundary::is_lower_hex_256(local) {
                return Err(corrupt("invalid local fingerprint"));
            }
            Ok(SyncAction::PreserveConflict {
                remote: remote.clone(),
            })
        }
    }
}

fn local_error(_: impl std::fmt::Display) -> ProviderError {
    ProviderError::new(ErrorKind::Transient)
}

fn pds(app: &AppHandle) -> Result<crate::persistent_store::PersistentStore> {
    super::runtime::native_store(app)
}

async fn capture_for_publication(
    app: &AppHandle,
    connected: &ConnectedRepository,
    job: &DurableJob,
    expected: Option<&ObservedHead>,
    cancel: &Cancellation,
) -> Result<(
    crate::persistent_store::external_capture::CapturedSnapshot,
    [u8; 32],
)> {
    let worker_app = app.clone();
    let worker_job = job.clone();
    let repository_id = connected.stored.descriptor.repository_id.clone();
    let strategy = connected
        .stored
        .descriptor
        .publication_strategy
        .ok_or_else(|| ProviderError::new(ErrorKind::Unsupported))?;
    let expected_head = expected
        .map(|head| observation_json(&head.observation))
        .transpose()?;
    let cancel = cancel.clone();
    tokio::task::spawn_blocking(move || {
        let probe = super::runtime::CancelProbe(cancel);
        let mut store = pds(&worker_app)?;
        if let Some(existing) = store
            .external_jobs(&worker_job.request.connection_id)
            .map_err(local_error)?
            .into_iter()
            .find(|item| item.id == worker_job.id)
        {
            if existing.phase != "ready" {
                return Err(ProviderError::new(ErrorKind::PreconditionFailed));
            }
            let admission = worker_app
                .state::<crate::native_file_jobs::NativeFileJobState>()
                .admission
                .clone();
            let _permit = admission.file(true).map_err(local_error)?;
            super::runtime::read_job_session(&worker_app, &worker_job.id)?;
            super::runtime::require_admitted_library(
                &worker_job,
                &store.external_identity().map_err(local_error)?,
            )?;
            let capture = store
                .reopen_external_capture(&existing.capture_id)
                .map_err(local_error)?;
            let fingerprint = capture
                .catalog
                .content_fingerprint(&library_fingerprint_domain())
                .map_err(local_error)?;
            return Ok((capture, fingerprint));
        }
        let hydration = store
            .hydrate_external_capture_dependencies(
                &worker_job.request.connection_id,
                &probe,
            )
            .map_err(local_error)?;
        let admission = worker_app
            .state::<crate::native_file_jobs::NativeFileJobState>()
            .admission
            .clone();
        let _permit = admission.file(true).map_err(local_error)?;
        let publication_permit =
            super::runtime::publication_permit(&worker_app, &worker_job.id)?;
        super::runtime::require_admitted_library(
            &worker_job,
            &store.external_identity().map_err(local_error)?,
        )?;
        let capture = store
            .capture_external_library(
                &worker_job.request.connection_id,
                &hydration,
                &probe,
            )
            .map_err(local_error)?;
        let intent = crate::persistent_store::external_storage_state::PublishIntent {
            job_id: &worker_job.id,
            connection_id: &worker_job.request.connection_id,
            repository_id: &repository_id,
            capture_id: &capture.id,
            identity: &capture.identity,
            strategy: match strategy {
                risunest_external_storage_format::format::Strategy::Cas => "cas",
                risunest_external_storage_format::format::Strategy::Sequential => "sequential",
            },
            expected_head: expected_head.as_deref(),
            commit_id: &worker_job.id,
        };
        store
            .external_prepare_publication(&intent, &publication_permit)
            .map_err(local_error)?;
        let jobs = JobStore::open(&super::runtime::root(&worker_app)?)?;
        let mut durable = jobs.read(&worker_job.id)?;
        durable.capture_id = Some(capture.id.clone());
        jobs.put(&durable)?;
        let fingerprint = capture
            .catalog
            .content_fingerprint(&library_fingerprint_domain())
            .map_err(local_error)?;
        Ok((capture, fingerprint))
    })
    .await
    .map_err(local_error)?
}

#[allow(clippy::too_many_arguments)]
async fn package_capture(
    app: &AppHandle,
    connected: &ConnectedRepository,
    job: &DurableJob,
    capture: crate::persistent_store::external_capture::CapturedSnapshot,
    fingerprint: [u8; 32],
    expected: Option<&ObservedHead>,
    cancel: &Cancellation,
) -> Result<(
    CompletedSnapshot,
    TransferJournal,
    Vec<super::sections::SectionPublication>,
)> {
    let root = super::runtime::root(app)?;
    let directory = super::runtime::job_directory(&root, &job.request.connection_id, &job.id);
    let identity = capture.identity.clone();
    let mut journal = TransferJournal::open(
        &directory,
        JobIdentity {
            job_id: job.id.clone(),
            connection_id: job.request.connection_id.clone(),
            repository_id: connected.handle.repository_id.clone(),
            capture_id: capture.id.clone(),
            capture: identity.clone(),
        },
    )?;
    journal.set_spool_budget(super::runtime::spool_budget(&root, &job.id));
    // Step 4 of the publication contract needs the latest state, not just the
    // head: the new state inherits its sections and continues its commit order.
    let parent = match expected {
        Some(head) => Some(
            control::read_snapshot_document(connected, &head.document.state, cancel).await?,
        ),
        None => None,
    };
    // Selected and recorded before anything reuses it, so a cleanup that finds
    // this job stopped still protects the graph it is reading from.
    let parent_graph = match &parent {
        Some(view) => {
            let mut roots = vec![
                (CatalogRoot::Records, view.library.record_catalog.clone()),
                (CatalogRoot::Assets, view.library.asset_catalog.clone()),
            ];
            roots.extend(view.sections.iter().map(|(id, section)| {
                (
                    CatalogRoot::Section(id.clone()),
                    section.entries_root.clone(),
                )
            }));
            let graph = super::packaging::ParentGraph::new(roots, &connected.handle)?;
            journal.record_parent(graph.stored())?;
            Some(graph)
        }
        None => None,
    };
    let generation = match &parent {
        Some(view) => Sequence::try_from(view.revision.clone())
            .map_err(corrupt)?
            .next()
            .map_err(corrupt)?,
        None => Sequence::from(1u64),
    };
    let (sections, publications) = {
        let worker_app = app.clone();
        let worker_spool = directory.join("sections");
        let worker_generation = generation.clone();
        let worker_cancel = cancel.clone();
        // The sections the observed state carries say which removals the remote
        // still holds and how far it has reclaimed, which is what decides both
        // whether this device may publish an increment and what it may drop.
        let worker_parent = parent
            .as_ref()
            .map(|view| view.sections.clone())
            .unwrap_or_default();
        let worker_connection = job.request.connection_id.clone();
        let worker_lineage = identity.library_epoch.clone();
        tokio::task::spawn_blocking(move || -> Result<_> {
            let mut store = pds(&worker_app)?;
            super::sections::capture_state_sections(
                &mut store,
                &worker_generation,
                &worker_parent,
                &worker_connection,
                &worker_lineage,
                &worker_spool,
                &worker_cancel,
            )
        })
        .await
        .map_err(local_error)??
    };
    let metadata = SnapshotMetadata {
        snapshot_id: job.snapshot_id.clone(),
        repository_id: connected.stored.descriptor.repository_id.clone(),
        library_id: identity.library_epoch.clone(),
        author_device_id: identity.store_id.clone(),
        created_at_ms: job.summary["startedAtMs"]
            .as_str()
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| corrupt("invalid job timestamp"))?,
        logical_revision: u64::try_from(identity.revision).map_err(corrupt)?,
        parent_snapshot_id: parent.as_ref().map(|view| view.snapshot_id.clone()),
        content_fingerprint: fingerprint,
        purpose: SnapshotPurpose::SyncState {
            epoch: identity.generation.clone(),
            generation,
            parent_sections: parent.map(|view| view.sections).unwrap_or_default(),
        },
    };
    let cache = super::runtime::package_cache_root(&directory)?;
    let completed = super::snapshot::package_and_upload(
        capture,
        sections,
        &root,
        &cache,
        metadata,
        &connected.root_key,
        PackageLimits::from_capabilities(&connected.stored.capabilities)?
            .with_maintenance(super::runtime::maintenance_allowed(app, connected, job)),
        parent_graph.as_ref(),
        &mut journal,
        connected.provider.as_ref(),
        &connected.handle,
        &super::runtime::preparation_progress(&root, &job.id),
        cancel,
    )
    .await?;
    match super::packaging::verify_publication(
        &completed,
        &root,
        &cache,
        &mut journal,
        &connected.root_key,
        connected.provider.as_ref(),
        &connected.handle,
        cancel,
    ).await? {
        super::packaging::PublicationReadiness::Verified => {}
        super::packaging::PublicationReadiness::Repackage => {
            return Err(ProviderError::new(ErrorKind::Transient));
        }
    }
    Ok((completed, journal, publications))
}

async fn receive_remote(
    app: &AppHandle,
    connected: &ConnectedRepository,
    job: &DurableJob,
    expected: &CaptureIdentity,
    remote: &ObservedHead,
    cancel: &Cancellation,
) -> Result<Value> {
    cancel.check()?;
    remember_discovery(
        app,
        &job.request.connection_id,
        remote.document.state.object_id.trim_start_matches("snapshot-"),
        &remote.document.state,
    );
    // A download runs for as long as the provider takes, and nothing local
    // may be held for that. The library is admitted for the reads and writes
    // around it and released over the transfer itself.
    let admission = app
        .state::<crate::native_file_jobs::NativeFileJobState>()
        .admission
        .clone();
    let (observation, participation, base_records) = {
        let _admission = admission.file(false).map_err(local_error)?;
        let mut store = pds(app)?;
        require_exact_receive_identity(expected, &store.external_identity().map_err(local_error)?)?;
        super::runtime::require_admitted_library(job, expected)?;
        let existing = store.external_job(&job.id).map_err(local_error)?;
        let observation = if let Some(pending) = existing.as_ref()
            .filter(|item| item.role == "restore" && item.phase == "ready")
        {
            let original = pending.expected_head.as_deref()
                .ok_or_else(|| corrupt("missing received head observation"))?;
            require_received_head(original, Some(&remote.observation))?;
            original.to_owned()
        } else {
            observation_json(&remote.observation)?
        };
        let intent = crate::persistent_store::external_storage_state::ReceiveIntent {
            job_id: &job.id,
            connection_id: &job.request.connection_id,
            repository_id: &connected.stored.descriptor.repository_id,
            snapshot_id: remote.document.state.object_id.trim_start_matches("snapshot-"),
            commit_id: &remote.document.commit_id,
            authenticated_head: &observation,
            identity: expected,
        };
        if job.request.kind == JobKind::ResolveConflict {
            if existing.as_ref().is_some_and(|item| matches!(item.phase.as_str(), "stale" | "cancelled")) {
                store.external_prepare_receive(&intent).map_err(local_error)?;
            }
        } else if existing.is_none() {
            store.external_prepare_receive(&intent).map_err(local_error)?;
        }
        store.external_validate_receive(&job.id, &job.request.connection_id, expected, &observation)
            .map_err(|_| ProviderError::new(ErrorKind::PreconditionFailed))?;
        let participation = receive_participation(&mut store)?;
        // What a difference is taken against. A record it names under the
        // identity the snapshot names is one the library already holds.
        let base_records = store.external_base_records(&job.request.connection_id)
            .map_err(local_error)?;
        drop(store);
        // A process restart loses the native commit handle. Only this worker
        // may revalidate the downloaded files and build another inactive
        // generation.
        discard_receive_preparation(app, &job.id)?;
        (observation, participation, base_records)
    };
    let root = super::runtime::root(app)?;
    let directory = super::runtime::job_directory(&root, &job.request.connection_id, &job.id);
    let staging = directory.join("receive");
    super::receive_artifacts::hold(&root, &job.id)?;
    // What this receive reads is what the next publication would select as its
    // parent, so keep it rather than reading the same catalogs again.
    let cache = super::runtime::package_cache_root(&directory)?;
    // A body the library already holds under the identity this snapshot names
    // is the same body, so a normal receive does not fetch it again.
    let transferred = super::runtime::transfer_progress(&root, &job.id);
    let mut trust = match base_records.as_ref() {
        Some(records) => super::snapshot_restore::SourceTrust::AdmittedLibraryAt {
            root: &root, records, within: RECEIVE_DIFFERENCE_BUDGET,
        },
        None => super::snapshot_restore::SourceTrust::AdmittedLibrary(&root),
    };
    let prepared = loop {
        let downloaded = super::snapshot_restore::download_snapshot(
            &remote.document.state, &staging, &connected.root_key, Some(&cache), trust,
            connected.provider.as_ref(), &connected.handle, &transferred, cancel,
        ).await?;
        cancel.check()?;
        let sections = super::snapshot_restore::download_sections(
            &remote.document.state, &wanted_receive_sections(&participation), &staging,
            &connected.root_key, Some(&cache), connected.provider.as_ref(), &connected.handle,
            &transferred, cancel,
        ).await?;
        transferred.flush();
        let worker_app = app.clone();
        let worker_job = job.clone();
        let worker_identity = expected.clone();
        let worker_head = observation.clone();
        let worker_participation = participation.clone();
        let worker_cancel = cancel.clone();
        let worker_staging = super::runtime::preparation_progress(&root, &job.id);
        let worker_admission = admission.clone();
        let preparation = tokio::task::spawn_blocking(move || {
            worker_cancel.check()?;
            let _admission = worker_admission.file(false).map_err(local_error)?;
            super::runtime::read_job_session(&worker_app, &worker_job.id)?;
            prepare_receive_input(&mut pds(&worker_app)?, &worker_job, worker_identity,
                worker_head, downloaded, sections, worker_participation, &worker_staging,
                &worker_cancel)
        }).await.map_err(local_error)??;
        match preparation {
            Preparation::Ready(prepared) => break prepared,
            // Only a difference too heavy to apply in place gets here: the
            // records it skipped are fetched now, next to the ones it has.
            Preparation::NeedsRecords
                if matches!(trust, super::snapshot_restore::SourceTrust::AdmittedLibraryAt { .. }) =>
            {
                trust = super::snapshot_restore::SourceTrust::AdmittedLibrary(&root);
            }
            Preparation::NeedsRecords => return Err(corrupt("received records are still unfetched")),
        }
    };
    let recorded = (|| {
        let jobs = JobStore::open(&root)?;
        let mut durable = jobs.read(&job.id)?;
        durable.receive_staging_id = prepared.apply.staging_id().map(str::to_owned);
        jobs.put(&durable)
    })();
    if let Err(error) = recorded {
        if prepared.apply.staging_id().is_some_and(|staging_id| {
            pds(app).and_then(|mut store| store.replace_abort(staging_id).map_err(local_error))
                .is_err()
        }) {
            crate::nlog!("error", "Unrecorded external receive stage could not be removed");
        }
        return Err(error);
    }
    let checked = async {
        let current = control::read_head(
            connected.provider.as_ref(), &connected.handle, &connected.stored.descriptor,
            &connected.root_key, None, cancel,
        ).await?;
        require_received_head(&prepared.authenticated_head,
            current.as_ref().map(|head| &head.observation))?;
        {
            let _admission = admission.file(false).map_err(local_error)?;
            let mut store = pds(app)?;
            require_exact_receive_identity(&prepared.expected, &store.external_identity().map_err(local_error)?)?;
            store.external_validate_receive(&job.id, &job.request.connection_id,
                &prepared.expected, &prepared.authenticated_head)
                .map_err(|_| ProviderError::new(ErrorKind::PreconditionFailed))?;
            require_receive_participation(&mut store, &prepared.participation)?;
        }
        super::runtime::read_job_session(app, &job.id)?;
        let state = app.state::<super::job_store::JobCommandState>();
        let active = state.active.lock().map_err(local_error)?;
        let (connection, owner_cancel) = active.get(&job.id)
            .ok_or_else(|| ProviderError::new(ErrorKind::PreconditionFailed))?;
        if connection != &job.request.connection_id {
            return Err(ProviderError::new(ErrorKind::PreconditionFailed));
        }
        owner_cancel.check()?;
        cancel.check()?;
        let result = prepared.ready_result();
        let mut entries = state.prepared_receives.lock().map_err(local_error)?;
        if entries.contains_key(&job.id) {
            return Err(ProviderError::new(ErrorKind::PreconditionFailed));
        }
        entries.insert(job.id.clone(), prepared);
        Ok(result)
    }.await;
    if checked.is_err() {
        let discarded = admission.file(false).map_err(local_error).and_then(|permit| {
            let outcome = discard_receive_preparation(app, &job.id);
            drop(permit);
            outcome
        });
        if discarded.is_err() {
            crate::nlog!("error", "External receive preparation could not be discarded");
        }
    }
    checked
}

fn require_received_head(expected: &str, observed: Option<&HeadObservation>) -> Result<()> {
    let expected = parse_observation(expected)?;
    if !observed.is_some_and(|current| {
        current.commit_id == expected.commit_id
            && current.authenticated_body_hash == expected.authenticated_body_hash
    }) {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ApplyReceivedRequest {
    job_id: String,
    expected_revision: String,
}

/// Records what a confirmed publication put on the remote, so the next cycle
/// does not offer the same values again.
fn note_sections_published(
    app: &AppHandle,
    connection_id: &str,
    library_lineage: &str,
    sections: &std::collections::BTreeMap<
        String,
        risunest_external_storage_format::snapshot::SectionSnapshotRef,
    >,
    publications: &[super::sections::SectionPublication],
) -> Result<()> {
    if sections.is_empty() && publications.is_empty() {
        return Ok(());
    }
    let mut store = pds(app)?;
    note_sections_published_in_store(
        &mut store,
        connection_id,
        library_lineage,
        sections,
        publications,
    )
}

fn note_sections_published_in_store(
    store: &mut PersistentStore,
    connection_id: &str,
    library_lineage: &str,
    sections: &std::collections::BTreeMap<
        String,
        risunest_external_storage_format::snapshot::SectionSnapshotRef,
    >,
    publications: &[super::sections::SectionPublication],
) -> Result<()> {
    let device = store.device_store_mut().map_err(local_error)?;
    for publication in publications {
        let mut matching = sections.values().filter(|reference| {
            super::sections::section_of(reference.kind) == Some(publication.section)
        });
        let reference = matching.next()
            .ok_or_else(|| corrupt("published section is absent from the confirmed state"))?;
        if matching.next().is_some() {
            return Err(corrupt("confirmed state repeats a published section"));
        }
        super::sections::note_prepared_section_published(
            device,
            publication,
            connection_id,
            library_lineage,
            reference,
        )?;
    }
    Ok(())
}

/// Brings in the sections this remote lineage has never exchanged with this
/// device, before a publication can put local rows in their place. Without it
/// a device that takes a section back on publishes over whatever the other
/// devices left there.
async fn rejoin_sections(
    app: &AppHandle,
    connected: &ConnectedRepository,
    job: &DurableJob,
    identity: &CaptureIdentity,
    base: &ExternalBase,
    remote: &ObservedHead,
    cancel: &Cancellation,
) -> Result<()> {
    if base.repository_id != connected.stored.descriptor.repository_id
        || !same_local_lineage(identity, &base.identity)
        || identity.revision < base.identity.revision
    {
        return Ok(());
    }
    let lineage = &base.identity.library_epoch;
    let (wanted, awaiting) = {
        let mut store = pds(app)?;
        let wanted =
            super::sections::rejoining_sections(&mut store, &job.request.connection_id, lineage)?;
        let awaiting = store
            .device_store_mut()
            .map_err(local_error)?
            .sections_await_publication(&job.request.connection_id, lineage)
            .map_err(local_error)?;
        (wanted, awaiting)
    };
    // Nothing takes their place while the library and the remote both stay
    // where they are, so the remote content is read only on a cycle that can
    // publish or receive.
    if wanted.is_empty()
        || !(awaiting
            || identity.revision != base.identity.revision
            || remote_content_differs(&stored_observation(base)?, remote))
    {
        return Ok(());
    }
    let directory = super::runtime::job_directory(
        &super::runtime::root(app)?,
        &job.request.connection_id,
        &job.id,
    );
    let staging = directory.join("rejoin");
    super::receive_artifacts::hold(&super::runtime::root(app)?, &job.id)?;
    let cache = super::runtime::package_cache_root(&directory)?;
    let transferred =
        super::runtime::transfer_progress(&super::runtime::root(app)?, &job.id);
    let received = super::snapshot_restore::download_sections(
        &remote.document.state,
        &wanted,
        &staging,
        &connected.root_key,
        Some(&cache),
        connected.provider.as_ref(),
        &connected.handle,
        &transferred,
        cancel,
    )
    .await?;
    transferred.flush();
    let mut store = pds(app)?;
    for prepared in &received {
        super::sections::apply_received_section(
            &mut store,
            &job.request.connection_id,
            lineage,
            super::sections::SectionArrival::Rejoining,
            prepared,
        )?;
    }
    Ok(())
}

fn receive_revision(request: &ApplyReceivedRequest) -> Result<i64> {
    let revision = request.expected_revision.parse::<i64>()
        .map_err(|_| corrupt("invalid staged receive revision"))?;
    if request.job_id.is_empty() || request.job_id.len() > 1024 || request.job_id.contains('\0')
        || revision < 0 || revision.to_string() != request.expected_revision
    {
        return Err(corrupt("invalid staged receive request"));
    }
    Ok(revision)
}

fn completed_receive_result(store: &PersistentStore, job: &DurableJob, expected: i64) -> Result<Option<Value>> {
    let Some(completed) = store.external_receive_completion(&job.id, &job.request.connection_id)
        .map_err(local_error)? else { return Ok(None) };
    if completed.expected_revision != expected {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    Ok(Some(json!({"snapshotId":completed.snapshot_id,
        "receivedRevision":completed.revision.to_string()})))
}

fn take_prepared_receive(
    state: &super::job_store::JobCommandState,
    claim: &super::job_store::JobClaim,
    job: &DurableJob,
    expected: i64,
) -> Result<PreparedReceive> {
    claim.require_job(state, job)?;
    let active = state.active.lock().map_err(local_error)?;
    let (connection, cancel) = active.get(&job.id)
        .ok_or_else(|| ProviderError::new(ErrorKind::PreconditionFailed))?;
    if connection != &job.request.connection_id {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    cancel.check()?;
    let mut entries = state.prepared_receives.lock().map_err(local_error)?;
    let entry = entries.get(&job.id)
        .ok_or_else(|| ProviderError::new(ErrorKind::PreconditionFailed))?;
    if entry.job_id != job.id || entry.connection_id != job.request.connection_id
        || entry.expected.revision != expected
    {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    Ok(entries.remove(&job.id).expect("prepared receive held under mutex"))
}

fn commit_prepared_receive(
    store: &mut PersistentStore,
    state: &super::job_store::JobCommandState,
    claim: &super::job_store::JobClaim,
    job: &DurableJob,
    expected: i64,
    current: &CaptureIdentity,
) -> Result<(String, i64)> {
    let prepared = take_prepared_receive(state, claim, job, expected)?;
    let snapshot_id = prepared.snapshot_id.clone();
    match activate_prepared_receive(store, &prepared, current) {
        Ok(revision) => Ok((snapshot_id, revision)),
        Err(error) => {
            if !matches!(error.kind, ErrorKind::PreconditionFailed | ErrorKind::Corrupt) {
                // The claim excludes another preparation, cancellation, or
                // apply until this original input has been restored.
                state.prepared_receives.lock().map_err(local_error)?
                    .insert(job.id.clone(), prepared);
            }
            Err(error)
        }
    }
}

fn apply_received(app: &AppHandle, request: &ApplyReceivedRequest) -> Result<Value> {
    let expected = receive_revision(request)?;
    let jobs = JobStore::open(&super::runtime::root(app)?)?;
    let mut job = jobs.read(&request.job_id)?;
    if !matches!(job.request.kind, JobKind::Sync | JobKind::ResolveConflict) {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    if let Some(result) = completed_receive_result(&pds(app)?, &job, expected)? {
        return Ok(result);
    }
    let state = app.state::<super::job_store::JobCommandState>();
    let (cancel, claim) = state.claim(&job)?;
    super::runtime::read_job_session(app, &job.id)?;
    let permit = app.state::<crate::native_file_jobs::NativeFileJobState>()
        .admission.file(true).map_err(local_error)?;
    let mut store = pds(app)?;
    // A command admitted after another apply observes that original commit.
    if let Some(result) = completed_receive_result(&store, &job, expected)? {
        return Ok(result);
    }
    let current = store.external_identity().map_err(local_error)?;
    cancel.check()?;
    super::runtime::read_job_session(app, &job.id)?;
    let outcome = commit_prepared_receive(&mut store, &state, &claim, &job, expected, &current);
    drop(store);
    drop(permit);
    let retained = state.prepared_receives.lock().map_err(local_error)?;
    job.receive_staging_id = retained.get(&job.id)
        .and_then(|prepared| prepared.apply.staging_id().map(str::to_owned));
    let ready_result = retained.get(&job.id).map(PreparedReceive::ready_result);
    drop(retained);
    let (snapshot_id, revision) = match outcome {
        Ok(result) => result,
        Err(error) => {
            let permanent = ready_result.is_none()
                && matches!(error.kind, ErrorKind::PreconditionFailed | ErrorKind::Corrupt);
            let mut preserving = false;
            if permanent {
                let mut store = pds(app)?;
                let cancelled = store.external_cancel_prepared(&job.id);
                if cancelled.is_err() {
                    crate::nlog!("error", "Rejected external receive intent could not be settled");
                }
                preserving = conflict_record(app, &job.id)?
                    .is_some_and(|record| !record.resolved && record.remote_point.is_some());
            }
            job.summary["state"] = json!(if preserving { "conflict" } else if permanent { "failed" } else { "waiting" });
            job.summary["phase"] = json!(if preserving { "conflict-choice" } else { "paused" });
            job.summary.as_object_mut().unwrap().remove("result");
            if let Some(ready) = ready_result {
                job.summary["phase"] = json!("remote-apply");
                job.summary["result"] = ready;
            } else if preserving {
                job.summary["result"] = json!({"conflictId":job.id});
            }
            job.summary["error"] = super::runtime::error_dto(&error);
            job.summary["updatedAtMs"] = json!(super::runtime::now_ms().to_string());
            if jobs.put(&job).is_err() {
                crate::nlog!("error", "External receive reprepare state could not be persisted");
            }
            return Err(error);
        }
    };
    if job.request.kind == JobKind::ResolveConflict {
        if mark_conflict_resolved(app, &job.id).is_err() {
            crate::nlog!("error", "External receive committed but conflict bookkeeping did not finish");
        }
    }
    super::runtime::record_connection_completion(
        app,
        &job.request.connection_id,
        super::connection_store::CompletionKind::Sync,
    );
    let result = json!({"snapshotId":snapshot_id,"receivedRevision":revision.to_string()});
    job.summary["state"] = json!("succeeded");
    job.summary["phase"] = json!("complete");
    job.summary["result"] = result.clone();
    job.summary.as_object_mut().unwrap().remove("error");
    job.summary["updatedAtMs"] = json!(super::runtime::now_ms().to_string());
    if jobs.put(&job).is_err() {
        crate::nlog!("error", "External receive committed but its UI summary could not be persisted");
    }
    // Terminal leases are reconciled by the next repository worker. Activation
    // neither opens credentials nor starts a provider request, even for cleanup.
    // The consumed handle and the permit are released; the claim still keeps
    // every other job of the connection from starting while bodies move aside.
    let detached =
        super::receive_artifacts::detach_claimed(app, &claim, &job.request.connection_id);
    drop(claim);
    super::receive_artifacts::remove_detached_later(app, detached);
    Ok(result)
}

#[tauri::command]
pub(crate) async fn external_storage_apply_received(
    app: AppHandle,
    request: ApplyReceivedRequest,
) -> Result<Value> {
    tokio::task::spawn_blocking(move || apply_received(&app, &request))
        .await.map_err(local_error)?
}

async fn reconcile_unknown(
    app: &AppHandle,
    connected: &ConnectedRepository,
    job: &DurableJob,
    cancel: &Cancellation,
) -> Result<Option<Value>> {
    let Some(intent) = pds(app)?
        .external_jobs(&job.request.connection_id)
        .map_err(local_error)?
        .into_iter()
        .find(|item| {
            item.id == job.id && matches!(item.phase.as_str(), "publishing" | "publicationUnknown")
        })
    else {
        return Ok(None);
    };
    if intent.phase == "publishing" {
        pds(app)?
            .external_publication_unknown(&job.id)
            .map_err(local_error)?;
    }
    let intended_state = format!("snapshot-{}", job.snapshot_id);
    match observe_unknown_publication(
        connected.provider.as_ref(),
        &connected.handle,
        &connected.stored.descriptor,
        &connected.root_key,
        &intent.commit_id,
        &intended_state,
        cancel,
    )
    .await?
    {
        Some(observed) => {
            remember_discovery(
                app,
                &job.request.connection_id,
                &job.snapshot_id,
                &observed.document.state,
            );
            let confirmed = control::read_snapshot_document(
                connected,
                &observed.document.state,
                cancel,
            )
            .await?;
            let observation = observation_json(&observed.observation)?;
            let permit = super::runtime::publication_permit(app, &job.id)?;
            let sections_spool = super::runtime::job_directory(
                &super::runtime::root(app)?,
                &job.request.connection_id,
                &job.id,
            )
            .join("sections");
            let publications =
                super::sections::load_prepared_section_publications(&sections_spool)?;
            note_sections_published(
                app,
                &job.request.connection_id,
                &intent.identity.library_epoch,
                &confirmed.sections,
                &publications,
            )?;
            pds(app)?
                .external_confirm_publication(
                    &permit,
                    &intent.commit_id,
                    &job.snapshot_id,
                    &observation,
                )
                .map_err(local_error)?;
            super::runtime::record_connection_completion(
                app,
                &job.request.connection_id,
                super::connection_store::CompletionKind::Sync,
            );
            return Ok(Some(
                json!({"snapshotId":job.snapshot_id,"publishedRevision":intent.identity.revision.to_string()}),
            ));
        }
        None => {}
    }
    Ok(Some(
        json!({"stopReason":"uncertain","reason":"publication-unknown"}),
    ))
}

async fn observe_unknown_publication(
    provider: &dyn super::contract::Provider,
    repository: &super::contract::RepositoryHandle,
    descriptor: &Descriptor,
    root_key: &[u8; 32],
    intended_commit: &str,
    intended_state: &str,
    cancel: &Cancellation,
) -> Result<Option<ObservedHead>> {
    for attempt in 0..2 {
        cancel.check()?;
        match control::read_head(
            provider,
            repository,
            descriptor,
            root_key,
            None,
            cancel,
        )
        .await
        {
            Ok(observed) => {
                let authenticated_head = observed.as_ref().map(|head| {
                    (
                        head.document.commit_id.as_str(),
                        head.document.state.object_id.as_str(),
                    )
                });
                if super::publication::classify_publication(
                    intended_commit,
                    intended_state,
                    authenticated_head,
                    false,
                ) == super::publication::PublicationObservation::Confirmed
                {
                    return Ok(observed);
                }
            }
            Err(error) => {
                let retryable = matches!(
                    error.kind,
                    ErrorKind::Transient
                        | ErrorKind::RateLimited
                        | ErrorKind::DailyQuotaExhausted
                );
                if !retryable || attempt == 1 {
                    return Err(error);
                }
            }
        }
    }
    Ok(None)
}

async fn preserve_conflict(
    app: &AppHandle,
    connected: &ConnectedRepository,
    job: &DurableJob,
    remote: ObservedHead,
    capture: crate::persistent_store::external_capture::CapturedSnapshot,
    _fingerprint: [u8; 32],
    protection: Option<&super::runtime::RepositoryProtection<'_>>,
    cancel: &Cancellation,
) -> Result<Value> {
    let local = capture
        .durable_reference(&super::runtime::root(app)?)
        .map_err(local_error)?;
    let record = conflict_record_from_remote(connected, job, local, &remote, cancel).await?;
    complete_local_conflict(app, &record, cancel).await?;
    let record = preserve_local_conflict(app, job, record)?;
    ensure_conflict_point(app, connected, job, record, false, protection, cancel).await
}

async fn conflict_record_from_remote(
    connected: &ConnectedRepository,
    job: &DurableJob,
    local: super::capture::DurableCaptureReference,
    remote: &ObservedHead,
    cancel: &Cancellation,
) -> Result<ExternalConflictRecord> {
    let view = control::read_snapshot_document(connected, &remote.document.state, cancel).await?;
    Ok(ExternalConflictRecord {
        id: job.id.clone(),
        created_at_ms: i64::try_from(super::runtime::now_ms()).map_err(corrupt)?,
        connection_id: job.request.connection_id.clone(),
        repository_id: connected.stored.descriptor.repository_id.clone(),
        local,
        remote: PreservedRemoteState {
            snapshot: remote.document.state.stored(&connected.handle)?,
            logical_revision: view.revision.parse().map_err(corrupt)?,
            commit_id: remote.document.commit_id.clone(),
            head: PreservedHeadObservation {
                commit_id: remote.document.commit_id.clone(),
                authenticated_body_hash: remote.observation.authenticated_body_hash.clone(),
            },
        },
        remote_point: None,
        resolved: false,
    })
}

/// The conflict's local side is what the user recovers from, so every payload
/// its capture names is held locally before the capture is kept for it.
async fn complete_local_conflict(
    app: &AppHandle,
    record: &ExternalConflictRecord,
    cancel: &Cancellation,
) -> Result<()> {
    let app = app.clone();
    let capture_id = record.local.capture_id.clone();
    let probe = super::runtime::CancelProbe(cancel.clone());
    tokio::task::spawn_blocking(move || {
        pds(&app)?
            .complete_external_capture(&capture_id, &probe)
            .map_err(local_error)
    })
    .await
    .map_err(local_error)?
    .map_err(|error| {
        if cancel.check().is_err() {
            ProviderError::new(ErrorKind::Cancelled)
        } else {
            error
        }
    })
}

fn preserve_local_conflict(
    app: &AppHandle,
    job: &DurableJob,
    record: ExternalConflictRecord,
) -> Result<ExternalConflictRecord> {
    let owner = external_conflicts::conflict_capture_owner(&record.id).map_err(local_error)?;
    let mut store = pds(app)?;
    let existing = external_conflicts::external_conflict(
        store.device_store().map_err(local_error)?.connection(),
        &record.id,
    )
    .map_err(local_error)?;
    let preserved = if existing.is_some() {
        external_conflicts::preserve_local_conflict(
            store.device_store().map_err(local_error)?.connection(),
            &record,
        )
        .map_err(local_error)?
    } else {
        store
            .retain_external_capture(&record.local.capture_id, &owner)
            .map_err(local_error)?;
        match external_conflicts::preserve_local_conflict(
            store.device_store().map_err(local_error)?.connection(),
            &record,
        ) {
            Ok(preserved) => preserved,
            Err(error) => {
                if store
                    .release_external_capture(&record.local.capture_id, &owner)
                    .is_err()
                {
                    crate::nlog!("error", "Failed to compensate a rejected conflict capture owner");
                }
                return Err(local_error(error));
            }
        }
    };
    if store
        .external_job(&job.id)
        .map_err(local_error)?
        .is_some_and(|intent| matches!(intent.phase.as_str(), "ready" | "publishing"))
    {
        store
            .external_publication_rejected(&job.id)
            .map_err(local_error)?;
    }
    store
        .release_external_capture(&record.local.capture_id, &job.id)
        .map_err(local_error)?;
    Ok(preserved)
}

fn conflict_journal(
    app: &AppHandle,
    connected: &ConnectedRepository,
    job: &DurableJob,
    record: &ExternalConflictRecord,
) -> Result<TransferJournal> {
    let root = super::runtime::root(app)?;
    let directory = super::runtime::job_directory(&root, &job.request.connection_id, &job.id);
    let mut journal = TransferJournal::open(
        &directory,
        JobIdentity {
            job_id: job.id.clone(),
            connection_id: job.request.connection_id.clone(),
            repository_id: connected.handle.repository_id.clone(),
            capture_id: record.local.capture_id.clone(),
            capture: record.local.identity.clone(),
        },
    )?;
    journal.set_spool_budget(super::runtime::spool_budget(&root, &job.id));
    Ok(journal)
}

async fn resume_conflict_preservation(
    app: &AppHandle,
    connected: &ConnectedRepository,
    job: &DurableJob,
    record: ExternalConflictRecord,
    protection: Option<&super::runtime::RepositoryProtection<'_>>,
    cancel: &Cancellation,
) -> Result<Value> {
    ensure_conflict_point(app, connected, job, record, false, protection, cancel).await
}

pub(crate) async fn ensure_conflict_point(
    app: &AppHandle,
    connected: &ConnectedRepository,
    job: &DurableJob,
    record: ExternalConflictRecord,
    force_recheck: bool,
    protection: Option<&super::runtime::RepositoryProtection<'_>>,
    cancel: &Cancellation,
) -> Result<Value> {
    if record.connection_id != job.request.connection_id
        || record.repository_id != connected.stored.descriptor.repository_id
        || (record.resolved && !force_recheck)
    {
        return Err(corrupt("invalid local conflict preservation"));
    }
    if record.remote_point.is_some() && !force_recheck {
        return Ok(json!({"snapshotId":record.local.capture_id,"conflictId":job.id,"preservation":"remote-complete"}));
    }
    let mut journal = conflict_journal(app, connected, job, &record)?;
    let remote_snapshot = super::packaging::RemoteObject::from_stored(
        &record.remote.snapshot,
        &connected.handle,
    )?;
    let created_at_ms = u64::try_from(record.created_at_ms).map_err(corrupt)?;
    let remote_bundle = control::ensure_remote_conflict_bundle(
        connected,
        &record.id,
        &record.remote.commit_id,
        created_at_ms,
        &remote_snapshot,
        &mut journal,
        cancel,
    )
    .await?;
    if let Some(protection) = protection {
        protection.recheck(cancel).await?;
    }
    let point = control::ensure_remote_conflict_point(
        &connected.stored.descriptor,
        &record.id,
        created_at_ms,
        remote_bundle,
        &mut journal,
        connected,
        cancel,
    )
    .await?;
    let store = pds(app)?;
    external_conflicts::confirm_external_conflict_point(
        store.device_store().map_err(local_error)?.connection(),
        &record.id,
        &point.stored(&connected.handle)?,
    )
    .map_err(local_error)?;
    let _ = journal
        .release_completed_sessions(connected.dependencies.vault.as_ref())
        .await;
    Ok(json!({"snapshotId":record.local.capture_id,"conflictId":job.id,"preservation":"remote-complete"}))
}

pub(crate) fn conflict_record(app: &AppHandle, id: &str) -> Result<Option<ExternalConflictRecord>> {
    let store = pds(app)?;
    external_conflicts::external_conflict(
        store.device_store().map_err(local_error)?.connection(),
        id,
    )
    .map_err(local_error)
}

pub(crate) fn mark_conflict_resolved(app: &AppHandle, id: &str) -> Result<()> {
    let store = pds(app)?;
    external_conflicts::mark_external_conflict_resolved(
        store.device_store().map_err(local_error)?.connection(),
        id,
    )
    .map(|_| ())
    .map_err(local_error)
}

async fn run_resolve_conflict(
    app: &AppHandle,
    connected: &ConnectedRepository,
    job: &DurableJob,
    protection: Option<&super::runtime::RepositoryProtection<'_>>,
    cancel: &Cancellation,
) -> Result<Value> {
    let conflict_id = job
        .request
        .conflict_id
        .as_deref()
        .ok_or_else(|| corrupt("missing conflict selection"))?;
    if conflict_id != job.id {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    let choice = job
        .request
        .choice
        .as_deref()
        .ok_or_else(|| corrupt("missing conflict choice"))?;
    let stored = {
        let store = pds(app)?;
        external_conflicts::external_conflict_for_resolution(
            store.device_store().map_err(local_error)?.connection(),
            conflict_id,
        )
        .map_err(local_error)?
    };
    if stored.connection_id != job.request.connection_id
        || stored.repository_id != connected.stored.descriptor.repository_id
    {
        return Err(corrupt("conflict connection binding differs"));
    }
    let remote = control::read_head(
        connected.provider.as_ref(),
        &connected.handle,
        &connected.stored.descriptor,
        &connected.root_key,
        None,
        cancel,
    )
    .await?
    .ok_or_else(|| ProviderError::new(ErrorKind::PreconditionFailed))?;
    let observation = observation_json(&remote.observation)?;
    if stored.remote.head.commit_id != remote.document.commit_id
        || stored.remote.head.authenticated_body_hash
            != remote.observation.authenticated_body_hash
        || stored.remote.snapshot != remote.document.state.stored(&connected.handle)?
    {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    remember_discovery(
        app,
        &job.request.connection_id,
        remote.document.state.object_id.trim_start_matches("snapshot-"),
        &remote.document.state,
    );
    if choice == "remote" {
        let expected = pds(app)?.external_job(&job.id).map_err(local_error)?
            .filter(|intent| intent.role == "restore" && intent.phase == "ready")
            .map(|intent| intent.identity).unwrap_or(stored.local.identity.clone());
        return receive_remote(app, connected, job, &expected, &remote, cancel).await;
    }
    if choice != "local" {
        return Err(corrupt("invalid conflict choice"));
    }
    let capture = pds(app)?
        .reopen_external_capture(&stored.local.capture_id)
        .map_err(local_error)?;
    if capture.durable_reference(&super::runtime::root(app)?).map_err(local_error)?
        != stored.local
    {
        return Err(corrupt("conflict capture binding differs"));
    }
    let fingerprint = capture
        .catalog
        .content_fingerprint(&library_fingerprint_domain())
        .map_err(local_error)?;
    let base = {
        let store = pds(app)?;
        store
            .external_base(&job.request.connection_id)
            .map_err(local_error)?
    };
    if let Some(base) = base {
        rejoin_sections(
            app,
            connected,
            job,
            &stored.local.identity,
            &base,
            &remote,
            cancel,
        )
        .await?;
    }
    let strategy = connected
        .stored
        .descriptor
        .publication_strategy
        .ok_or_else(|| ProviderError::new(ErrorKind::Unsupported))?;
    let commit_id = format!("resolve-{}", job.id);
    let (completed, mut resolve_journal, publications) = package_capture(
        app,
        connected,
        job,
        capture,
        fingerprint,
        Some(&remote),
        cancel,
    )
    .await?;
    let document = HeadDocument::new(
        &connected.stored.descriptor,
        remote.document.library_id.clone(),
        commit_id.clone(),
        Some(remote.document.commit_id.clone()),
        completed.fingerprint.clone(),
        completed.reference.clone(),
    )?;
    let prepared = control::prepare_head(
        &connected.stored.descriptor,
        &connected.root_key,
        &connected.handle,
        document,
    )?;
    let intent = crate::persistent_store::external_storage_state::PublishIntent {
        job_id: &job.id,
        connection_id: &job.request.connection_id,
        repository_id: &connected.stored.descriptor.repository_id,
        capture_id: &stored.local.capture_id,
        identity: &stored.local.identity,
        strategy: match strategy {
            risunest_external_storage_format::format::Strategy::Cas => "cas",
            risunest_external_storage_format::format::Strategy::Sequential => "sequential",
        },
        expected_head: Some(&observation),
        commit_id: &commit_id,
    };
    let _permit = app
        .state::<crate::native_file_jobs::NativeFileJobState>()
        .admission
        .file(false)
        .map_err(local_error)?;
    let prepare_permit = super::runtime::publication_permit(app, &job.id)?;
    pds(app)?
        .external_prepare_publication(&intent, &prepare_permit)
        .map_err(local_error)?;
    let mut publication_permit = None;
    if let Some(protection) = protection {
        protection.recheck(cancel).await?;
    }
    let result = match control::publish_head_guarded(
        connected.provider.as_ref(),
        &connected.handle,
        &connected.stored.capabilities,
        &connected.stored.descriptor,
        &connected.root_key,
        strategy,
        Some(&remote),
        &prepared,
        || super::runtime::read_job_session(app, &job.id),
        |live| {
            let permit = super::runtime::publication_permit(app, &job.id)?;
            if permit.mode() != live {
                return Err(ProviderError::new(ErrorKind::Cancelled));
            }
            pds(app)?
                .external_begin_publication(&permit)
                .map_err(local_error)?;
            publication_permit = Some(permit);
            Ok(())
        },
        cancel,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return Err(error),
    };
    match result {
        PublicationResult::Confirmed(head) => {
            remember_discovery(
                app,
                &job.request.connection_id,
                &completed.snapshot_id,
                &completed.reference,
            );
            let observation = observation_json(&head.observation)?;
            let permit = publication_permit
                .as_ref()
                .ok_or_else(|| corrupt("missing publication permit"))?;
            note_sections_published(
                app,
                &job.request.connection_id,
                &stored.local.identity.library_epoch,
                &completed.sections,
                &publications,
            )?;
            pds(app)?
                .external_confirm_publication(
                    permit,
                    &commit_id,
                    &completed.snapshot_id,
                    &observation,
                )
                .map_err(local_error)?;
            let _ = resolve_journal
                .release_completed_sessions(connected.dependencies.vault.as_ref())
                .await;
            if mark_conflict_resolved(app, conflict_id).is_err() {
                crate::nlog!("error","External conflict publication committed but conflict bookkeeping did not finish");
            }
            super::runtime::record_connection_completion(
                app,
                &job.request.connection_id,
                super::connection_store::CompletionKind::Sync,
            );
            Ok(json!({
                "snapshotId": completed.snapshot_id,
                "publishedRevision": stored.local.identity.revision.to_string(),
                "maintenancePacks": completed.maintenance.packs.to_string(),
                "maintenanceBytes": completed.maintenance.source_bytes.to_string(),
                "maintenanceLeaves": completed.maintenance.leaves.to_string(),
                "maintenanceRequiredLeaves": completed.maintenance.required_leaves.to_string(),
                "sourceDownloadObjects": completed.hydration.objects.to_string(),
                "sourceDownloadBytes": completed.hydration.bytes.to_string(),
            }))
        }
        PublicationResult::Conflict(_) => {
            pds(app)?
                .external_publication_rejected(&job.id)
                .map_err(local_error)?;
            Err(ProviderError::new(ErrorKind::PreconditionFailed))
        }
        PublicationResult::Unknown { .. } => {
            pds(app)?
                .external_publication_unknown(&job.id)
                .map_err(local_error)?;
            Err(ProviderError::new(ErrorKind::Transient))
        }
    }
}

pub(crate) async fn run_sync(
    app: &AppHandle,
    connected: &ConnectedRepository,
    job: &DurableJob,
    protection: Option<&super::runtime::RepositoryProtection<'_>>,
    cancel: &Cancellation,
) -> Result<Value> {
    if job.request.kind == JobKind::ResolveConflict {
        if let Some(result) = reconcile_unknown(app, connected, job, cancel).await? {
            if mark_conflict_resolved(app, &job.id).is_err() {
                crate::nlog!("error","External conflict publication reconciled but conflict bookkeeping did not finish");
            }
            return Ok(result);
        }
        return run_resolve_conflict(app, connected, job, protection, cancel).await;
    }
    if let Some(result) = reconcile_unknown(app, connected, job, cancel).await? {
        return Ok(result);
    }
    super::runtime::read_job_session(app, &job.id)?;
    let strategy = connected
        .stored
        .descriptor
        .publication_strategy
        .ok_or_else(|| ProviderError::new(ErrorKind::Unsupported))?;
    connected.stored.capabilities.require(strategy)?;
    if let Some(record) = conflict_record(app, &job.id)?.filter(|record| !record.resolved) {
        return resume_conflict_preservation(app, connected, job, record, protection, cancel).await;
    }
    let remote = control::read_head(
        connected.provider.as_ref(),
        &connected.handle,
        &connected.stored.descriptor,
        &connected.root_key,
        None,
        cancel,
    )
    .await?;
    if let Some(remote) = remote.as_ref() {
        remember_discovery(
            app,
            &job.request.connection_id,
            remote.document.state.object_id.trim_start_matches("snapshot-"),
            &remote.document.state,
        );
    }
    let store = pds(app)?;
    let identity = store.external_identity().map_err(local_error)?;
    let local_pristine = store.external_library_is_pristine().map_err(local_error)?;
    let base = store
        .external_base(&job.request.connection_id)
        .map_err(local_error)?;
    let pending_receive = store.external_job(&job.id).map_err(local_error)?
        .filter(|intent| intent.role == "restore" && intent.phase == "ready");
    drop(store);
    if let Some(intent) = pending_receive {
        let remote = remote.as_ref().ok_or_else(|| ProviderError::new(ErrorKind::PreconditionFailed))?;
        if intent.repository_id != connected.stored.descriptor.repository_id
            || intent.commit_id != remote.document.commit_id
            || remote.document.state.object_id != format!("snapshot-{}", intent.capture_id)
        {
            return Err(ProviderError::new(ErrorKind::PreconditionFailed));
        }
        return receive_remote(app, connected, job, &intent.identity, remote, cancel).await;
    }
    let local_changed = base.as_ref().is_none_or(|base| {
        identity.revision != base.identity.revision
            || !same_local_lineage(&identity, &base.identity)
    });
    let sections_changed = match (&base, &remote) {
        (Some(base), Some(_)) => {
            let mut store = pds(app)?;
            let device = store.device_store_mut().map_err(local_error)?;
            device
                .sections_await_publication(
                    &job.request.connection_id,
                    &base.identity.library_epoch,
                )
                .map_err(local_error)?
        }
        _ => false,
    };
    let mut captured = None;
    let mut fingerprint = None;
    if (local_changed || sections_changed) && !(base.is_none() && remote.is_some() && local_pristine)
    {
        let value = capture_for_publication(app, connected, job, remote.as_ref(), cancel).await?;
        fingerprint = Some(hex::encode(value.1));
        captured = Some(value);
    }
    // A resumed job may retain an R5 capture while the live library has
    // already advanced to R10. The retained capture is the only identity that
    // can be bound to this publication and its resulting base.
    let decision_identity = captured
        .as_ref()
        .map(|(capture, _)| &capture.identity)
        .unwrap_or(&identity);
    let decision_revision = decision_identity.revision;
    let action = decide_sync(SyncInputs {
        descriptor: &connected.stored.descriptor,
        connection_id: &job.request.connection_id,
        current_identity: decision_identity,
        local_pristine,
        local_fingerprint: fingerprint.as_deref(),
        local_sections_changed: sections_changed,
        base: base.as_ref(),
        remote: remote.as_ref(),
    })?;
    match action {
        SyncAction::UpToDate => {
            if captured.is_some() {
                pds(app)?
                    .external_cancel_prepared(&job.id)
                    .map_err(local_error)?;
            }
            Ok(json!({"publishedRevision":decision_revision.to_string()}))
        }
        SyncAction::ReceiveRemote { remote } => {
            receive_remote(app, connected, job, &identity, &remote, cancel).await
        }
        SyncAction::AcceptEquivalent { remote } => {
            if captured.is_some() {
                return Err(corrupt("equivalent state unexpectedly captured local changes"));
            }
            let publication_permit = super::runtime::publication_permit(app, &job.id)?;
            let base = base.ok_or_else(|| corrupt("equivalent state has no base"))?;
            let observation = observation_json(&remote.observation)?;
            pds(app)?
                .external_accept_equivalent(
                    &publication_permit,
                    &job.request.connection_id,
                    &connected.stored.descriptor.repository_id,
                    &base.commit_id,
                    &base.head_observation,
                    remote
                        .document
                        .state
                        .object_id
                        .trim_start_matches("snapshot-"),
                    &remote.document.commit_id,
                    &observation,
                    &identity,
                )
                .map_err(local_error)?;
            Ok(
                json!({"snapshotId":remote.document.state.object_id.trim_start_matches("snapshot-"),"publishedRevision":identity.revision.to_string()}),
            )
        }
        SyncAction::PublishLocal { expected } => {
            let (capture, fingerprint) = captured
                .take()
                .ok_or_else(|| corrupt("missing publication capture"))?;
            let published_identity = capture.identity.clone();
            let local_reference = capture
                .durable_reference(&super::runtime::root(app)?)
                .map_err(local_error)?;
            // Rejoining changes device rows, so it belongs to publication, not
            // to a receive's read-only preparation or conflict preservation.
            if let (Some(base), Some(remote)) = (base.as_ref(), expected.as_ref()) {
                rejoin_sections(app, connected, job, &published_identity, base, remote, cancel).await?;
            }
            let (completed, mut journal, publications) = package_capture(
                app,
                connected,
                job,
                capture,
                fingerprint,
                expected.as_ref(),
                cancel,
            )
            .await?;
            let document = HeadDocument::new(
                &connected.stored.descriptor,
                expected
                    .as_ref()
                    .map(|head| head.document.library_id.clone())
                    .unwrap_or_else(|| published_identity.library_epoch.clone()),
                job.id.clone(),
                expected
                    .as_ref()
                    .map(|head| head.document.commit_id.clone()),
                completed.fingerprint.clone(),
                completed.reference.clone(),
            )?;
            let prepared = control::prepare_head(
                &connected.stored.descriptor,
                &connected.root_key,
                &connected.handle,
                document,
            )?;
            let _permit = app
                .state::<crate::native_file_jobs::NativeFileJobState>()
                .admission
                .file(false)
                .map_err(local_error)?;
            let mut publication_permit = None;
            if let Some(protection) = protection {
                protection.recheck(cancel).await?;
            }
            match control::publish_head_guarded(
                connected.provider.as_ref(),
                &connected.handle,
                &connected.stored.capabilities,
                &connected.stored.descriptor,
                &connected.root_key,
                strategy,
                expected.as_ref(),
                &prepared,
                || super::runtime::read_job_session(app, &job.id),
                |live| {
                    let permit = super::runtime::publication_permit(app, &job.id)?;
                    if permit.mode() != live {
                        return Err(ProviderError::new(ErrorKind::Cancelled));
                    }
                    pds(app)?
                        .external_begin_publication(&permit)
                        .map_err(local_error)?;
                    publication_permit = Some(permit);
                    Ok(())
                },
                cancel,
            )
            .await?
            {
                PublicationResult::Confirmed(observed) => {
                    remember_discovery(
                        app,
                        &job.request.connection_id,
                        &completed.snapshot_id,
                        &completed.reference,
                    );
                    let observation = observation_json(&observed.observation)?;
                    let permit = publication_permit
                        .as_ref()
                        .ok_or_else(|| corrupt("missing publication permit"))?;
                    note_sections_published(
                        app,
                        &job.request.connection_id,
                        &published_identity.library_epoch,
                        &completed.sections,
                        &publications,
                    )?;
                    pds(app)?
                        .external_confirm_publication(
                            permit,
                            &job.id,
                            &completed.snapshot_id,
                            &observation,
                        )
                        .map_err(local_error)?;
                    let _ = journal
                        .release_completed_sessions(connected.dependencies.vault.as_ref())
                        .await;
                    super::runtime::record_connection_completion(
                        app,
                        &job.request.connection_id,
                        super::connection_store::CompletionKind::Sync,
                    );
                    Ok(
                        json!({"snapshotId":completed.snapshot_id,"publishedRevision":published_identity.revision.to_string()}),
                    )
                }
                PublicationResult::Conflict(remote) => {
                    drop(_permit);
                    let remote = match remote {
                        Some(remote) => remote,
                        None => control::read_head(
                            connected.provider.as_ref(),
                            &connected.handle,
                            &connected.stored.descriptor,
                            &connected.root_key,
                            None,
                            cancel,
                        )
                        .await?
                        .ok_or_else(|| ProviderError::new(ErrorKind::PreconditionFailed))?,
                    };
                    let record = conflict_record_from_remote(
                        connected,
                        job,
                        local_reference,
                        &remote,
                        cancel,
                    )
                    .await?;
                    complete_local_conflict(app, &record, cancel).await?;
                    let record = preserve_local_conflict(app, job, record)?;
                    drop(journal);
                    ensure_conflict_point(app, connected, job, record, false, protection, cancel).await
                }
                PublicationResult::Unknown { .. } => {
                    pds(app)?
                        .external_publication_unknown(&job.id)
                        .map_err(local_error)?;
                    Err(ProviderError::new(ErrorKind::Transient))
                }
            }
        }
        SyncAction::PreserveConflict { remote } | SyncAction::FirstAttachDecision { remote } => {
            let (capture, fingerprint) = captured
                .take()
                .ok_or_else(|| corrupt("missing conflict capture"))?;
            preserve_conflict(
                app, connected, job, remote, capture, fingerprint, protection, cancel,
            ).await
        }
        SyncAction::DecisionRequired | SyncAction::RecoveryRequired => {
            Err(ProviderError::new(ErrorKind::PreconditionFailed))
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ExternalConflictCursorRequest {
    created_at_ms: i64,
    id: String,
}

fn remember_discovery(
    app: &AppHandle,
    connection_id: &str,
    snapshot_id: &str,
    reference: &super::packaging::RemoteObject,
) {
    let result = super::runtime::root(app).and_then(|root| {
        super::connection_store::ConnectionStore::open(&root)?
            .remember_discovery(connection_id, snapshot_id, reference)
    });
    if result.is_err() {
        crate::nlog!("warn", "External snapshot locator could not be cached");
    }
}

fn conflict_summary(
    record: &ExternalConflictRecord,
    local_available: bool,
    remote_available: bool,
) -> Value {
    json!({
        "id":record.id,
        "connectionId":record.connection_id,
        "detectedAtMs":record.created_at_ms.to_string(),
        "localRevision":record.local.identity.revision.to_string(),
        "remoteRevision":record.remote.logical_revision.to_string(),
        "localAvailable":local_available,
        "remoteAvailable":remote_available,
        "remotePointConfirmed":record.remote_point.is_some(),
        "resolved":record.resolved,
    })
}

#[tauri::command]
pub(crate) fn external_storage_list_conflicts(
    app: AppHandle,
    cursor: Option<ExternalConflictCursorRequest>,
    limit: Option<usize>,
) -> Result<Value> {
    let limit = limit.unwrap_or(50);
    if limit == 0 || limit > 50 {
        return Err(corrupt("invalid conflict page limit"));
    }
    let root = super::runtime::root(&app)?;
    let store = pds(&app)?;
    let cursor = cursor.map(|cursor| external_conflicts::ExternalConflictCursor {
        created_at_ms: cursor.created_at_ms,
        id: cursor.id,
    });
    let page = external_conflicts::external_conflicts_page(
        store.device_store().map_err(local_error)?.connection(),
        cursor.as_ref(),
        limit,
    )
    .map_err(local_error)?;
    let connections = super::connection_store::ConnectionStore::open(&root).ok();
    let conflicts = page
        .conflicts
        .iter()
        .map(|record| {
            let local_available =
                super::capture::registered_capture_roots([&record.local], &root).is_ok();
            let remote_available = connections
                .as_ref()
                .is_some_and(|connections| connections.read(&record.connection_id).is_ok());
            conflict_summary(record, local_available, remote_available)
        })
        .collect::<Vec<_>>();
    let mut result = json!({"conflicts":conflicts});
    if let Some(next) = page.next {
        result["nextCursor"] = json!({"createdAtMs":next.created_at_ms,"id":next.id});
    }
    Ok(result)
}

#[tauri::command]
pub(crate) async fn external_storage_delete_conflict(
    app: AppHandle,
    id: String,
    delete_remote_point: bool,
) -> std::result::Result<Value, crate::native_file_jobs::NativeJobError> {
    if id.is_empty() || id.len() > 1024 || id.contains('\0') {
        return Err(crate::native_file_jobs::NativeJobError::new(
            "invalid-input",
            "External conflict identity is invalid",
        ));
    }
    let state = app.state::<crate::native_file_jobs::NativeFileJobState>();
    let mutation = state.external_conflict_mutation_admission()?;
    let record = {
        let mut store = pds(&app).map_err(|error| {
            crate::native_file_jobs::NativeJobError::new("store-error", error.to_string())
        })?;
        let in_use = state.external_conflict_in_use(&id)?;
        if in_use {
            return Err(crate::native_file_jobs::NativeJobError::new(
                "conflict-source-in-use",
                "External conflict source is in use",
            ));
        }
        let record = external_conflicts::delete_external_conflict(
            store
                .device_store()
                .map_err(|error| {
                    crate::native_file_jobs::NativeJobError::new(
                        "store-error",
                        error.to_string(),
                    )
                })?
                .connection(),
            &id,
        )
        .map_err(|error| {
            crate::native_file_jobs::NativeJobError::new("store-error", error.to_string())
        })?;
        match store.cleanup_deleted_conflict_capture(&record.local) {
            Ok(true) => store.collect_released_external_content(),
            Ok(false) => {}
            Err(_) => crate::nlog!(
                "warn",
                "Deleted external conflict capture could not be cleaned immediately"
            ),
        }
        record
    };
    drop(mutation);
    drop(state);
    release_deleted_conflict_bodies(&app, &record.id).await;
    if !delete_remote_point || record.remote_point.is_none() {
        return Ok(json!({"localDeleted":true,"remotePoint":"left-remote"}));
    }
    let remote = async {
        let connected = super::connection_commands::open_connected(&app, &record.connection_id).await?;
        if connected.stored.descriptor.repository_id != record.repository_id {
            return Err(ProviderError::new(ErrorKind::Corrupt));
        }
        control::delete_authenticated_conflict_point(
            &connected,
            &record.id,
            record.remote_point.as_ref().expect("checked conflict point"),
            &Cancellation::default(),
        )
        .await
    }
    .await;
    let remote_point = match remote {
        Ok(control::RemoteConflictPointDeleteOutcome::Deleted) => "deleted",
        Ok(control::RemoteConflictPointDeleteOutcome::NotFound) => "not-found",
        Err(_) => "left-remote",
    };
    Ok(json!({"localDeleted":true,"remotePoint":remote_point}))
}

/// The deleted conflict no longer holds its job's downloaded bodies. While
/// another job holds the connection, that job's settlement runs the pass.
async fn release_deleted_conflict_bodies(app: &AppHandle, job_id: &str) {
    let Ok(job) =
        super::runtime::root(app).and_then(|root| JobStore::open(&root)?.read(job_id))
    else {
        return;
    };
    let Ok((_, claim)) = app.state::<super::job_store::JobCommandState>().claim(&job) else {
        return;
    };
    let (pass_app, connection) = (app.clone(), job.request.connection_id);
    let detached = tokio::task::spawn_blocking(move || {
        super::receive_artifacts::detach_claimed(&pass_app, &claim, &connection)
    })
    .await
    .unwrap_or_default();
    super::receive_artifacts::remove_detached_later(app, detached);
}

#[tauri::command]
pub(crate) async fn external_storage_recheck_conflict(
    app: AppHandle,
    id: String,
) -> Result<Value> {
    if id.is_empty() || id.len() > 1024 || id.contains('\0') {
        return Err(corrupt("invalid conflict identity"));
    }
    let root = super::runtime::root(&app)?;
    super::runtime::recheck_preserved_conflict(&app, &id, &Cancellation::default()).await?;
    let record = conflict_record(&app, &id)?
        .ok_or_else(|| ProviderError::new(ErrorKind::NotFound))?;
    let local_available =
        super::capture::validate_recovery_sources(
            [&record.local], &root, &crate::local_backup::NeverCancelled,
        ).is_ok();
    Ok(conflict_summary(&record, local_available, true))
}

#[cfg(test)]
#[path = "receive_artifact_tests.rs"]
mod receive_artifact_tests;

#[cfg(test)]
mod receive_tests {
    use super::*;
    use crate::logical_records::{
        encode_logical_record, encode_logical_record_key, LogicalRecordEnvelope, LogicalRecordLocator,
    };
    use crate::persistent_store::external_storage_state::ReceiveIntent;
    use risunest_external_storage_format::{
        content_identity::hash,
        format::fingerprint,
        section::{local_plugin_entry_key, LocalPluginValue, PluginSpace, SectionEntry, SectionEntryVersion, SectionValue},
        snapshot::{self as wire, CatalogEntryKind},
    };
    use std::{collections::BTreeMap, fs};

    pub(super) fn bind(store: &mut PersistentStore, snapshot: &str) -> DurableJob {
        let identity = store.external_identity().unwrap();
        let request = serde_json::from_value(json!({
            "connectionId":"connection", "kind":"sync", "reason":"automatic",
            "session":"foreground", "sessionId":"synthetic-session"
        })).unwrap();
        let job = DurableJob::new(request, false, 1, identity.clone());
        store.external_prepare_receive(&ReceiveIntent {
            job_id: &job.id, connection_id: "connection", repository_id: "repository",
            snapshot_id: snapshot, commit_id: snapshot, authenticated_head: "authenticated-head",
            identity: &identity,
        }).unwrap();
        job
    }

    pub(super) fn fixture() -> (tempfile::TempDir, PersistentStore, DurableJob, super::super::snapshot_restore::PreparedRemoteSnapshot) {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let epoch = store.external_selection().unwrap().epoch;
        store.external_select(&epoch, &crate::persistent_store::sync_selection::SyncTarget::External("connection".into())).unwrap();
        store.device_store_mut().unwrap().set_section_participating(PdsSection::LocalPlugins, true).unwrap();
        let job = bind(&mut store, "snapshot");
        let staging = directory.path().join("received");
        fs::create_dir(&staging).unwrap();
        let key = encode_logical_record_key(&LogicalRecordLocator::Root).unwrap();
        let record = encode_logical_record(&LogicalRecordEnvelope::Root {
            value: json!({"marker":"remote"}), owner_heads: vec![],
        }).unwrap();
        let path = staging.join("root");
        fs::write(&path, &record.bytes).unwrap();
        let digest = hex::decode(&record.hash).unwrap().try_into().unwrap();
        let library_fingerprint = hex::encode(fingerprint(&library_fingerprint_domain(), &BTreeMap::from([(key.clone(), digest)])));
        let downloaded = super::super::snapshot_restore::PreparedRemoteSnapshot {
            snapshot_id: "snapshot".into(), repository_id: "repository".into(),
            fingerprint: library_fingerprint.clone(), library_fingerprint, logical_revision: 7,
            staging_root: staging, captured_by_device: None, objects: vec![],
            records: vec![super::super::snapshot_restore::PreparedRecord {
                key, content_hash: record.hash, byte_length: record.size,
                source: super::super::content_store::ObjectSource::File(path),
            }],
        };
        (directory, store, job, downloaded)
    }

    fn plugin_section(staging_root: &std::path::Path) -> super::super::sections::CapturedSection {
        let kind = SectionKind::LocalPlugins;
        let key = local_plugin_entry_key("owner", "string", "key").unwrap();
        let entry = SectionEntry::new(kind, key.clone(), SectionValue::LocalPlugin(LocalPluginValue {
            space: PluginSpace::String, value: json!("remote-device-value"),
        }), Some(SectionEntryVersion {
            write_clock: Sequence::from(7u64), writer_id: "remote-writer".into(),
        })).unwrap().encode().unwrap();
        let content_sha256 = hex::encode(hash(&entry));
        let spool = staging_root.join("section-spool");
        fs::create_dir_all(&spool).unwrap();
        let path = spool.join(&content_sha256);
        fs::write(&path, &entry).unwrap();
        super::super::sections::CapturedSection {
            kind, generation: Sequence::from(7u64), gc_floor: Sequence::from(0u64),
            max_write_clock: Sequence::from(7u64),
            content_fingerprint: fingerprint(&kind.fingerprint_domain(), &BTreeMap::from([(key.clone(), hash(&entry))])),
            sources: vec![super::super::sections::SectionSource {
                kind: CatalogEntryKind::SectionEntry, key, content_sha256,
                byte_length: entry.len() as u64, path,
            }],
        }
    }

    fn plugin_section_reference(generation: u64) -> wire::SectionSnapshotRef {
        wire::SectionSnapshotRef {
            kind: SectionKind::LocalPlugins,
            codec: risunest_external_storage_format::section::SECTION_CODEC.into(),
            generation: Sequence::from(generation),
            gc_floor: Sequence::from(0u64),
            max_write_clock: Sequence::from(7u64),
            entries_root: wire::StoredObject {
                header: wire::PublicObjectHeader::new(
                    "repository".into(),
                    "catalog-synthetic".into(),
                    wire::ObjectRole::Catalog,
                    1,
                ).unwrap(),
                locator: wire::WireLocator {
                    connection_identity: "connection".into(),
                    collection: None,
                    object: "catalog-synthetic".into(),
                },
                ciphertext_length: 1,
                ciphertext_sha256: [0; 32],
                plaintext_length: 1,
                plaintext_sha256: [0; 32],
            },
            content_fingerprint: [0; 32],
        }
    }

    pub(super) fn prepare(store: &mut PersistentStore, job: &DurableJob,
        downloaded: super::super::snapshot_restore::PreparedRemoteSnapshot) -> PreparedReceive
    {
        prepare_counted(store, job, downloaded, &crate::external_storage::phase_progress::PhaseProgress::silent())
    }

    fn prepare_counted(store: &mut PersistentStore, job: &DurableJob,
        downloaded: super::super::snapshot_restore::PreparedRemoteSnapshot,
        prepared: &crate::external_storage::phase_progress::PhaseProgress) -> PreparedReceive
    {
        let participation = receive_participation(store).unwrap();
        let section = plugin_section(&downloaded.staging_root);
        prepare_receive_input(store, job, job.admission_identity.clone(), "authenticated-head".into(),
            downloaded, vec![section], participation, prepared, &Cancellation::default()).unwrap().ready()
    }

    pub(super) fn local_edit(store: &mut PersistentStore, revision: i64) {
        let commit = serde_json::from_value(json!({
            "expectedRevision": revision, "root": {"marker":"newer-local"}
        })).unwrap();
        store.commit(&commit).unwrap();
    }


    fn staged_record(
        staging: &std::path::Path,
        locator: LogicalRecordLocator,
        envelope: LogicalRecordEnvelope,
    ) -> (super::super::snapshot_restore::PreparedRecord, [u8; 32]) {
        let key = encode_logical_record_key(&locator).unwrap();
        let record = encode_logical_record(&envelope).unwrap();
        let path = staging.join(&record.hash);
        fs::write(&path, &record.bytes).unwrap();
        let digest = hex::decode(&record.hash).unwrap().try_into().unwrap();
        (
            super::super::snapshot_restore::PreparedRecord {
                key,
                content_hash: record.hash,
                byte_length: record.size,
                source: super::super::content_store::ObjectSource::File(path),
            },
            digest,
        )
    }

    fn root_at(staging: &std::path::Path, marker: &str)
        -> (super::super::snapshot_restore::PreparedRecord, [u8; 32])
    {
        staged_record(staging, LogicalRecordLocator::Root, LogicalRecordEnvelope::Root {
            value: json!({"marker": marker}), owner_heads: vec![],
        })
    }

    fn character_at(staging: &std::path::Path, id: &str, index: u64)
        -> (super::super::snapshot_restore::PreparedRecord, [u8; 32])
    {
        staged_record(
            staging,
            LogicalRecordLocator::Character { character_id: id.into() },
            LogicalRecordEnvelope::Character {
                configured_index: index,
                detail: json!({"chaId":id,"name":id,"chatPage":0,"lastInteraction":0}),
                owner_heads: vec![crate::logical_records::LogicalOwnerHead::absent(
                    crate::logical_records::LogicalOwnerLocator::CharacterAdditional {
                        character_id: id.into(),
                    },
                )],
            },
        )
    }

    fn preset_at(staging: &std::path::Path, id: &str)
        -> (super::super::snapshot_restore::PreparedRecord, [u8; 32])
    {
        staged_record(
            staging,
            LogicalRecordLocator::Preset { preset_id: id.into() },
            LogicalRecordEnvelope::Preset { configured_index: 0, value: json!({"name": id}) },
        )
    }

    fn conversation_at(staging: &std::path::Path, character: &str, id: &str, recent_at: i64)
        -> (super::super::snapshot_restore::PreparedRecord, [u8; 32])
    {
        staged_record(
            staging,
            LogicalRecordLocator::Conversation {
                character_id: character.into(), conversation_id: id.into(),
            },
            LogicalRecordEnvelope::Conversation {
                configured_index: 0, recent_at,
                detail: json!({"id": id, "name": id}), message_page_hashes: vec![],
            },
        )
    }

    fn staged_snapshot(
        staging: &std::path::Path,
        id: &str,
        records: Vec<(super::super::snapshot_restore::PreparedRecord, [u8; 32])>,
    ) -> super::super::snapshot_restore::PreparedRemoteSnapshot {
        let hashes: BTreeMap<String, [u8; 32]> = records
            .iter()
            .map(|(record, digest)| (record.key.clone(), *digest))
            .collect();
        let library_fingerprint =
            hex::encode(fingerprint(&library_fingerprint_domain(), &hashes));
        super::super::snapshot_restore::PreparedRemoteSnapshot {
            snapshot_id: id.into(), repository_id: "repository".into(),
            fingerprint: library_fingerprint.clone(), library_fingerprint, logical_revision: 7,
            staging_root: staging.to_path_buf(), captured_by_device: None, objects: vec![],
            records: records.into_iter().map(|(record, _)| record).collect(),
        }
    }

    /// A library that has already received one snapshot, so the next one has a
    /// view to be compared against.
    fn received_library() -> (tempfile::TempDir, PersistentStore, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let epoch = store.external_selection().unwrap().epoch;
        store.external_select(&epoch, &crate::persistent_store::sync_selection::SyncTarget::External("connection".into())).unwrap();
        store.device_store_mut().unwrap().set_section_participating(PdsSection::LocalPlugins, true).unwrap();
        let staging = directory.path().join("received");
        fs::create_dir(&staging).unwrap();
        let job = bind(&mut store, "first");
        let first = first_snapshot(&staging, "first");
        let prepared = prepare(&mut store, &job, first);
        let identity = job.admission_identity.clone();
        activate_prepared_receive(&mut store, &prepared, &identity).unwrap();
        (directory, store, staging)
    }

    fn first_snapshot(staging: &std::path::Path, id: &str)
        -> super::super::snapshot_restore::PreparedRemoteSnapshot
    {
        staged_snapshot(staging, id, vec![
            root_at(staging, "remote"),
            character_at(staging, "kept", 0),
            character_at(staging, "dropped", 1),
            conversation_at(staging, "kept", "chat", 10),
            // The removed character takes this with it, without the snapshot
            // having to name it.
            conversation_at(staging, "dropped", "gone", 5),
            preset_at(staging, "preset"),
        ])
    }

    fn second_snapshot(staging: &std::path::Path)
        -> super::super::snapshot_restore::PreparedRemoteSnapshot
    {
        // A conversation whose character arrives in the same difference, ahead
        // of that character, because catalog order is not an apply order.
        staged_snapshot(staging, "second", vec![
            conversation_at(staging, "added", "fresh", 30),
            root_at(staging, "moved"),
            character_at(staging, "kept", 0),
            character_at(staging, "added", 2),
            conversation_at(staging, "kept", "chat", 20),
            preset_at(staging, "preset"),
        ])
    }

    fn touched_keys(store: &PersistentStore, revision: i64) -> Vec<(String, String, String)> {
        let mut query = store.library_rows().prepare(
            "SELECT kind,key1,key2 FROM content_changes WHERE revision=?1 ORDER BY kind,key1,key2",
        ).unwrap();
        let rows = query.query_map([revision], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
        rows.map(|row| row.unwrap()).collect()
    }


    /// A23 and §10.4's no-op. A snapshot that moves nothing has nothing to
    /// stage, so the staging phase says nothing rather than replacing what the
    /// download reported with a pair of zeroes.
    #[test]
    fn c_a_receive_with_nothing_to_move_leaves_the_reading_before_it() {
        use crate::external_storage::phase_progress::PhaseProgress;
        let (_directory, mut store, staging) = received_library();
        let revision = store.revision().unwrap();
        let job = bind(&mut store, "again");
        let readings = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let collected = std::sync::Arc::clone(&readings);
        let progress = PhaseProgress::new(move |reading| collected.lock().unwrap().push(reading));
        let prepared = prepare_counted(&mut store, &job, first_snapshot(&staging, "again"), &progress);
        assert!(matches!(prepared.apply, PreparedApply::Difference(_)));
        assert!(readings.lock().unwrap().is_empty());
        assert_eq!(progress.read().total_items, 0);

        // It is still a receive, so it still commits what the snapshot names.
        let identity = job.admission_identity.clone();
        assert_eq!(
            activate_prepared_receive(&mut store, &prepared, &identity).unwrap(),
            revision + 1,
        );
    }

    /// A23. A receive counts the staging it does, which is the part of a
    /// receive that costs. A replacement stages every record the snapshot
    /// names as well as every body; a difference stages the bodies and leaves
    /// the record rows it is not moving alone, so the same snapshot plans
    /// fewer items for it.
    #[test]
    fn c_a_receive_counts_what_it_stages_and_a_difference_stages_less() {
        use crate::external_storage::phase_progress::PhaseProgress;
        let (_directory, mut store, staging) = received_library();
        let job = bind(&mut store, "second");
        let identity = job.admission_identity.clone();
        let prepared = prepare(&mut store, &job, second_snapshot(&staging));
        activate_prepared_receive(&mut store, &prepared, &identity).unwrap();

        // One conversation moved and everything else stayed where it was.
        let third = |staging: &std::path::Path| staged_snapshot(staging, "third", vec![
            conversation_at(staging, "added", "fresh", 30),
            root_at(staging, "moved"),
            character_at(staging, "kept", 0),
            character_at(staging, "added", 2),
            conversation_at(staging, "kept", "chat", 21),
            preset_at(staging, "preset"),
        ]);
        let job = bind(&mut store, "third");
        let readings = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let collected = std::sync::Arc::clone(&readings);
        let progress = PhaseProgress::new(move |reading| collected.lock().unwrap().push(reading));
        let prepared = prepare_counted(&mut store, &job, third(&staging), &progress);
        assert!(matches!(prepared.apply, PreparedApply::Difference(_)));
        let difference = progress.read();
        assert_eq!(difference.total_items, 1);
        assert_eq!(difference.items, difference.total_items);
        assert_eq!(difference.bytes, difference.total_bytes);
        // The phase ends on a written reading rather than on whichever tick
        // the report interval last allowed through.
        assert_eq!(readings.lock().unwrap().last().copied().unwrap(), difference);

        // The same snapshot arriving at a library with nothing to compare it
        // against is a replacement, and a replacement stages every record.
        let other = tempfile::tempdir().unwrap();
        let mut fresh = PersistentStore::open(other.path()).unwrap();
        let epoch = fresh.external_selection().unwrap().epoch;
        fresh.external_select(
            &epoch,
            &crate::persistent_store::sync_selection::SyncTarget::External("connection".into()),
        ).unwrap();
        fresh.device_store_mut().unwrap()
            .set_section_participating(PdsSection::LocalPlugins, true).unwrap();
        let fresh_job = bind(&mut fresh, "third");
        let replacement = PhaseProgress::silent();
        let replaced = prepare_counted(&mut fresh, &fresh_job, third(&staging), &replacement);
        assert!(matches!(replaced.apply, PreparedApply::Replace(_)));
        let reading = replacement.read();
        assert_eq!(reading.total_items, 6);
        assert_eq!(reading.items, reading.total_items);
    }

    /// A17. What the snapshot moved is what the library records as touched,
    /// and the generation the records live in does not change, so every row
    /// the snapshot left alone is the row it already was.
    #[test]
    fn c_a_normal_receive_touches_only_what_the_snapshot_moved() {
        let (_directory, mut store, staging) = received_library();
        let generation = store.external_active_generation().unwrap();
        let job = bind(&mut store, "second");
        let prepared = prepare(&mut store, &job, second_snapshot(&staging));
        assert!(matches!(prepared.apply, PreparedApply::Difference(_)));
        let identity = job.admission_identity.clone();
        let revision = activate_prepared_receive(&mut store, &prepared, &identity).unwrap();
        assert_eq!(store.external_active_generation().unwrap(), generation);
        // The character the snapshot left alone is still stamped, because its
        // conversation moved and recounting its conversations writes its row.
        // Its own record was neither read nor rewritten.
        assert_eq!(touched_keys(&store, revision), vec![
            ("character".to_owned(), "added".to_owned(), String::new()),
            ("character".to_owned(), "dropped".to_owned(), String::new()),
            ("character".to_owned(), "kept".to_owned(), String::new()),
            ("conversation".to_owned(), "added".to_owned(), "fresh".to_owned()),
            ("conversation".to_owned(), "dropped".to_owned(), "gone".to_owned()),
            ("conversation".to_owned(), "kept".to_owned(), "chat".to_owned()),
            ("owner".to_owned(), "character-additional-assets".to_owned(), "added".to_owned()),
            ("owner".to_owned(), "character-additional-assets".to_owned(), "dropped".to_owned()),
            ("root".to_owned(), String::new(), String::new()),
        ]);
    }

    /// The difference is what reaches the library, so a record the snapshot
    /// changed arrives, one it added appears, one it stopped naming goes, and
    /// one it left alone stays.
    #[test]
    fn c_a_normal_receive_applies_exactly_the_difference() {
        let (_directory, mut store, staging) = received_library();
        let job = bind(&mut store, "second");
        let prepared = prepare(&mut store, &job, second_snapshot(&staging));
        let identity = job.admission_identity.clone();
        activate_prepared_receive(&mut store, &prepared, &identity).unwrap();
        let generation = store.external_active_generation().unwrap();
        let characters: Vec<String> = {
            let mut query = store.library_rows().prepare(
                "SELECT character_id FROM characters WHERE generation=?1 ORDER BY character_id",
            ).unwrap();
            let rows = query.query_map([&generation], |row| row.get(0)).unwrap();
            rows.map(|row| row.unwrap()).collect()
        };
        assert_eq!(characters, ["added", "kept"]);
        let marker: String = store.library_rows().query_row(
            "SELECT json_extract(value,'$.marker') FROM root WHERE generation=?1",
            [&generation], |row| row.get(0),
        ).unwrap();
        assert_eq!(marker, "moved");
        assert_eq!(
            store.external_base("connection").unwrap().unwrap().snapshot_id,
            "second"
        );
        // The map is written key by key rather than rewritten, so this is what
        // says the keys it wrote are exactly the ones that moved.
        let named: std::collections::BTreeMap<String, String> = second_snapshot(&staging)
            .records
            .into_iter()
            .map(|record| (record.key, record.content_hash))
            .collect();
        assert_eq!(store.external_base_records("connection").unwrap(), Some(named));
    }

    /// A16. The library moved under the prepared receive, so the difference it
    /// measured is no longer the difference, and the edit stays.
    #[test]
    fn c_a_local_edit_under_a_prepared_difference_keeps_the_edit() {
        let (_directory, mut store, staging) = received_library();
        let job = bind(&mut store, "second");
        let prepared = prepare(&mut store, &job, second_snapshot(&staging));
        let identity = job.admission_identity.clone();
        let revision = store.revision().unwrap();
        local_edit(&mut store, revision);
        let error = activate_prepared_receive(&mut store, &prepared, &identity).unwrap_err();
        assert_eq!(error.kind, ErrorKind::PreconditionFailed);
        let generation = store.external_active_generation().unwrap();
        let marker: String = store.library_rows().query_row(
            "SELECT json_extract(value,'$.marker') FROM root WHERE generation=?1",
            [&generation], |row| row.get(0),
        ).unwrap();
        assert_eq!(marker, "newer-local");
        assert_eq!(
            store.external_base("connection").unwrap().unwrap().snapshot_id,
            "first"
        );
    }

    /// A snapshot downloaded against a base, with the records that base
    /// already holds left unfetched.
    fn withheld(store: &PersistentStore, mut snapshot: super::super::snapshot_restore::PreparedRemoteSnapshot,
        also: &[&str]) -> super::super::snapshot_restore::PreparedRemoteSnapshot
    {
        let base = store.external_base_records("connection").unwrap().unwrap();
        for record in &mut snapshot.records {
            if base.get(&record.key) == Some(&record.content_hash)
                || also.iter().any(|key| record.key.contains(key))
            {
                record.source = super::super::content_store::ObjectSource::Unchanged;
            }
        }
        snapshot
    }

    fn preparation_within(store: &mut PersistentStore, job: &DurableJob,
        downloaded: super::super::snapshot_restore::PreparedRemoteSnapshot,
        budget: super::super::receive_difference::DifferenceBudget) -> Preparation
    {
        let participation = receive_participation(store).unwrap();
        let section = plugin_section(&downloaded.staging_root);
        prepare_receive_within(store, job, job.admission_identity.clone(), "authenticated-head".into(),
            downloaded, vec![section], participation,
            &crate::external_storage::phase_progress::PhaseProgress::silent(),
            &Cancellation::default(), budget).unwrap()
    }

    /// R04. Records the base already holds are never fetched for a difference,
    /// which applies without them. Every route that would need one of them
    /// (too many records, too much dependent work, or a record that moved
    /// after the download chose what to skip) hands the receive back before
    /// staging anything, and the full download then lands the same library.
    #[test]
    fn c_a_receive_without_its_unchanged_records_applies_or_asks_for_them() {
        let (_directory, mut store, staging) = received_library();
        let revision = store.revision().unwrap();
        let job = bind(&mut store, "second");
        let skipped = withheld(&store, second_snapshot(&staging), &[]);
        assert_eq!(skipped.records.iter()
            .filter(|record| unfetched(&record.source)).count(), 2);
        let refusing = [
            ("records", super::super::receive_difference::DifferenceBudget {
                records: 0, ..RECEIVE_DIFFERENCE_BUDGET
            }, &[][..]),
            ("dependents", budget(0, 0), &[][..]),
            ("moved", RECEIVE_DIFFERENCE_BUDGET, &["root"][..]),
        ];
        for (case, within, also) in refusing {
            let snapshot = withheld(&store, second_snapshot(&staging), also);
            assert!(matches!(preparation_within(&mut store, &job, snapshot, within),
                Preparation::NeedsRecords), "{case}");
            assert_eq!(store.revision().unwrap(), revision, "{case}");
            assert_eq!(store.external_job(&job.id).unwrap().unwrap().phase, "ready", "{case}");
        }
        let replaced = prepare_within(&mut store, &job, second_snapshot(&staging),
            super::super::receive_difference::DifferenceBudget { records: 0, ..RECEIVE_DIFFERENCE_BUDGET });
        assert!(matches!(replaced.apply, PreparedApply::Replace(_)));
        let identity = job.admission_identity.clone();
        activate_prepared_receive(&mut store, &replaced, &identity).unwrap();

        let (_other, mut differing, other_staging) = received_library();
        let job = bind(&mut differing, "second");
        let snapshot = withheld(&differing, second_snapshot(&other_staging), &[]);
        let prepared = prepare_within(&mut differing, &job, snapshot, RECEIVE_DIFFERENCE_BUDGET);
        assert!(matches!(prepared.apply, PreparedApply::Difference(_)));
        let identity = job.admission_identity.clone();
        activate_prepared_receive(&mut differing, &prepared, &identity).unwrap();
        assert_eq!(differing.materialize(None).unwrap(), store.materialize(None).unwrap());
    }

    /// A18. A record that cannot be applied fails the whole transaction, so the
    /// revision, the base and the job's completion are all still the old ones.
    #[test]
    fn c_a_refused_record_leaves_the_revision_base_and_job_where_they_were() {
        let (_directory, mut store, staging) = received_library();
        let before = store.revision().unwrap();
        let job = bind(&mut store, "second");
        let second = second_snapshot(&staging);
        // The same length and a hash the catalog names, with other bytes behind
        // it, so only reading the record can tell.
        let poisoned = second.records.iter().find(|record| record.key.contains("root")).unwrap();
        let super::super::content_store::ObjectSource::File(path) = &poisoned.source else {
            unreachable!("staged record is a file")
        };
        let mut bytes = fs::read(path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(path, &bytes).unwrap();
        // Every record is read before the apply, so the refusal comes from
        // preparing and nothing is left for the apply to attempt.
        let participation = receive_participation(&mut store).unwrap();
        let section = plugin_section(&second.staging_root);
        assert!(prepare_receive_input(&mut store, &job, job.admission_identity.clone(),
            "authenticated-head".into(), second, vec![section], participation,
            &crate::external_storage::phase_progress::PhaseProgress::silent(),
            &Cancellation::default()).is_err());
        assert_eq!(store.revision().unwrap(), before);
        assert_eq!(
            store.external_base("connection").unwrap().unwrap().snapshot_id,
            "first"
        );
        assert_eq!(store.external_job(&job.id).unwrap().unwrap().phase, "ready");
    }


    fn staged_object(staging: &std::path::Path, bytes: &[u8])
        -> super::super::snapshot_restore::PreparedObject
    {
        let digest = hex::encode(hash(bytes));
        let path = staging.join(format!("object-{digest}"));
        fs::write(&path, bytes).unwrap();
        super::super::snapshot_restore::PreparedObject {
            content_hash: digest, byte_length: bytes.len() as u64,
            source: super::super::content_store::ObjectSource::File(path),
        }
    }

    fn synthetic_messages(conversation: &str, count: usize) -> Vec<Value> {
        (0..count).map(|index| json!({
            "chatId": format!("{conversation}-{index}"), "role": "user",
            "data": format!("{conversation} message {index}"),
        })).collect()
    }

    /// A conversation whose messages arrive as full pages, with the page
    /// objects the snapshot carries for it.
    fn paged_conversation_at(staging: &std::path::Path, character: &str, id: &str, messages: usize)
        -> ((super::super::snapshot_restore::PreparedRecord, [u8; 32]),
            Vec<super::super::snapshot_restore::PreparedObject>)
    {
        let mut pages = Vec::new();
        let mut objects = Vec::new();
        for chunk in synthetic_messages(id, messages).chunks(crate::logical_records::LOGICAL_MESSAGE_PAGE_SIZE) {
            let page = crate::logical_records::encode_message_page(chunk).unwrap();
            objects.push(staged_object(staging, &page.bytes));
            pages.push(page.hash);
        }
        let record = staged_record(
            staging,
            LogicalRecordLocator::Conversation {
                character_id: character.into(), conversation_id: id.into(),
            },
            LogicalRecordEnvelope::Conversation {
                configured_index: 0, recent_at: 1,
                detail: json!({"id": id, "name": id}), message_page_hashes: pages,
            },
        );
        (record, objects)
    }

    /// A root whose module assets are an owner manifest of `entries` entries.
    fn owned_root_at(staging: &std::path::Path, entries: usize)
        -> ((super::super::snapshot_restore::PreparedRecord, [u8; 32]),
            Vec<super::super::snapshot_restore::PreparedObject>)
    {
        use crate::asset_repository::owner_manifest_codec::{encode_owner_manifest, OwnerManifestEntry};
        let payloads: Vec<_> = (0..entries)
            .map(|index| staged_object(staging, format!("synthetic module asset {index}").as_bytes()))
            .collect();
        let manifest = encode_owner_manifest(&payloads.iter().enumerate().map(|(index, payload)| OwnerManifestEntry {
            tuple: ["asset".into(), format!("asset-{index}.bin"), "binary".into()],
            payload_hash: Some(hex::decode(&payload.content_hash).unwrap().try_into().unwrap()),
        }).collect::<Vec<_>>()).unwrap();
        let manifest = staged_object(staging, &manifest);
        let record = staged_record(staging, LogicalRecordLocator::Root, LogicalRecordEnvelope::Root {
            value: json!({"marker": "remote", "modules": [{"name": "module"}]}),
            owner_heads: vec![crate::logical_records::LogicalOwnerHead::present(
                crate::logical_records::LogicalOwnerLocator::RootModule { index: 0 },
                manifest.content_hash.clone(), entries as u64, 1,
            ).unwrap()],
        });
        (record, std::iter::once(manifest).chain(payloads).collect())
    }

    /// How a library changes between the snapshot it received and the next.
    #[derive(Clone, Copy, Debug)]
    enum Moved {
        /// "kept/chat" gains this many messages.
        Conversation(usize),
        /// The root's module assets become a manifest of this many entries.
        Owner(usize),
        /// "dropped" and its conversation leave the library.
        Removal,
    }

    /// The first snapshot a library received: "dropped" carries `history`
    /// messages in its one conversation.
    fn library_snapshot(staging: &std::path::Path, id: &str, history: usize, moved: Option<Moved>)
        -> super::super::snapshot_restore::PreparedRemoteSnapshot
    {
        let (root, mut objects) = match moved {
            Some(Moved::Owner(entries)) => owned_root_at(staging, entries),
            _ => (root_at(staging, "remote"), Vec::new()),
        };
        let chat = match moved {
            Some(Moved::Conversation(messages)) => {
                let (record, pages) = paged_conversation_at(staging, "kept", "chat", messages);
                objects.extend(pages);
                record
            }
            _ => conversation_at(staging, "kept", "chat", 10),
        };
        let mut records = vec![root, character_at(staging, "kept", 0), chat, preset_at(staging, "preset")];
        if !matches!(moved, Some(Moved::Removal)) {
            let (gone, pages) = paged_conversation_at(staging, "dropped", "gone", history);
            objects.extend(pages);
            records.push(character_at(staging, "dropped", 1));
            records.push(gone);
        }
        let mut snapshot = staged_snapshot(staging, id, records);
        snapshot.objects = objects;
        snapshot
    }

    fn library_with_history(history: usize) -> (tempfile::TempDir, PersistentStore, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let epoch = store.external_selection().unwrap().epoch;
        store.external_select(&epoch, &crate::persistent_store::sync_selection::SyncTarget::External("connection".into())).unwrap();
        store.device_store_mut().unwrap().set_section_participating(PdsSection::LocalPlugins, true).unwrap();
        let staging = directory.path().join("received");
        fs::create_dir(&staging).unwrap();
        let job = bind(&mut store, "first");
        let prepared = prepare(&mut store, &job, library_snapshot(&staging, "first", history, None));
        let identity = job.admission_identity.clone();
        activate_prepared_receive(&mut store, &prepared, &identity).unwrap();
        (directory, store, staging)
    }

    fn budget(dependent_bytes: u64, work: u64) -> super::super::receive_difference::DifferenceBudget {
        super::super::receive_difference::DifferenceBudget {
            records: RECEIVE_DIFFERENCE_BUDGET.records, bytes: RECEIVE_DIFFERENCE_BUDGET.bytes,
            dependent_bytes, work,
        }
    }

    fn prepare_within(store: &mut PersistentStore, job: &DurableJob,
        downloaded: super::super::snapshot_restore::PreparedRemoteSnapshot,
        budget: super::super::receive_difference::DifferenceBudget) -> PreparedReceive
    {
        let participation = receive_participation(store).unwrap();
        let section = plugin_section(&downloaded.staging_root);
        prepare_receive_within(store, job, job.admission_identity.clone(), "authenticated-head".into(),
            downloaded, vec![section], participation,
            &crate::external_storage::phase_progress::PhaseProgress::silent(),
            &Cancellation::default(), budget).unwrap().ready()
    }

    fn remove_from_cas(directory: &std::path::Path, hashes: &[String]) {
        let cas = crate::asset_repository::PayloadCas::new(directory).unwrap();
        for hash in hashes {
            fs::remove_file(cas.object_path(hash).unwrap().unwrap()).unwrap();
        }
    }

    fn character<'a>(library: &'a Value, id: &str) -> Option<&'a Value> {
        library["characters"].as_array().unwrap().iter().find(|character| character["chaId"] == id)
    }

    /// R05. Everything the rows are made from is read before the apply takes
    /// the writer, so the apply succeeds with the record files, the message
    /// pages and the manifest all gone from where they were read.
    #[test]
    fn c_a_difference_reads_every_body_before_it_takes_the_writer() {
        for moved in [Moved::Conversation(300), Moved::Owner(3)] {
            let (directory, mut store, staging) = library_with_history(0);
            let job = bind(&mut store, "second");
            let snapshot = library_snapshot(&staging, "second", 0, Some(moved));
            let read: Vec<String> = match moved {
                // Pages are consumed by the apply; the manifest stays named by
                // the owner head, so only its staged file goes.
                Moved::Conversation(_) => snapshot.objects.iter()
                    .map(|object| object.content_hash.clone()).collect(),
                _ => Vec::new(),
            };
            let prepared = prepare(&mut store, &job, snapshot);
            assert!(matches!(prepared.apply, PreparedApply::Difference(_)), "{moved:?}");
            for entry in fs::read_dir(&staging).unwrap() {
                let path = entry.unwrap().path();
                if path.is_file() {
                    fs::remove_file(path).unwrap();
                }
            }
            remove_from_cas(directory.path(), &read);
            let identity = job.admission_identity.clone();
            activate_prepared_receive(&mut store, &prepared, &identity).unwrap();
            let library = store.materialize(None).unwrap();
            match moved {
                Moved::Conversation(count) => {
                    let chat = &character(&library, "kept").unwrap()["chats"][0];
                    assert_eq!(chat["message"], Value::Array(synthetic_messages("chat", count)));
                }
                Moved::Owner(entries) => {
                    let assets = library["modules"][0]["assets"].as_array().unwrap();
                    assert_eq!(assets.len(), entries);
                    assert_eq!(assets[2], json!(["asset", "asset-2.bin", "binary"]));
                }
                Moved::Removal => unreachable!(),
            }
        }
        // The pages really are read: without them the receive cannot be prepared.
        let (_directory, mut store, staging) = library_with_history(0);
        let job = bind(&mut store, "second");
        let snapshot = library_snapshot(&staging, "second", 0, Some(Moved::Conversation(300)));
        for object in &snapshot.objects {
            let super::super::content_store::ObjectSource::File(path) = &object.source else {
                unreachable!("staged object is a file")
            };
            fs::remove_file(path).unwrap();
        }
        let participation = receive_participation(&mut store).unwrap();
        let section = plugin_section(&snapshot.staging_root);
        assert!(prepare_receive_input(&mut store, &job, job.admission_identity.clone(),
            "authenticated-head".into(), snapshot, vec![section], participation,
            &crate::external_storage::phase_progress::PhaseProgress::silent(),
            &Cancellation::default()).is_err());
    }

    /// R05. The route counts what a few keys bring with them: a conversation's
    /// pages at 128 rows each, a manifest's length and the bodies it names,
    /// and the rows a removal deletes from this library. One unit under the exact figure
    /// stages the whole snapshot instead, and either route lands the same
    /// library.
    #[test]
    fn c_dependent_work_decides_the_route_and_both_routes_land_the_same_library() {
        let manifest_length = {
            let staging = tempfile::tempdir().unwrap();
            owned_root_at(staging.path(), 3).1[0].byte_length
        };
        let page_length = |messages: usize| {
            let staging = tempfile::tempdir().unwrap();
            paged_conversation_at(staging.path(), "kept", "chat", messages).1
                .iter().map(|object| object.byte_length).sum::<u64>()
        };
        let unbounded = u64::MAX;
        // (history of "dropped", what moves, dependent bytes, work)
        let cases = [
            // One changed record and three full pages.
            (0, Moved::Conversation(384), page_length(384), 1 + 3 * 128),
            // One changed record, and the manifest and its three payloads
            // confirmed as the rows are written.
            (0, Moved::Owner(3), manifest_length,
                1 + 4 * crate::persistent_store::external_apply::PRESENCE_CHECK_WORK),
            // Two removed keys, and the 300 messages and one conversation the
            // character takes with it.
            (300, Moved::Removal, 0, 2 + 300 + 1),
            // The same removal against a library that holds no history.
            (0, Moved::Removal, 0, 2 + 1),
        ];
        for (history, moved, bytes, work) in cases {
            let mut landed = Vec::new();
            for (limit, in_place) in [
                (budget(bytes, work), true),
                (budget(bytes, work - 1), false),
                (budget(bytes.saturating_sub(1), unbounded), bytes == 0),
            ] {
                let (_directory, mut store, staging) = library_with_history(history);
                let job = bind(&mut store, "second");
                let snapshot = library_snapshot(&staging, "second", history, Some(moved));
                let prepared = prepare_within(&mut store, &job, snapshot, limit);
                assert_eq!(matches!(prepared.apply, PreparedApply::Difference(_)), in_place,
                    "{moved:?} with history {history} under {limit:?}");
                let identity = job.admission_identity.clone();
                activate_prepared_receive(&mut store, &prepared, &identity).unwrap();
                landed.push(store.materialize(None).unwrap());
            }
            assert!(landed.windows(2).all(|pair| pair[0] == pair[1]), "{moved:?}");
            let library = &landed[0];
            match moved {
                Moved::Conversation(count) => assert_eq!(
                    character(library, "kept").unwrap()["chats"][0]["message"],
                    Value::Array(synthetic_messages("chat", count))),
                Moved::Owner(entries) => assert_eq!(
                    library["modules"][0]["assets"].as_array().unwrap().len(), entries),
                Moved::Removal => assert!(character(library, "dropped").is_none()),
            }
        }
    }

    /// R05. A body the rows go on naming is confirmed when they are written,
    /// not only when it was read: a manifest gone after preparing stops the
    /// apply and leaves the library where it was.
    #[test]
    fn c_a_manifest_lost_after_preparing_stops_the_apply() {
        let (directory, mut store, staging) = library_with_history(0);
        let before = store.revision().unwrap();
        let held = store.materialize(None).unwrap();
        let job = bind(&mut store, "second");
        let snapshot = library_snapshot(&staging, "second", 0, Some(Moved::Owner(3)));
        let manifest = snapshot.objects[0].content_hash.clone();
        let prepared = prepare(&mut store, &job, snapshot);
        assert!(matches!(prepared.apply, PreparedApply::Difference(_)));
        remove_from_cas(directory.path(), &[manifest]);
        let identity = job.admission_identity.clone();
        assert!(activate_prepared_receive(&mut store, &prepared, &identity).is_err());
        assert_eq!(store.revision().unwrap(), before);
        assert_eq!(store.materialize(None).unwrap(), held);
    }

    /// What the budget bounds is how long a receive holds the active database.
    /// Run with `--ignored --nocapture` to read the arithmetic behind the
    /// constant: the incremental hold grows with the difference, the replace
    /// path's hold grows with the library.
    #[test]
    #[ignore = "measurement"]
    fn measures_what_a_receive_holds_the_active_database_for() {
        const LIBRARY: usize = 20_000;
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let epoch = store.external_selection().unwrap().epoch;
        store.external_select(&epoch, &crate::persistent_store::sync_selection::SyncTarget::External("connection".into())).unwrap();
        store.device_store_mut().unwrap().set_section_participating(PdsSection::LocalPlugins, true).unwrap();
        let staging = directory.path().join("received");
        fs::create_dir(&staging).unwrap();

        let library = |revision: usize, changed: usize| {
            let mut records = vec![root_at(&staging, "remote")];
            for index in 0..LIBRARY {
                let id = format!("character-{index:06}");
                records.push(character_at(&staging, &id, index as u64));
                let moved = if index < changed { revision as i64 } else { 0 };
                records.push(conversation_at(&staging, &id, "chat", moved));
            }
            records
        };

        let job = bind(&mut store, "first");
        let first = staged_snapshot(&staging, "first", library(0, 0));
        let batches = std::rc::Rc::new(std::cell::RefCell::new((0usize, std::time::Duration::ZERO)));
        let seen = batches.clone();
        crate::persistent_store::external_apply::after_stage_batch(Some(Box::new(move |written, held| {
            let mut seen = seen.borrow_mut();
            *seen = (written, seen.1.max(held));
        })));
        let started = std::time::Instant::now();
        let prepared = prepare(&mut store, &job, first);
        let staged_ms = started.elapsed().as_millis();
        crate::persistent_store::external_apply::after_stage_batch(None);
        let (stage_batches, longest_batch) = *batches.borrow();
        assert!(matches!(prepared.apply, PreparedApply::Replace(_)));
        let identity = job.admission_identity.clone();
        let started = std::time::Instant::now();
        activate_prepared_receive(&mut store, &prepared, &identity).unwrap();
        let replace_hold_ms = started.elapsed().as_millis();

        let mut holds = Vec::new();
        for (revision, changed) in [(1usize, 1usize), (2, 10), (3, 100), (4, 1000)] {
            assert!(changed <= RECEIVE_DIFFERENCE_BUDGET.records);
            let job = bind(&mut store, &format!("snapshot-{revision}"));
            let snapshot = staged_snapshot(&staging, &format!("snapshot-{revision}"), library(revision, changed));
            let started = std::time::Instant::now();
            let prepared = prepare(&mut store, &job, snapshot);
            let prepare_ms = started.elapsed().as_millis();
            assert!(matches!(prepared.apply, PreparedApply::Difference(_)),
                "{changed} changed records must stay inside the budget");
            let identity = job.admission_identity.clone();
            let started = std::time::Instant::now();
            activate_prepared_receive(&mut store, &prepared, &identity).unwrap();
            holds.push((changed, prepare_ms, started.elapsed().as_millis()));
        }
        println!("library={LIBRARY} records={} staged_ms={staged_ms} stage_batches={stage_batches} longest_batch_ms={} replace_hold_ms={replace_hold_ms}",
            LIBRARY * 2 + 1, longest_batch.as_millis());
        for (changed, prepare_ms, hold_ms) in holds {
            println!("changed={changed} prepare_ms={prepare_ms} incremental_hold_ms={hold_ms}");
        }

        // What a few keys bring with them, applied in place whatever the
        // budget would say: one conversation's pages, one character's whole
        // history leaving, one root's owner manifest.
        type WithObjects = ((super::super::snapshot_restore::PreparedRecord, [u8; 32]),
            Vec<super::super::snapshot_restore::PreparedObject>);
        let conversation = |index: usize| format!("character-{index:06}");
        let mut paged: BTreeMap<usize, WithObjects> = BTreeMap::new();
        let snapshot_of = |id: &str, paged: &BTreeMap<usize, WithObjects>, removed: Option<usize>,
            root: Option<WithObjects>| {
            let mut records = library(4, 1000);
            let mut objects = Vec::new();
            for (index, (record, pages)) in paged {
                records[2 + index * 2] = record.clone();
                objects.extend(pages.iter().cloned());
            }
            if let Some(index) = removed {
                records.drain(1 + index * 2..3 + index * 2);
            }
            if let Some((record, owner)) = root {
                records[0] = record;
                objects.extend(owner);
            }
            // Conversations that share messages share pages, and a catalog
            // names each object once.
            let mut seen = std::collections::BTreeSet::new();
            objects.retain(|object: &super::super::snapshot_restore::PreparedObject|
                seen.insert(object.content_hash.clone()));
            let mut snapshot = staged_snapshot(&staging, id, records);
            snapshot.objects = objects;
            snapshot
        };
        let unbounded = budget(u64::MAX, u64::MAX);
        let mut dependents = Vec::new();
        let mut apply = |store: &mut PersistentStore, name: String, snapshot| {
            let job = bind(store, &name);
            let started = std::time::Instant::now();
            let prepared = prepare_within(store, &job, snapshot, unbounded);
            let prepare_ms = started.elapsed().as_millis();
            assert!(matches!(prepared.apply, PreparedApply::Difference(_)));
            let identity = job.admission_identity.clone();
            let started = std::time::Instant::now();
            activate_prepared_receive(store, &prepared, &identity).unwrap();
            dependents.push((name, prepare_ms, started.elapsed().as_millis()));
        };
        for (index, pages) in [(0usize, 16usize), (1, 64), (2, 256)] {
            paged.insert(index, paged_conversation_at(&staging, &conversation(index), "chat",
                pages * crate::logical_records::LOGICAL_MESSAGE_PAGE_SIZE));
            let name = format!("pages={pages}");
            let snapshot = snapshot_of(&name, &paged, None, None);
            apply(&mut store, name, snapshot);
        }
        let name = format!("removed_messages={}", 256 * 128);
        let snapshot = snapshot_of(&name, &paged, Some(2), None);
        apply(&mut store, name, snapshot);
        paged.remove(&2);
        for entries in [1_000usize, 10_000] {
            let name = format!("owner_entries={entries}");
            let snapshot = snapshot_of(&name, &paged, Some(2), Some(owned_root_at(&staging, entries)));
            apply(&mut store, name, snapshot);
        }
        for (name, prepare_ms, hold_ms) in dependents {
            println!("{name} prepare_ms={prepare_ms} incremental_hold_ms={hold_ms}");
        }
    }

    fn rows(store: &mut PersistentStore) -> Vec<crate::persistent_store::device_store::sections::SectionRow> {
        store.device_store_mut().unwrap().read_section_rows(PdsSection::LocalPlugins).unwrap()
    }

    /// An object the repository already holds is registered where it is. The
    /// staging directory never had it, so preparing it cannot have read it out
    /// of one.
    #[test]
    fn a_library_object_is_registered_without_being_staged() {
        let (directory, mut store, job, mut downloaded) = fixture();
        let cas = crate::asset_repository::PayloadCas::new(directory.path()).unwrap();
        let held = cas.prepare_bytes(b"asset").unwrap();
        downloaded.objects.push(super::super::snapshot_restore::PreparedObject {
            content_hash: held.content_hash.clone(),
            byte_length: held.byte_size,
            source: super::super::content_store::ObjectSource::Library(held.content_hash.clone()),
        });
        let prepared = prepare(&mut store, &job, downloaded);
        assert_eq!(store.materialize_staging(prepared.apply.staging_id().unwrap())
            .unwrap()["marker"], "remote");
        let path = cas.object_path(&held.content_hash).unwrap().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"asset");
    }

    #[test]
    fn final_apply_dispatcher_uses_only_prepared_local_handles() {
        let (_directory, mut store, job, downloaded) = fixture();
        let source = downloaded.staging_root.clone();
        let prepared = prepare(&mut store, &job, downloaded);
        let stage = prepared.apply.staging_id().unwrap().to_owned();
        assert_eq!(store.revision().unwrap(), 0);
        assert!(rows(&mut store).is_empty());
        assert_eq!(store.materialize_staging(&stage).unwrap()["marker"], "remote");
        // Final apply must need neither the downloaded library nor section sources.
        fs::remove_dir_all(&source).unwrap();
        // Match the runtime: preparation and activation use separate native handles.
        let mut reopened = store.open_native_job_store().unwrap();
        let state = super::super::job_store::JobCommandState::default();
        state.prepared_receives.lock().unwrap().insert(job.id.clone(), prepared);
        let (cancel, claim) = state.claim(&job).unwrap();
        let current = reopened.external_identity().unwrap();
        cancel.check().unwrap();
        super::super::snapshot_restore::reset_test_read_counts(&source);
        assert_eq!(
            commit_prepared_receive(&mut reopened, &state, &claim, &job, 1, &current)
                .unwrap_err().kind,
            ErrorKind::PreconditionFailed,
        );
        let (snapshot_id, received_revision) = commit_prepared_receive(
            &mut reopened,
            &state,
            &claim,
            &job,
            0,
            &current,
        )
        .unwrap();
        assert_eq!(snapshot_id, "snapshot");
        assert_eq!(received_revision, 1);
        assert_eq!(
            super::super::snapshot_restore::take_test_read_counts(&source),
            Default::default()
        );
        assert_eq!(store.materialize(None).unwrap()["marker"], "remote");
        assert_eq!(rows(&mut store).len(), 1);
        assert!(store.prepare_replace_commit(&stage, Some(1)).is_err());
    }

    #[test]
    fn failed_receive_reuses_the_original_stage_and_sections_without_downloads() {
        let (directory, mut store, job, downloaded) = fixture();
        let source = downloaded.staging_root.clone();
        let prepared = prepare(&mut store, &job, downloaded);
        let stage = prepared.apply.staging_id().unwrap().to_owned();
        fs::remove_dir_all(&source).unwrap();
        let injector = rusqlite::Connection::open(directory.path().join("persistent").join(crate::persistent_store::DATABASE_FILE)).unwrap();
        injector.execute_batch("CREATE TRIGGER synthetic_receive_failure BEFORE INSERT ON external_storage_bases BEGIN SELECT RAISE(ABORT,'synthetic'); END").unwrap();
        let state = super::super::job_store::JobCommandState::default();
        state.prepared_receives.lock().unwrap().insert(job.id.clone(), prepared);
        let mut first_rows = None;
        for _ in 0..2 {
            let mut native = store.open_native_job_store().unwrap();
            let (_, claim) = state.claim(&job).unwrap();
            let error = commit_prepared_receive(&mut native, &state, &claim, &job, 0, &job.admission_identity).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Transient);
            assert_eq!(native.external_identity().unwrap(), job.admission_identity);
            assert!(completed_receive_result(&native, &job, 0).unwrap().is_none());
            assert_eq!(native.materialize_staging(&stage).unwrap()["marker"], "remote");
            let partial = rows(&mut native);
            assert_eq!(partial.len(), 1);
            if let Some(first) = &first_rows { assert_eq!(&partial, first); }
            else { first_rows = Some(partial); }
            let retained = state.prepared_receives.lock().unwrap();
            assert_eq!(retained.len(), 1);
            assert_eq!(retained[&job.id].apply.staging_id().unwrap(), stage);
            assert_eq!(retained[&job.id].expected, job.admission_identity);
            assert_eq!(retained[&job.id].snapshot_id, "snapshot");
        }
        injector.execute_batch("DROP TRIGGER synthetic_receive_failure").unwrap();
        let mut native = store.open_native_job_store().unwrap();
        let (_, claim) = state.claim(&job).unwrap();
        assert_eq!(commit_prepared_receive(&mut native, &state, &claim, &job, 0, &job.admission_identity).unwrap(),
            ("snapshot".into(), 1));
        assert!(state.prepared_receives.lock().unwrap().is_empty());
        assert!(!source.exists());
        assert_eq!(rows(&mut native), first_rows.unwrap());
        assert_eq!(native.materialize(None).unwrap()["marker"], "remote");
        assert!(native.prepare_replace_commit(&stage, Some(1)).is_err());
        assert_eq!(completed_receive_result(&native, &job, 0).unwrap().unwrap()["receivedRevision"], "1");
    }

    #[test]
    fn retry_after_a_failed_receive_still_rejects_a_changed_library() {
        let (directory, mut store, job, downloaded) = fixture();
        let prepared = prepare(&mut store, &job, downloaded);
        let stage = prepared.apply.staging_id().unwrap().to_owned();
        let injector = rusqlite::Connection::open(directory.path().join("persistent").join(crate::persistent_store::DATABASE_FILE)).unwrap();
        injector.execute_batch("CREATE TRIGGER synthetic_receive_failure BEFORE INSERT ON external_storage_bases BEGIN SELECT RAISE(ABORT,'synthetic'); END").unwrap();
        let state = super::super::job_store::JobCommandState::default();
        state.prepared_receives.lock().unwrap().insert(job.id.clone(), prepared);
        let (_, claim) = state.claim(&job).unwrap();
        assert_eq!(commit_prepared_receive(&mut store, &state, &claim, &job, 0, &job.admission_identity).unwrap_err().kind,
            ErrorKind::Transient);
        let partial = rows(&mut store);
        assert_eq!(partial.len(), 1);
        injector.execute_batch("DROP TRIGGER synthetic_receive_failure").unwrap();
        local_edit(&mut store, 0);
        let current = store.external_identity().unwrap();
        assert_eq!(commit_prepared_receive(&mut store, &state, &claim, &job, 0, &current).unwrap_err().kind,
            ErrorKind::PreconditionFailed);
        assert!(state.prepared_receives.lock().unwrap().is_empty());
        assert!(store.prepare_replace_commit(&stage, Some(1)).is_err());
        assert_eq!(store.materialize(None).unwrap()["marker"], "newer-local");
        assert!(completed_receive_result(&store, &job, 0).unwrap().is_none());
        // Device-section progress is preserved honestly; this is not rollback.
        assert_eq!(rows(&mut store), partial);
    }

    #[test]
    fn completed_receive_is_stable_after_later_local_writes_and_another_receive() {
        let (_directory, mut store, job, downloaded) = fixture();
        let second_input = downloaded.clone();
        let prepared = prepare(&mut store, &job, downloaded);
        let expected = job.admission_identity.clone();
        activate_prepared_receive(&mut store, &prepared, &expected).unwrap();
        // The auxiliary summary deliberately remains queued, as after a failed cache write.
        assert_eq!(job.summary["state"], "queued");
        local_edit(&mut store, 1);
        let second = bind(&mut store, "second-snapshot");
        let mut second_input = second_input;
        second_input.snapshot_id = "second-snapshot".into();
        let prepared = prepare(&mut store, &second, second_input);
        activate_prepared_receive(&mut store, &prepared, &second.admission_identity).unwrap();
        assert_eq!(store.external_base("connection").unwrap().unwrap().snapshot_id, "second-snapshot");
        let result = completed_receive_result(&store, &job, 0).unwrap().unwrap();
        assert_eq!(result["receivedRevision"], "1");
        assert_eq!(result["snapshotId"], "snapshot");
        assert!(completed_receive_result(&store, &job, 1).is_err());
        assert_eq!(store.revision().unwrap(), 3);
    }

    #[test]
    fn failed_auxiliary_summary_write_does_not_erase_or_repeat_a_committed_receive() {
        let (directory, mut store, mut job, downloaded) = fixture();
        let prepared = prepare(&mut store, &job, downloaded);
        job.receive_staging_id = Some(prepared.apply.staging_id().unwrap().to_owned());
        let cache = JobStore::open(directory.path()).unwrap();
        cache.put(&job).unwrap();
        let injector = rusqlite::Connection::open(directory.path().join("external-jobs.sqlite")).unwrap();
        injector.execute_batch(
            "CREATE TRIGGER synthetic_summary_failure BEFORE UPDATE ON external_requests
             BEGIN SELECT RAISE(ABORT,'synthetic summary failure'); END;"
        ).unwrap();
        activate_prepared_receive(&mut store, &prepared, &job.admission_identity).unwrap();
        job.summary["state"] = json!("succeeded");
        job.receive_staging_id = None;
        assert!(cache.put(&job).is_err());
        let stale = cache.read(&job.id).unwrap();
        assert_eq!(stale.summary["state"], "queued");
        assert!(stale.receive_staging_id.is_some());
        for _ in 0..3 {
            assert_eq!(completed_receive_result(&store, &stale, 0).unwrap().unwrap()["receivedRevision"], "1");
        }
        assert_eq!(store.revision().unwrap(), 1);
        assert_eq!(rows(&mut store).len(), 1);
    }

    #[test]
    fn selection_changes_invalidate_ready_receives_before_device_rows_are_changed() {
        let (_directory, mut store, job, downloaded) = fixture();
        let prepared = prepare(&mut store, &job, downloaded);
        let epoch = store.external_selection().unwrap().epoch;
        store.external_select(&epoch, &crate::persistent_store::sync_selection::SyncTarget::None).unwrap();
        assert_eq!(store.external_job(&job.id).unwrap().unwrap().phase, "stale");
        let current = store.external_identity().unwrap();
        assert!(activate_prepared_receive(&mut store, &prepared, &current).is_err());
        assert_eq!(store.revision().unwrap(), 0);
        assert!(rows(&mut store).is_empty());
    }

    #[test]
    fn prepared_handle_is_claimed_once_and_wrong_requests_do_not_consume_it() {
        let (_directory, mut store, job, downloaded) = fixture();
        let prepared = prepare(&mut store, &job, downloaded);
        let state = super::super::job_store::JobCommandState::default();
        state.prepared_receives.lock().unwrap().insert(job.id.clone(), prepared);
        let other_state = super::super::job_store::JobCommandState::default();
        let (_, unrelated) = other_state.claim(&job).unwrap();
        assert!(take_prepared_receive(&state, &unrelated, &job, 0).is_err());
        let (_, claim) = state.claim(&job).unwrap();
        assert!(take_prepared_receive(&state, &claim, &job, 1).is_err());
        assert_eq!(state.prepared_receives.lock().unwrap().len(), 1);
        let prepared = take_prepared_receive(&state, &claim, &job, 0).unwrap();
        assert!(take_prepared_receive(&state, &claim, &job, 0).is_err());
        activate_prepared_receive(&mut store, &prepared, &job.admission_identity).unwrap();
        drop(claim);
        assert_eq!(completed_receive_result(&store, &job, 0).unwrap().unwrap()["receivedRevision"], "1");
        assert_eq!(store.revision().unwrap(), 1);
    }

    #[test]
    fn cancellation_does_not_consume_an_apply_handle_or_release_its_worker() {
        let (_directory, mut store, job, downloaded) = fixture();
        let prepared = prepare(&mut store, &job, downloaded);
        let state = super::super::job_store::JobCommandState::default();
        state.prepared_receives.lock().unwrap().insert(job.id.clone(), prepared);
        let (cancel, claim) = state.claim(&job).unwrap();
        cancel.cancel();
        assert_eq!(take_prepared_receive(&state, &claim, &job, 0).err().unwrap().kind, ErrorKind::Cancelled);
        assert_eq!(state.prepared_receives.lock().unwrap().len(), 1);
        assert!(state.claim(&job).is_err());
        assert_eq!(store.revision().unwrap(), 0);
        drop(claim);
        assert!(state.claim(&job).is_ok());
    }

    fn staged_generations(directory: &std::path::Path) -> i64 {
        rusqlite::Connection::open_with_flags(
            directory.join("persistent").join(crate::persistent_store::DATABASE_FILE),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM root WHERE generation LIKE 'staging-%')
                  + (SELECT count(*) FROM characters WHERE generation LIKE 'staging-%')",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// A receive cancelled between the batches of its stage stops there,
    /// reports a cancellation rather than a rejected snapshot, removes its
    /// stage and leaves the job preparing, so the only next step is to
    /// prepare the same download again.
    #[test]
    fn cancellation_during_receive_staging_leaves_one_resumable_preparation() {
        let (directory, mut store, job, downloaded) = fixture();
        let staging = downloaded.staging_root.clone();
        let mut records = vec![root_at(&staging, "remote")];
        for index in 0..600 {
            let id = format!("character-{index:04}");
            records.push(character_at(&staging, &id, index as u64));
            records.push(conversation_at(&staging, &id, "chat", 0));
        }
        let downloaded = staged_snapshot(&staging, "snapshot", records);
        let held = store.materialize(None).unwrap();
        let phase = store.external_job(&job.id).unwrap().unwrap().phase;
        let cancel = Cancellation::default();
        let trigger = cancel.clone();
        let batches = std::rc::Rc::new(std::cell::Cell::new(0));
        let seen = batches.clone();
        crate::persistent_store::external_apply::after_stage_batch(Some(Box::new(move |written, _| {
            seen.set(written);
            trigger.cancel();
        })));
        let participation = receive_participation(&mut store).unwrap();
        let section = plugin_section(&staging);
        let outcome = prepare_receive_input(&mut store, &job, job.admission_identity.clone(),
            "authenticated-head".into(), downloaded.clone(), vec![section], participation,
            &crate::external_storage::phase_progress::PhaseProgress::silent(), &cancel);
        crate::persistent_store::external_apply::after_stage_batch(None);
        assert_eq!(outcome.err().unwrap().kind, ErrorKind::Cancelled);
        assert_eq!(batches.get(), 1);
        assert_eq!(staged_generations(directory.path()), 0);
        assert_eq!(store.revision().unwrap(), 0);
        assert_eq!(store.materialize(None).unwrap(), held);
        assert_eq!(store.external_job(&job.id).unwrap().unwrap().phase, phase);
        assert!(store.external_receive_completion(&job.id, "connection").unwrap().is_none());

        let prepared = prepare(&mut store, &job, downloaded);
        activate_prepared_receive(&mut store, &prepared, &job.admission_identity).unwrap();
        assert_eq!(completed_receive_result(&store, &job, 0).unwrap().unwrap()["receivedRevision"], "1");
        assert_eq!(store.materialize(None).unwrap()["characters"].as_array().unwrap().len(), 600);
    }

    /// Cancelled before its apply, a receive keeps its handle and the library;
    /// cancelled as its apply commits, it keeps the completed outcome and has
    /// nothing left that could be applied a second time.
    #[test]
    fn cancellation_around_activation_never_repeats_or_loses_the_apply() {
        let (_directory, mut store, job, downloaded) = fixture();
        let prepared = prepare(&mut store, &job, downloaded);
        let stage = prepared.apply.staging_id().unwrap().to_owned();
        let held = store.materialize(None).unwrap();
        let state = super::super::job_store::JobCommandState::default();
        state.prepared_receives.lock().unwrap().insert(job.id.clone(), prepared);
        let current = store.external_identity().unwrap();

        let (cancel, claim) = state.claim(&job).unwrap();
        cancel.cancel();
        assert_eq!(commit_prepared_receive(&mut store, &state, &claim, &job, 0, &current)
            .unwrap_err().kind, ErrorKind::Cancelled);
        assert_eq!(state.prepared_receives.lock().unwrap().len(), 1);
        assert_eq!(store.revision().unwrap(), 0);
        assert_eq!(store.materialize(None).unwrap(), held);
        drop(claim);

        let (cancel, claim) = state.claim(&job).unwrap();
        let (_, revision) =
            commit_prepared_receive(&mut store, &state, &claim, &job, 0, &current).unwrap();
        cancel.cancel();
        assert_eq!(revision, 1);
        let applied = store.materialize(None).unwrap();
        assert_eq!(applied["marker"], "remote");
        assert!(state.prepared_receives.lock().unwrap().is_empty());
        assert!(commit_prepared_receive(&mut store, &state, &claim, &job, 0, &current).is_err());
        // What a cancellation discards afterwards is the stage the apply
        // consumed, which no longer names any of the library.
        store.replace_abort(&stage).unwrap();
        drop(claim);
        assert_eq!(completed_receive_result(&store, &job, 0).unwrap().unwrap()["receivedRevision"], "1");
        assert_eq!(store.revision().unwrap(), 1);
        assert_eq!(store.materialize(None).unwrap(), applied);
    }

    #[test]
    fn each_identity_component_is_checked_before_device_rows_or_library_activation() {
        for field in ["store", "library", "generation", "selection", "revision"] {
            let (_directory, mut store, job, downloaded) = fixture();
            let prepared = prepare(&mut store, &job, downloaded);
            let stage = prepared.apply.staging_id().unwrap().to_owned();
            let mut current = job.admission_identity.clone();
            match field {
                "store" => current.store_id.push('x'),
                "library" => current.library_epoch.push('x'),
                "generation" => current.generation.push('x'),
                "selection" => current.selection_epoch.push('x'),
                "revision" => current.revision += 1,
                _ => unreachable!(),
            }
            assert_eq!(activate_prepared_receive(&mut store, &prepared, &current).err().unwrap().kind,
                ErrorKind::PreconditionFailed, "{field}");
            assert!(rows(&mut store).is_empty(), "{field}");
            assert_eq!(store.revision().unwrap(), 0, "{field}");
            assert!(store.prepare_replace_commit(&stage, Some(0)).is_err(), "{field}");
        }
    }

    #[test]
    fn a_local_edit_during_preparation_or_before_apply_is_never_rebased() {
        for before_stage in [true, false] {
            let (_directory, mut store, job, downloaded) = fixture();
            if before_stage {
                local_edit(&mut store, 0);
                let participation = receive_participation(&mut store).unwrap();
                let section = plugin_section(&downloaded.staging_root);
                assert!(prepare_receive_input(&mut store, &job, job.admission_identity.clone(),
                    "authenticated-head".into(), downloaded, vec![section], participation,
                    &crate::external_storage::phase_progress::PhaseProgress::silent(),
                    &Cancellation::default()).is_err());
            } else {
                let prepared = prepare(&mut store, &job, downloaded);
                local_edit(&mut store, 0);
                let current = store.external_identity().unwrap();
                assert!(activate_prepared_receive(&mut store, &prepared, &current).is_err());
            }
            assert_eq!(store.revision().unwrap(), 1);
            assert_eq!(store.materialize(None).unwrap()["marker"], "newer-local");
            assert!(rows(&mut store).is_empty());
            assert!(store.external_receive_completion(&job.id, "connection").unwrap().is_none());
        }
    }

    #[test]
    fn participation_generation_changes_reject_even_when_the_flag_returns_to_true() {
        let (_directory, mut store, job, downloaded) = fixture();
        let prepared = prepare(&mut store, &job, downloaded);
        let device = store.device_store_mut().unwrap();
        device.set_section_participating(PdsSection::LocalPlugins, false).unwrap();
        device.set_section_participating(PdsSection::LocalPlugins, true).unwrap();
        assert!(activate_prepared_receive(&mut store, &prepared, &job.admission_identity).is_err());
        assert!(rows(&mut store).is_empty());
        assert_eq!(store.revision().unwrap(), 0);
    }

    #[test]
    fn publication_ack_skips_carried_sections() {
        let (_directory, mut store, _job, _downloaded) = fixture();
        let sections = BTreeMap::from([(
            SectionKind::LocalPlugins.id().to_owned(),
            plugin_section_reference(7),
        )]);
        note_sections_published_in_store(&mut store, "connection", "library", &sections, &[])
            .unwrap();
        assert!(store.device_store_mut().unwrap()
            .read_section_cursor("connection", "library", PdsSection::LocalPlugins)
            .unwrap().is_none());
    }

    #[test]
    fn corrupt_sections_or_snapshot_bindings_never_create_an_applicable_receive() {
        for case in ["section", "snapshot", "repository", "record"] {
            let (_directory, mut store, job, mut downloaded) = fixture();
            let participation = receive_participation(&mut store).unwrap();
            let section = plugin_section(&downloaded.staging_root);
            match case {
                "section" => fs::write(&section.sources[0].path, b"corrupt").unwrap(),
                "snapshot" => downloaded.snapshot_id = "other-snapshot".into(),
                "repository" => downloaded.repository_id = "other-repository".into(),
                "record" => fs::write(downloaded.records[0].source.file().unwrap(), b"corrupt").unwrap(),
                _ => unreachable!(),
            }
            assert!(prepare_receive_input(&mut store, &job, job.admission_identity.clone(),
                "authenticated-head".into(), downloaded, vec![section], participation,
                &crate::external_storage::phase_progress::PhaseProgress::silent(),
                &Cancellation::default()).is_err(), "{case}");
            assert_eq!(store.revision().unwrap(), 0, "{case}");
            assert!(rows(&mut store).is_empty(), "{case}");
        }
    }

    #[test]
    fn interrupted_section_progress_is_idempotent_after_worker_repreparation() {
        let (directory, mut store, job, downloaded) = fixture();
        let prepared = prepare(&mut store, &job, downloaded.clone());
        for section in &prepared.sections {
            super::super::sections::apply_prepared_section(&mut store, section).unwrap();
        }
        let first = rows(&mut store);
        assert_eq!(first.len(), 1);
        assert_eq!(store.revision().unwrap(), 0);
        store.replace_abort(prepared.apply.staging_id().unwrap()).unwrap();
        drop(prepared);
        drop(store);
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let state = super::super::job_store::JobCommandState::default();
        let (_, claim) = state.claim(&job).unwrap();
        assert!(take_prepared_receive(&state, &claim, &job, 0).is_err());
        let prepared = prepare(&mut store, &job, downloaded);
        activate_prepared_receive(&mut store, &prepared, &job.admission_identity).unwrap();
        assert_eq!(rows(&mut store), first);
        assert_eq!(store.revision().unwrap(), 1);
    }

    #[test]
    fn receive_head_checks_content_without_binding_to_an_old_provider_token() {
        let original = HeadObservation {
            commit_id: "received-commit".into(),
            authenticated_body_hash: "12".repeat(32),
            version: Some(VersionToken("before".into())),
        };
        let encoded = observation_json(&original).unwrap();
        let mut current = original.clone();
        for version in [Some(VersionToken("after".into())), None] {
            current.version = version;
            assert!(require_received_head(&encoded, Some(&current)).is_ok());
        }
        current.commit_id = "different-commit".into();
        assert_eq!(require_received_head(&encoded, Some(&current)).unwrap_err().kind,
            ErrorKind::PreconditionFailed);
        current.commit_id = original.commit_id;
        current.authenticated_body_hash = "34".repeat(32);
        assert_eq!(require_received_head(&encoded, Some(&current)).unwrap_err().kind,
            ErrorKind::PreconditionFailed);
        assert_eq!(require_received_head(&encoded, None).unwrap_err().kind,
            ErrorKind::PreconditionFailed);
        assert_eq!(require_received_head("invalid", Some(&current)).unwrap_err().kind,
            ErrorKind::Corrupt);
    }

    #[test]
    fn apply_requests_accept_only_canonical_revisions_and_no_native_stage_fields() {
        for revision in ["-1", "+1", "01", "1.0", " 1", "9223372036854775808"] {
            assert!(receive_revision(&ApplyReceivedRequest {
                job_id: "job".into(), expected_revision: revision.into(),
            }).is_err());
        }
        assert!(serde_json::from_value::<ApplyReceivedRequest>(json!({
            "jobId":"job", "expectedRevision":"0", "stagingId":"staging-injected"
        })).is_err());
        assert_eq!(receive_revision(&ApplyReceivedRequest {
            job_id: "job".into(), expected_revision: "0".into(),
        }).unwrap(), 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_storage::{
        contract::{ObjectReceipt, ObjectRole, RemoteLocator},
        control::HeadDocument,
        fake,
        packaging::RemoteObject,
    };
    use risunest_external_storage_format::{
        format::Strategy,
        snapshot as wire,
    };

    fn descriptor() -> Descriptor {
        Descriptor::new("descriptor-repository".into(), Some(Strategy::Cas),
        )
        .unwrap()
    }
    fn identity(revision: i64) -> CaptureIdentity {
        CaptureIdentity {
            store_id: "store".into(),
            library_epoch: "epoch".into(),
            generation: "generation".into(),
            selection_epoch: "selection".into(),
            revision,
        }
    }
    fn snapshot() -> RemoteObject {
        let header = wire::PublicObjectHeader::new(
            "descriptor-repository".into(),
            "snapshot-s1".into(),
            wire::ObjectRole::SyncState,
            4,
        )
        .unwrap();
        RemoteObject {
            repository_id: "descriptor-repository".into(),
            object_id: "snapshot-s1".into(),
            role: ObjectRole::SyncState,
            receipt: ObjectReceipt {
                locator: RemoteLocator {
                    connection_identity: "provider-root".into(),
                    collection: None,
                    object: "opaque".into(),
                },
                byte_length: wire::envelope_length(&header).unwrap(),
                version: None,
                checksum: None,
                complete: true,
            },
            ciphertext_sha256: "11".repeat(32),
            plaintext_length: 4,
            plaintext_sha256: "22".repeat(32),
        }
    }
    fn remote(commit: &str, fingerprint: &str, body: u8, version: &str) -> ObservedHead {
        let document = HeadDocument::new(
            &descriptor(),
            "library".into(),
            commit.into(),
            None,
            fingerprint.into(),
            snapshot(),
        )
        .unwrap();
        ObservedHead {
            document,
            observation: HeadObservation {
                commit_id: commit.into(),
                authenticated_body_hash: format!("{body:02x}").repeat(32),
                version: Some(VersionToken(version.into())),
            },
        }
    }
    fn base(identity: CaptureIdentity, head: &ObservedHead) -> ExternalBase {
        ExternalBase {
            repository_id: "descriptor-repository".into(),
            snapshot_id: "s0".into(),
            commit_id: head.observation.commit_id.clone(),
            head_observation: observation_json(&head.observation).unwrap(),
            identity,
        }
    }

    async fn observe(
        provider: &fake::FakeProvider,
        cancel: &Cancellation,
    ) -> Result<Option<ObservedHead>> {
        observe_unknown_publication(
            provider,
            &fake::repository(),
            &descriptor(),
            &[7; 32],
            "intended-commit",
            "snapshot-intended",
            cancel,
        )
        .await
    }

    #[test]
    fn publication_unknown_observation_is_bounded_cancelable_and_read_only() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let missing = fake::FakeProvider::new(false);
            assert!(observe(&missing, &Cancellation::default()).await.unwrap().is_none());
            assert_eq!(missing.read_attempts("head"), 2);
            assert!(!missing.holds("head"));
            assert!(missing.uploaded_ids().is_empty());

            let transient = fake::FakeProvider::new(false);
            transient.fail_read("head", ErrorKind::Transient);
            assert!(observe(&transient, &Cancellation::default()).await.unwrap().is_none());
            assert_eq!(transient.read_attempts("head"), 2);
            assert!(!transient.holds("head"));
            assert!(transient.uploaded_ids().is_empty());

            let cancelled = fake::FakeProvider::new(false);
            let cancellation = Cancellation::default();
            cancelled.cancel_after_read("head", 1, &cancellation);
            assert_eq!(
                observe(&cancelled, &cancellation).await.unwrap_err().kind,
                ErrorKind::Cancelled,
            );
            assert_eq!(cancelled.read_attempts("head"), 1);
            assert!(!cancelled.holds("head"));
            assert!(cancelled.uploaded_ids().is_empty());

            for kind in [ErrorKind::Unauthorized, ErrorKind::ReauthRequired, ErrorKind::Corrupt] {
                let permanent = fake::FakeProvider::new(false);
                permanent.fail_read("head", kind);
                assert_eq!(
                    observe(&permanent, &Cancellation::default()).await.unwrap_err().kind,
                    kind,
                );
                assert_eq!(permanent.read_attempts("head"), 1, "{kind:?}");
                assert!(!permanent.holds("head"), "{kind:?}");
                assert!(permanent.uploaded_ids().is_empty(), "{kind:?}");
            }
        });
    }

    /// Invariant 30. A device value that moved on its own publishes; it never
    /// turns into a library disagreement.
    #[test]
    fn a_section_only_change_publishes_instead_of_becoming_a_library_conflict() {
        let remote = remote("remote", "55".repeat(32).as_str(), 1, "1");
        let base = base(identity(4), &remote);
        assert_eq!(
            decide_sync(SyncInputs {
                descriptor: &descriptor(),
                connection_id: "connection",
                current_identity: &identity(4),
                local_pristine: false,
                local_fingerprint: None,
                local_sections_changed: true,
                base: Some(&base),
                remote: Some(&remote),
            })
            .unwrap(),
            SyncAction::PublishLocal {
                expected: Some(remote.clone())
            }
        );
        assert_eq!(
            decide_sync(SyncInputs {
                descriptor: &descriptor(),
                connection_id: "connection",
                current_identity: &identity(4),
                local_pristine: false,
                local_fingerprint: None,
                local_sections_changed: false,
                base: Some(&base),
                remote: Some(&remote),
            })
            .unwrap(),
            SyncAction::UpToDate
        );
    }

    #[test]
    fn first_attach_never_overwrites_an_existing_remote_head() {
        let remote = remote("remote", "44".repeat(32).as_str(), 1, "1");
        assert!(matches!(
            decide_sync(SyncInputs {
                descriptor: &descriptor(),
                connection_id: "connection",
                current_identity: &identity(1),
                local_pristine: false,
                local_fingerprint: None,
                local_sections_changed: false,
                base: None,
                remote: Some(&remote)
            })
            .unwrap(),
            SyncAction::FirstAttachDecision { .. }
        ));
        assert_eq!(
            decide_sync(SyncInputs {
                descriptor: &descriptor(),
                connection_id: "connection",
                current_identity: &identity(1),
                local_pristine: false,
                local_fingerprint: None,
                local_sections_changed: false,
                base: None,
                remote: None
            })
            .unwrap(),
            SyncAction::PublishLocal { expected: None }
        );
        assert!(matches!(
            decide_sync(SyncInputs {
                descriptor: &descriptor(),
                connection_id: "connection",
                current_identity: &identity(0),
                local_pristine: true,
                local_fingerprint: None,
                local_sections_changed: false,
                base: None,
                remote: Some(&remote)
            })
            .unwrap(),
            SyncAction::ReceiveRemote { .. }
        ));
    }
    #[test]
    fn local_remote_and_two_sided_changes_are_distinguished() {
        let old = remote("old", "33".repeat(32).as_str(), 1, "1");
        let base = base(identity(1), &old);
        assert_eq!(
            decide_sync(SyncInputs {
                descriptor: &descriptor(),
                connection_id: "c",
                current_identity: &identity(2),
                local_pristine: false,
                local_fingerprint: Some(&"55".repeat(32)),
                local_sections_changed: false,
                base: Some(&base),
                remote: Some(&old)
            })
            .unwrap(),
            SyncAction::PublishLocal {
                expected: Some(old.clone())
            }
        );
        let new = remote("new", "44".repeat(32).as_str(), 2, "2");
        assert_eq!(
            decide_sync(SyncInputs {
                descriptor: &descriptor(),
                connection_id: "c",
                current_identity: &identity(1),
                local_pristine: false,
                local_fingerprint: None,
                local_sections_changed: false,
                base: Some(&base),
                remote: Some(&new)
            })
            .unwrap(),
            SyncAction::ReceiveRemote {
                remote: new.clone()
            }
        );
        assert_eq!(
            decide_sync(SyncInputs {
                descriptor: &descriptor(),
                connection_id: "c",
                current_identity: &identity(2),
                local_pristine: false,
                local_fingerprint: Some(&"66".repeat(32)),
                local_sections_changed: false,
                base: Some(&base),
                remote: Some(&new)
            })
            .unwrap(),
            SyncAction::PreserveConflict {
                remote: new.clone()
            }
        );
        assert_eq!(
            decide_sync(SyncInputs {
                descriptor: &descriptor(),
                connection_id: "c",
                current_identity: &identity(2),
                local_pristine: false,
                local_fingerprint: Some(&"44".repeat(32)),
                local_sections_changed: false,
                base: Some(&base),
                remote: Some(&new)
            })
            .unwrap(),
            SyncAction::PreserveConflict { remote: new }
        );
    }
    #[test]
    fn missing_known_head_and_replaced_local_lineage_require_explicit_recovery() {
        let old = remote("old", "33".repeat(32).as_str(), 1, "1");
        let base = base(identity(1), &old);
        assert_eq!(
            decide_sync(SyncInputs {
                descriptor: &descriptor(),
                connection_id: "c",
                current_identity: &identity(1),
                local_pristine: false,
                local_fingerprint: None,
                local_sections_changed: false,
                base: Some(&base),
                remote: None
            })
            .unwrap(),
            SyncAction::RecoveryRequired
        );
        let mut replaced = identity(2);
        replaced.library_epoch = "replacement".into();
        assert_eq!(
            decide_sync(SyncInputs {
                descriptor: &descriptor(),
                connection_id: "c",
                current_identity: &replaced,
                local_pristine: false,
                local_fingerprint: None,
                local_sections_changed: false,
                base: Some(&base),
                remote: Some(&old)
            })
            .unwrap(),
            SyncAction::DecisionRequired
        );
    }
}
