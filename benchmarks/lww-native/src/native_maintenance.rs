use super::*;

pub(crate) async fn remote_backup_restore_raw(
    store:&mut PersistentStore,state:&crate::persistent_store::commands::PersistentStoreState,
    connected:&crate::external_storage::connection_commands::ConnectedRepository,
    job:&crate::external_storage::job_store::DurableJob,provider:&Arc<crate::external_storage::fake::FakeProvider>,
    permit:crate::native_file_jobs::admission::Permit,asset_hashes:&[String],selected_character_id:Option<String>,
    iteration:u32,all_present:bool,
) ->Result<(Value,Option<crate::native_file_jobs::admission::Permit>),String> {
    use crate::external_storage::{runtime_restore,worker_observation};
    if iteration>=6 {return Err("remote backup slot differs from the fixed matrix".into());}
    if Arc::as_ptr(&connected.provider) as *const ()!=Arc::as_ptr(provider) as *const () {
        return Err("remote backup IO observer belongs to a different provider".into());
    }
    let root=store.repository_root().to_owned();
    let requests_before=provider.read_count()+provider.upload_count()+provider.listing_count();
    let bytes_before=provider.transferred_body_bytes();
    reset_work(asset_hashes);worker_observation::begin();
    let cancel=Cancellation::default();
    let started=Instant::now();
    let mut held=Some(permit);
    let mut preparation=None;
    let mut activation=None;
    let mut activation_ms=None;
    let mut body=None;
    let mut errors=vec![];
    let outcome=async {
        let (prepared,sections)=runtime_restore::prepare_database_first_backup(&root,connected,job,&cancel)
            .await.map_err(|e|format!("{e:?}"))?;
        preparation=Some((prepared.required.len(),prepared.present.len(),prepared.missing.len(),sections.len()));
        let (receipt,guard)=runtime_restore::activate_database_first_backup(store,state,job,prepared,sections,
            cancel.clone(),held.take().ok_or("actual remote restore permit missing")?).map_err(|e|format!("{e:?}"))?;
        held=Some(guard);
        let revision=receipt["receivedRevision"].as_str().ok_or("actual remote activation receipt missing revision")?.to_owned();
        activation=Some(receipt);activation_ms=Some(started.elapsed().as_secs_f64()*1000.0);
        let worker=store.open_native_job_store().map_err(|e|e.to_string())?;
        body=Some(runtime_restore::settle_database_first_backup(worker,connected,job,runtime_restore::RestoreAdoptionRequest {
            job_id:job.id.clone(),received_revision:revision,selected_character_id},&mut held,&cancel)
            .await.map_err(|e|format!("{e:?}"))?);
        Ok::<(),String>(())
    }.await;
    let elapsed_ms=started.elapsed().as_secs_f64()*1000.0;
    let mut observation=take_work(true);
    for worker in worker_observation::take() {
        let mut incomplete=worker.hashes.incomplete.into_iter().map(|(domain,count)|format!("{domain}:{count}")).collect::<Vec<_>>();
        if !worker.completed {incomplete.push("actual external worker did not complete".into());}
        let receipt=native_observation::NativeWorkerHashReceipt {thread:worker.thread,
            domains:worker.hashes.domains.into_iter().map(|(name,work)|(name.into(),native_observation::HashDomain {calls:work.calls,bytes:work.bytes})).collect(),incomplete};
        if let Err(error)=observation.attach_worker_hash_receipt(receipt) {errors.push(error);}
    }
    if let Err(error)=outcome {errors.push(error);}
    if let Err(error)=observation.validate_costly_native_coverage() {errors.push(error);}
    let bytes_after=provider.transferred_body_bytes();
    let requests_after=provider.read_count()+provider.upload_count()+provider.listing_count();
    let frozen_inventory=(||->Result<Value,String> {
        let connection=rusqlite::Connection::open_with_flags(root.join("external-jobs.sqlite"),rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e|e.to_string())?;
        let mut statement=connection.prepare("SELECT hash,present,settled,json_extract(source,'$.byteLength') FROM external_restore_bodies WHERE job_id=?1 ORDER BY hash")
            .map_err(|e|e.to_string())?;
        let rows=statement.query_map([&job.id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,bool>(1)?,row.get::<_,bool>(2)?,sql_u64(row,3)?)))
            .map_err(|e|e.to_string())?;
        let mut result=std::collections::BTreeMap::new();
        for row in rows {let (hash,present,settled,size)=row.map_err(|e|e.to_string())?;
            result.insert(hash,json!({"byteSize":size,"alreadyPresent":present,"settled":settled}));}
        Ok(json!(result))
    })();
    let frozen_inventory=match frozen_inventory {Ok(value)=>Some(value),Err(error)=>{errors.push(error);None}};
    let preparation=preparation.map(|(required,present,missing,sections)|json!({"requiredCount":required,
        "alreadyPresentCount":present,"missingCount":missing,"sectionCount":sections}));
    let mut raw=json!({"status":"INVALID","route":"remote-backup","scenario":if all_present {"restore-all-present"} else {"restore-missing"},
        "iteration":iteration,"warmup":iteration==0,"preparationReceipt":preparation,"frozenBodyInventory":frozen_inventory,"activationReceipt":activation,
        "nativeActivationMs":activation_ms,"nativeBodyPhaseCompleteMs":body.as_ref().map(|_|elapsed_ms),"bodyCompletion":body,
        "sourceObservation":null,"rendererAdoption":null,"providerRequests":requests_after.checked_sub(requests_before),
        "sentBodyBytes":bytes_after.0.checked_sub(bytes_before.0),"receivedBodyBytes":bytes_after.1.checked_sub(bytes_before.1),
        "nativeObservation":observation,"errors":errors});
    // The caller retains this actual guard for any pending operation or failed ACK.
    raw["admissionReturnedToOwner"]=json!(held.is_some());
    Ok((raw,held))
}

pub(crate) async fn external_bootstrap_raw(
    store:&mut PersistentStore,engine:&crate::external_storage::lww_engine::ExternalLwwEngine,
    connected:Arc<crate::external_storage::connection_commands::ConnectedRepository>,
    provider:&Arc<crate::external_storage::fake::FakeProvider>,asset_hashes:&[String],selected_character_id:Option<&str>,
    scenario:measurement::Scenario,iteration:u32,
) ->Result<Value,String> {
    if !matches!(scenario,measurement::Scenario::FullBootstrap|measurement::Scenario::RestoreAllPresent
        |measurement::Scenario::RestoreMissing|measurement::Scenario::ConsolidatedSnapshot|measurement::Scenario::IncomparableSnapshots)
        || iteration>=6 {return Err("external bootstrap slot differs from the fixed matrix".into());}
    if Arc::as_ptr(&engine.provider) as *const ()!=Arc::as_ptr(provider) as *const ()
        || Arc::as_ptr(&connected.provider) as *const ()!=Arc::as_ptr(provider) as *const () {
        return Err("external bootstrap observer belongs to a different provider".into());
    }
    let writer_before=store.lww_clock_state().map_err(|e|e.to_string())?.writer_id;
    let before=store.lww_binding_state().map_err(|e|e.to_string())?;
    if before.target!=SyncTarget::None {return Err("full serverless bootstrap requires a fresh unbound destination".into());}
    let _source_connection=crate::external_storage::lww_residency::install_test_source_connection(store.repository_root(),connected)
        .map_err(|e|format!("{e:?}"))?;
    let target=SyncTarget::External(engine.connection_id.clone());
    let requests_before=provider.read_count()+provider.upload_count()+provider.listing_count();
    let bytes_before=provider.transferred_body_bytes();
    reset_work(asset_hashes);
    crate::external_storage::worker_observation::begin();
    let cancel=Cancellation::default();
    let started=Instant::now();
    let mut activation=None;
    let mut activation_ms=None;
    let mut bodies_ms=None;
    let mut replacement=None;
    let mut receive_finishes=None;
    let mut errors=vec![];
    let outcome=async {
        let objects=engine.listing(&cancel).await.map_err(|e|format!("{e:?}"))?;
        let snapshots=engine.snapshot_listing(&cancel).await.map_err(|e|format!("{e:?}"))?;
        if objects.is_empty() && snapshots.is_empty() {return Err("full external bootstrap requires a nonempty actual target".into());}
        let inspection=store.register_lww_binding_inspection(before.target_authority.clone(),&target,
            &engine.repository.connection_identity,&engine.library).map_err(|e|e.to_string())?;
        let stage_header=lww::Header {binding_authority:before.target_authority.clone(),request_id:uuid::Uuid::new_v4().to_string()};
        let staged=engine.stage_binding(store,&stage_header,&inspection,&cancel).await.map_err(|e|format!("{e:?}"))?;
        let switched=store.switch_lww_binding(&SwitchBindingRequest { initial_publication: false,header:lww::Header {
            binding_authority:before.target_authority.clone(),request_id:uuid::Uuid::new_v4().to_string()},
            expected_selection_epoch:before.selection_epoch.clone(),target,inspection_id:Some(inspection)}).map_err(|e|e.to_string())?;
        let request=crate::persistent_store::sync_selection::ReplaceBindingRequest {
            header:lww::Header {binding_authority:switched.target_authority,request_id:stage_header.request_id.clone()},
            expected_selection_epoch:switched.selection_epoch,staging_id:staged.staging_id,receive_id:stage_header.request_id,
            target_id:engine.repository.connection_identity.clone(),library_id:engine.library.clone()};
        let receipt=store.replace_lww_binding(&request).map_err(|e|e.to_string())?;
        activation=Some(receipt.revision);activation_ms=Some(started.elapsed().as_secs_f64()*1000.0);
        replacement=Some(request);
        let active_authority=store.lww_binding_authority().map_err(|e|e.to_string())?;
        receive_finishes=Some(engine.receive_and_apply(store,active_authority,&[],&cancel).await.map_err(|e|format!("{e:?}"))?);
        if store.server_asset_policy().map_err(|e|format!("{e:?}"))?!=server_sync::residency::AssetPolicy::Full {
            return Err("all-local bootstrap completion requires the actual Full policy".into());
        }
        let worker=store.open_native_job_store().map_err(|e|e.to_string())?;
        let authority=worker.lww_binding_authority().map_err(|e|e.to_string())?;
        let identity=worker.external_identity().map_err(|e|e.to_string())?;
        let selected=selected_character_id.map(str::to_owned);
        let worker_cancel=cancel.clone();
        crate::external_storage::worker_observation::spawn_blocking(move || {
            worker.hydrate_registered_remote_assets_prioritized(None,selected.as_deref(),|| {
                worker_cancel.check().map_err(|_|server_sync::SyncError::new("cancelled",409))?;
                let current=worker.external_identity().map_err(|_|server_sync::SyncError::new("store-error",500))?;
                if worker.lww_binding_authority().map_err(|_|server_sync::SyncError::new("store-error",500))?!=authority
                    || current.store_id!=identity.store_id || current.library_epoch!=identity.library_epoch || current.generation!=identity.generation {
                    return Err(server_sync::SyncError::new("sync-authority-changed",409));
                }
                Ok(())
            },||{}).map_err(|e|format!("{e:?}"))
        }).await.map_err(|e|e.to_string())??;
        bodies_ms=Some(started.elapsed().as_secs_f64()*1000.0);
        Ok::<(),String>(())
    }.await;
    let mut observation=take_work(true);
    for worker in crate::external_storage::worker_observation::take() {
        let mut incomplete=worker.hashes.incomplete.into_iter().map(|(domain,count)|format!("{domain}:{count}")).collect::<Vec<_>>();
        if !worker.completed {incomplete.push("actual external worker did not complete".into());}
        let receipt=native_observation::NativeWorkerHashReceipt {thread:worker.thread,
            domains:worker.hashes.domains.into_iter().map(|(name,work)|(name.into(),native_observation::HashDomain {calls:work.calls,bytes:work.bytes})).collect(),incomplete};
        if let Err(error)=observation.attach_worker_hash_receipt(receipt) {errors.push(error);}
    }
    if let Err(error)=outcome {errors.push(error);}
    let bytes_after=provider.transferred_body_bytes();
    let requests_after=provider.read_count()+provider.upload_count()+provider.listing_count();
    let mut replay=None;
    if let Some(request)=replacement {
        match store.replace_lww_binding(&request) {Ok(receipt)=>replay=Some(receipt.revision),Err(error)=>errors.push(error.to_string())}
    }
    let body_completion=if bodies_ms.is_some() {
        match store.asset_residency_status() {
            Ok(status)=>{
                let all_local=!status.has_remote_or_missing();
                if !all_local {errors.push("actual post-hydration residency still has remote or unavailable objects".into());}
                Some(json!({"residency":status,"allBodiesLocal":all_local,"verification":"actual full referenced native residency inventory after scope"}))
            },
            Err(error)=>{errors.push(format!("{error:?}"));None},
        }
    }else {None};
    Ok(json!({"status":"INVALID","route":"serverless-bootstrap","scenario":scenario,"iteration":iteration,"warmup":iteration==0,
        "writerBefore":writer_before,"writerAfter":store.lww_clock_state().ok().map(|clock|clock.writer_id),"activationRevision":activation,"replayRevision":replay,
        "nativeActivationMs":activation_ms,"actualReceiveFinishes":receive_finishes,"nativeHydrationConsumerReturnedMs":bodies_ms,"bodyCompletion":body_completion,"sourceObservation":null,"rendererAdoption":null,
        "providerRequests":requests_after.checked_sub(requests_before),"sentBodyBytes":bytes_after.0.checked_sub(bytes_before.0),
        "receivedBodyBytes":bytes_after.1.checked_sub(bytes_before.1),"nativeObservation":observation,
        "unobserved":["physical external source receipt","renderer adoption"],"errors":errors}))
}

pub(crate) fn portable_restore_raw(
    source:crate::native_file_jobs::OpenedJobSource,store:PersistentStore,owned:std::path::PathBuf,
    jobs:Arc<crate::native_file_jobs::NativeFileJobState>,
    persistent:Arc<crate::persistent_store::commands::PersistentStoreState>,
    device:Arc<crate::device_backup::DeviceBackupState>,asset_hashes:Vec<String>,iteration:u32,all_present:bool,
) ->Result<Value,String> {
    use crate::{native_file_jobs::{JobPhase,JobState,portable},portable_backup::source_io};
    if iteration>=6 {return Err("portable slot is outside the frozen one-plus-five policy".into());}
    let revision=store.revision().map_err(|e|e.to_string())?;
    reset_work(&asset_hashes);
    source_io::reset_source_io();
    let (job,permit)=jobs.create_portable_restore_fixture(revision).map_err(|e|format!("{}: {}",e.code,e.message))?;
    let worker_job=Arc::clone(&job);
    let worker_device=Arc::clone(&device);
    let worker_permit=Arc::clone(&permit);
    let started=Instant::now();
    let worker=std::thread::spawn(move || {
        let _retained_admission=worker_permit;
        let context=portable::NativePortableRestoreContext {persistent:&persistent,coordinator:&worker_device};
        portable_restore_worker_raw(source,store,&owned,&worker_job,&context,&asset_hashes,iteration,all_present)
    });
    let mut selected=false;
    let mut finalized=false;
    let mut adopted=false;
    let mut observed_activation_ms=None;
    let mut errors=vec![];
    let mut retired=false;
    let mut cleanup_failed=false;
    let mut terminal_status=None;
    while !worker.is_finished() {
        let status=job.status();
        if matches!(status.state,JobState::Failed|JobState::Cancelled) {
            terminal_status=Some(status);
            errors.push("actual portable body worker failed or was cancelled".into());
            match jobs.begin_cleanup() {Ok(())=>retired=true,Err(error)=>{errors.push(error);cleanup_failed=true;}}
            break;
        }
        let outcome=(||->Result<(),String> {
            if !selected && status.state==JobState::WaitingForInput && status.phase==JobPhase::AwaitingBackupSelection {
                jobs.select_portable_restore_fixture(&job.id(),portable::PortableSelection::default())
                    .map_err(|e|format!("{}: {}",e.code,e.message))?;
                selected=true;
            }
            if !finalized && status.state==JobState::WaitingForInput && status.phase==JobPhase::AwaitingActivation {
                jobs.finalize(&job.id(),Some(revision)).map_err(|e|format!("{}: {}",e.code,e.message))?;
                finalized=true;
            }
            if !adopted {
                if let Some(activated)=status.activation_revision {
                    observed_activation_ms=Some(started.elapsed().as_secs_f64()*1000.0);
                    let authority=status.activation_authority.as_deref().ok_or("portable activation authority missing")?;
                    let session=status.device_session_id.as_deref().ok_or("portable activation device session missing")?;
                    device.recovery_complete(session).map_err(|e|e.message)?;
                    jobs.confirm_portable_restore_adoption(&device,&job.id(),&activated.to_string(),authority,session)
                        .map_err(|e|format!("{}: {}",e.code,e.message))?;
                    adopted=true;
                }
            }
            Ok(())
        })();
        if let Err(error)=outcome {
            errors.push(error);
            terminal_status=Some(job.status());
            match jobs.begin_cleanup() {Ok(())=>retired=true,Err(error)=>{errors.push(error);cleanup_failed=true;}}
            break;
        }
        std::thread::yield_now();
    }
    let settled=!cleanup_failed && (retired || worker.is_finished() || (errors.is_empty() && adopted));
    let worker_raw=if settled {
        match worker.join() {
            Ok(Ok(raw))=>Some(raw),
            Ok(Err(error))=>{errors.push(error);None},
            Err(_)=>{errors.push("actual portable restore worker panicked".into());None},
        }
    } else {
        // Failed retirement leaves the actual waiter pending; do not invent ACK or join it.
        drop(worker);None
    };
    let parent=take_work(false);
    let source=if settled {source_io::take_source_io()} else {source_io::snapshot_source_io()};
    if !source.complete() {errors.push("portable source workers or reads remain incomplete".into());}
    if let Err(error)=parent.validate_costly_native_coverage() {errors.push(error);}
    Ok(json!({"status":"INVALID","route":"device-file-backup","scenario":if all_present {"restore-all-present"} else {"restore-missing"},
        "iteration":iteration,"warmup":iteration==0,"selected":selected,"finalized":finalized,"nativeAdoptionConfirmed":adopted,
        "jobStatus":terminal_status.unwrap_or_else(||job.status()),"nativeActivationStatusObservedMs":observed_activation_ms,
        "nativeActivationMs":null,"rendererAdoption":null,"workerJoined":settled,
        "nativeParentObservation":parent,"nativeWorkerRaw":worker_raw,"sourceObservation":portable_source_receipt(&source),"errors":errors}))
}

pub(crate) fn snapshot_restore_raw(
    store:&mut PersistentStore,snapshot_id:&str,request_id:&str,
    jobs:&crate::native_file_jobs::NativeFileJobState,scratch:&Path,asset_hashes:&[String],
    iteration:u32,all_present:bool,native_activated:impl FnOnce(i64)->Result<(),String>,
) ->Result<Value,String> {
    if iteration>=6 {return Err("snapshot slot is outside the frozen one-plus-five policy".into());}
    let expected=store.revision().map_err(|e|e.to_string())?;
    let authority=store.lww_binding_authority().map_err(|e|e.to_string())?;
    reset_work(asset_hashes);
    crate::portable_backup::source_io::reset_source_io();
    let started=Instant::now();
    let mut activation=None;
    let mut activation_ms=None;
    let mut body=None;
    let mut stage=None;
    let mut errors=vec![];
    let outcome=(||->Result<(),String> {
        let staged=store.snapshot_restore_stage(snapshot_id,request_id).map_err(|e|e.to_string())?;
        stage=Some(staged.staging_id.clone());
        let activated=store.snapshot_restore_activate(&staged.staging_id,expected,authority.clone()).map_err(|e|e.to_string())?;
        activation_ms=Some(started.elapsed().as_secs_f64()*1000.0);
        activation=Some(activated.revision);
        native_activated(activated.revision)?;
        let (mut worker,plan,job)=jobs.prepare_snapshot_bodies(store,&staged.staging_id,activated.revision,&authority.0.to_string())
            .map_err(|e|format!("{}: {}",e.code,e.message))?;
        body=Some(crate::native_file_jobs::snapshot_bodies::run(&mut worker,&plan,&job,scratch).map_err(|e|format!("{}: {}",e.code,e.message))?);
        Ok(())
    })();
    let elapsed_ms=started.elapsed().as_secs_f64()*1000.0;
    let observation=take_work(false);
    let source=crate::portable_backup::source_io::take_source_io();
    if let Err(error)=outcome {errors.push(error);}
    if let Err(error)=observation.validate_costly_native_coverage() {errors.push(error);}
    if !source.complete() {errors.push("snapshot source scope is incomplete".into());}
    if let (Some(stage),Some(revision))=(stage.as_deref(),activation) {
        match store.snapshot_restore_activate(stage,expected,authority) {
            Ok(replayed) if replayed.revision==revision=>(),
            Ok(_)=>errors.push("snapshot exact activation replay returned a different revision".into()),
            Err(error)=>errors.push(error.to_string()),
        }
    }
    Ok(json!({"status":"INVALID","route":"native-periodic-snapshot",
        "scenario":if all_present {"restore-all-present"} else {"restore-missing"},"iteration":iteration,"warmup":iteration==0,
        "stagingId":stage,"activationRevision":activation,"nativeActivationMs":activation_ms,
        "nativeBodyPhaseCompleteMs":body.as_ref().map(|_|elapsed_ms),"bodyResult":body,"nativeObservation":observation,
        "sourceObservation":portable_source_receipt(&source),"rendererAdoption":null,"errors":errors}))
}

/// Run on the actual restore worker. Its owner drives the real selection/finalize/adoption gates.
pub(crate) fn portable_restore_worker_raw(
    source:crate::native_file_jobs::OpenedJobSource,store:PersistentStore,
    owned:&Path,job:&crate::native_file_jobs::JobControl,
    context:&crate::native_file_jobs::portable::NativePortableRestoreContext<'_>,
    asset_hashes:&[String],iteration:u32,all_present:bool,
) ->Result<Value,String> {
    if iteration>=6 {return Err("restore slot is outside the frozen one-plus-five policy".into());}
    if job.status().kind!=crate::native_file_jobs::JobKind::RestorePortableBackup {
        return Err("portable restore requires its actual restore job".into());
    }
    let revision=store.revision().map_err(|e|e.to_string())?;
    reset_work(asset_hashes);
    let started=Instant::now();
    let outcome=crate::native_file_jobs::portable::restore_portable_with_context(
        source,false,revision,owned,store,job,Some((context,None)));
    let mut errors=vec![];
    let summary=match outcome {
        Ok(summary)=>{
            let encoded=serde_json::to_value(&summary).map_err(|e|e.to_string());
            if let Err(error)=job.finish_success(summary) {errors.push(error);}
            match encoded {Ok(value)=>Some(value),Err(error)=>{errors.push(error);None}}
        },
        Err(error)=>{
            if let Err(failed)=job.finish_failure(&error.code,&error.message) {errors.push(failed);}
            errors.push(format!("{}: {}",error.code,error.message));None
        },
    };
    let elapsed_ms=started.elapsed().as_secs_f64()*1000.0;
    let observation=take_work(false);
    if let Err(error)=observation.validate_costly_native_coverage() {errors.push(error);}
    let status=job.status();
    Ok(json!({"status":"INVALID","scenario":if all_present {"restore-all-present"} else {"restore-missing"},
        "iteration":iteration,"warmup":iteration==0,"nativeBodiesCompleteMs":summary.as_ref().map(|_|elapsed_ms),
        "activationRevision":status.activation_revision,"jobStatus":status,"result":summary,
        "nativeObservation":observation,"sourceObservation":null,"nativeActivationMs":null,
        "rendererAdoption":null,"errors":errors}))
}

/// The caller may run a foreground edit when the actual coherent capture callback fires.
pub(crate) fn portable_backup_raw(
    store:PersistentStore,owned:&Path,handoffs:&Path,asset_hashes:&[String],iteration:u32,
    capture_ready:impl Fn(&str,i64)+Send+Sync+'static,
) ->Result<Value,String> {
    use crate::{native_file_jobs::{JobRegistry,JobKind,portable},portable_backup::source_io};
    if iteration>=6 {return Err("backup slot is outside the frozen one-plus-five policy".into());}
    let revision=store.revision().map_err(|e|e.to_string())?;
    reset_work(asset_hashes);
    source_io::reset_source_io();
    source_io::on_capture_ready(capture_ready);
    let registry=JobRegistry::default();
    let job=registry.create_with_context(JobKind::ExportPortableBackup,Some(revision),vec![])?;
    let started=Instant::now();
    let outcome=portable::export_portable(None,revision,owned,handoffs,store,&job,None,"synthetic-lww-harness");
    let mut errors=vec![];
    let summary=match outcome {
        Ok(summary)=>{
            let encoded=serde_json::to_value(&summary).map_err(|e|e.to_string());
            if let Err(error)=job.finish_success(summary) {errors.push(error);}
            match encoded {Ok(value)=>Some(value),Err(error)=>{errors.push(error);None}}
        },
        Err(error)=>{
            if let Err(failed)=job.finish_failure(&error.code,&error.message) {errors.push(failed);}
            errors.push(format!("{}: {}",error.code,error.message));None
        },
    };
    let elapsed_ms=started.elapsed().as_secs_f64()*1000.0;
    let observation=take_work(false);
    let source=source_io::take_source_io();
    if !source.complete() {errors.push("portable backup source scope is incomplete".into());}
    if let Err(error)=observation.validate_costly_native_coverage() {errors.push(error);}
    Ok(json!({"status":"INVALID","scenario":"during-backup","iteration":iteration,"warmup":iteration==0,
        "nativeBackupCompleteMs":summary.as_ref().map(|_|elapsed_ms),"result":summary,"nativeObservation":observation,
        "sourceObservation":portable_source_receipt(&source),
        "foregroundObservation":null,"rendererAdoption":null,"errors":errors}))
}

/// Convert the owner's joined archive/SQL scope, without treating Blob reads as File IO.
pub(crate) fn portable_source_receipt(source:&crate::portable_backup::source_io::SourceIo)->Value {
    use crate::portable_backup::source_io;
    let ranges=|ranges:&[source_io::ReadRange]|ranges.iter().map(|range|json!({"offset":range.offset,"bytes":range.bytes})).collect::<Vec<_>>();
    let objects=source.objects.iter().map(|(hash,work)|(hash.clone(),json!({"opens":work.opens,"reads":work.reads,
        "bytes":work.bytes,"hashedBytes":work.hashed_bytes,"hashChecks":work.hash_checks,"hashFailures":work.hash_failures,
        "incomplete":work.incomplete,"outstanding":work.outstanding,"ranges":ranges(&work.ranges)}))).collect::<std::collections::BTreeMap<_,_>>();
    let declared=source.declared_ranges.iter().map(|(hash,range)|(hash.clone(),json!({"offset":range.offset,"bytes":range.bytes}))).collect::<std::collections::BTreeMap<_,_>>();
    let captures=source.captures.iter().map(|capture|json!({"lease":capture.lease,"revision":capture.revision,
        "pinnedDeviceMetaRevision":capture.pinned_device_meta_revision})).collect::<Vec<_>>();
    let cached=source.cached_sql_reads.iter().map(|(hash,work)|(hash.clone(),json!({"reads":work.reads,"bytes":work.bytes,
        "failures":work.failures,"failedAdoptions":work.failed_adoptions}))).collect::<std::collections::BTreeMap<_,_>>();
    json!({"complete":source.complete(),"metadataRanges":ranges(&source.metadata_ranges),
            "metadataFailures":source.metadata_failures,"catalogHashedBytes":source.catalog_hashed_bytes,
            "catalogHashChecks":source.catalog_hash_checks,"catalogHashFailures":source.catalog_hash_failures,
            "packHashedBytes":source.pack_hashed_bytes,"objects":objects,"declaredRanges":declared,
            "cachedSqlReads":cached,"workers":source.workers,"activeWorkers":source.active_workers,
            "scopeViolations":source.scope_violations,"captures":captures})
}

#[derive(Serialize)]
pub(crate) struct RawCompaction {
    pub status:&'static str,
    pub fixture_for_scenario:measurement::Scenario,
    pub iteration:u32,
    pub warmup:bool,
    pub native_publication_complete_ms:Option<f64>,
    pub snapshot:Option<Value>,
    pub protection_run_completed:bool,
    pub temporary_costs:Option<Value>,
    pub compaction_catalog_merge_visits:Option<u64>,
    pub compaction_capture_rows:Option<u64>,
    pub native_observation:NativeObservation,
    pub source_observation:Option<Value>,
    pub error:Option<String>,
}

/// Snapshot creation is excluded setup for listing/restore cases, and background work for overlap.
pub(crate) async fn compact_native_raw(
    engine:&crate::external_storage::lww_engine::ExternalLwwEngine,
    provider:&Arc<crate::external_storage::fake::FakeProvider>,store:&mut PersistentStore,
    directory:&Path,job_id:&str,writer:&str,
    capabilities:&crate::external_storage::capabilities::Capabilities,
    protection:Option<(&crate::external_storage::leases::LeaseOwner,&crate::external_storage::leases::LeaseContext<'_>)>,
    asset_hashes:&[String],scenario:measurement::Scenario,iteration:u32,
) ->Result<RawCompaction,String> {
    if !matches!(scenario,measurement::Scenario::ConsolidatedSnapshot|measurement::Scenario::IncomparableSnapshots
        |measurement::Scenario::DuringCompaction) || iteration>=6 {
        return Err("compaction slot is outside the frozen one-plus-five policy".into());
    }
    if Arc::as_ptr(&engine.provider) as *const ()!=Arc::as_ptr(provider) as *const () {
        return Err("compaction IO observer belongs to a different provider".into());
    }
    let requests_before=provider.read_count()+provider.upload_count()+provider.listing_count();
    let bytes_before=provider.transferred_body_bytes();
    reset_work(asset_hashes);
    crate::external_storage::worker_observation::begin();
    let started=Instant::now();
    let cancel=Cancellation::default();
    let outcome=if let Some((owner,context))=protection {
        owner.run(context,&cancel,async {
            if let Some(reason)=owner.recheck(context,&cancel).await? {
                return Err(crate::external_storage::leases::yield_error(reason));
            }
            engine.compact_published(store,directory,job_id,writer,capabilities,&cancel,Some((owner,context))).await
        }).await
    } else {
        engine.compact_published(store,directory,job_id,writer,capabilities,&cancel,None).await
    };
    let elapsed_ms=started.elapsed().as_secs_f64()*1000.0;
    let mut observation=take_work(true);
    for worker in crate::external_storage::worker_observation::take() {
        let mut incomplete=worker.hashes.incomplete.into_iter().map(|(domain,count)|format!("{domain}:{count}")).collect::<Vec<_>>();
        if !worker.completed {incomplete.push("actual external worker did not complete".into());}
        let receipt=native_observation::NativeWorkerHashReceipt {thread:worker.thread,
            domains:worker.hashes.domains.into_iter().map(|(name,work)|(name.into(),native_observation::HashDomain {
                calls:work.calls,bytes:work.bytes})).collect(),incomplete};
        if let Err(reason)=observation.attach_worker_hash_receipt(receipt) {observation.incomplete.push(reason);}
    }
    let bytes_after=provider.transferred_body_bytes();
    observation.requests=(provider.read_count()+provider.upload_count()+provider.listing_count()-requests_before) as u64;
    observation.uploaded_bytes=bytes_after.0-bytes_before.0;
    observation.downloaded_bytes=bytes_after.1-bytes_before.1;
    let compaction_catalog_merge_visits=outcome.as_ref().ok().and_then(|completed|completed.compaction_catalog_merge_visits);
    let compaction_capture_rows=outcome.as_ref().ok().and_then(|completed|completed.compaction_capture_rows);
    let (snapshot,mut error)=match outcome {
        Ok(completed)=>(Some(json!({"snapshotId":completed.snapshot_id,"repositoryId":completed.repository_id,
            "stateFingerprint":completed.fingerprint,"libraryFingerprint":completed.library_fingerprint,
            "logicalRevision":completed.logical_revision,"referencedObjectCount":completed.referenced_objects.len(),
            "sectionCount":completed.sections.len()})),None),
        Err(error)=>(None,Some(format!("{error:?}"))),
    };
    // Structural inspection occurs after elapsed timing and every counter take.
    let temporary_costs=match retained_compaction_costs(directory) {
        Ok(value)=>Some(value),Err(reason)=>{error=Some(match error {Some(prior)=>format!("{prior}; {reason}"),None=>reason});None}
    };
    Ok(RawCompaction {status:"INVALID",fixture_for_scenario:scenario,iteration,warmup:iteration==0,
        native_publication_complete_ms:snapshot.as_ref().map(|_|elapsed_ms),
        protection_run_completed:protection.is_some() && snapshot.is_some(),snapshot,temporary_costs,compaction_catalog_merge_visits,compaction_capture_rows,native_observation:observation,
        source_observation:None,error})
}

fn retained_compaction_costs(directory:&Path)->Result<Value,String> {
    let mut pending=vec![directory.to_owned()];let mut files=Vec::new();let mut retained_bytes=0u64;
    while let Some(path)=pending.pop() {
        if !path.exists() {continue;}
        for entry in std::fs::read_dir(&path).map_err(|e|e.to_string())? {
            let path=entry.map_err(|e|e.to_string())?.path();
            let metadata=std::fs::symlink_metadata(&path).map_err(|e|e.to_string())?;
            if metadata.file_type().is_symlink() {return Err("compaction cost inspection encountered a symbolic link".into());}
            if metadata.is_dir() {pending.push(path);continue;}
            retained_bytes=retained_bytes.checked_add(metadata.len()).ok_or("retained compaction file byte overflow")?;
            let mut row=json!({"relativePath":path.strip_prefix(directory).map_err(|e|e.to_string())?,"fileLengthBytes":metadata.len()});
            if path.extension().is_some_and(|extension|extension=="sqlite") {
                let db=rusqlite::Connection::open_with_flags(&path,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e|e.to_string())?;
                let pages:u64=db.query_row("PRAGMA page_count",[],|row|sql_u64(row,0)).map_err(|e|e.to_string())?;
                let page_size:u64=db.query_row("PRAGMA page_size",[],|row|sql_u64(row,0)).map_err(|e|e.to_string())?;
                row["sqliteAllocatedBytes"]=json!(pages.checked_mul(page_size).ok_or("SQLite allocation overflow")?);
                let mut statement=db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").map_err(|e|e.to_string())?;
                let names=statement.query_map([],|row|row.get::<_,String>(0)).map_err(|e|e.to_string())?.collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
                let mut counts=std::collections::BTreeMap::new();
                for name in names {
                    let query=format!("SELECT count(*) FROM \"{}\"",name.replace('"',"\"\""));
                    let count=db.query_row(&query,[],|row|sql_u64(row,0)).map_err(|e|e.to_string())?;counts.insert(name,count);
                }
                row["tableRows"]=json!(counts);
            }
            files.push(row);
        }
    }
    files.sort_by_key(|row|row["relativePath"].as_str().unwrap_or("").to_owned());
    Ok(json!({"inspectionPhase":"post-completion excluded structural inspection","survivingFileLengthBytes":retained_bytes,
        "files":files,"peakDiskBytes":null,"removedCatalogRows":null,
        "scope":"actual surviving file lengths and SQLite page allocation/row counts; removed files and peak allocation are unobserved"}))
}

/// Both stale views contain actual published receipts, and freeze before either checkpoint exists.
pub(crate) async fn produce_incomparable_native(
    engine:&crate::external_storage::lww_engine::ExternalLwwEngine,
    provider:&Arc<crate::external_storage::fake::FakeProvider>,stores:[&mut PersistentStore;2],directories:[&Path;2],jobs:[&str;2],writers:[&str;2],
    capabilities:&crate::external_storage::capabilities::Capabilities)->Result<Value,String> {
    use crate::external_storage::contract::{Provider,Collection,ObjectPage,parse_segment_object_id};
    use crate::external_storage::lww_compaction::{CompactionBarrier,install_compaction_barrier};
    if writers[0]==writers[1] || jobs[0]==jobs[1] {return Err("incomparable producer identities must be independent".into());}
    let cancel=Cancellation::default();
    let mut snapshots=ObjectPage {objects:Vec::new(),next_cursor:None};
    let mut cursor=None;let mut seen=std::collections::BTreeSet::new();
    loop {
        let page=provider.list_objects(&engine.repository,Collection::Snapshots,cursor.as_deref(),1000,&cancel).await
            .map_err(|e|format!("{e:?}"))?;
        snapshots.objects.extend(page.objects);
        match page.next_cursor {Some(next) if seen.insert(next.clone())=>cursor=Some(next),
            Some(_)=>return Err("snapshot listing did not advance".into()),None=>break}
    }
    if !snapshots.objects.is_empty() {
        return Err("incomparable producer requires a fresh actual snapshot listing".into());
    }
    let mut sequences=[std::collections::BTreeMap::new(),std::collections::BTreeMap::new()];
    let mut cursor=None;let mut seen=std::collections::BTreeSet::new();
    loop {
    let segments=provider.list_objects(&engine.repository,Collection::Segments,cursor.as_deref(),1000,&cancel).await
        .map_err(|e|format!("{e:?}"))?;
    for receipt in segments.objects {
        let name=receipt.locator.object.rsplit('/').next().ok_or("actual segment name absent")?;
        let (writer,sequence,_)=parse_segment_object_id(name).map_err(|e|format!("{e:?}"))?;
        let index=writers.iter().position(|expected|*expected==writer).ok_or("unexpected writer in incomparable producer")?;
        if sequences[index].insert(sequence,receipt).is_some() {return Err("actual writer sequence is duplicated".into());}
    }
    match segments.next_cursor {Some(next) if seen.insert(next.clone())=>cursor=Some(next),
        Some(_)=>return Err("segment listing did not advance".into()),None=>break}
    }
    let mut prefixes=[0u64;2];
    let mut views=[Vec::new(),Vec::new()];
    for index in 0..2 {
        for (sequence,receipt) in std::mem::take(&mut sequences[index]) {
            if sequence!=prefixes[index]+1 {return Err("actual writer prefix is not contiguous from sequence1".into());}
            prefixes[index]=sequence;views[index].push(receipt);
        }
        if prefixes[index]==0 {return Err("both actual writer prefixes must be published before checkpoint construction".into());}
    }
    let barriers=[CompactionBarrier::new(),CompactionBarrier::new()];
    for index in 0..2 {install_compaction_barrier(jobs[index],barriers[index].clone());}
    provider.script_page(Collection::Snapshots,Ok(snapshots.clone()));
    for (page,objects) in views[0].chunks(100).enumerate() {
        let more=(page+1)*100<views[0].len();
        provider.script_page(Collection::Segments,Ok(ObjectPage {objects:objects.to_vec(),next_cursor:more.then(||format!("frozen-a-{}",page+1))}));
    }
    let [first_store,second_store]=stores;
    let first=engine.compact_published(first_store,directories[0],jobs[0],writers[0],capabilities,&cancel,None);
    let second=async {
        barriers[0].reached.notified().await;
        provider.script_page(Collection::Snapshots,Ok(snapshots));
        for (page,objects) in views[1].chunks(100).enumerate() {
            let more=(page+1)*100<views[1].len();
            provider.script_page(Collection::Segments,Ok(ObjectPage {objects:objects.to_vec(),next_cursor:more.then(||format!("frozen-b-{}",page+1))}));
        }
        let capture=engine.compact_published(second_store,directories[1],jobs[1],writers[1],capabilities,&cancel,None);
        let release=async {
            barriers[1].reached.notified().await;
            barriers[0].resume.notify_one();barriers[1].resume.notify_one();
            Ok::<(),crate::external_storage::contract::ProviderError>(())
        };
        tokio::try_join!(capture,release).map(|(completed,())|completed)
    };
    let (first,second)=tokio::try_join!(first,second).map_err(|e|format!("{e:?}"))?;
    let completed=[first,second];
    let mut coverages=vec![];
    for index in 0..2 {
        let (_,checkpoint)=engine.checkpoint(&completed[index].reference.receipt,&cancel).await.map_err(|e|format!("{e:?}"))?;
        if checkpoint.covered_prefixes.len()!=1 || checkpoint.covered_prefixes.get(writers[index]).map(|seq|seq.0)!=Some(prefixes[index]) {
            return Err("actual authenticated checkpoint did not retain its frozen exclusive writer prefix".into());
        }
        coverages.push(checkpoint.covered_prefixes);
    }
    Ok(json!({"status":"INVALID","phase":"excluded-authenticated-incomparable-producer-setup",
        "snapshotIds":jobs,"authenticatedCoveredPrefixes":coverages,"bothInputsFrozenBeforePublication":true,
        "restoreActivation":null,"allBodiesSettled":null,"sourceObservation":null}))
}
