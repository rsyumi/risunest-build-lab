//! Downloaded remote bodies a job keeps under its own directory, and their
//! removal once nothing that owns the job can read them again.
//!
//! A pass runs under a claim on the connection, which keeps every worker,
//! apply and cancellation of its jobs from starting. Under that claim it
//! decides ownership and moves each reclaimable directory aside; a moved
//! directory is never opened again, so deleting it needs no claim.
use super::{
    connection_store::ConnectionStore,
    contract::{ErrorKind, ProviderError, Result},
    job_store::{DurableJob, JobClaim, JobCommandState, JobKind, JobStore, ReceiveArtifacts},
    runtime::{job_directory, local_error, native_store, root},
};
use crate::{
    native_file_jobs::admission::Admission,
    persistent_store::{external_conflicts, PersistentStore},
    trust_boundary::{is_link_like, sync_directory},
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::{AppHandle, Manager};

/// What a receive and a section rejoin download into. The transfer journal,
/// its spools and the connection's package cache belong to other owners.
const ARTIFACTS: [&str; 2] = ["receive", "rejoin"];
/// Every examined job counts, including retained owners and interrupted removals.
const JOBS_PER_PASS: usize = 8;

fn moved_name(name: &str) -> String {
    format!(".reclaim-{name}")
}

fn linked() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "receive artifact path is redirected")
}

fn io_error(error: io::Error) -> ProviderError {
    if error.kind() == io::ErrorKind::InvalidData {
        ProviderError::new(ErrorKind::Corrupt)
    } else {
        local_error(error)
    }
}

/// Recorded before the first downloaded body is written, so whatever a crash
/// leaves behind is still found by a later pass.
pub(crate) fn hold(root: &Path, job_id: &str) -> Result<()> {
    let jobs = JobStore::open(root)?;
    let mut job = jobs.read(job_id)?;
    if job.receive_artifacts.is_none() {
        job.receive_artifacts = Some(ReceiveArtifacts::Held);
        jobs.put(&job)?;
    }
    Ok(())
}

/// Whether no worker, prepared apply, pause, unknown outcome, preserved
/// conflict or later resolution of `job` can read its downloaded bodies again.
/// Only the authoritative job row decides; the cached summary can only hold
/// bodies back, never release them.
pub(crate) fn reclaimable(
    store: &PersistentStore,
    job: &DurableJob,
    repository_id: &str,
    prepared: bool,
) -> Result<bool> {
    if prepared || !matches!(job.request.kind, JobKind::Sync | JobKind::ResolveConflict) {
        return Ok(false);
    }
    let conflict = external_conflicts::external_conflict(
        store.device_store().map_err(local_error)?.connection(),
        &job.id,
    )
    .map_err(local_error)?;
    if conflict.is_some_and(|record| !record.resolved) {
        return Ok(false);
    }
    let Some(intent) = store.external_job(&job.id).map_err(local_error)? else {
        return Ok(false);
    };
    if intent.id != job.id
        || intent.connection_id != job.request.connection_id
        || intent.repository_id != repository_id
    {
        return Ok(false);
    }
    // A completed receive stays complete after later receives replace the base.
    if store
        .external_receive_completion(&job.id, &job.request.connection_id)
        .map_err(local_error)?
        .is_some()
    {
        return Ok(true);
    }
    // A cancelled or invalidated intent can still be prepared again by the
    // same job while it is open.
    Ok(job.terminal() && matches!(intent.phase.as_str(), "complete" | "cancelled" | "stale"))
}

/// Where a worker's pass goes relative to publishing the outcome it settled
/// on. Whoever reads that outcome may act on the job's connection at once,
/// and the claim the pass holds would refuse them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettlementPass {
    /// A receive waiting for its apply, which runs a pass of its own.
    Skip,
    /// The job stays open, so the pass cannot release its own bodies and
    /// nothing is lost by finishing it first.
    BeforeOutcome,
    /// Only the settled row releases the job's own bodies.
    AfterOutcome,
}

pub(crate) fn settlement_pass(settled: &DurableJob) -> SettlementPass {
    if settled.terminal() {
        SettlementPass::AfterOutcome
    } else if settled.summary["state"] == "waiting" && settled.summary["phase"] == "remote-apply" {
        SettlementPass::Skip
    } else {
        SettlementPass::BeforeOutcome
    }
}

pub(crate) struct Owners<'a> {
    pub root: &'a Path,
    pub state: &'a JobCommandState,
    pub store: &'a PersistentStore,
    pub admission: &'a Arc<Admission>,
    pub repository_id: &'a str,
}

/// Moves aside the downloaded bodies of jobs on `connection_id` that nothing
/// owns any more, and returns those jobs for [`remove_detached`].
pub(crate) fn detach_connection(
    owners: &Owners<'_>,
    claim: &JobClaim,
    connection_id: &str,
) -> Result<Vec<String>> {
    claim.require_connection(owners.state, connection_id)?;
    let jobs = JobStore::open(owners.root)?;
    let after = jobs.receive_cleanup_after(connection_id)?;
    let page = jobs.list_holding_receive_artifacts(connection_id, after, JOBS_PER_PASS)?;
    // Continue past retained owners as well, then revisit them after reaching
    // the end. A failed deletion cannot starve later jobs or grow this pass.
    let next = if page.len() == JOBS_PER_PASS {
        page.last().map_or(0, |(cursor, _)| *cursor)
    } else {
        0
    };
    let mut detached = Vec::new();
    for (_, job) in page {
        match detach(owners, &job) {
            Ok(Some(_)) => detached.push(job.id),
            Ok(None) => {}
            Err(error) => crate::nlog!(
                "warn",
                "External receive bodies were kept for a later pass: {error}"
            ),
        }
    }
    jobs.set_receive_cleanup_after(connection_id, next)?;
    Ok(detached)
}

/// `None` while something still owns the job's bodies, otherwise whether any
/// of them moved aside now.
fn detach(owners: &Owners<'_>, job: &DurableJob) -> Result<Option<bool>> {
    let prepared = owners
        .state
        .prepared_receives
        .lock()
        .map_err(local_error)?
        .contains_key(&job.id);
    let _admission = owners.admission.file(false).map_err(local_error)?;
    if !reclaimable(owners.store, job, owners.repository_id, prepared)? {
        return Ok(None);
    }
    let directory = job_directory(owners.root, &job.request.connection_id, &job.id);
    if !managed_directory(owners.root, &directory).map_err(io_error)? {
        return Ok(Some(false));
    }
    move_aside(&directory).map(Some).map_err(io_error)
}

/// Every directory from the external storage root down to the job's own must
/// be a real directory. `false` when the job never created one.
pub(super) fn managed_directory(root: &Path, directory: &Path) -> io::Result<bool> {
    let base = root.join("external-storage");
    if !directory.starts_with(&base) {
        return Err(linked());
    }
    let chain: Vec<PathBuf> = directory
        .ancestors()
        .take_while(|path| path.starts_with(&base))
        .map(Path::to_path_buf)
        .collect();
    for path in chain.iter().rev() {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
            Ok(metadata) if is_link_like(&metadata) || !metadata.is_dir() => return Err(linked()),
            Ok(_) => {}
        }
    }
    Ok(true)
}

/// Checks each artifact itself before moving any, so a redirected one leaves
/// the job's bodies where they were. A rename never follows a link inside the
/// tree, and [`remove_tree`] stops at one. On Windows an open file inside one
/// refuses the move, which postpones the whole job before anything is deleted.
/// Returns whether anything was moved.
fn move_aside(directory: &Path) -> io::Result<bool> {
    let mut present = Vec::new();
    for name in ARTIFACTS {
        let path = directory.join(name);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
            Ok(metadata) if is_link_like(&metadata) || !metadata.is_dir() => return Err(linked()),
            Ok(_) => {}
        }
        present.push((path, directory.join(moved_name(name))));
    }
    if present.is_empty() {
        return Ok(false);
    }
    for (path, moved) in present {
        // What an interrupted pass had already moved is garbage by now.
        remove_tree(&moved)?;
        fs::rename(&path, &moved)?;
    }
    sync_directory(directory)?;
    Ok(true)
}

/// Deletes without ever following a link; a link stops the removal instead.
pub(super) fn remove_tree(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        other => other?,
    };
    if is_link_like(&metadata) {
        return Err(linked());
    }
    let removed = if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            remove_tree(&entry?.path())?;
        }
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    };
    match removed {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Deletes what [`detach_connection`] moved aside and forgets each job's hold
/// once nothing of it is left. A failure keeps the hold for a later pass.
pub(crate) fn remove_detached(root: &Path, job_ids: &[String]) {
    for id in job_ids {
        if let Err(error) = remove_job(root, id) {
            crate::nlog!("warn", "External receive bodies were kept for a later pass: {error}");
        }
    }
}

fn remove_job(root: &Path, id: &str) -> Result<()> {
    let jobs = JobStore::open(root)?;
    let job = jobs.read(id)?;
    let directory = job_directory(root, &job.request.connection_id, &job.id);
    if managed_directory(root, &directory).map_err(io_error)? {
        for name in ARTIFACTS {
            remove_tree(&directory.join(moved_name(name))).map_err(io_error)?;
        }
        // Only a job that kept nothing else there loses its directory.
        if fs::remove_dir(&directory).is_ok() {
            if let Some(parent) = directory.parent() {
                sync_directory(parent).map_err(local_error)?;
            }
        } else {
            sync_directory(&directory).map_err(local_error)?;
        }
    }
    jobs.release_receive_artifacts(id)
}

/// A settled job on `connection_id` that still holds downloaded bodies. Its
/// claim is the one its own settlement pass held, so a start on the
/// connection waits for a pass run under it.
pub(crate) fn settled_holder(root: &Path, connection_id: &str) -> Result<Option<DurableJob>> {
    let jobs = JobStore::open(root)?;
    let mut after = 0;
    loop {
        let page = jobs.list_holding_receive_artifacts(connection_id, after, JOBS_PER_PASS)?;
        let Some((cursor, _)) = page.last() else {
            return Ok(None);
        };
        after = *cursor;
        if let Some((_, job)) = page.into_iter().find(|(_, job)| job.terminal()) {
            return Ok(Some(job));
        }
    }
}

/// Every page of the pass on `connection_id`, from the first job on, for the
/// bodies no later settlement on the connection will release.
pub(crate) fn detach_every_page(
    owners: &Owners<'_>,
    claim: &JobClaim,
    connection_id: &str,
) -> Result<Vec<String>> {
    let jobs = JobStore::open(owners.root)?;
    jobs.set_receive_cleanup_after(connection_id, 0)?;
    let mut detached = Vec::new();
    loop {
        detached.extend(detach_connection(owners, claim, connection_id)?);
        if jobs.receive_cleanup_after(connection_id)? == 0 {
            return Ok(detached);
        }
    }
}

fn with_owners<T>(
    app: &AppHandle,
    connection_id: &str,
    pass: impl FnOnce(&Owners<'_>) -> Result<T>,
) -> Result<T> {
    let root = root(app)?;
    let repository_id = ConnectionStore::open(&root)?
        .read(connection_id)?
        .descriptor
        .repository_id;
    let store = native_store(app)?;
    let admission = app
        .state::<crate::native_file_jobs::NativeFileJobState>()
        .admission
        .clone();
    let state = app.state::<JobCommandState>();
    pass(&Owners {
        root: &root,
        state: &state,
        store: &store,
        admission: &admission,
        repository_id: &repository_id,
    })
}

/// The pass for the connection `claim` holds, as the running application sees
/// it. Nothing it finds is reported as the job's own outcome.
pub(crate) fn detach_claimed(app: &AppHandle, claim: &JobClaim, connection_id: &str) -> Vec<String> {
    with_owners(app, connection_id, |owners| detach_connection(owners, claim, connection_id))
        .unwrap_or_else(|error| {
            crate::nlog!("warn", "External receive bodies were not examined: {error}");
            Vec::new()
        })
}

/// Runs the whole pass on every connection once the library store is open,
/// for bodies whose job settled with no later settlement on its connection.
/// A connection another job holds is left to that job's settlement.
pub(crate) fn reclaim_settled_later(app: &AppHandle) {
    let Ok(root) = root(app) else { return };
    if !root.join("external-storage").is_dir() {
        return;
    }
    let worker = app.state::<JobCommandState>().track_worker();
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _worker = worker;
        let connections = match ConnectionStore::open(&root).and_then(|store| store.ids()) {
            Ok(connections) => connections,
            Err(error) => {
                crate::nlog!("warn", "External receive bodies were not examined: {error}");
                return;
            }
        };
        for connection_id in connections {
            let detached = (|| {
                let Some(settled) = settled_holder(&root, &connection_id)? else {
                    return Ok(Vec::new());
                };
                let Ok((_, claim)) = app.state::<JobCommandState>().claim(&settled) else {
                    return Ok(Vec::new());
                };
                with_owners(&app, &connection_id, |owners| {
                    detach_every_page(owners, &claim, &connection_id)
                })
            })()
            .unwrap_or_else(|error: ProviderError| {
                crate::nlog!("warn", "External receive bodies were not examined: {error}");
                Vec::new()
            });
            remove_detached(&root, &detached);
        }
    });
}

/// Deletes moved bodies off the caller's path, as a worker application cleanup
/// waits for.
pub(crate) fn remove_detached_later(app: &AppHandle, job_ids: Vec<String>) {
    if job_ids.is_empty() {
        return;
    }
    let Ok(root) = root(app) else { return };
    let worker = app.state::<JobCommandState>().track_worker();
    tauri::async_runtime::spawn_blocking(move || {
        let _worker = worker;
        remove_detached(&root, &job_ids);
    });
}
