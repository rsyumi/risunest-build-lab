use super::{Result, SyncError};
use crate::persistent_store::{commands::with_store_mut, PersistentStore};
use std::collections::BTreeSet;
use std::sync::{atomic::{AtomicBool, AtomicU64, Ordering}, Arc, Mutex};
use tauri::{AppHandle, Manager};
#[derive(Default)]
pub(crate) struct ServerSyncCommandState {
    cleanup_closed: AtomicBool,
    running: AtomicBool,
    cancelled: Mutex<Arc<AtomicBool>>,
    notification: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    notification_active: Arc<AtomicBool>,
    notification_epoch: AtomicU64,
    notification_start: Mutex<Option<Arc<AtomicBool>>>,
    transports: Mutex<TransportJobs>,
}
pub(crate) struct NotificationStart { epoch: u64, cancelled: Arc<AtomicBool> }
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
    // A stop also cancels a start that is still checking the server identity, so it never
    // installs a notification job after the stop returned.
    fn stop_notification(&self) -> Result<Option<tauri::async_runtime::JoinHandle<()>>> {
        let mut slot=self.notification.lock().map_err(|_|SyncError::new("server-sync-state-unavailable",503))?;
        self.notification_epoch.fetch_add(1,Ordering::AcqRel);
        if let Some(start)=self.notification_start.lock().map_err(|_|SyncError::new("server-sync-state-unavailable",503))?.take(){start.store(true,Ordering::Release);}
        Ok(slot.take())
    }
    fn begin_notification(&self) -> Result<NotificationStart> {
        let _slot=self.notification.lock().map_err(|_|SyncError::new("server-sync-state-unavailable",503))?;
        let cancelled=Arc::new(AtomicBool::new(false));
        *self.notification_start.lock().map_err(|_|SyncError::new("server-sync-state-unavailable",503))?=Some(cancelled.clone());
        Ok(NotificationStart{epoch:self.notification_epoch.load(Ordering::Acquire),cancelled})
    }
    fn install_notification(&self,start:NotificationStart,spawn:impl FnOnce(NotificationJob)->tauri::async_runtime::JoinHandle<()>) -> Result<()> {
        let mut slot=self.notification.lock().map_err(|_|SyncError::new("server-sync-state-unavailable",503))?;
        if self.cleanup_closed.load(Ordering::Acquire){return Err(SyncError::new("cleanup-pending",409));}
        let mut current=self.notification_start.lock().map_err(|_|SyncError::new("server-sync-state-unavailable",503))?;
        if current.as_ref().is_some_and(|flag|Arc::ptr_eq(flag,&start.cancelled)){*current=None;}
        drop(current);
        if start.cancelled.load(Ordering::Acquire)||self.notification_epoch.load(Ordering::Acquire)!=start.epoch{return Err(SyncError::new("cancelled",409));}
        self.notification_active.store(true,Ordering::Release);
        *slot=Some(spawn(NotificationJob(self.notification_active.clone())));
        Ok(())
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
    recorded(stage, blocking(operation).await)
}
/// Refusals a caller meets in normal use, such as another operation holding
/// the library. With the transient outcomes `retryable` names they are logged
/// as warnings rather than failures.
const EXPECTED_REFUSALS: [&str; 7] = [
    "cancelled",
    "server-sync-busy",
    "library-operation-busy",
    "cleanup-pending",
    "server-unconfigured",
    "binding-authority-changed",
    "generation-active",
];
fn record_failure(stage: &str, error: &SyncError) {
    let expected = EXPECTED_REFUSALS.contains(&error.code.as_str())
        || super::TRANSIENT_CODES.contains(&error.code.as_str());
    let cause = match (&error.cause, expected) {
        (Some(cause), false) => format!(" cause={cause}"),
        _ => String::new(),
    };
    crate::native_log::log_global(
        if expected { "warn" } else { "error" },
        "server-sync",
        format!(
            "{stage} failed: code={} status={}{cause} at={}:{}",
            error.code,
            error.status,
            error.at.file(),
            error.at.line(),
        ),
    );
}
/// A failed command leaves one line in this device's log with its code, the
/// failure behind it and where it was raised.
fn recorded<T>(stage: &str, result: Result<T>) -> Result<T> {
    if let Err(error) = &result {
        record_failure(stage, error);
    }
    result
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
    logged_blocking("configure",move|| {let _permit=claim_library(&app)?;let store=job_store(&app)?;let client=super::client::ServerClient::new(config.clone())?;client.resolve_identity()?;let stored=super::credentials::StoredConfig::persist(store.repository_root(),&config)?;super::lww_client::OperationLog::open(store.repository_root())?.save_config("candidate",&stored)?;Ok(())}).await
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
pub(crate) async fn server_sync_cancel(app:AppHandle)->Result<()> {recorded("cancel",app.state::<ServerSyncCommandState>().cancel())}
#[tauri::command]
pub(crate) async fn server_sync_lww_fence(app:AppHandle,new_device:bool)->Result<()> {
    logged_blocking("fence",move|| {let state=app.state::<ServerSyncCommandState>();if !state.cleanup_drained()?{return Err(SyncError::new("server-sync-busy",409));}let mut store=job_store(&app)?;if store.server_stored_config()?.is_none(){return Ok(());}let core=lww_client(&store)?;if new_device{core.fence_new_device(&mut store)}else{super::binding::fence_for_binding_change(&core,&mut store)}}).await
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
pub(crate) async fn server_sync_lww_prepare_fresh_writer(app:AppHandle,request:crate::persistent_store::lww::Header,inspection_id:String)->Result<crate::persistent_store::lww::NewDevicePreparation> {logged_blocking("prepare-fresh-writer",move||{let _permit=claim_library(&app)?;super::binding::prepare_fresh_writer(&mut job_store(&app)?,&request,&inspection_id)}).await}
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
pub(crate) async fn server_sync_notify_stop(app:AppHandle)->Result<()> {recorded("notify-stop",stop_notification_job(&app).await)}
async fn stop_notification_job(app:&AppHandle)->Result<()> {
    let job=app.state::<ServerSyncCommandState>().stop_notification()?;
    if let Some(job)=job {job.abort();let _=job.await;}
    Ok(())
}
#[tauri::command]
pub(crate) async fn server_sync_notify_start(app:AppHandle,request:crate::persistent_store::lww::Header)->Result<()> {recorded("notify-start",start_notification_job(app,request).await)}
async fn start_notification_job(app:AppHandle,request:crate::persistent_store::lww::Header)->Result<()> {
    stop_notification_job(&app).await?;
    if app.state::<ServerSyncCommandState>().cleanup_closed.load(Ordering::Acquire){return Err(SyncError::new("cleanup-pending",409));}
    let start=app.state::<ServerSyncCommandState>().begin_notification()?;
    let lookup=start.cancelled.clone();
    let copy=app.clone();
    let config=blocking(move|| {let store=job_store(&copy)?;if store.lww_binding_authority()?!=request.binding_authority {return Err(SyncError::new("binding-authority-changed",409));}let core=lww_client_cancelled(&store,Some(lookup))?;core.client.resolve_identity()?;Ok(core.client.config())}).await?;
    let events=app.clone();
    let connection=app.clone();
    app.state::<ServerSyncCommandState>().install_notification(start,move|guard| tauri::async_runtime::spawn(async move {
        let _guard=guard;
        use tauri::Emitter;
        let _=super::notification::run(config,move|frame| {let _=events.emit("risu-server-sync-remote-hint",frame);},move|connected| {let _=connection.emit("risu-server-sync-notification",serde_json::json!({"connected":connected}));}).await;
    }))
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
    fn a_stop_during_the_identity_check_keeps_the_late_notification_start_from_installing() {
        let state=ServerSyncCommandState::default();
        let start=state.begin_notification().unwrap();let lookup=start.cancelled.clone();
        assert!(state.stop_notification().unwrap().is_none());
        assert!(lookup.load(Ordering::Acquire));
        let result=state.install_notification(start,|guard|tauri::async_runtime::spawn(async move {let _guard=guard;std::future::pending::<()>().await;}));
        assert_eq!(result.err().map(|error|error.code),Some("cancelled".to_string()));
        assert!(state.notification.lock().unwrap().is_none());assert!(state.cleanup_drained().unwrap());
        let start=state.begin_notification().unwrap();
        state.install_notification(start,|guard|tauri::async_runtime::spawn(async move {let _guard=guard;std::future::pending::<()>().await;})).unwrap();
        assert!(!state.cleanup_drained().unwrap());
        state.stop_notification().unwrap().unwrap().abort();
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

#[cfg(test)]
mod failure_log_tests {
    use super::*;

    fn logged(stage: &str) -> crate::native_log::LogEntry {
        let prefix = format!("{stage} failed: ");
        crate::native_log::global_state()
            .tail(None)
            .into_iter()
            .rev()
            .find(|entry| entry.target == "server-sync" && entry.message.starts_with(&prefix))
            .expect("the failure is logged")
    }

    #[test]
    fn a_failure_logs_its_cause_and_origin_and_a_refusal_is_a_warning() {
        let io = std::io::Error::from(std::io::ErrorKind::InvalidData);
        let line = line!() + 1;
        let failure = recorded("synthetic-storage-stage", Err::<(), _>(SyncError::from(io)));
        assert_eq!(failure.unwrap_err().code, "local-storage");
        let entry = logged("synthetic-storage-stage");
        assert_eq!(entry.level, "error");
        assert!(entry
            .message
            .contains("code=local-storage status=503 cause=InvalidData: "));
        assert!(entry.message.ends_with(&format!("commands.rs:{line}")));

        let _ = recorded(
            "synthetic-busy-stage",
            Err::<(), _>(SyncError::new("server-sync-busy", 409)),
        );
        let entry = logged("synthetic-busy-stage");
        assert_eq!(entry.level, "warn");
        assert!(!entry.message.contains("cause="));

        assert!(recorded("synthetic-quiet-stage", Ok(())).is_ok());
        assert!(crate::native_log::global_state()
            .tail(None)
            .iter()
            .all(|entry| !entry.message.starts_with("synthetic-quiet-stage")));
    }

    #[test]
    fn routine_refusals_and_transient_codes_are_warnings_without_their_cause() {
        for (index, code) in EXPECTED_REFUSALS
            .iter()
            .chain(super::super::TRANSIENT_CODES.iter())
            .enumerate()
        {
            let stage = format!("synthetic-expected-stage-{index}");
            let error = SyncError::caused(code, 409, "synthetic-hidden-cause".to_owned());
            let _ = recorded(&stage, Err::<(), _>(error));
            let entry = logged(&stage);
            assert_eq!(entry.level, "warn", "{code} is a warning");
            assert!(entry.message.contains(&format!("code={code} status=409 at=")));
            assert!(!entry.message.contains("synthetic-hidden-cause"));
        }
    }

    #[test]
    fn retryable_local_and_transport_failures_are_errors() {
        let sqlite = SyncError::from(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
            None,
        ));
        for (stage, error, code) in [
            (
                "synthetic-local-storage-stage",
                SyncError::from(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
                "local-storage",
            ),
            ("synthetic-local-metadata-stage", sqlite, "local-metadata"),
            (
                "synthetic-unreachable-stage",
                SyncError::new("server-unreachable", 503),
                "server-unreachable",
            ),
        ] {
            assert!(error.retryable);
            let _ = recorded(stage, Err::<(), _>(error));
            let entry = logged(stage);
            assert_eq!(entry.level, "error", "{code} is a failure");
            assert!(entry.message.contains(&format!("code={code} status=503")));
        }
    }

    #[test]
    fn a_blocking_command_logs_its_failure_once() {
        let line = line!() + 2;
        let result = tauri::async_runtime::block_on(logged_blocking("synthetic-blocking-stage", || {
            Err::<(), _>(SyncError::new("local-validation", 409))
        }));
        assert_eq!(result.unwrap_err().code, "local-validation");
        let entry = logged("synthetic-blocking-stage");
        assert_eq!(entry.level, "error");
        assert!(entry.message.ends_with(&format!("commands.rs:{line}")));
        assert_eq!(
            crate::native_log::global_state()
                .tail(None)
                .iter()
                .filter(|entry| entry.message.starts_with("synthetic-blocking-stage failed: "))
                .count(),
            1
        );
    }
}
