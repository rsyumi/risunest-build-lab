//! Manual external snapshot restore and durable commit recovery.
//! Metadata is verified before activation; missing bodies follow renderer adoption.
use super::{
    connection_commands::ConnectedRepository,
    connection_store::ConnectionStore,
    contract::{Cancellation, ErrorKind, ProviderError, Result},
    job_store::{DurableJob, JobKind, JobStore},
    runtime,
    snapshot_restore::{self, PreparedRemoteSnapshot},
};
use crate::persistent_store::{
    commands::PersistentStoreState,
    external_apply::{ExternalSnapshotApplication, ExternalSnapshotObject, ExternalSnapshotRecord},
    PersistentStore, StoreError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeSet, path::Path};
use tauri::{AppHandle, Manager};
#[cfg(test)]
use super::worker_observation::spawn_blocking;
#[cfg(not(test))]
use tokio::task::spawn_blocking;

#[derive(Clone, Debug, PartialEq, Eq)]
struct RestoreSelection {
    library: bool,
    sections: BTreeSet<String>,
}

const RESTORE_COMMIT_SCHEMA: &str = "risunest.external-restore-commit/v1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestoreCommitIntent {
    schema: String,
    job_id: String,
    connection_id: String,
    snapshot_id: String,
    expected_revision: String,
    header: crate::persistent_store::lww::Header,
    staging_id: String,
}

fn restore_intent(job: &DurableJob, expected_revision: i64, header: crate::persistent_store::lww::Header, staging_id:&str) -> Result<RestoreCommitIntent> {
    Ok(RestoreCommitIntent {
        schema: RESTORE_COMMIT_SCHEMA.into(), job_id:job.id.clone(),
        connection_id:job.request.connection_id.clone(),
        snapshot_id:job.request.snapshot_id.clone().ok_or_else(corrupt)?,
        expected_revision:expected_revision.to_string(), header, staging_id:staging_id.into(),
    })
}

fn completed_restore_in_store(store: &PersistentStore, job: &DurableJob) -> Result<Option<Value>> {
    if job.request.kind != JobKind::Restore {return Ok(None)}
    let Some(value) = job.summary.get("restoreCommit") else {return Ok(None)};
    let intent:RestoreCommitIntent = serde_json::from_value(value.clone()).map_err(|_| corrupt())?;
    let expected_revision = job.request.target_revision.as_deref().ok_or_else(corrupt)?
        .parse::<i64>().map_err(|_| corrupt())?;
    if serde_json::to_value(&intent).map_err(runtime::local_error)? != *value
        || intent != restore_intent(job,expected_revision,intent.header.clone(),&intent.staging_id)?
        || intent.header.request_id != format!("external-backup-restore:{}",job.id)
        || intent.staging_id.is_empty() {return Err(corrupt())}
    let Some(receipt) = store.lww_device_replacement_receipt(&intent.header,&intent.staging_id).map_err(pds_error)? else {return Ok(None)};
    Ok(Some(json!({"snapshotId":intent.snapshot_id,"receivedRevision":receipt.revision.to_string()})))
}

fn persist_restore_intent(root:&Path, job:&DurableJob, intent:&RestoreCommitIntent) -> Result<()> {
    let jobs=JobStore::open(root)?;
    let mut current=jobs.read(&job.id)?;
    let value=serde_json::to_value(intent).map_err(runtime::local_error)?;
    if current.summary.get("restoreCommit").is_some_and(|old| old != &value) {return Err(corrupt())}
    current.summary["restoreCommit"]=value;
    current.summary["applicationStarted"]=json!(true);
    current.summary["phase"]=json!("applying-local");
    current.summary["updatedAtMs"]=json!(runtime::now_ms().to_string());
    jobs.put(&current)
}

/// Read-only completion recovery used before reopening a provider connection.
pub(crate) fn completed_restore(app: &AppHandle, job: &DurableJob) -> Result<Option<Value>> {
    if job.summary.get("restoreAdopted").is_none() || job.summary["restoreBodiesComplete"]!=true
        || !JobStore::open(&runtime::root(app)?)?.restore_bodies_settled(&job.id)? {
        return Ok(None);
    }
    completed_restore_in_store(&runtime::native_store(app)?, job)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub(crate) struct RestoreAdoptionRequest {
    pub job_id:String,
    pub received_revision:String,
    pub selected_character_id:Option<String>,
}

fn confirm_restore_adoption_in_store(store:&PersistentStore,root:&Path,request:&RestoreAdoptionRequest)->Result<Value> {
    let revision=request.received_revision.parse::<i64>().map_err(|_|corrupt())?;
    if revision<0 || revision.to_string()!=request.received_revision {return Err(corrupt());}
    let jobs=JobStore::open(root)?;
    let mut job=jobs.read(&request.job_id)?;
    let receipt=completed_restore_in_store(store,&job)?.ok_or_else(||ProviderError::new(ErrorKind::PreconditionFailed))?;
    if receipt["receivedRevision"]!=request.received_revision {return Err(ProviderError::new(ErrorKind::PreconditionFailed));}
    require_activated_target(store,&job,revision)?;
    let adopted=serde_json::to_value(request).map_err(runtime::local_error)?;
    if let Some(existing)=job.summary.get("restoreAdopted") {
        if existing!=&adopted {return Err(ProviderError::new(ErrorKind::PreconditionFailed));}
        return Ok(receipt);
    }
    if let Some(id)=&request.selected_character_id {
        if id.is_empty() || id.contains('\0') {return Err(corrupt());}
        if store.read_character_summary(id,None).map_err(pds_error)?.is_none() {
            return Err(ProviderError::new(ErrorKind::PreconditionFailed));
        }
    }
    job.summary["restoreAdopted"]=adopted;
    job.summary["updatedAtMs"]=json!(runtime::now_ms().to_string());
    jobs.put(&job)?;
    Ok(receipt)
}

fn require_activated_target(store:&PersistentStore,job:&DurableJob,revision:i64)->Result<()> {
    let intent:RestoreCommitIntent=serde_json::from_value(job.summary["restoreCommit"].clone()).map_err(|_|corrupt())?;
    let identity=store.external_identity().map_err(pds_error)?;
    if identity.store_id!=job.admission_identity.store_id
        || identity.library_epoch!=job.admission_identity.library_epoch
        || identity.selection_epoch!=job.admission_identity.selection_epoch
        || identity.generation!=format!("revision-{revision}")
        || store.lww_binding_authority().map_err(pds_error)?!=intent.header.binding_authority {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    Ok(())
}

pub(crate) fn confirmed_restore_activation(app:&AppHandle,job:&DurableJob)->Result<Option<Value>> {
    if job.summary["applicationStarted"]!=true {return Ok(None);}
    let store=runtime::native_store(app)?;
    let Some(receipt)=completed_restore_in_store(&store,job)? else {return Ok(None)};
    let revision=receipt["receivedRevision"].as_str().ok_or_else(corrupt)?.parse().map_err(|_|corrupt())?;
    match require_activated_target(&store,job,revision) {
        Ok(())=>Ok(Some(receipt)),
        Err(error) if error.kind==ErrorKind::PreconditionFailed=>Ok(None),
        Err(error)=>Err(error),
    }
}

#[tauri::command]
pub(crate) fn external_storage_confirm_restore_adoption(app:AppHandle,request:RestoreAdoptionRequest)->Result<Value> {
    confirm_restore_adoption_in_store(&runtime::native_store(&app)?,&runtime::root(&app)?,&request)
}

pub(crate) fn application_started(job: &DurableJob) -> bool {
    job.request.kind == JobKind::Restore && job.summary["applicationStarted"] == true
}

#[cfg(test)]
fn mark_application_started(root: &Path, job: &DurableJob) -> Result<()> {
    let jobs = JobStore::open(root)?;
    let mut current = jobs.read(&job.id)?;
    current.summary["applicationStarted"] = json!(true);
    current.summary["phase"] = json!("applying-local");
    current.summary["updatedAtMs"] = json!(runtime::now_ms().to_string());
    jobs.put(&current)
}

fn corrupt() -> ProviderError {
    ProviderError::new(ErrorKind::Corrupt)
}

fn pds_error(error: StoreError) -> ProviderError {
    match error {
        StoreError::RevisionConflict { .. } => ProviderError::new(ErrorKind::PreconditionFailed),
        StoreError::Validation { .. } => corrupt(),
        _ => ProviderError::new(ErrorKind::Transient),
    }
}

fn decode_hash(value: &str) -> Result<[u8; 32]> {
    if !crate::trust_boundary::is_lower_hex_256(value) {
        return Err(corrupt());
    }
    hex::decode(value)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(corrupt)
}

const FULL_RESTORE_AREAS: [&str;5]=["library","referencedAssets","hypa","local-plugins","local-settings"];
fn restore_selection(restore_areas:Option<&[String]>)->Result<RestoreSelection> {
    if let Some(areas)=restore_areas {
        let unique=areas.iter().map(String::as_str).collect::<BTreeSet<_>>();
        if unique.len()!=areas.len() || unique!=FULL_RESTORE_AREAS.into_iter().collect() {return Err(corrupt());}
    }
    Ok(RestoreSelection{library:true,sections:BTreeSet::from(["hypa".into(),"local-plugins".into(),"local-settings".into()])})
}
fn require_restorable_sections(_selection:&RestoreSelection,snapshot:&PreparedRemoteSnapshot,_store_id:&str)->Result<()> {
    if snapshot.captured_by_device.as_deref().is_none_or(str::is_empty) {return Err(ProviderError::new(ErrorKind::PreconditionFailed));}
    Ok(())
}
fn checked_required_bytes(
    snapshot: &PreparedRemoteSnapshot,
    selection: &RestoreSelection,
) -> Result<u64> {
    let mut total = 0u64;
    let mut add = |length: u64| -> Result<()> {
        total = total.checked_add(length).ok_or_else(corrupt)?;
        Ok(())
    };
    if selection.library {
        for record in &snapshot.records {
            add(record.byte_length)?;
        }
        for object in &snapshot.objects {
            add(object.byte_length)?;
        }
    }
    Ok(total)
}

#[cfg(windows)]
fn available_space(path: &Path) -> std::io::Result<u64> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    wide.push(0);
    let mut available = 0u64;
    let result = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(available)
    }
}

#[cfg(unix)]
fn available_space(path: &Path) -> std::io::Result<u64> {
    use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt};
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid path"))?;
    let mut stats = MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let stats = unsafe { stats.assume_init() };
    Ok((stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64))
}

fn validate_download(
    connected: &ConnectedRepository,
    requested_snapshot: &str,
    snapshot: &PreparedRemoteSnapshot,
) -> Result<()> {
    if snapshot.snapshot_id != requested_snapshot
        || snapshot.repository_id != connected.stored.descriptor.repository_id
    {
        return Err(corrupt());
    }
    decode_hash(&snapshot.fingerprint)?;
    decode_hash(&snapshot.library_fingerprint)?;
    Ok(())
}

fn update_phase(root: &Path, job: &DurableJob, phase: &str) -> Result<()> {
    let store = JobStore::open(root)?;
    let mut current = store.read(&job.id)?;
    if current.request.connection_id != job.request.connection_id || current.terminal() {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    current.summary["state"] = json!("running");
    current.summary["phase"] = json!(phase);
    current.summary["updatedAtMs"] = json!(runtime::now_ms().to_string());
    store.put(&current)
}

pub(crate) async fn prepare_database_first_backup(
    root:&Path,connected:&ConnectedRepository,job:&DurableJob,cancel:&Cancellation,
)->Result<(snapshot_restore::DatabaseFirstSnapshot,Vec<super::sections::CapturedSection>)> {
    if job.request.kind!=JobKind::Restore {return Err(corrupt());}
    let snapshot_id=job.request.snapshot_id.as_deref().ok_or_else(corrupt)?;
    update_phase(&root, job, "downloading")?;
    let current=JobStore::open(&root)?.read(&job.id)?;
    let remote=if let Some(source)=current.summary.get("restoreSource") {
        let source:risunest_external_storage_format::snapshot::StoredObject=serde_json::from_value(source.clone()).map_err(|_|corrupt())?;
        super::packaging::RemoteObject::from_stored(&source,&connected.handle)?
    } else {
    let known = match ConnectionStore::open(&root)?
        .discovery_snapshot(&job.request.connection_id, snapshot_id)
    {
        Ok(value) => Some(value),
        Err(error) if error.kind == ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let cache_root = root.to_owned();
    let cache_connection = job.request.connection_id.clone();
    let cache_id = snapshot_id.to_owned();
    let remote = super::control::find_snapshot_with_locator_invalidation(
        connected,
        snapshot_id,
        known.as_ref(),
        move || {
            ConnectionStore::open(&cache_root)?.forget_discovery(&cache_connection, &cache_id)
        },
        cancel,
    )
    .await?;
    ConnectionStore::open(&root)?.remember_discovery(
        &job.request.connection_id,
        snapshot_id,
        &remote,
    )?;
    let jobs=JobStore::open(&root)?;
    let mut current=jobs.read(&job.id)?;
    current.summary["restoreSource"]=serde_json::to_value(remote.stored(&connected.handle)?).map_err(runtime::local_error)?;
    jobs.put(&current)?;
    remote
    };
    let staging_root = staging_directory(&root, job);
    let transferred = runtime::transfer_progress(&root, &job.id);
    let database=snapshot_restore::download_snapshot_database_first(&remote,&staging_root,&root,&root,
        &job.request.connection_id,&connected.root_key,connected.provider.as_ref(),&connected.handle,cancel).await?;
    let snapshot=&database.snapshot;
    cancel.check()?;
    validate_download(connected, snapshot_id, snapshot)?;
    let selection = restore_selection(job.request.restore_areas.as_deref())?;
    require_restorable_sections(&selection, snapshot, &job.admission_identity.store_id)?;
    let sections = snapshot_restore::download_sections(
        &remote,
        &selection.sections,
        &staging_root,
        &connected.root_key,
        None,
        connected.provider.as_ref(),
        &connected.handle,
        &transferred,
        cancel,
    )
    .await?;
    transferred.flush();
    if sections.len() != selection.sections.len() {
        return Err(ProviderError::new(ErrorKind::NotFound));
    }
    let required = checked_required_bytes(snapshot, &selection)?;
    if available_space(&staging_root).map_err(runtime::local_error)? < required {
        return Err(ProviderError::new(ErrorKind::StorageFull));
    }
    update_phase(&root, job, "preparing-local")?;

    Ok((database,sections))
}

pub(crate) async fn run_restore(
    app: &AppHandle,
    connected: &ConnectedRepository,
    job: &DurableJob,
    cancel: &Cancellation,
) -> Result<Value> {
    if job.request.kind != JobKind::Restore {
        return Err(corrupt());
    }
    if let Some(result) = completed_restore(app, job)? {
        return Ok(result);
    }
    if let Some(result)=completed_restore_in_store(&runtime::native_store(app)?,job)? {
        return finish_restore_bodies(app,connected,job,result,None,cancel).await;
    }
    let permit=app.state::<crate::native_file_jobs::NativeFileJobState>().admission.staging().map_err(runtime::local_error)?;
    let expected_revision = job
        .request
        .target_revision
        .as_deref()
        .ok_or_else(corrupt)?
        .parse::<i64>()
        .map_err(|_| corrupt())?;
    let root = runtime::root(app)?;
    let (database,sections)=prepare_database_first_backup(&root,connected,job,cancel).await?;
    let snapshot=database.snapshot;
    let original_units=database.original_units;
    let selection=restore_selection(job.request.restore_areas.as_deref())?;

    let worker_app = app.clone();
    let worker_job = job.clone();
    let worker_cancel = cancel.clone();
    let prepared = spawn_blocking(move || {
        prepare_local_restore(
            &worker_app,
            &worker_job,
            expected_revision,
            snapshot,
            original_units,
            selection,
            sections,
            worker_cancel,
            database.sources,
            database.present,
            permit,
        )
    })
    .await.map_err(runtime::local_error)?;
    let result=match prepared {
        Ok(result)=>result,
        Err(error)=>{
            if !application_started(&JobStore::open(&root)?.read(&job.id)?) {
                if let Ok(mut pins)=crate::asset_repository::job_pins::DurableCasJob::open(&root,&job.id) {
                    pins.release(crate::asset_repository::job_pins::CasReleaseOutcome::Aborted).map_err(runtime::local_error)?;
                }
            }
            return Err(error);
        }
    };
    finish_restore_bodies(app,connected,job,result.0,Some(result.1),cancel).await
}

fn restore_pins(root:&Path,id:&str)->Result<crate::asset_repository::job_pins::DurableCasJob> {
    use crate::asset_repository::job_pins::{DurableCasJob,CasJobKind};
    let pins=match DurableCasJob::begin(root,id,CasJobKind::LocalBackupRestore,runtime::now_ms() as i64) {
        Ok(pins)=>pins,
        Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists=>DurableCasJob::open(root,id).map_err(runtime::local_error)?,
        Err(error)=>return Err(runtime::local_error(error)),
    };
    if pins.kind()!=CasJobKind::LocalBackupRestore || pins.is_released() {return Err(corrupt());}
    Ok(pins)
}

async fn finish_restore_bodies(app:&AppHandle,connected:&ConnectedRepository,job:&DurableJob,result:Value,
    mut permit:Option<crate::native_file_jobs::admission::Permit>,cancel:&Cancellation)->Result<Value> {
    let root=runtime::root(app)?;
    let jobs=JobStore::open(&root)?;
    if jobs.read(&job.id)?.summary["restoreBodiesReady"]!=true {return Err(corrupt());}
    let revision=result["receivedRevision"].as_str().ok_or_else(corrupt)?.parse::<i64>().map_err(|_|corrupt())?;
    let adoption=loop {
        let current=jobs.read(&job.id)?;
        require_activated_target(&runtime::native_store(app)?,&current,revision)?;
        if let Some(value)=current.summary.get("restoreAdopted") {
            let request:RestoreAdoptionRequest=serde_json::from_value(value.clone()).map_err(|_|corrupt())?;
            confirm_restore_adoption_in_store(&runtime::native_store(app)?,&root,&request)?;
            break request;
        }
        if permit.is_none() {permit=Some(app.state::<crate::native_file_jobs::NativeFileJobState>().admission.file(true).map_err(runtime::local_error)?);}
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    drop(permit);
    receive_restore_bodies_in_store(runtime::native_store(app)?,&root,connected,job,result,adoption.selected_character_id,cancel).await
}

async fn receive_restore_bodies_in_store(mut store:PersistentStore,root:&Path,connected:&ConnectedRepository,job:&DurableJob,result:Value,selected_character_id:Option<String>,cancel:&Cancellation)->Result<Value> {
    let root=root.to_owned();
    let jobs=JobStore::open(&root)?;
    let current=jobs.read(&job.id)?;
    let job=&current;
    let revision=result["receivedRevision"].as_str().ok_or_else(corrupt)?.parse::<i64>().map_err(|_|corrupt())?;
    if completed_restore_in_store(&store,job)?.as_ref()!=Some(&result) {return Err(corrupt());}
    require_activated_target(&store,job,revision)?;
    update_phase(&root,job,"receiving-assets")?;
    let priority=match selected_character_id {
        Some(id)=>store.selected_character_asset_hashes(&id).map_err(pds_error)?.into_iter().collect::<BTreeSet<_>>(),
        None=>BTreeSet::new(),
    };
    let mut after=String::new();let mut sources=Vec::new();
    loop {
        let page=jobs.restore_body_page(&job.id,&after)?;
        if page.is_empty() {break;}
        for source in page {after=source.hash.clone();sources.push(source);}
    }
    let groups=super::lww_residency::packed_body_groups(sources,&priority);
    let stage=root.join("native-file-jobs/jobs").join(&job.id).join("external-restore-bodies");
    for mut group in groups {
        group.sort_by_key(|source|!priority.contains(&source.hash));
        let cas=crate::asset_repository::PayloadCas::new(&root).map_err(runtime::local_error)?;
        let mut missing=Vec::new();
        for source in group {
            match cas.stat_object(&source.hash).map_err(runtime::local_error)? {
                Some(size) if size==source.byte_length=>{store=receive_restore_body(store,&root,connected,job,revision,&source,None,cancel).await?;}
                Some(_)=>return Err(corrupt()),
                None=>missing.push(source),
            }
        }
        if missing.is_empty() {continue;}
        cancel.check()?;require_activated_target(&store,job,revision)?;
        let mut files=snapshot_restore::download_packed_body_files(&missing,&stage,&connected.root_key,connected.provider.as_ref(),&connected.handle,cancel).await?;
        for source in missing {
            let path=files.remove(&source.hash).ok_or_else(corrupt)?;
            store=receive_restore_body(store,&root,connected,job,revision,&source,Some(path),cancel).await?;
        }
        cleanup_staging(&stage);
    }
    cancel.check()?;
    require_activated_target(&store,job,revision)?;
    let worker_root=root.clone();let worker_id=job.id.clone();let worker_job=job.clone();
    spawn_blocking(move || {
        require_activated_target(&store,&worker_job,revision)?;
        let mut pins=restore_pins(&worker_root,&worker_id)?;
        pins.seal(&mut store,runtime::now_ms() as i64).map_err(runtime::local_error)?;
        pins.release(crate::asset_repository::job_pins::CasReleaseOutcome::Committed).map_err(runtime::local_error)?;
        let jobs=JobStore::open(&worker_root)?;
        let mut current=jobs.read(&worker_id)?;
        if !jobs.restore_bodies_settled(&worker_id)? {return Err(corrupt());}
        current.summary["restoreBodiesComplete"]=json!(true);
        jobs.put(&current)
    }).await.map_err(runtime::local_error)??;
    Ok(result)
}

async fn receive_restore_body(mut store:PersistentStore,root:&Path,connected:&ConnectedRepository,job:&DurableJob,revision:i64,
    source:&super::lww_residency::PackedSource,path:Option<std::path::PathBuf>,cancel:&Cancellation)->Result<PersistentStore> {
    cancel.check()?;
    require_activated_target(&store,job,revision)?;
    super::lww_residency::validate_packed_source(source,&connected.handle)?;
    let cas=crate::asset_repository::PayloadCas::new(root).map_err(runtime::local_error)?;
    match cas.stat_object(&source.hash).map_err(runtime::local_error)? {
        Some(size) if size==source.byte_length=>{
            let mut pins=restore_pins(root,&job.id)?;
            pins.pin_existing(&cas,&source.hash,source.byte_length,crate::asset_repository::job_pins::CasObjectRole::DirectObject).map_err(runtime::local_error)?;
        }
        Some(_)=>return Err(corrupt()),
        None=>{
            let path=path.ok_or_else(corrupt)?;
            cancel.check()?;
            require_activated_target(&store,job,revision)?;
            let worker_root=root.to_path_buf();let worker_id=job.id.clone();let worker_source=source.clone();let worker_cancel=cancel.clone();let worker_job=job.clone();
            store=spawn_blocking(move || {
                require_activated_target(&store,&worker_job,revision)?;
                let cas=crate::asset_repository::PayloadCas::new(&worker_root).map_err(runtime::local_error)?;
                let mut pins=restore_pins(&worker_root,&worker_id)?;
                let adopted=pins.adopt_import_payload(&cas,&path,&worker_source.hash,worker_source.byte_length,&|| {
                    worker_cancel.check().is_err() || require_activated_target(&store,&worker_job,revision).is_err()
                });
                require_activated_target(&store,&worker_job,revision)?;
                adopted.map_err(runtime::local_error)?;
                Ok::<_,ProviderError>(store)
            }).await.map_err(runtime::local_error)??;
        }
    }
    require_activated_target(&store,job,revision)?;
    JobStore::open(root)?.settle_restore_body(&job.id,&source.hash)?;
    Ok(store)
}

fn stage_original_backup_controls(
    store:&mut PersistentStore,
    snapshot:&PreparedRemoteSnapshot,
    units:&std::collections::BTreeMap<risunest_sync_wire::unit::UnitKey,risunest_sync_wire::unit::UnitValue>,
    cancel:&Cancellation,
) -> Result<()> {
    use crate::persistent_store::external_capture::{original_unit_dependency_inventory,BackupBodyRole};
    let content=super::content_store::ContentStore::open(&snapshot.staging_root.join("external-storage")).map_err(pds_error)?;
    let declared=snapshot.objects.iter().map(|object| (object.content_hash.as_str(),object)).collect::<std::collections::BTreeMap<_,_>>();
    if declared.len() != snapshot.objects.len() {return Err(corrupt())}
    let read=|hash:&str| -> crate::persistent_store::StoreResult<Option<Vec<u8>>> {
        cancel.check().map_err(|_| StoreError::Validation{message:"External restore cancelled".into()})?;
        let Some(object)=declared.get(hash) else {return Ok(None)};
        if matches!(object.source,super::content_store::ObjectSource::Library(_)) {return Ok(None)}
        if object.byte_length > risunest_sync_wire::MAX_METADATA_BYTES as u64 {return Err(StoreError::Validation{message:"Backup control exceeds its bound".into()})}
        let mut source=content.open_source(&object.source)?;
        let mut bytes=Vec::new();
        use std::io::Read;
        source.by_ref().take(risunest_sync_wire::MAX_METADATA_BYTES as u64+1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != object.byte_length {return Err(StoreError::Validation{message:"Backup control size differs".into()})}
        Ok(Some(bytes))
    };
    let size=|hash:&str| Ok(declared.get(hash).map(|object| object.byte_length));
    let probe=runtime::CancelProbe(cancel.clone());
    let metadata=original_unit_dependency_inventory(units,&read,&size,&probe,false,&mut |_,_,_| Ok(()));
    cancel.check()?;
    let metadata=metadata.map_err(pds_error)?;
    let mut spool=super::capture::BackupDependencySpool::new(&snapshot.staging_root.join("verified-original-controls")).map_err(pds_error)?;
    let inventory=original_unit_dependency_inventory(units,
        &|hash| if metadata.controls.contains_key(hash) {read(hash)} else {Ok(None)},
        &size,&probe,true,&mut |hash,bytes,role| Ok(spool.push(hash,bytes,role)?),
    );
    cancel.check()?;
    let inventory=inventory.map_err(pds_error)?;
    if !inventory.spooled_payloads.is_empty() {return Err(corrupt())}
    cancel.check()?;
    spool.seal().map_err(pds_error)?;
    let result=spool.visit(&mut |hash,bytes,role| {
        if cancel.check().is_err() {return Err(StoreError::Validation{message:"External restore cancelled".into()})}
        if role != BackupBodyRole::Control {return Err(StoreError::Validation{message:"Backup control role differs".into()})}
        store.lww_put_object(hash,bytes)
    });
    cancel.check()?;
    result.map_err(pds_error)
}

#[allow(clippy::too_many_arguments)]
fn prepare_local_restore(
    app: &AppHandle,
    job: &DurableJob,
    expected_revision: i64,
    snapshot: PreparedRemoteSnapshot,
    original_units: std::collections::BTreeMap<risunest_sync_wire::unit::UnitKey,risunest_sync_wire::unit::UnitValue>,
    selection: RestoreSelection,
    sections: Vec<super::sections::CapturedSection>,
    cancel: Cancellation,
    sources:Vec<super::lww_residency::PackedSource>,
    present:BTreeSet<String>,
    permit:crate::native_file_jobs::admission::Permit,
) -> Result<(Value,crate::native_file_jobs::admission::Permit)> {
    let mut store=runtime::native_store(app)?;
    prepare_local_restore_in_store(&mut store,&app.state::<PersistentStoreState>(),job,expected_revision,snapshot,original_units,selection,sections,cancel,sources,present,permit)
}

#[allow(clippy::too_many_arguments)]
fn prepare_local_restore_in_store(
    store:&mut PersistentStore,
    state:&PersistentStoreState,
    job: &DurableJob,
    expected_revision: i64,
    snapshot: PreparedRemoteSnapshot,
    original_units: std::collections::BTreeMap<risunest_sync_wire::unit::UnitKey,risunest_sync_wire::unit::UnitValue>,
    selection: RestoreSelection,
    sections: Vec<super::sections::CapturedSection>,
    cancel: Cancellation,
    sources:Vec<super::lww_residency::PackedSource>,
    present:BTreeSet<String>,
    mut permit:crate::native_file_jobs::admission::Permit,
) -> Result<(Value,crate::native_file_jobs::admission::Permit)> {
    cancel.check()?;
    // Open a dedicated native connection while renderer admission is still
    // available. It remains owned across the maintenance WebView reload.
    let root=store.repository_root().to_owned();
    let current_identity=store.external_identity().map_err(pds_error)?;
    runtime::require_admitted_library(job,&current_identity)?;
    if current_identity.selection_epoch != job.admission_identity.selection_epoch {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    cancel.check()?;
    if store.revision().map_err(pds_error)? != expected_revision {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    stage_original_backup_controls(store,&snapshot,&original_units,&cancel)?;
    JobStore::open(&root)?.freeze_restore_bodies(job,&sources,&present)?;
    let registrations=sources.iter().map(|source|crate::persistent_store::asset_object_catalog::AssetObjectRegistration{object_hash:source.hash.clone(),byte_size:source.byte_length}).collect::<Vec<_>>();
    for batch in registrations.chunks(crate::persistent_store::asset_object_catalog::ASSET_OBJECT_CATALOG_MAX_PAGE as usize) {
        store.asset_object_catalog().register(batch,runtime::now_ms() as i64).map_err(pds_error)?;
    }
    let current=JobStore::open(&root)?.read(&job.id)?;
    let previous_intent=current.summary.get("restoreCommit").map(|value|serde_json::from_value::<RestoreCommitIntent>(value.clone()).map_err(|_|corrupt())).transpose()?;
    if previous_intent.is_some() {completed_restore_in_store(store,&current)?;}
    for source in &sources {super::lww_residency::register_verified_packed(store.repository_root(),source)?;}
    let cas=crate::asset_repository::PayloadCas::new(store.repository_root()).map_err(runtime::local_error)?;
    let mut pins=restore_pins(store.repository_root(),&job.id)?;
    for batch in sources.iter().filter(|source|present.contains(&source.hash)).map(|source|(source.hash.clone(),source.byte_length,crate::asset_repository::job_pins::CasObjectRole::DirectObject)).collect::<Vec<_>>().chunks(128) {
        pins.pin_existing_batch(&cas,batch).map_err(runtime::local_error)?;
    }
    let snapshot_id = snapshot.snapshot_id.clone();
    let staging_root = snapshot.staging_root.clone();
    let prepared = if selection.library && previous_intent.is_none() {
        let scope_id = risunest_external_storage_format::format::library_fingerprint_domain();
        let fingerprint = decode_hash(&snapshot.library_fingerprint)?;
        let records = snapshot.records.into_iter().map(|record| {
            Ok(ExternalSnapshotRecord {
                key: record.key,
                content_hash: record.content_hash,
                byte_length: record.byte_length,
                source: record.source,
            })
        });
        let objects = snapshot.objects.into_iter().map(|object| {
            Ok(ExternalSnapshotObject {
                content_hash: object.content_hash,
                byte_length: object.byte_length,
                source: object.source,
            })
        });
        let probe = runtime::CancelProbe(cancel.clone());
        Some(
            store
                .prepare_external_snapshot_application(
                    &ExternalSnapshotApplication {
                        expected_revision,
                        staging_root: &staging_root,
                        scope_id: &scope_id,
                        fingerprint: &fingerprint,
                        probe: &probe,
                    },
                    records,
                    objects,
                )
                .map_err(|error| {
                    if cancel.check().is_err() {
                        ProviderError::new(ErrorKind::Cancelled)
                    } else {
                        pds_error(error)
                    }
                })?,
        )
    } else {
        None
    };

    // Preparing every selected section before touching the device file keeps a
    // bundle with a missing object from installing half of itself.
    let prepared_sections =
        super::sections::prepare_received_backup_sections(&sections, &cancel)?;
    let intent=if let Some(intent)=previous_intent {intent} else {
    let header = crate::persistent_store::lww::Header {
        binding_authority:store.lww_binding_authority().map_err(pds_error)?,
        request_id:format!("external-backup-restore:{}",job.id),
    };
    restore_intent(job,expected_revision,header,prepared.as_ref().ok_or_else(corrupt)?.external_staging_id())?
    };
    cancel.check()?;
    permit.upgrade_staging().map_err(runtime::local_error)?;
    let maintenance=state.acquire_device_maintenance().map_err(pds_error)?;
    if store.revision().map_err(pds_error)?!=expected_revision {return Err(ProviderError::new(ErrorKind::PreconditionFailed));}
    persist_restore_intent(&root,job,&intent)?;
    let revision = store.lww_commit_replacement_with_device_sections(
        &intent.header,&intent.staging_id,Some(&original_units),&prepared_sections.iter().collect::<Vec<_>>(),
    ).map_err(pds_error)?;
    drop(prepared);
    drop(maintenance);
    cleanup_staging(&staging_root);
    let result=json!({
        "snapshotId": snapshot_id,
        "receivedRevision": revision.revision.to_string()
    });
    let jobs=JobStore::open(&root)?;
    let mut current=jobs.read(&job.id)?;
    current.summary["result"]=result.clone();
    current.summary["phase"]=json!("awaiting-adoption");
    jobs.put(&current)?;
    Ok((result,permit))
}

#[cfg(test)]
pub(crate) fn activate_database_first_backup(
    store:&mut PersistentStore,state:&PersistentStoreState,job:&DurableJob,
    prepared:snapshot_restore::DatabaseFirstSnapshot,sections:Vec<super::sections::CapturedSection>,
    cancel:Cancellation,permit:crate::native_file_jobs::admission::Permit,
)->Result<(Value,crate::native_file_jobs::admission::Permit)> {
    let expected=job.request.target_revision.as_deref().ok_or_else(corrupt)?.parse::<i64>().map_err(|_|corrupt())?;
    let selection=restore_selection(job.request.restore_areas.as_deref())?;
    require_restorable_sections(&selection,&prepared.snapshot,&job.admission_identity.store_id)?;
    prepare_local_restore_in_store(store,state,job,expected,prepared.snapshot,prepared.original_units,
        selection,sections,cancel,prepared.sources,prepared.present,permit)
}

#[cfg(test)]
pub(crate) async fn settle_database_first_backup(
    store:PersistentStore,connected:&ConnectedRepository,job:&DurableJob,
    adoption:RestoreAdoptionRequest,permit:&mut Option<crate::native_file_jobs::admission::Permit>,cancel:&Cancellation,
)->Result<Value> {
    if permit.is_none() {return Err(ProviderError::new(ErrorKind::PreconditionFailed));}
    let root=store.repository_root().to_owned();
    let result=confirm_restore_adoption_in_store(&store,&root,&adoption)?;
    drop(permit.take());
    receive_restore_bodies_in_store(store,&root,connected,job,result,adoption.selected_character_id,cancel).await
}

fn staging_directory(root: &Path, job: &DurableJob) -> std::path::PathBuf {
    runtime::job_directory(root, &job.request.connection_id, &job.id).join("restore-snapshot")
}

/// A finished restore never reads its download again, whether it succeeded,
/// failed or was cancelled.
pub(crate) fn discard_finished_staging(root: &Path, job: &DurableJob) {
    if job.request.kind == JobKind::Restore && job.terminal() {
        cleanup_staging(&staging_directory(root, job));
    }
}

fn cleanup_staging(path: &Path) {
    let safe = std::fs::symlink_metadata(path)
        .ok()
        .is_some_and(|metadata| {
            metadata.is_dir() && !crate::trust_boundary::is_link_like(&metadata)
        });
    if safe {
        let _ = std::fs::remove_dir_all(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistent_store::sync_selection::CaptureIdentity;

    fn identity() -> CaptureIdentity {
        CaptureIdentity {
            store_id: "store".into(),
            library_epoch: "library".into(),
            generation: "generation".into(),
            selection_epoch: "selection".into(),
            revision: 0,
        }
    }

    #[test]
    fn original_control_admission_certifies_held_manifest_and_rejects_missing_tree_before_install() {
        use risunest_sync_wire::{descriptor::RecordDescriptor,unit::{UnitKey,UnitValue}};
        let directory=tempfile::tempdir().unwrap();
        let mut store=PersistentStore::open(directory.path()).unwrap();
        let staging=directory.path().join("staging");
        let mut content=super::super::content_store::ContentStore::open(&staging.join("external-storage")).unwrap();
        let manifest=risunest_external_storage_format::message_pages::MessageManifest {
            schema:risunest_external_storage_format::message_pages::MANIFEST_SCHEMA.into(),message_count:0.into(),pages:Vec::new(),
        }.encode().unwrap();
        content.put(&manifest.hash,&manifest.bytes).unwrap();
        content.commit().unwrap();
        let units=std::collections::BTreeMap::from([(UnitKey::new(&["messages","missing-character","missing-conversation"]).unwrap(),UnitValue::object(RecordDescriptor::content(manifest.hash.clone())).unwrap())]);
        let mut snapshot=prepared(Some("source-device"));
        snapshot.staging_root=staging;
        snapshot.objects.push(snapshot_restore::PreparedObject {content_hash:manifest.hash.clone(),byte_length:manifest.bytes.len() as u64,source:super::super::content_store::ObjectSource::Captured(manifest.hash.clone())});
        let mut invalid_units=units.clone();
        let mut invalid=RecordDescriptor::content(risunest_sync_wire::hash(b"synthetic absent opaque payload"));
        invalid.dependency_root=Some("ab".repeat(32));
        invalid_units.insert(UnitKey::new(&["z-future-unit","missing-tree"]).unwrap(),UnitValue::object(invalid).unwrap());
        assert!(stage_original_backup_controls(&mut store,&snapshot,&invalid_units,&Cancellation::default()).is_err());
        assert!(store.lww_object_body(&manifest.hash).unwrap().is_none());
        stage_original_backup_controls(&mut store,&snapshot,&units,&Cancellation::default()).unwrap();
        assert_eq!(store.lww_object_body(&manifest.hash).unwrap(),Some(manifest.bytes));
    }

    #[test]
    fn original_control_admission_never_opens_present_opaque_payload() {
        use risunest_sync_wire::{descriptor::RecordDescriptor,unit::{UnitKey,UnitValue}};
        let directory=tempfile::tempdir().unwrap();
        let mut store=PersistentStore::open(directory.path()).unwrap();
        let body=crate::asset_repository::PayloadCas::new(directory.path()).unwrap().prepare_bytes(b"synthetic present opaque payload").unwrap();
        let units=std::collections::BTreeMap::from([(UnitKey::new(&["future-unit","present-body"]).unwrap(),UnitValue::object(RecordDescriptor::content(body.content_hash.clone())).unwrap())]);
        let mut snapshot=prepared(Some("source-device"));
        snapshot.staging_root=directory.path().join("staging");
        snapshot.objects.push(snapshot_restore::PreparedObject {content_hash:body.content_hash.clone(),byte_length:body.byte_size,source:super::super::content_store::ObjectSource::Library(body.content_hash)});
        crate::asset_repository::body_io::reset_body_io();
        stage_original_backup_controls(&mut store,&snapshot,&units,&Cancellation::default()).unwrap();
        let observed=crate::asset_repository::body_io::take_body_io();
        assert!(observed.complete());
        assert_eq!(observed.asset_work(),Default::default());
        assert!(observed.objects.is_empty());
    }

    fn commit_fixture(store:&mut PersistentStore,root:&Path,job:&DurableJob) -> DurableJob {
        use risunest_external_storage_format::section::SectionKind;
        JobStore::open(root).unwrap().put(job).unwrap();
        let source_dir=tempfile::tempdir().unwrap();
        let mut source=PersistentStore::open(source_dir.path()).unwrap();
        source.device_store_mut().unwrap().write_setting("accountst",&json!("synthetic restored setting")).unwrap();
        let sections=source.device_store_mut().unwrap().capture_backup_sections(&[SectionKind::Hypa,SectionKind::LocalPlugins,SectionKind::LocalSettings]).unwrap();
        let stage=store.replace_begin().unwrap();
        store.replace_put_root(&stage.staging_id,&json!({"language":"synthetic restored language"})).unwrap();
        let header=crate::persistent_store::lww::Header {binding_authority:store.lww_binding_authority().unwrap(),request_id:format!("external-backup-restore:{}",job.id)};
        let intent=restore_intent(job,0,header,&stage.staging_id).unwrap();
        persist_restore_intent(root,job,&intent).unwrap();
        let pending=JobStore::open(root).unwrap().read(&job.id).unwrap();
        assert!(completed_restore_in_store(store,&pending).unwrap().is_none());
        store.lww_commit_replacement_with_device_sections(&intent.header,&intent.staging_id,Some(&Default::default()),&sections.iter().collect::<Vec<_>>()).unwrap();
        assert_eq!(store.device_store().unwrap().read_setting("accountst").unwrap(),Some(json!("synthetic restored setting")));
        pending
    }

    #[test]
    fn renderer_adoption_accepts_exact_durable_restore_and_refuses_another_generation() {
        let root=tempfile::tempdir().unwrap();
        let mut store=PersistentStore::open(root.path()).unwrap();
        let input=serde_json::from_value(json!({"connectionId":"synthetic-connection","kind":"restore","snapshotId":"synthetic-snapshot","targetRevision":"0"})).unwrap();
        let job=DurableJob::new(input,false,1,store.external_identity().unwrap());
        let pending=commit_fixture(&mut store,root.path(),&job);
        let receipt=completed_restore_in_store(&store,&pending).unwrap().unwrap();
        let request=RestoreAdoptionRequest{job_id:job.id.clone(),received_revision:receipt["receivedRevision"].as_str().unwrap().into(),selected_character_id:None};
        assert_eq!(confirm_restore_adoption_in_store(&store,root.path(),&request).unwrap(),receipt);
        let adopted=JobStore::open(root.path()).unwrap().read(&job.id).unwrap();
        assert_eq!(confirm_restore_adoption_in_store(&store,root.path(),&request).unwrap(),receipt);
        assert_eq!(JobStore::open(root.path()).unwrap().read(&job.id).unwrap().summary,adopted.summary);
        let mut wrong=request.clone(); wrong.selected_character_id=Some("synthetic-missing-character".into());
        assert_eq!(confirm_restore_adoption_in_store(&store,root.path(),&wrong).unwrap_err().kind,ErrorKind::PreconditionFailed);
        let mut wrong=request.clone(); wrong.received_revision="01".into();
        assert_eq!(confirm_restore_adoption_in_store(&store,root.path(),&wrong).unwrap_err().kind,ErrorKind::Corrupt);
        wrong.received_revision="2".into();
        assert_eq!(confirm_restore_adoption_in_store(&store,root.path(),&wrong).unwrap_err().kind,ErrorKind::PreconditionFailed);
        let stage=store.replace_begin().unwrap();
        store.replace_put_root(&stage.staging_id,&json!({"language":"synthetic later replacement"})).unwrap();
        store.replace_commit(&stage.staging_id,Some(1)).unwrap();
        assert_eq!(confirm_restore_adoption_in_store(&store,root.path(),&request).unwrap_err().kind,ErrorKind::PreconditionFailed);
    }

    #[test]
    fn identical_adoption_survives_ordinary_selected_character_deletion() {
        let root=tempfile::tempdir().unwrap();
        let mut store=PersistentStore::open(root.path()).unwrap();
        let input=serde_json::from_value(json!({"connectionId":"synthetic-connection","kind":"restore","snapshotId":"synthetic-snapshot","targetRevision":"0"})).unwrap();
        let job=DurableJob::new(input,false,1,store.external_identity().unwrap());
        let pending=commit_fixture(&mut store,root.path(),&job);
        let receipt=completed_restore_in_store(&store,&pending).unwrap().unwrap();
        store.commit(&crate::persistent_store::WorkingSetCommit{expected_revision:1,
            add_character:Some(json!({"chaId":"synthetic-selected","type":"character","name":"Synthetic selected","chats":[]})),
            ..Default::default()}).unwrap();
        let request=RestoreAdoptionRequest{job_id:job.id.clone(),received_revision:receipt["receivedRevision"].as_str().unwrap().into(),selected_character_id:Some("synthetic-selected".into())};
        assert_eq!(confirm_restore_adoption_in_store(&store,root.path(),&request).unwrap(),receipt);
        store.commit(&crate::persistent_store::WorkingSetCommit{expected_revision:2,
            delete_character_ids:Some(vec!["synthetic-selected".into()]),..Default::default()}).unwrap();
        assert!(store.read_character_summary("synthetic-selected",None).unwrap().is_none());
        assert_eq!(confirm_restore_adoption_in_store(&store,root.path(),&request).unwrap(),receipt);
    }

    #[test]
    fn local_application_evidence_survives_reopen_and_later_phase_updates() {
        let root=tempfile::tempdir().unwrap();
        let input=serde_json::from_value(json!({"connectionId":"synthetic-connection","kind":"restore","snapshotId":"synthetic-snapshot","targetRevision":"0"})).unwrap();
        let job=DurableJob::new(input,false,1,identity());
        JobStore::open(root.path()).unwrap().put(&job).unwrap();
        mark_application_started(root.path(),&job).unwrap();
        update_phase(root.path(),&job,"downloading").unwrap();
        let mut store=PersistentStore::open(root.path()).unwrap();
        let pending=commit_fixture(&mut store,root.path(),&job);
        let receipt=completed_restore_in_store(&store,&pending).unwrap().unwrap();
        update_phase(root.path(),&pending,"settling-bodies").unwrap();
        drop(store);
        let reopened=PersistentStore::open(root.path()).unwrap();
        let durable=JobStore::open(root.path()).unwrap().read(&job.id).unwrap();
        assert!(application_started(&durable));
        assert_eq!(durable.summary["phase"],"settling-bodies");
        for _ in 0..2 {assert_eq!(completed_restore_in_store(&reopened,&durable).unwrap(),Some(receipt.clone()));}
        assert_eq!(reopened.revision().unwrap().to_string(),receipt["receivedRevision"].as_str().unwrap());
    }

    #[test]
    fn restore_area_selection_matches_the_full_renderer_contract() {
        let areas=FULL_RESTORE_AREAS.map(str::to_owned);
        assert_eq!(restore_selection(None).unwrap(),restore_selection(Some(&areas)).unwrap());
        assert_eq!(restore_selection(None).unwrap().sections.len(),3);
    }
    #[test]
    fn restore_area_selection_rejects_partial_duplicate_and_undeclared_areas() {
        for areas in [vec!["library".into()],vec!["hypa".into()],vec!["devicePlugins".into()],vec!["library".into(),"library".into()],vec![]] {assert!(restore_selection(Some(&areas)).is_err());}
    }
    fn prepared(captured_by_device: Option<&str>) -> PreparedRemoteSnapshot {
        PreparedRemoteSnapshot {
            snapshot_id: "synthetic-snapshot".into(),
            repository_id: "synthetic-repository".into(),
            fingerprint: "00".repeat(32),
            library_fingerprint: "00".repeat(32),
            logical_revision: 1,
            staging_root: std::path::PathBuf::from("staging"),
            records: Vec::new(),
            objects: Vec::new(),
            captured_by_device: captured_by_device.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn complete_backup_restores_device_sections_across_devices_but_periodic_state_does_not() {
        let selection=restore_selection(None).unwrap();
        assert!(require_restorable_sections(&selection,&prepared(Some("other-device")),"this-device").is_ok());
        assert!(require_restorable_sections(&selection,&prepared(None),"this-device").is_err());
        assert!(require_restorable_sections(&selection,&prepared(Some("")),"this-device").is_err());
    }
    #[test]
    fn library_restore_receipt_recovers_only_the_exact_durable_request() {
        let root=tempfile::tempdir().unwrap();
        let mut store=PersistentStore::open(root.path()).unwrap();
        let request=serde_json::from_value(json!({"connectionId":"synthetic-connection","kind":"restore","snapshotId":"synthetic-snapshot","targetRevision":"0"})).unwrap();
        let job=DurableJob::new(request,false,1,identity());
        assert!(completed_restore_in_store(&store,&job).unwrap().is_none());
        let pending=commit_fixture(&mut store,root.path(),&job);
        assert!(completed_restore_in_store(&store,&pending).unwrap().is_some());
        let mut different=pending.clone();
        different.request.snapshot_id=Some("different-snapshot".into());
        assert!(completed_restore_in_store(&store,&different).is_err());
        different=pending.clone();
        different.summary["restoreCommit"]["stagingId"]=json!("different-stage");
        assert!(matches!(completed_restore_in_store(&store,&different),Err(ProviderError{kind:ErrorKind::Corrupt,..})));
        different=pending.clone();
        different.summary["restoreCommit"]["header"]["bindingAuthority"]=json!("1");
        assert!(matches!(completed_restore_in_store(&store,&different),Err(ProviderError{kind:ErrorKind::Corrupt,..})));
        store.set_app_kv(&format!("external-restore-commit:{}",job.id),&json!({"receivedRevision":"999"})).unwrap();
        assert!(completed_restore_in_store(&store,&job).unwrap().is_none());
    }
}
