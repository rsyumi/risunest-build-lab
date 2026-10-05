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
    stage: Mutex<Option<Arc<AtomicBool>>>,
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
struct StageJob<'a>(&'a ServerSyncCommandState, Arc<AtomicBool>);
impl Drop for StageJob<'_> {
    fn drop(&mut self) {if let Ok(mut slot)=self.0.stage.lock(){if slot.as_ref().is_some_and(|flag|Arc::ptr_eq(flag,&self.1)){*slot=None;}}}
}
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
    // The first-binding stage holds the library permit outside the transport lanes. Hiding the
    // app stops those lanes but not the stage; only a reload of the page that awaits it does.
    fn claim_stage(&self) -> Result<(StageJob<'_>, Arc<AtomicBool>)> {
        let flag=Arc::new(AtomicBool::new(false));
        *self.stage.lock().map_err(|_|SyncError::new("server-sync-state-unavailable",503))?=Some(flag.clone());
        Ok((StageJob(self,flag.clone()),flag))
    }
    pub(crate) fn reset_renderer_session(&self) -> Result<()> {
        self.cancel()?;
        if let Some(flag)=self.stage.lock().map_err(|_|SyncError::new("server-sync-state-unavailable",503))?.as_ref(){flag.store(true,Ordering::Release);}
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
pub(crate) async fn asset_residency_download_remote(
    app: AppHandle,
    connection_id: Option<String>,
    selected_character_id: Option<String>,
) -> Result<crate::persistent_store::asset_residency::ResidencyStatus> {
    logged_blocking("asset-download", move || {
        let _admission = claim_library(&app)?;
        let state = app.state::<ServerSyncCommandState>();
        let (_running, cancelled) = state.claim_preparation()?;
        job_store(&app)?.asset_residency_download_remote(connection_id.as_deref(), Some(cancelled.clone()), selected_character_id.as_deref(), || {
            if cancelled.load(Ordering::Acquire) {
                Err(SyncError::new("cancelled", 409))
            } else {
                Ok(())
            }
        })
    }).await
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
    logged_blocking("fence",move|| {let state=app.state::<ServerSyncCommandState>();if !state.cleanup_drained()?{return Err(SyncError::new("server-sync-busy",409));}let mut store=job_store(&app)?;super::binding::fence_stored(&mut store,new_device)}).await
}

#[tauri::command]
pub(crate) async fn server_sync_lww_activate(app:AppHandle,request:crate::persistent_store::lww::Header)->Result<()> {
    logged_blocking("activate",move|| {super::binding::activate(&mut job_store(&app)?,&request,None)}).await
}
#[tauri::command]
pub(crate) async fn server_sync_lww_pending_binding(app:AppHandle)->Result<Option<super::binding::PendingBinding>> {logged_blocking("pending-binding",move||{super::binding::pending_binding(&job_store(&app)?)}).await}
#[tauri::command]
pub(crate) async fn server_sync_lww_inspect(app:AppHandle,request:crate::persistent_store::lww::Header)->Result<super::binding::Inspection> {logged_blocking("inspect",move||{super::binding::inspect(&job_store(&app)?,&request)}).await}
#[tauri::command]
pub(crate) async fn server_sync_lww_stage_target(app:AppHandle,request:crate::persistent_store::lww::Header,inspection_id:String)->Result<super::binding::StagedTarget> {logged_blocking("stage-target",move||{let _permit=claim_library(&app)?;let state=app.state::<ServerSyncCommandState>();let (_stage,cancelled)=state.claim_stage()?;super::binding::stage(&mut job_store(&app)?,&request,&inspection_id,Some(cancelled))}).await}
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
async fn stop_notification_job<R: tauri::Runtime>(app:&AppHandle<R>)->Result<()> {
    let job=app.state::<ServerSyncCommandState>().stop_notification()?;
    if let Some(job)=job {job.abort();let _=job.await;}
    Ok(())
}
#[tauri::command]
pub(crate) async fn server_sync_notify_start(app:AppHandle,request:crate::persistent_store::lww::Header)->Result<()> {recorded("notify-start",start_notification_job(app,request).await)}
async fn start_notification_job<R: tauri::Runtime>(app:AppHandle<R>,request:crate::persistent_store::lww::Header)->Result<()> {
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
    #[test]
    fn a_reload_stops_the_first_binding_stage_and_still_releases_its_state_pin() {
        use super::super::lww_tests::{LocalServerFixture, local, save, header};
        let state=Arc::new(ServerSyncCommandState::default());
        let released=Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reload=state.clone();let counted=released.clone();
        // The page reloads while the server answers the first state page.
        let server=LocalServerFixture::with_router(move|router|router.layer(axum::middleware::from_fn(move|request:axum::extract::Request,next:axum::middleware::Next| {
            let reload=reload.clone();let counted=counted.clone();
            async move {
                let (method,path)=(request.method().clone(),request.uri().path().to_owned());
                let response=next.run(request).await;
                if method==axum::http::Method::GET&&path.ends_with("/state") {reload.reset_renderer_session().unwrap();}
                if method==axum::http::Method::DELETE&&path.contains("/state/pins/") {counted.fetch_add(1,Ordering::AcqRel);}
                response
            }
        })));
        let (_a,mut source)=local();let (_b,mut target)=local();
        let core=server.client(&source);
        save(&mut source,&["root","language"],serde_json::json!("ja"));
        let source_header=header(&source);core.push(&mut source,&source_header,&[]).unwrap();
        server.prepare_binding_candidate(&target);
        let inspected=super::super::binding::inspect(&target,&header(&target)).unwrap();
        let inspected_pins=released.load(Ordering::Acquire);
        let (stage,cancelled)=state.claim_stage().unwrap();
        let target_header=header(&target);
        let Err(error)=super::super::binding::stage(&mut target,&target_header,&inspected.inspection_id,Some(cancelled)) else {panic!("a reload must stop the stage")};
        assert_eq!(error.code,"cancelled");
        assert_eq!(released.load(Ordering::Acquire),inspected_pins+1);
        drop(stage);assert!(state.stage.lock().unwrap().is_none());
    }

    use crate::persistent_store::commands::{with_store, PersistentStoreState};
    use super::super::residency::AssetPolicy;
    use tauri::test::MockRuntime;
    const WAIT: std::time::Duration = std::time::Duration::from_secs(30);
    fn mock_app(store: PersistentStore) -> tauri::App<MockRuntime> {
        let root = store.repository_root().to_owned();
        mock_app_with(PersistentStoreState::with_test_store(store), &root)
    }
    fn mock_app_with(store: PersistentStoreState, root: &std::path::Path) -> tauri::App<MockRuntime> {
        let app = tauri::test::mock_builder().build(tauri::test::mock_context(tauri::test::noop_assets())).unwrap();
        app.manage(store);
        app.manage(crate::native_file_jobs::NativeFileJobState::initialize(root.join("native-file-jobs")));
        app.manage(ServerSyncCommandState::default());
        app
    }
    fn policy(app: &tauri::App<MockRuntime>) -> AssetPolicy {
        with_store(app.state(), |store| store.device_store()?.asset_residency_policy()).unwrap()
    }
    fn assert_released(app: &tauri::App<MockRuntime>) {
        assert!(!app.state::<ServerSyncCommandState>().running.load(Ordering::Acquire));
        drop(app.state::<crate::native_file_jobs::NativeFileJobState>().admission.server().unwrap());
    }
    #[test]
    fn a_refused_asset_policy_change_keeps_the_previous_policy_and_releases_admission() {
        let (_root, store) = super::super::lww_tests::local();
        let app = mock_app(store);
        assert_eq!(policy(&app), AssetPolicy::Full);
        let error = server_sync_asset_policy_operation(app.handle(), AssetPolicy::Remote, None).err().expect("remote assets need a server");
        assert_eq!((error.code.as_str(), error.retryable), ("server-not-bound", false));
        assert_eq!(policy(&app), AssetPolicy::Full);
        assert_released(&app);
    }
    #[test]
    fn an_unavailable_job_store_returns_a_retryable_fault_and_releases_admission() {
        let root = tempfile::tempdir().unwrap();
        let app = mock_app_with(PersistentStoreState::default(), root.path());
        let error = server_sync_asset_evict_operation(app.handle()).err().expect("eviction without a local store must fail");
        assert_eq!((error.code.as_str(), error.status, error.retryable), ("local-store-unavailable", 503, true));
        assert_released(&app);
        let error = server_sync_asset_policy_operation(app.handle(), AssetPolicy::Full, None).err().expect("a policy change without a local store must fail");
        assert_eq!((error.code.as_str(), error.status, error.retryable), ("local-store-unavailable", 503, true));
        assert_released(&app);
    }
    #[test]
    fn a_cancel_reaches_the_running_asset_command_and_a_retry_gets_a_fresh_flag() {
        use super::super::lww_tests::{drain_publications, local, put_asset, LocalServerFixture};
        let armed = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let gate = Arc::new(tokio::sync::Notify::new());
        let (hold, wait) = (armed.clone(), gate.clone());
        // The first request of the armed command waits until the test releases it.
        let server = LocalServerFixture::with_router(move |router| router.layer(axum::middleware::from_fn(move |request: axum::extract::Request, next: axum::middleware::Next| {
            let (hold, wait, entered) = (hold.clone(), wait.clone(), entered_tx.clone());
            async move {
                if hold.swap(false, Ordering::AcqRel) {
                    entered.send(()).unwrap();
                    wait.notified().await;
                }
                next.run(request).await
            }
        })));
        let (_root, mut store) = local();
        let core = server.client(&store);
        put_asset(&mut store, "assets/cancel-command.png", b"synthetic cancel command");
        drain_publications(&core, &mut store, &[]).unwrap();
        store.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
        store.asset_residency_evict(|| Ok(())).unwrap();
        let app = mock_app(store);
        armed.store(true, Ordering::Release);
        let handle = app.handle().clone();
        let worker = std::thread::spawn(move || server_sync_asset_policy_operation(&handle, AssetPolicy::Full, None).map(|_| ()));
        entered_rx.recv_timeout(WAIT).unwrap();
        let state = app.state::<ServerSyncCommandState>();
        assert!(state.running.load(Ordering::Acquire));
        let cancelled = state.cancelled.lock().unwrap().clone();
        assert!(!cancelled.load(Ordering::Acquire));
        state.cancel().unwrap();
        assert!(cancelled.load(Ordering::Acquire));
        gate.notify_one();
        let error = worker.join().unwrap().err().expect("a cancelled asset policy command must fail");
        assert_eq!(error.code, "cancelled");
        assert_released(&app);
        server_sync_asset_policy_operation(app.handle(), AssetPolicy::Remote, None).unwrap();
        let current = state.cancelled.lock().unwrap().clone();
        assert!(!Arc::ptr_eq(&current, &cancelled));
        assert!(!current.load(Ordering::Acquire));
        assert_eq!(policy(&app), AssetPolicy::Remote);
        assert_released(&app);
    }
    #[test]
    fn a_head_move_on_the_server_reaches_the_remote_hint_event() {
        use super::super::lww_tests::{drain_publications, header, local, save, LocalServerFixture};
        use risunest_sync_wire::lww::SeqNotification;
        use tauri::Listener;
        let server = LocalServerFixture::new();
        let (_source_root, mut source) = local();
        let sender = server.client(&source);
        let (_root, store) = local();
        server.client(&store);
        let request = header(&store);
        let app = mock_app(store);
        let (hint_tx, hint_rx) = std::sync::mpsc::channel();
        app.listen("risu-server-sync-remote-hint", move |event| {
            let _ = hint_tx.send(serde_json::from_str::<SeqNotification>(event.payload()).unwrap());
        });
        let (link_tx, link_rx) = std::sync::mpsc::channel();
        app.listen("risu-server-sync-notification", move |event| {
            let _ = link_tx.send(serde_json::from_str::<serde_json::Value>(event.payload()).unwrap());
        });
        tauri::async_runtime::block_on(start_notification_job(app.handle().clone(), request)).unwrap();
        assert_eq!(link_rx.recv_timeout(WAIT).unwrap(), serde_json::json!({"connected": true}));
        let SeqNotification::Seq { seq: before } = hint_rx.recv_timeout(WAIT).unwrap();
        save(&mut source, &["root", "language"], serde_json::json!("ja"));
        drain_publications(&sender, &mut source, &[]).unwrap();
        let SeqNotification::Seq { seq: after } = hint_rx.recv_timeout(WAIT).unwrap();
        assert!(after.0 > before.0, "{} after {}", after.0, before.0);
        tauri::async_runtime::block_on(stop_notification_job(app.handle())).unwrap();
        assert!(app.state::<ServerSyncCommandState>().cleanup_drained().unwrap());
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
