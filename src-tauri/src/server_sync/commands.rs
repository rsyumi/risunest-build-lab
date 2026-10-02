use super::{Result, SyncError};
use crate::persistent_store::{commands::with_store_mut, PersistentStore};
use std::collections::BTreeSet;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};
use tauri::{AppHandle, Manager};
#[derive(Default)]
pub(crate) struct ServerSyncCommandState {
    cleanup_closed: AtomicBool,
    running: AtomicBool,
    cancelled: Mutex<Arc<AtomicBool>>,
    notification: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    notification_active: Arc<AtomicBool>,
    transports: Mutex<TransportJobs>,
}
#[derive(Default)]
struct TransportJobs {
    permit: Option<crate::native_file_jobs::admission::Permit>,
    send: Option<Arc<AtomicBool>>,
    receive: Option<Arc<AtomicBool>>,
    hydrate: Option<Arc<AtomicBool>>,
}
struct TransportJob<'a> { state: &'a ServerSyncCommandState, lane: &'static str }
impl Drop for TransportJob<'_> {
    fn drop(&mut self) {if let Ok(mut jobs)=self.state.transports.lock(){match self.lane {"send"=>jobs.send=None,"receive"=>jobs.receive=None,_=>jobs.hydrate=None}if jobs.send.is_none()&&jobs.receive.is_none()&&jobs.hydrate.is_none(){jobs.permit=None;}}}
}
struct NotificationJob(Arc<AtomicBool>);
impl Drop for NotificationJob {fn drop(&mut self){self.0.store(false,Ordering::Release);}}
struct Running<'a>(&'a ServerSyncCommandState);
impl Drop for Running<'_> { fn drop(&mut self) { self.0.running.store(false, Ordering::Release); } }
impl ServerSyncCommandState {
    pub(crate) fn begin_cleanup(&self) -> Result<()> { self.cleanup_closed.store(true, Ordering::Release);if let Some(job)=self.notification.lock().map_err(|_|SyncError::new("server-sync-state-unavailable",503))?.as_ref(){job.abort();}self.cancel() }
    pub(crate) fn cleanup_drained(&self) -> Result<bool> { let jobs=self.transports.lock().map_err(|_|SyncError::new("server-sync-state-unavailable",503))?;Ok(!self.running.load(Ordering::Acquire)&&!self.notification_active.load(Ordering::Acquire)&&jobs.send.is_none()&&jobs.receive.is_none()&&jobs.hydrate.is_none()) }
    pub(crate) fn finish_cleanup(&self) -> Result<()> { if !self.cleanup_drained()? { return Err(SyncError::new("server-sync-busy",409)); } self.cleanup_closed.store(false,Ordering::Release); Ok(()) }
    pub(crate) fn cancel(&self) -> Result<()> { self.cancelled.lock().map_err(|_| SyncError::new("server-sync-state-unavailable",503))?.store(true,Ordering::Release);let jobs=self.transports.lock().map_err(|_|SyncError::new("server-sync-state-unavailable",503))?;for flag in [&jobs.send,&jobs.receive,&jobs.hydrate].into_iter().flatten(){flag.store(true,Ordering::Release);}Ok(()) }
    fn claim_transport(&self, admission: &Arc<crate::native_file_jobs::admission::Admission>, lane: &'static str)->Result<(TransportJob<'_>,Arc<AtomicBool>)> {
        let mut jobs=self.transports.lock().map_err(|_|SyncError::new("server-sync-state-unavailable",503))?;
        if self.cleanup_closed.load(Ordering::Acquire)||self.running.load(Ordering::Acquire){return Err(SyncError::new("server-sync-busy",409));}
        if (match lane{"send"=>&jobs.send,"receive"=>&jobs.receive,_=>&jobs.hydrate}).is_some(){return Err(SyncError::new("server-sync-busy",409));}
        if jobs.permit.is_none(){jobs.permit=Some(admission.server().map_err(|code|SyncError::new(code,409))?);}
        let flag=Arc::new(AtomicBool::new(false));match lane{"send"=>jobs.send=Some(flag.clone()),"receive"=>jobs.receive=Some(flag.clone()),_=>jobs.hydrate=Some(flag.clone())}
        Ok((TransportJob{state:self,lane},flag))
    }
    fn claim(&self) -> Result<Running<'_>> {
        let jobs=self.transports.lock().map_err(|_|SyncError::new("server-sync-state-unavailable",503))?;
        if jobs.permit.is_some(){return Err(SyncError::new("server-sync-busy",409));}
        self.running.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire).map_err(|_| SyncError::new("server-sync-busy",409))?;
        if self.cleanup_closed.load(Ordering::Acquire) { self.running.store(false,Ordering::Release); return Err(SyncError::new("cleanup-pending",409)); }
        Ok(Running(self))
    }
    fn claim_preparation(&self) -> Result<(Running<'_>, Arc<AtomicBool>)> {
        let running=self.claim()?; let flag=Arc::new(AtomicBool::new(false));
        *self.cancelled.lock().map_err(|_| SyncError::new("server-sync-state-unavailable",503))?=flag.clone(); Ok((running,flag))
    }
}
fn claim_library<R: tauri::Runtime>(app: &AppHandle<R>) -> Result<crate::native_file_jobs::admission::Permit> {
    app.state::<crate::native_file_jobs::NativeFileJobState>()
        .admission
        .server()
        .map_err(|code| SyncError::new(code, 409))
}
fn job_store<R: tauri::Runtime>(app: &AppHandle<R>) -> Result<PersistentStore> {
    with_store_mut(app.state(), |store| store.open_native_job_store())
        .map_err(|error| SyncError::caused("local-store-unavailable", 503, error.to_string()))
}
async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|_| SyncError::new("server-sync-worker-unavailable", 503))?
}
async fn logged_blocking<T: Send + 'static>(
    stage: &str,
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    blocking(operation).await.map_err(|error| { crate::nlog!("warn", "Server {stage} operation failed: {}", error.code); error })
}
#[tauri::command]
pub(crate) async fn server_sync_asset_status(
    app: AppHandle,
) -> Result<crate::persistent_store::asset_residency::ResidencyStatus> {
    logged_blocking("asset-status", move || job_store(&app)?.asset_residency_status()).await
}
#[tauri::command]
pub(crate) async fn server_sync_asset_policy(
    app: AppHandle,
    policy: super::residency::AssetPolicy,
    selected_character_id: Option<String>,
) -> Result<crate::persistent_store::asset_residency::ResidencyStatus> {
    logged_blocking("asset-policy", move || server_sync_asset_policy_operation(&app, policy, selected_character_id.as_deref())).await
}

fn server_sync_asset_policy_operation<R: tauri::Runtime>(app: &AppHandle<R>, policy: super::residency::AssetPolicy, selected_character_id: Option<&str>) -> Result<crate::persistent_store::asset_residency::ResidencyStatus> {
    let _admission = claim_library(&app)?;
    let state = app.state::<ServerSyncCommandState>();
    let (_running, cancelled) = state.claim_preparation()?;
    job_store(&app)?.asset_residency_set_policy_prioritized(policy, Some(cancelled.clone()), selected_character_id, || {
        if cancelled.load(Ordering::Acquire) {
            Err(SyncError::new("cancelled", 409))
        } else {
            Ok(())
        }
    })
}
#[tauri::command]
pub(crate) async fn server_sync_asset_evict(
    app: AppHandle,
) -> Result<crate::persistent_store::asset_residency::ResidencyStatus> {
    logged_blocking("asset-evict", move || server_sync_asset_evict_operation(&app)).await
}

fn server_sync_asset_evict_operation<R: tauri::Runtime>(app: &AppHandle<R>) -> Result<crate::persistent_store::asset_residency::ResidencyStatus> {
    let _admission = claim_library(&app)?;
    let state = app.state::<ServerSyncCommandState>();
    let (_running, cancelled) = state.claim_preparation()?;
    job_store(&app)?.asset_residency_evict_cancelled(Some(cancelled.clone()), || {
        if cancelled.load(Ordering::Acquire) {
            Err(SyncError::new("cancelled", 409))
        } else {
            Ok(())
        }
    })
}
#[tauri::command]
pub(crate) async fn server_sync_cache_usage(
    app: AppHandle,
) -> Result<super::management::CacheUsage> {
    logged_blocking("cache-usage", move || manage_cache(&app, false)).await
}
#[tauri::command]
pub(crate) async fn server_sync_cache_cleanup(
    app: AppHandle,
) -> Result<super::management::CacheUsage> {
    logged_blocking("cache-cleanup", move || manage_cache(&app, true)).await
}
fn manage_cache(app: &AppHandle, clean: bool) -> Result<super::management::CacheUsage> {
    let state=app.state::<ServerSyncCommandState>();
    let _admission=if clean { Some(claim_library(app)?) } else { None };
    let _running=if clean { Some(state.claim()?) } else { None };
    let store=job_store(app)?;
    let blocked=if !clean && state.running.load(Ordering::Acquire) { Some("server-sync-busy") } else { None };
    let active=store.server_stored_config()?.map(|config| risunest_sync_wire::hash(format!("{}:{}",config.library_id,config.device_id).as_bytes()));
    super::management::cache_usage(store.repository_root(),active.as_deref(),&BTreeSet::new(),blocked,clean)
}

#[derive(serde::Serialize)]
#[serde(rename_all="camelCase")]
pub(crate) struct LwwStatus { configured:bool, library_id:Option<String>, device_id:Option<String>, writer_id:String, binding_authority:risunest_sync_wire::stamp::DecimalU64 }
fn lww_client(store:&PersistentStore)->Result<super::lww_client::LwwClient> {lww_client_cancelled(store,None)}
fn lww_client_cancelled(store:&PersistentStore,cancelled:Option<Arc<AtomicBool>>)->Result<super::lww_client::LwwClient> {
    let stored=store.server_stored_config()?.ok_or_else(||SyncError::new("server-unconfigured",409))?;
    let mut client=super::lww_client::LwwClient::with_cancellation(store.repository_root(),stored.resolve(store.repository_root())?,cancelled)?;client.access=Some(stored);Ok(client)
}
#[tauri::command]
pub(crate) async fn server_sync_status(app:AppHandle)->Result<LwwStatus> {
    logged_blocking("status",move|| {let store=job_store(&app)?;let config=store.server_stored_config()?;let clock=store.lww_clock_state()?;Ok(LwwStatus{configured:config.is_some(),library_id:config.as_ref().map(|c|c.library_id.clone()),device_id:config.map(|c|c.device_id),writer_id:clock.writer_id,binding_authority:clock.binding_authority})}).await
}
#[tauri::command]
pub(crate) async fn server_sync_configure(app:AppHandle,config:super::client::ServerConfig)->Result<()> {
    logged_blocking("configure",move|| {let _permit=claim_library(&app)?;let store=job_store(&app)?;let client=super::client::ServerClient::new(config.clone())?;client.resolve_identity(false)?;let stored=super::credentials::StoredConfig::persist(store.repository_root(),&config)?;super::lww_client::OperationLog::open(store.repository_root())?.save_config("candidate",&stored)?;Ok(())}).await
}
#[tauri::command]
pub(crate) async fn server_sync_lww_push(app:AppHandle,request:crate::persistent_store::lww::Header,generating:Vec<crate::persistent_store::lww::MessageLocator>)->Result<Option<risunest_sync_wire::lww::PushReceipt>> {
    logged_blocking("push",move|| {let state=app.state::<ServerSyncCommandState>();let (_job,cancelled)=state.claim_transport(&app.state::<crate::native_file_jobs::NativeFileJobState>().admission,"send")?;let mut store=job_store(&app)?;lww_client_cancelled(&store,Some(cancelled))?.push(&mut store,&request,&generating)}).await
}
#[tauri::command]
pub(crate) async fn server_sync_lww_pull(app:AppHandle,request:crate::persistent_store::lww::Header)->Result<crate::persistent_store::lww::StageReceive> {
    logged_blocking("pull",move|| {let state=app.state::<ServerSyncCommandState>();let (_job,cancelled)=state.claim_transport(&app.state::<crate::native_file_jobs::NativeFileJobState>().admission,"receive")?;let mut store=job_store(&app)?;lww_client_cancelled(&store,Some(cancelled))?.receive_page(&mut store,&request)}).await
}
#[tauri::command]
pub(crate) async fn server_sync_lww_ack(app:AppHandle,request:crate::persistent_store::lww::Header)->Result<()> {
    logged_blocking("ack",move|| {let state=app.state::<ServerSyncCommandState>();let (_job,cancelled)=state.claim_transport(&app.state::<crate::native_file_jobs::NativeFileJobState>().admission,"receive")?;let store=job_store(&app)?;lww_client_cancelled(&store,Some(cancelled))?.finish_receive(&store,&request)}).await
}
#[tauri::command]
pub(crate) async fn server_sync_cancel(app:AppHandle)->Result<()> {app.state::<ServerSyncCommandState>().cancel()}
#[tauri::command]
pub(crate) async fn server_sync_lww_fence(app:AppHandle,new_device:bool)->Result<()> {
    logged_blocking("fence",move|| {let state=app.state::<ServerSyncCommandState>();if !state.cleanup_drained()?{return Err(SyncError::new("server-sync-busy",409));}let mut store=job_store(&app)?;if store.server_stored_config()?.is_none(){return Ok(());}let core=lww_client(&store)?;if new_device{core.fence_new_device(&mut store)}else{core.fence(&mut store)}}).await
}

#[tauri::command]
pub(crate) async fn server_sync_lww_activate(app:AppHandle,request:crate::persistent_store::lww::Header)->Result<()> {
    logged_blocking("activate",move|| {super::binding::activate(&mut job_store(&app)?,&request,None)}).await
}
#[tauri::command]
pub(crate) async fn server_sync_lww_inspect(app:AppHandle,request:crate::persistent_store::lww::Header)->Result<super::binding::Inspection> {logged_blocking("inspect",move||{super::binding::inspect(&job_store(&app)?,&request)}).await}
#[tauri::command]
pub(crate) async fn server_sync_lww_stage_target(app:AppHandle,request:crate::persistent_store::lww::Header,inspection_id:String)->Result<super::binding::StagedTarget> {logged_blocking("stage-target",move||{let _permit=claim_library(&app)?;super::binding::stage(&mut job_store(&app)?,&request,&inspection_id)}).await}
#[tauri::command]
pub(crate) async fn server_sync_lww_prepare_new_device(app:AppHandle,request:crate::persistent_store::lww::Header,staging_id:String)->Result<crate::persistent_store::lww::NewDevicePreparation> {logged_blocking("prepare-new-device",move||{let _permit=claim_library(&app)?;super::binding::prepare_new_device(&mut job_store(&app)?,&request,&staging_id)}).await}
#[tauri::command]
pub(crate) async fn server_sync_lww_activate_new_device(app:AppHandle,request:crate::persistent_store::lww::Header,authorization_id:String,writer_id:String)->Result<()> {logged_blocking("activate-new-device",move||{super::binding::activate(&mut job_store(&app)?,&request,Some((&authorization_id,&writer_id)))}).await}
#[tauri::command]
pub(crate) async fn server_sync_lww_retry(app:AppHandle,request:crate::persistent_store::lww::Header)->Result<crate::persistent_store::lww::ApplyResult> {logged_blocking("retry",move||{let _permit=claim_library(&app)?;let mut store=job_store(&app)?;lww_client(&store)?.retry_unpublished(&mut store,&request)}).await}
#[tauri::command]
pub(crate) async fn server_sync_lww_drain(app:AppHandle,request:crate::persistent_store::lww::Header,generating:Vec<crate::persistent_store::lww::MessageLocator>)->Result<()> {
    logged_blocking("drain",move||{let state=app.state::<ServerSyncCommandState>();let (_job,cancelled)=state.claim_transport(&app.state::<crate::native_file_jobs::NativeFileJobState>().admission,"send")?;let mut store=job_store(&app)?;let core=lww_client_cancelled(&store,Some(cancelled))?;let mut next=request;while core.push(&mut store,&next,&generating)?.is_some(){next.request_id=uuid::Uuid::new_v4().to_string();}if !store.lww_read_outbox(next.binding_authority,1)?.entries.is_empty(){return Err(SyncError::new("generation-active",409));}Ok(())}).await
}
#[tauri::command]
pub(crate) async fn server_sync_lww_hydrate(app:AppHandle,request:crate::persistent_store::lww::Header,selected_character_id:Option<String>)->Result<()> {
    logged_blocking("hydrate",move|| {let state=app.state::<ServerSyncCommandState>();let (_job,cancelled)=state.claim_transport(&app.state::<crate::native_file_jobs::NativeFileJobState>().admission,"hydrate")?;let store=job_store(&app)?;hydrate_binding_assets(&store,&request,cancelled,selected_character_id.as_deref(),||{})}).await
}
pub(crate) fn hydrate_binding_assets(store:&crate::persistent_store::PersistentStore,request:&crate::persistent_store::lww::Header,cancelled:Arc<AtomicBool>,selected_character_id:Option<&str>,on_object_done:impl Fn())->Result<()> {
    if store.server_asset_policy()?!=super::residency::AssetPolicy::Full{return Ok(());}
    let authority=request.binding_authority;
    store.hydrate_registered_remote_assets_prioritized(Some(cancelled.clone()),selected_character_id,||{if cancelled.load(Ordering::Acquire){return Err(SyncError::new("cancelled",409));}if store.lww_binding_authority()?!=authority{return Err(SyncError::new("binding-authority-changed",409));}Ok(())},on_object_done)
}

#[tauri::command]
pub(crate) async fn server_sync_notify_stop(app:AppHandle)->Result<()> {
    let state=app.state::<ServerSyncCommandState>();
    let job={state.notification.lock().map_err(|_|SyncError::new("server-sync-state-unavailable",503))?.take()};
    if let Some(job)=job {job.abort();let _=job.await;}
    Ok(())
}
#[tauri::command]
pub(crate) async fn server_sync_notify_start(app:AppHandle,request:crate::persistent_store::lww::Header)->Result<()> {
    server_sync_notify_stop(app.clone()).await?;
    if app.state::<ServerSyncCommandState>().cleanup_closed.load(Ordering::Acquire){return Err(SyncError::new("cleanup-pending",409));}
    let copy=app.clone();
    let config=blocking(move|| {let store=job_store(&copy)?;if store.lww_binding_authority()?!=request.binding_authority {return Err(SyncError::new("binding-authority-changed",409));}let core=lww_client(&store)?;core.client.resolve_identity(false)?;Ok(core.client.config())}).await?;
    let events=app.clone();
    let connection=app.clone();
    let state=app.state::<ServerSyncCommandState>();
    let mut slot=state.notification.lock().map_err(|_|SyncError::new("server-sync-state-unavailable",503))?;
    if app.state::<ServerSyncCommandState>().cleanup_closed.load(Ordering::Acquire){return Err(SyncError::new("cleanup-pending",409));}
    app.state::<ServerSyncCommandState>().notification_active.store(true,Ordering::Release);
    let guard=NotificationJob(app.state::<ServerSyncCommandState>().notification_active.clone());
    let job=tauri::async_runtime::spawn(async move {
        let _guard=guard;
        use tauri::Emitter;
        let _=super::notification::run(config,move|frame| {let _=events.emit("risu-server-sync-remote-hint",frame);},move|connected| {let _=connection.emit("risu-server-sync-notification",serde_json::json!({"connected":connected}));}).await;
    });
    *slot=Some(job);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_stalled_real_receive_does_not_delay_push_and_finished_jobs_release_admission() {
        use super::super::lww_tests::{LocalServerFixture, local, save, header};
        let (entered_tx, entered_rx)=std::sync::mpsc::channel();
        let gate=Arc::new(tokio::sync::Notify::new()); let wait=gate.clone();
        let server=LocalServerFixture::with_router(move|router|router.layer(axum::middleware::from_fn(move|request:axum::extract::Request,next:axum::middleware::Next| {
            let gate=wait.clone();let entered=entered_tx.clone();
            async move {if request.uri().path().ends_with("/changes") {entered.send(()).unwrap();gate.notified().await;} next.run(request).await}
        })));
        let (root,mut store)=local();let core=server.client(&store);let config=core.client.config();
        save(&mut store,&["root","language"],serde_json::json!("en"));
        let state=Arc::new(ServerSyncCommandState::default());let admission=Arc::new(crate::native_file_jobs::admission::Admission::default());
        let receiver_state=state.clone();let receiver_admission=admission.clone();let receiver_root=root.path().to_path_buf();
        let receive=std::thread::spawn(move|| {
            let (_job,cancelled)=receiver_state.claim_transport(&receiver_admission,"receive").unwrap();
            let mut store=crate::persistent_store::PersistentStore::open(&receiver_root).unwrap();
            let core=super::super::lww_client::LwwClient::with_cancellation(&receiver_root,config,Some(cancelled)).unwrap();
            let request=header(&store);core.receive_native(&mut store,&request,&[]).unwrap();
        });
        entered_rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        let (send,_)=state.claim_transport(&admission,"send").unwrap();
        let request=header(&store);assert!(core.push(&mut store,&request,&[]).unwrap().is_some());
        drop(send);assert!(admission.file(true).is_err());
        gate.notify_one();receive.join().unwrap();assert!(state.cleanup_drained().unwrap());assert!(admission.file(true).is_ok());
    }
    #[test]
    fn transport_lanes_overlap_and_only_the_last_active_job_holds_replacement_admission() {
        let state=ServerSyncCommandState::default();let admission=Arc::new(crate::native_file_jobs::admission::Admission::default());
        let (receive,_)=state.claim_transport(&admission,"receive").unwrap();let (send,_)=state.claim_transport(&admission,"send").unwrap();let (hydrate,_)=state.claim_transport(&admission,"hydrate").unwrap();
        assert!(admission.file(true).is_err());drop(receive);drop(hydrate);assert!(admission.file(true).is_err());drop(send);assert!(admission.file(true).is_ok());assert!(state.cleanup_drained().unwrap());
    }
    #[test]
    fn cleanup_cancels_every_lane_and_refuses_reconnection_until_all_native_jobs_end() {
        let state=ServerSyncCommandState::default();let admission=Arc::new(crate::native_file_jobs::admission::Admission::default());
        let (send,a)=state.claim_transport(&admission,"send").unwrap();let (receive,b)=state.claim_transport(&admission,"receive").unwrap();let (hydrate,c)=state.claim_transport(&admission,"hydrate").unwrap();
        state.notification_active.store(true,Ordering::Release);let notification=NotificationJob(state.notification_active.clone());state.begin_cleanup().unwrap();
        assert!(a.load(Ordering::Acquire)&&b.load(Ordering::Acquire)&&c.load(Ordering::Acquire));assert!(!state.cleanup_drained().unwrap());assert!(state.claim_transport(&admission,"send").is_err());
        drop(send);drop(receive);drop(hydrate);assert!(!state.cleanup_drained().unwrap());drop(notification);assert!(state.cleanup_drained().unwrap());state.finish_cleanup().unwrap();assert!(state.claim_transport(&admission,"send").is_ok());
    }
}
