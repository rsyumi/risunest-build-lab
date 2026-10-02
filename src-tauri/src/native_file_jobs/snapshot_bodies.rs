use super::{JobControl, JobPhase, JobState, JobProgress, NativeJobError};
use crate::{asset_repository::{PayloadCas, job_pins::{CasJobKind,CasObjectRole,CasReleaseOutcome,DurableCasJob}}, persistent_store::PersistentStore, server_sync::residency::AssetPolicy};
use rusqlite::{Connection,OpenFlags};
use serde::Serialize;
use std::{fs::File,io::{self,Read},path::{Path,PathBuf}};

pub(crate) struct BodyObject {pub hash:String,pub size:u64,pub owner:bool,pub cached:bool}
pub(crate) struct BodyPlan {
    pub stage_id:String,pub revision:i64,pub authority:String,pub protection_id:String,
    pub objects:Vec<BodyObject>,pub source:PathBuf,pub policy:AssetPolicy,
}

#[derive(Clone,Debug,Serialize,PartialEq,Eq)]
#[serde(rename_all="camelCase")]
pub(crate) struct BodyResult {
    pub stage_id:String,pub activated_revision:i64,pub binding_authority:String,
    pub policy:AssetPolicy,pub total:u64,pub locally_present:u64,pub remote_held:u64,pub unavailable:u64,
    pub all_bodies_local:bool,pub settled:bool,
}

fn error(value:impl std::fmt::Display)->NativeJobError {NativeJobError::new("snapshot-bodies-failed",value.to_string())}
fn sync_error(value:crate::server_sync::SyncError)->NativeJobError {NativeJobError::new(&value.code,value.cause.unwrap_or_else(||"Snapshot body source is unavailable".to_owned()))}

pub(super) fn prepare_directory(root:&Path,job:&JobControl)->Result<PathBuf,NativeJobError> {
    match super::create_owned_directory(root,&job.id()) {
        Ok(directory)=>Ok(directory),
        Err(failure)=>{
            let error=NativeJobError::new("store-error",failure);
            job.finish_failure(&error.code,&error.message).map_err(|failure|NativeJobError::new("store-error",failure))?;
            Err(error)
        }
    }
}

fn check(store:&PersistentStore,plan:&BodyPlan,job:&JobControl)->Result<(),NativeJobError> {
    if job.is_cancel_requested() {return Err(NativeJobError::new("cancelled","Snapshot body transfer was cancelled"));}
    if store.lww_binding_authority().map_err(error)?.0.to_string()!=plan.authority
        || store.server_asset_policy().map_err(sync_error)?!=plan.policy {
        return Err(NativeJobError::new("snapshot-body-authority-changed","Snapshot body transfer authority or policy changed"));
    }
    Ok(())
}

fn observe(store:&PersistentStore,plan:&BodyPlan)->Result<BodyResult,NativeJobError> {
    let cas=PayloadCas::new(store.repository_root()).map_err(error)?;
    let residency=crate::server_sync::residency::Residency::open(store.repository_root()).map_err(sync_error)?;
    let mut result=BodyResult {stage_id:plan.stage_id.clone(),activated_revision:plan.revision,binding_authority:plan.authority.clone(),policy:plan.policy,total:plan.objects.len() as u64,locally_present:0,remote_held:0,unavailable:0,all_bodies_local:false,settled:false};
    for object in &plan.objects {
        match cas.stat_object(&object.hash).map_err(error)? {
            Some(size) if size==object.size=>result.locally_present+=1,
            Some(_)=>return Err(NativeJobError::new("snapshot-body-size-differs","Snapshot body size differs from the frozen inventory")),
            None=>{
                let held=residency.object(&object.hash,None).map_err(sync_error)?.is_some_and(|proof|proof.size==object.size)
                    || crate::external_storage::lww_residency::stat(store.repository_root(),&object.hash).map_err(error)?==Some(object.size);
                if held {result.remote_held+=1;} else {result.unavailable+=1;}
            }
        }
    }
    result.all_bodies_local=result.locally_present==result.total;
    result.settled=result.all_bodies_local || (plan.policy==AssetPolicy::Remote && result.unavailable==0);
    Ok(result)
}

struct CheckedRead<'a,R:Read> {inner:R,store:&'a PersistentStore,plan:&'a BodyPlan,job:&'a JobControl,#[cfg(test)] hash:&'a str,cached_identity:Option<(&'a File,&'a crate::asset_repository::ExactFileIdentity)>}
impl<R:Read> Read for CheckedRead<'_,R> {
    fn read(&mut self,output:&mut[u8])->io::Result<usize> {
        check(self.store,self.plan,self.job).map_err(|error|io::Error::other(error.message))?;
        #[cfg(test)] self.job.source_io_scope.before_object_read(self.hash);
        if let Some((file,identity))=self.cached_identity {
            if crate::asset_repository::exact_file_identity(file)?!=*identity {return Err(io::Error::other("Snapshot cached source identity changed"));}
        }
        let result=self.inner.read(output);
        #[cfg(test)] if self.cached_identity.is_some() {self.job.source_io_scope.cached_sql_read(self.hash,&result);}
        if let Some((file,identity))=self.cached_identity {
            if crate::asset_repository::exact_file_identity(file)?!=*identity {return Err(io::Error::other("Snapshot cached source identity changed"));}
        }
        let read=result?;
        if read>0 {
            let mut progress=self.job.status.lock().map_err(|_|io::Error::other("Snapshot body progress mutex is poisoned"))?.progress;
            progress.completed_bytes=progress.completed_bytes.checked_add(read as u64).ok_or_else(||io::Error::other("Snapshot body read count overflow"))?;
            self.job.set_progress(progress).map_err(io::Error::other)?;
        }
        Ok(read)
    }
}

pub(crate) fn run(store:&mut PersistentStore,plan:&BodyPlan,job:&JobControl,scratch:&Path)->Result<BodyResult,NativeJobError> {
    job.start(JobPhase::CopyingMissingBodies).map_err(error)?;
    let initial=observe(store,plan)?;
    job.set_snapshot_bodies(initial.clone()).map_err(error)?;
    check(store,plan,job)?;
    let cas=PayloadCas::new(store.repository_root()).map_err(error)?;
    let mut missing=Vec::new();
    for object in &plan.objects {
        if !store.portable_object_present(&object.hash,object.size).map_err(error)? {missing.push(object);}
    }
    let missing_bytes=missing.iter().try_fold(0u64,|total,object|total.checked_add(object.size)).ok_or_else(||error("Snapshot missing body size overflow"))?;
    job.set_progress(JobProgress {total_bytes:Some(missing_bytes),total_items:Some(missing.len() as u64),..Default::default()}).map_err(error)?;
    let mut pins=match DurableCasJob::open(store.repository_root(),&plan.protection_id) {
        Ok(pins)=>pins,
        Err(failure) if failure.kind()==io::ErrorKind::NotFound=>DurableCasJob::begin(store.repository_root(),&plan.protection_id,CasJobKind::LocalBackupRestore,super::portable::now()).map_err(error)?,
        Err(failure)=>return Err(error(failure)),
    };
    let outcome=(|| {
        if initial.settled {
            pins.seal(store,super::portable::now()).map_err(error)?;
            pins.release(CasReleaseOutcome::Committed).map_err(error)?;
            store.snapshot_restore_bodies_completed(&plan.stage_id).map_err(error)?;
            return Ok(initial);
        }
        for object in &missing {
            check(store,plan,job)?;
            let role=if object.owner {CasObjectRole::OwnerManifest} else {CasObjectRole::DirectObject};
            if store.portable_object_present(&object.hash,object.size).map_err(error)? {
                pins.pin_existing(&cas,&object.hash,object.size,role).map_err(error)?;
                continue;
            }
            if plan.policy==AssetPolicy::Remote && !object.cached {continue;}
            if object.cached {
                let file=File::open(&plan.source).map_err(error)?;
                let identity=crate::asset_repository::exact_file_identity(&file).map_err(error)?;
                let source=Connection::open_with_flags(&plan.source,OpenFlags::SQLITE_OPEN_READ_ONLY|OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(error)?;
                source.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF;").map_err(error)?;
                let (row,size):(i64,i64)=source.query_row("SELECT rowid,length(body) FROM message_page_objects WHERE hash=?1",[&object.hash],|row|Ok((row.get(0)?,row.get(1)?))).map_err(error)?;
                if u64::try_from(size).ok()!=Some(object.size) {return Err(error("Snapshot cached payload size differs"));}
                let blob=source.blob_open("main","message_page_objects","body",row,true).map_err(error)?;
                let mut input=CheckedRead {inner:blob,store:&*store,plan,job,#[cfg(test)] hash:&object.hash,cached_identity:Some((&file,&identity))};
                let installed=pins.prepare_reader_expected(&cas,&mut input,&object.hash,object.size,role);
                #[cfg(test)] job.source_io_scope.cached_sql_adoption(&object.hash,installed.is_ok());
                installed.map_err(error)?;
                if crate::asset_repository::exact_file_identity(&file).map_err(error)?!=identity {return Err(error("Snapshot cached source identity changed"));}
            } else {
                let check_source=||check(store,plan,job).map_err(|error|crate::server_sync::SyncError::new(error.code,409));
                if let Some(body)=crate::server_sync::residency::open_transient_server_with_check(store.repository_root(),scratch,&object.hash,&check_source).map_err(sync_error)? {
                    let mut input=CheckedRead {inner:body,store:&*store,plan,job,#[cfg(test)] hash:&object.hash,cached_identity:None};
                    pins.prepare_reader_expected(&cas,&mut input,&object.hash,object.size,role).map_err(error)?;
                } else {
                    let cancellation=crate::external_storage::contract::Cancellation::with_external_flag(job.cancellation_flag());
                    let body=tauri::async_runtime::block_on(crate::external_storage::lww_residency::spool_verified_remote_body(store.repository_root(),&object.hash,scratch,&cancellation)).map_err(error)?;
                    if let Some(mut body)=body {
                        let mut input=CheckedRead {inner:body.as_file_mut(),store:&*store,plan,job,#[cfg(test)] hash:&object.hash,cached_identity:None};
                        pins.prepare_reader_expected(&cas,&mut input,&object.hash,object.size,role).map_err(error)?;
                    }
                }
            }
            let mut progress=job.status.lock().map_err(|_|error("Snapshot body progress mutex is poisoned"))?.progress;
            progress.completed_items+=1;
            job.set_progress(progress).map_err(error)?;
        }
        check(store,plan,job)?;
        let result=observe(store,plan)?;
        job.set_snapshot_bodies(result.clone()).map_err(error)?;
        if !result.settled {return Err(NativeJobError::new("snapshot-bodies-incomplete","Snapshot library is restored, but some bodies remain unavailable"));}
        pins.seal(store,super::portable::now()).map_err(error)?;
        pins.release(CasReleaseOutcome::Committed).map_err(error)?;
        store.snapshot_restore_bodies_completed(&plan.stage_id).map_err(error)?;
        Ok(result)
    })();
    if outcome.is_err() {if let Ok(result)=observe(store,plan) {let _=job.set_snapshot_bodies(result);}}
    outcome
}

pub(crate) fn finish(job:&JobControl,outcome:Result<BodyResult,NativeJobError>)->Result<(),String> {
    match outcome {
        Ok(result)=>{
            job.set_snapshot_bodies(result)?;
            let mut status=job.status.lock().map_err(|_|"native job mutex poisoned".to_owned())?;
            status.state=JobState::Succeeded;
            status.phase=JobPhase::Complete;
            drop(status);
            job.mark_terminal()
        }
        Err(_) if job.is_cancel_requested()=>job.finish_cancelled(),
        Err(failure)=>job.finish_failure(&failure.code,&failure.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server_sync::lww_tests::{local,put_asset};

    fn activated(store:&mut PersistentStore)->BodyPlan {
        let snapshot=store.snapshot_create("synthetic-body-phase").unwrap();
        let stage=store.snapshot_restore_stage(&snapshot.id,&uuid::Uuid::new_v4().to_string()).unwrap();
        let authority=store.lww_binding_authority().unwrap();
        let receipt=store.snapshot_restore_activate(&stage.staging_id,store.revision().unwrap(),authority.clone()).unwrap();
        store.snapshot_restore_body_plan(&stage.staging_id,receipt.revision,&authority.0.to_string()).unwrap()
    }

    fn job(plan:&BodyPlan)->std::sync::Arc<JobControl> {
        let job=super::super::JobRegistry::default().create_internal(super::super::JobKind::SnapshotBodies,Some(plan.revision),vec![],false).unwrap();
        {
            let mut status=job.status.lock().unwrap();
            status.snapshot_staging_id=Some(plan.stage_id.clone());
            status.activation_revision=Some(plan.revision);
            status.activation_authority=Some(plan.authority.clone());
        }
        job
    }

    fn cached_plan(store:&mut PersistentStore)->(BodyPlan,String,Vec<u8>) {
        use crate::persistent_store::lww::{Header,Change,StageReceive,ApplyReceive,Progress};
        use risunest_sync_wire::{stamp::Stamp,unit::{UnitKey,UnitValue},descriptor::RecordDescriptor};
        let bytes=vec![93;128*1024+7];
        let hash=risunest_sync_wire::hash(&bytes);
        store.lww_put_object(&hash,&bytes).unwrap();
        let header=Header {binding_authority:store.lww_binding_authority().unwrap(),request_id:"synthetic-cached-source".into()};
        store.lww_stage_receive(&StageReceive {header:header.clone(),changes:vec![Change {key:UnitKey::new(&["future-snapshot","cached"]).unwrap(),value:UnitValue::object(RecordDescriptor::content(hash.clone())).unwrap(),stamp:Stamp {physical_ms:1.into(),logical:0,writer_id:"00000000-0000-4000-8000-000000000001".into()}}],progress:Progress {kind:"server".into(),cursor:1.into(),writer_id:None},admitted_time_upper_ms:u64::MAX.into()}).unwrap();
        store.lww_apply_receive(&ApplyReceive {header:header.clone(),generating:vec![]}).unwrap();
        store.lww_finish_receive(&header).unwrap();
        (activated(store),hash,bytes)
    }

    #[test]
    fn snapshot_bodies_cached_sql_is_read_only_when_missing_under_both_policies() {
        for policy in [AssetPolicy::Full,AssetPolicy::Remote] {
            let server=crate::server_sync::lww_tests::LocalServerFixture::new();
            let (root,mut store)=local();
            let _client=server.client(&store);
            store.asset_residency_set_policy(policy,||Ok(())).unwrap();
            let (plan,hash,bytes)=cached_plan(&mut store);
            assert!(plan.objects.iter().any(|object|object.hash==hash && object.cached));
            crate::portable_backup::source_io::reset_source_io();
            crate::asset_repository::body_io::reset_body_io();
            let result=run(&mut store,&plan,&job(&plan),root.path()).unwrap();
            let source=crate::portable_backup::source_io::take_source_io();
            let body=crate::asset_repository::body_io::take_body_io();
            assert!(result.all_bodies_local);
            assert!(source.complete());
            assert_eq!(source.cached_sql_reads[&hash].bytes,bytes.len() as u64);
            assert!(source.cached_sql_reads[&hash].reads>0);
            assert!(body.objects[&hash].owned_work.body_sha.values().map(|work|work.bytes).sum::<u64>()>=bytes.len() as u64);
            let revision=store.revision().unwrap();
            let retry=activated(&mut store);
            crate::portable_backup::source_io::reset_source_io();
            assert!(run(&mut store,&retry,&job(&retry),root.path()).unwrap().all_bodies_local);
            assert!(crate::portable_backup::source_io::take_source_io().cached_sql_reads.is_empty());
            assert_eq!(store.revision().unwrap(),revision+1);
        }
    }

    #[test]
    fn snapshot_bodies_cached_sql_tamper_fails_after_commit_without_source_identity_inference() {
        let (root,mut store)=local();
        let (plan,hash,bytes)=cached_plan(&mut store);
        let source=Connection::open(&plan.source).unwrap();
        source.execute_batch("DROP TRIGGER message_page_objects_immutable_update;").unwrap();
        source.execute("UPDATE message_page_objects SET body=?1 WHERE hash=?2",rusqlite::params![vec![94u8;bytes.len()],hash]).unwrap();
        drop(source);
        crate::portable_backup::source_io::reset_source_io();
        assert!(run(&mut store,&plan,&job(&plan),root.path()).is_err());
        let source=crate::portable_backup::source_io::take_source_io();
        assert_eq!(source.cached_sql_reads[&hash].bytes,bytes.len() as u64);
        assert!(!source.complete());
        assert_eq!(store.revision().unwrap(),plan.revision);
        assert!(PayloadCas::new(store.repository_root()).unwrap().stat_object(&hash).unwrap().is_none());
        assert!(plan.source.exists());
    }

    #[test]
    fn snapshot_bodies_setup_refusal_marks_owner_terminal() {
        let (root,mut store)=local();
        let plan=activated(&mut store);
        let job=job(&plan);
        let refused=root.path().join("not-a-directory");
        std::fs::write(&refused,b"synthetic refusal").unwrap();
        assert!(prepare_directory(&refused,&job).is_err());
        assert!(job.status().state.is_terminal());
        assert_eq!(job.status().state,JobState::Failed);
    }

    #[test]
    fn snapshot_bodies_authenticated_full_transfer_allows_foreground_edit_and_server_admission() {
        use crate::server_sync::lww_tests::{LocalServerFixture,header,save};
        let server=LocalServerFixture::new();
        let (root,mut store)=local();
        let client=server.client(&store);
        let bytes=vec![95;128*1024+3];
        let hash=put_asset(&mut store,"assets/held-snapshot.png",&bytes).object_hash.unwrap();
        let request=header(&store);
        client.push(&mut store,&request,&[]).unwrap();
        store.asset_residency_set_policy(AssetPolicy::Remote,||Ok(())).unwrap();
        store.asset_residency_evict(||Ok(())).unwrap();
        let policy_checks=std::sync::atomic::AtomicUsize::new(0);
        assert!(store.asset_residency_set_policy(AssetPolicy::Full,|| {
            if policy_checks.fetch_add(1,std::sync::atomic::Ordering::AcqRel)==0 {Ok(())} else {Err(crate::server_sync::SyncError::new("cancelled",499))}
        }).is_err());
        assert!(PayloadCas::new(store.repository_root()).unwrap().stat_object(&hash).unwrap().is_none());
        let original=activated(&mut store);
        assert_eq!(original.policy,AssetPolicy::Full);
        let state=super::super::NativeFileJobState::initialize(root.path().join("native-file-jobs"));
        crate::portable_backup::source_io::reset_source_io();
        crate::asset_repository::body_io::reset_body_io();
        let (mut worker_store,plan,job)=state.prepare_snapshot_bodies(&mut store,&original.stage_id,original.revision,&original.authority).unwrap();
        let (entered,waiting)=std::sync::mpsc::channel();
        let (release,resume)=std::sync::mpsc::channel();
        let resume=std::sync::Mutex::new(resume);
        let once=std::sync::atomic::AtomicBool::new(false);
        let held_hash=hash.clone();
        crate::portable_backup::source_io::on_object_read(move |hash| {
            if hash==held_hash && !once.swap(true,std::sync::atomic::Ordering::AcqRel) {
                entered.send(()).unwrap();
                resume.lock().unwrap().recv().unwrap();
            }
        });
        let source_scope=job.source_io_scope.clone();
        let body_scope=crate::asset_repository::body_io::capture_body_io_scope();
        let scratch=tempfile::tempdir().unwrap();
        let worker=std::thread::spawn(move||crate::asset_repository::body_io::with_body_io_scope(body_scope,|| {
            let _scope=crate::portable_backup::source_io::attach(&source_scope);
            run(&mut worker_store,&plan,&job,scratch.path())
        }));
        waiting.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
        let mut foreground=PersistentStore::open(root.path()).unwrap();
        save(&mut foreground,&["root","username"],serde_json::json!("synthetic foreground during body transfer"));
        let foreground_revision=foreground.revision().unwrap();
        {
            let _server_admission=state.admission.server().unwrap();
            let request=header(&foreground);
            client.push(&mut foreground,&request,&[]).unwrap();
        }
        assert!(PayloadCas::new(foreground.repository_root()).unwrap().stat_object(&hash).unwrap().is_none());
        assert!(crate::server_sync::residency::Residency::open(foreground.repository_root()).unwrap().object(&hash,None).unwrap().is_some());
        release.send(()).unwrap();
        let result=worker.join().unwrap().unwrap();
        assert!(result.all_bodies_local && result.settled);
        assert_eq!(result.locally_present,1);
        assert_eq!(foreground.revision().unwrap(),foreground_revision);
        assert_eq!(foreground.read_root(None).unwrap().value["username"],"synthetic foreground during body transfer");
        assert_eq!(PayloadCas::new(foreground.repository_root()).unwrap().stat_object(&hash).unwrap(),Some(bytes.len() as u64));
        assert!(crate::portable_backup::source_io::take_source_io().complete());
        let work=crate::asset_repository::body_io::take_body_io();
        assert_eq!(work.pending_worker_scopes,0);
        assert_eq!(work.worker_scopes_started,work.worker_scopes_settled);
    }

    #[test]
    fn snapshot_bodies_all_present_has_zero_asset_work_and_keeps_later_edit_receipt() {
        let (root,mut store)=local();
        put_asset(&mut store,"assets/synthetic.png",b"synthetic snapshot body");
        let plan=activated(&mut store);
        put_asset(&mut store,"assets/later.png",b"later foreground body");
        let foreground_revision=store.revision().unwrap();
        let retried=store.snapshot_restore_body_plan(&plan.stage_id,plan.revision,&plan.authority).unwrap();
        assert_eq!(retried.protection_id,plan.protection_id);
        crate::asset_repository::body_io::reset_body_io();
        let result=run(&mut store,&plan,&job(&plan),root.path()).unwrap();
        let io=crate::asset_repository::body_io::take_body_io();
        let work=io.asset_work();
        assert_eq!(work.opens,0);
        assert_eq!(work.read_bytes,0);
        assert_eq!(work.staging_written_bytes,0);
        assert_eq!(work.publication_attempts,0);
        assert!(result.all_bodies_local && result.settled);
        assert_eq!(result.locally_present,1);
        assert_eq!(store.revision().unwrap(),foreground_revision);
        assert!(!plan.source.exists());
    }

    #[test]
    fn snapshot_bodies_unavailable_and_cancel_keep_committed_revision_and_body_only_retry() {
        let (root,mut store)=local();
        let alias=put_asset(&mut store,"assets/synthetic.png",b"synthetic snapshot body");
        let plan=activated(&mut store);
        let cas=PayloadCas::new(store.repository_root()).unwrap();
        std::fs::remove_file(cas.object_path(alias.object_hash.as_ref().unwrap()).unwrap().unwrap()).unwrap();
        let failed=job(&plan);
        assert!(run(&mut store,&plan,&failed,root.path()).is_err());
        let status=failed.status.lock().unwrap();
        assert_eq!(status.snapshot_bodies.as_ref().unwrap().unavailable,1);
        drop(status);
        assert!(plan.source.exists());
        assert_eq!(store.revision().unwrap(),plan.revision);
        let retry=store.snapshot_restore_body_plan(&plan.stage_id,plan.revision,&plan.authority).unwrap();
        assert_eq!(retry.protection_id,plan.protection_id);
        let cancelled=job(&retry);
        cancelled.cancel_requested.store(true,std::sync::atomic::Ordering::Release);
        assert_eq!(run(&mut store,&retry,&cancelled,root.path()).unwrap_err().code,"cancelled");
        assert_eq!(store.revision().unwrap(),plan.revision);
        assert!(plan.source.exists());
        cas.prepare_bytes(b"synthetic snapshot body").unwrap();
        assert!(run(&mut store,&retry,&job(&retry),root.path()).unwrap().all_bodies_local);
        assert_eq!(store.revision().unwrap(),plan.revision);
    }
}
