use super::{
    connection_commands,
    contract::*,
    lww_engine::{ExternalLwwEngine, PublicationResult},
    lww_segment,
    providers::Dependencies,
    runtime,
};
use crate::persistent_store::{
    lww::{Header, MessageLocator, NewDevicePreparation, StageReceive},
    sync_selection::SyncTarget,
};
use crate::native_log::logged;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};
use tauri::{AppHandle,Manager};

struct Session {
    engine: Option<ExternalLwwEngine>,
    dependencies: Option<Dependencies>,
    fresh_after: Instant,
    cancel: Cancellation,
    /// Ids of the receive pages the last listing produced, in order.
    receive_pages: VecDeque<String>,
}
struct Context {
    session: tokio::sync::Mutex<Session>,
    cancel: Mutex<Cancellation>,
    maintenance:tokio::sync::Mutex<Session>,
    checkpoints:tokio::sync::Mutex<super::lww_compaction::CheckpointSummaries>,
    turn:Mutex<super::lww_compaction::CompactionTurn>,
}
static CONTEXTS: OnceLock<Mutex<BTreeMap<String, Arc<Context>>>> = OnceLock::new();
fn context(id: &str) -> Result<Arc<Context>> {
    if id.is_empty() {
        return Err(lww_segment::corrupt());
    }
    let mut contexts = CONTEXTS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(runtime::local_error)?;
    Ok(contexts
        .entry(id.into())
        .or_insert_with(|| {
            Arc::new(Context {
                session: tokio::sync::Mutex::new(Session {
                    engine: None,
                    dependencies: None,
                    fresh_after: Instant::now(),
                    cancel: Cancellation::default(),
                    receive_pages: VecDeque::new(),
                }),
                cancel: Mutex::new(Cancellation::default()),
                maintenance:tokio::sync::Mutex::new(Session{engine:None,dependencies:None,fresh_after:Instant::now(),cancel:Cancellation::default(),receive_pages:VecDeque::new()}),
                checkpoints:Default::default(),
                turn:Mutex::new(Default::default()),
            })
        })
        .clone())
}
async fn connect(app: &AppHandle, id: &str, session: &mut Session) -> Result<()> {
    if session.engine.is_none() {
        session.fresh_after = Instant::now();
        session.receive_pages.clear();
        let connected =
            connection_commands::open_connected_with_cancel(app, id, &session.cancel).await?;
        if !matches!(
            connected.stored.config.provider.as_str(),
            "webdav" | "s3" | "google_drive" | "onedrive"
        ) || connected.stored.descriptor.publication_strategy.is_none()
        {
            return Err(ProviderError::new(ErrorKind::Unsupported));
        }
        super::connection::validate_sync_location(&connected.stored.config)?;
        session.dependencies = Some(connected.dependencies);
        session.engine = Some(ExternalLwwEngine {
            provider: connected.provider,
            repository: connected.handle,
            library: connected.stored.descriptor.repository_id.clone(),
            root_key: connected.root_key,
            admission: None,
            connection_id: id.into(),
            connection_root: runtime::root(app)?,
            capabilities: connected.stored.capabilities,
            descriptor: connected.stored.descriptor,
        });
    }
    Ok(())
}
async fn open(app: &AppHandle, id: &str, session: &mut Session) -> Result<()> {
    connect(app, id, session).await?;
    let engine = session.engine.as_mut().ok_or_else(lww_segment::corrupt)?;
    if engine.admitted_upper().is_err() {
        let requests = &session
            .dependencies
            .as_ref()
            .ok_or_else(lww_segment::corrupt)?
            .requests;
        let mut sample =
            requests.clock_sample_after(&engine.repository.account, session.fresh_after)?;
        if sample.is_none() {
            session.fresh_after = Instant::now();
            engine
                .provider
                .list_objects(
                    &engine.repository,
                    Collection::Segments,
                    None,
                    1,
                    &session.cancel,
                )
                .await?;
            sample =
                requests.clock_sample_after(&engine.repository.account, session.fresh_after)?;
        }
        engine.admit(&sample.ok_or_else(|| ProviderError::new(ErrorKind::ClockSkew))?)?;
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ExitTarget {
    revision: risunest_sync_wire::stamp::DecimalU64,
    library_epoch: String,
    selection_epoch: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Request {
    pub connection_id: String,
    #[serde(flatten)]
    pub header: Header,
    #[serde(default)]
    pub generating: Vec<MessageLocator>,
    #[serde(default)]
    exit_target: Option<ExitTarget>,
}
fn check(
    app: &AppHandle,
    request: &Request,
    selected: bool,
) -> Result<crate::persistent_store::PersistentStore> {
    let store = runtime::native_store(app)?;
    check_store(&store, request, selected)?;
    Ok(store)
}
fn check_store(
    store: &crate::persistent_store::PersistentStore,
    request: &Request,
    selected: bool,
) -> Result<()> {
    if store
        .lww_binding_authority()
        .map_err(runtime::local_error)?
        != request.header.binding_authority
    {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    if selected
        && store
            .lww_binding_state()
            .map_err(runtime::local_error)?
            .target
            != SyncTarget::External(request.connection_id.clone())
    {
        return Err(ProviderError::new(ErrorKind::Cancelled));
    }
    Ok(())
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Inspection {
    inspection_id: String,
    target_id: String,
    library_id: String,
    empty: bool,
    previously_bound_library: bool,
    registration_changed: bool,
    server_restored: bool,
}
#[tauri::command]
pub(crate) async fn external_lww_inspect(app: AppHandle, request: Request) -> Result<Inspection> {
    logged("external_lww_inspect", async move {
        let context = context(&request.connection_id)?;
        let mut session = context.session.lock().await;
        session.cancel = context.cancel.lock().map_err(runtime::local_error)?.clone();
        open(&app, &request.connection_id, &mut session).await?;
        let store = check(&app, &request, false)?;
        let engine = session.engine.as_ref().ok_or_else(lww_segment::corrupt)?;
        let objects = engine.listing(&session.cancel).await?;
        let target_id = engine.repository.connection_identity.clone();
        let library_id = engine.library.clone();
        let inspection_id = store
            .register_lww_binding_inspection(
                request.header.binding_authority,
                &SyncTarget::External(request.connection_id),
                &target_id,
                &library_id,
            )
            .map_err(runtime::local_error)?;
        let previously_bound_library = store
            .lww_previously_bound(&library_id, &target_id)
            .map_err(runtime::local_error)?;
        Ok(Inspection {
            inspection_id,
            target_id,
            library_id,
            empty: objects.is_empty() && engine.snapshot_listing(&session.cancel).await?.is_empty(),
            previously_bound_library,
            registration_changed: false,
            server_restored: false,
        })
    }.await)
}
pub(crate) struct StageRequest {
    request: Request,
    inspection_id: String,
    target_id: String,
    library_id: String,
}
impl<'de> Deserialize<'de> for StageRequest {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Input {
            connection_id: String,
            binding_authority: risunest_sync_wire::stamp::DecimalU64,
            request_id: String,
            #[serde(default)]
            generating: Vec<MessageLocator>,
            inspection_id: String,
            target_id: String,
            library_id: String,
        }
        let input = Input::deserialize(deserializer)?;
        Ok(Self {
            request: Request {
                connection_id: input.connection_id,
                header: Header {
                    binding_authority: input.binding_authority,
                    request_id: input.request_id,
                },
                generating: input.generating,
                exit_target: None,
            },
            inspection_id: input.inspection_id,
            target_id: input.target_id,
            library_id: input.library_id,
        })
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Staged {
    target_id: String,
    library_id: String,
    staging_id: String,
    receive_id: String,
}
#[tauri::command]
pub(crate) async fn external_lww_stage_binding(
    app: AppHandle,
    request: StageRequest,
) -> Result<Staged> {
    logged("external_lww_stage_binding", async move {
        let context = context(&request.request.connection_id)?;
        let mut session = context.session.lock().await;
        open(&app, &request.request.connection_id, &mut session).await?;
        let mut store = check(&app, &request.request, false)?;
        let engine = session.engine.as_ref().ok_or_else(lww_segment::corrupt)?;
        if request.library_id != engine.library
            || request.target_id != engine.repository.connection_identity
        {
            return Err(lww_segment::corrupt());
        }
        let stage = engine
            .stage_binding(
                &mut store,
                &request.request.header,
                &request.inspection_id,
                &session.cancel,
            )
            .await?;
        Ok(Staged {
            target_id: request.target_id,
            library_id: request.library_id,
            staging_id: stage.staging_id,
            receive_id: request.request.header.request_id,
        })
    }.await)
}
#[tauri::command]
pub(crate) async fn external_lww_publish(
    app: AppHandle,
    request: Request,
) -> Result<PublicationResult> {
    logged("external_lww_publish", async move {
        let state = app.state::<crate::persistent_store::PersistentStoreState>();
        let _operation = state.admit_renderer_operation().map_err(runtime::local_error)?;
        let context = context(&request.connection_id)?;
        let mut session = context.session.lock().await;
        open(&app, &request.connection_id, &mut session).await?;
        let mut store = state.open_admitted_native_job_store(&_operation).map_err(runtime::local_error)?;
        check_store(&store, &request, true)?;
        if let Some(target) = &request.exit_target {
            let identity = store.external_identity().map_err(runtime::local_error)?;
            if identity.library_epoch != target.library_epoch
                || identity.selection_epoch != target.selection_epoch
                || u64::try_from(identity.revision).ok() != Some(target.revision.0)
            {
                return Err(ProviderError::new(ErrorKind::PreconditionFailed));
            }
        }
        if request.exit_target.is_none() {
            store
                .lww_finish_initial_publication(&request.header)
                .map_err(runtime::local_error)?;
        }
        let Session { engine, cancel, .. } = &mut *session;
        let result = engine
            .as_mut()
            .ok_or_else(lww_segment::corrupt)?
            .publish(
                &mut store,
                request.header.binding_authority,
                &request.generating,
                cancel,
            )
            .await?;
        if result.segments.0 > 0 {
            context.turn.lock().map_err(runtime::local_error)?.published(Instant::now());
        }
        if let Some(target) = &request.exit_target {
            let identity = store.external_identity().map_err(runtime::local_error)?;
            if identity.library_epoch != target.library_epoch
                || identity.selection_epoch != target.selection_epoch
                || u64::try_from(identity.revision).ok() != Some(target.revision.0)
                || !store
                    .lww_read_outbox(request.header.binding_authority, 1)
                    .map_err(runtime::local_error)?
                    .entries
                    .is_empty()
            {
                return Err(ProviderError::new(ErrorKind::PreconditionFailed));
            }
        }
        Ok(result)
    }.await)
}
#[tauri::command]
pub(crate) async fn external_lww_receive(
    app: AppHandle,
    request: Request,
) -> Result<Option<StageReceive>> {
    logged("external_lww_receive", async move {
        let context = context(&request.connection_id)?;
        let mut session = context.session.lock().await;
        open(&app, &request.connection_id, &mut session).await?;
        let mut store = check(&app, &request, true)?;
        let authority = request.header.binding_authority;
        // One page per call: pages from the last listing come first, and a
        // new listing runs only once every one of them is finished.
        let mut page = None;
        while let Some(id) = session.receive_pages.front() {
            match store.external_lww_unfinished_receive(id).map_err(runtime::local_error)? {
                Some(stored) if stored.header.binding_authority == authority => {
                    page = Some(stored);
                    break;
                }
                Some(_) => session.receive_pages.clear(),
                None => {
                    session.receive_pages.pop_front();
                }
            }
        }
        if page.is_none() {
            let pages = session
                .engine
                .as_ref()
                .ok_or_else(lww_segment::corrupt)?
                .receive_requests_cached(&mut store, authority, &context.checkpoints, &session.cancel)
                .await?;
            session.receive_pages = pages.iter().map(|page| page.header.request_id.clone()).collect();
            page = pages.into_iter().next();
        }
        let own = store.lww_clock_state().map_err(runtime::local_error)?.writer_id;
        if page.as_ref().is_some_and(|page| page.progress.writer_id.as_deref() != Some(own.as_str())) {
            context.turn.lock().map_err(runtime::local_error)?.observed_foreign(Instant::now());
        }
        Ok(page)
    }.await)
}

#[tauri::command]
pub(crate) async fn external_lww_maintenance(app:AppHandle,request:Request)->Result<Option<serde_json::Value>> {
    logged("external_lww_maintenance", async move {
        let context=context(&request.connection_id)?;
        let Ok(mut session)=context.maintenance.try_lock() else {return Ok(None)};
        let cancel=context.cancel.lock().map_err(runtime::local_error)?.clone();
        let result=maintain(&app,&request,&context,&mut session,&cancel).await;
        if result.is_err() {
            session.engine=None;
            session.dependencies=None;
        }
        result
    }.await)
}
/// One maintenance tick on the engine kept from earlier ticks. A replaced
/// cancellation or an expired clock admission takes a fresh clock sample, and
/// a changed connection or any failure connects again.
async fn maintain(app:&AppHandle,request:&Request,context:&Context,session:&mut Session,cancel:&Cancellation)->Result<Option<serde_json::Value>> {
    let root=runtime::root(app)?;
    let stored=super::connection_store::ConnectionStore::open(&root)?.read(&request.connection_id)?;
    if session.engine.as_ref().is_some_and(|engine|engine.descriptor!=stored.descriptor) {
        session.engine=None;
        session.dependencies=None;
    }
    if !session.cancel.same(cancel) || session.engine.as_ref().is_some_and(|engine|engine.admitted_upper().is_err()) {
        if let Some(engine)=session.engine.as_mut() {engine.invalidate_clock();}
        session.fresh_after=Instant::now();
    }
    session.cancel=cancel.clone();
    open(app,&request.connection_id,session).await?;
    let store=check(app,request,true)?;
    let writer=store.lww_clock_state().map_err(runtime::local_error)?.writer_id;
    let engine=session.engine.as_ref().ok_or_else(lww_segment::corrupt)?;
    let due=engine.maintenance_needed_cached(&context.checkpoints,cancel).await?;
    let compact=context.turn.lock().map_err(runtime::local_error)?.ready(due,Instant::now());
    let job=uuid::Uuid::new_v4().to_string();
    let protection=super::leases::LeaseContext{root:&root,connection_id:&request.connection_id,writer_id:&writer,descriptor:&stored.descriptor,root_key:&engine.root_key,provider:engine.provider.as_ref(),repository:&engine.repository,clock:super::leases::system_clock(),protection_supported:stored.capabilities.lease_operations,ledger:Some(app.state::<super::job_store::JobCommandState>().lease_ledger()?)};
    let snapshot_id=if compact {
        let owner=match super::leases::admit_shared_work(&protection,&job,cancel).await? {
            super::leases::Admission::Admitted(owner)=>owner,
            super::leases::Admission::Yield{..}=>return Ok(None),
            super::leases::Admission::UnsupportedProtection=>return Err(ProviderError::new(ErrorKind::Unsupported)),
        };
        let directory=root.join("external-storage").join("maintenance").join(&job);
        let result=owner.run(&protection,cancel,async {
            if let Some(reason)=owner.recheck(&protection,cancel).await? {return Err(super::leases::yield_error(reason));}
            check(app,request,true)?;
            let completed=engine.compact_published(&directory,&job,&writer,&stored.capabilities,cancel,Some((&owner,&protection))).await?;
            check(app,request,true)?;
            Ok(completed)
        }).await?;
        Some(result.snapshot_id)
    } else {None};
    check(app,request,true)?;
    let last_run=super::gc_store::GcStore::open(&root)?.last_run_ms(&request.connection_id)?;
    if !super::cleanup::lww_cleanup_due(snapshot_id.is_some(),last_run,runtime::now_ms()) {
        return Ok(Some(serde_json::json!({"snapshotId":snapshot_id,"cleanup":null})));
    }
    let connected=connection_commands::open_connected_with_cancel(app,&request.connection_id,cancel).await?;
    if connected.handle.repository_id!=engine.repository.repository_id || connected.handle.connection_identity!=engine.repository.connection_identity || connected.stored.descriptor.repository_id!=engine.library {return Err(lww_segment::corrupt())}
    let cleanup=runtime::run_connected_cleanup(app,&connected,&request.connection_id,&job,Some(engine),cancel).await?;
    check(app,request,true)?;
    Ok(Some(serde_json::json!({"snapshotId":snapshot_id,"cleanup":cleanup})))
}
#[tauri::command]
pub(crate) async fn external_lww_fence(
    app: AppHandle,
    connection_id: String,
    new_device: bool,
) -> Result<()> {
    logged("external_lww_fence", async move {
        let context = context(&connection_id)?;
        context
            .cancel
            .lock()
            .map_err(runtime::local_error)?
            .cancel();
        let mut session = context.session.lock().await;
        session.cancel = Cancellation::default();
        let fenced = fence(&app, &connection_id, &mut session, new_device).await;
        session.cancel.cancel();
        session.receive_pages.clear();
        if let Some(engine) = session.engine.as_mut() {
            engine.invalidate_clock()
        }
        session.fresh_after = Instant::now();
        fenced
    }.await)
}
/// Asks the repository about a sent segment only when one awaits an answer,
/// so a binding change away from a repository that cannot be reached goes
/// ahead without it.
async fn fence(app: &AppHandle, id: &str, session: &mut Session, new_device: bool) -> Result<()> {
    let mut store = runtime::native_store(app)?;
    if !store
        .external_lww_unconfirmed_dispatch()
        .map_err(runtime::local_error)?
    {
        return Ok(());
    }
    if let Err(error) = connect(app, id, session).await {
        return ExternalLwwEngine::fence_outcome(error, new_device);
    }
    session
        .engine
        .as_ref()
        .ok_or_else(lww_segment::corrupt)?
        .fence_binding_change(&mut store, new_device, &session.cancel)
        .await
}
#[tauri::command]
pub(crate) async fn external_lww_resume(connection_id: String) -> Result<()> {
    logged("external_lww_resume", async move {
        let context = context(&connection_id)?;
        let mut session = context.session.lock().await;
        let cancel = Cancellation::default();
        *context.cancel.lock().map_err(runtime::local_error)? = cancel.clone();
        session.cancel = cancel;
        session.fresh_after = Instant::now();
        session.receive_pages.clear();
        if let Some(engine) = session.engine.as_mut() {
            engine.invalidate_clock()
        }
        Ok(())
    }.await)
}
pub(crate) struct NewDeviceRequest {
    request: Request,
    staging_id: String,
}
impl<'de> Deserialize<'de> for NewDeviceRequest {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Input {
            connection_id: String,
            binding_authority: risunest_sync_wire::stamp::DecimalU64,
            request_id: String,
            #[serde(default)]
            generating: Vec<MessageLocator>,
            staging_id: String,
        }
        let input = Input::deserialize(deserializer)?;
        Ok(Self {
            request: Request {
                connection_id: input.connection_id,
                header: Header {
                    binding_authority: input.binding_authority,
                    request_id: input.request_id,
                },
                generating: input.generating,
                exit_target: None,
            },
            staging_id: input.staging_id,
        })
    }
}
#[tauri::command]
pub(crate) async fn external_lww_prepare_new_device(
    app: AppHandle,
    request: NewDeviceRequest,
) -> Result<NewDevicePreparation> {
    logged("external_lww_prepare_new_device", async move {
        let context = context(&request.request.connection_id)?;
        let mut session = context.session.lock().await;
        // Acquiring this lock settles every task using the old cancellation token.
        session.cancel = Cancellation::default();
        open(&app, &request.request.connection_id, &mut session).await?;
        let mut store = check(&app, &request.request, false)?;
        session
            .engine
            .as_ref()
            .ok_or_else(lww_segment::corrupt)?
            .prepare_new_device(
                &mut store,
                &request.request.header,
                &request.staging_id,
                &session.cancel,
            )
            .await
    }.await)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_commands_log_their_own_failures() {
        use crate::external_storage::contract::{ConnectionConfig, ErrorKind};
        let resumed = tauri::async_runtime::block_on(external_lww_resume(String::new()));
        assert_eq!(resumed.unwrap_err().kind, ErrorKind::Corrupt);
        let config = ConnectionConfig {
            provider: "synthetic-provider".into(),
            profile: None,
            endpoint: String::new(),
            account_id: String::new(),
            location: Default::default(),
            oauth_profile: None,
        };
        let validated = super::super::connection_commands::external_storage_validate_sync_root(config);
        assert_eq!(validated.unwrap_err().kind, ErrorKind::Unsupported);
        for (command, code, file) in [
            ("external_lww_resume", "corrupt", "lww_commands.rs:"),
            ("external_storage_validate_sync_root", "unsupported", "connection_commands.rs:"),
        ] {
            let entry = crate::native_log::global_state()
                .tail(None)
                .into_iter()
                .rev()
                .find(|entry| entry.message.starts_with(&format!("{command} failed: code={code} at=")))
                .unwrap_or_else(|| panic!("{command} logs its failure"));
            assert_eq!((entry.level.as_str(), entry.target.as_str()), ("error", "native-command"));
            assert!(entry.message.contains(file), "{}", entry.message);
        }
    }
    #[test]
    fn renderer_command_envelopes_deserialize_and_reject_unknown_fields() {
        let base = serde_json::json!({"connectionId":"sync","bindingAuthority":"4","requestId":"request","generating":[]});
        assert!(serde_json::from_value::<Request>(base.clone()).is_ok());
        let mut stage = base.clone();
        stage["inspectionId"] = serde_json::json!("inspection");
        stage["targetId"] = serde_json::json!("target");
        stage["libraryId"] = serde_json::json!("library");
        assert!(serde_json::from_value::<StageRequest>(stage.clone()).is_ok());
        let mut invalid_stage = stage.clone();
        invalid_stage["inspectionId"] = serde_json::json!(4);
        assert!(serde_json::from_value::<StageRequest>(invalid_stage).is_err());
        stage["rendererSettled"] = serde_json::json!(true);
        assert!(serde_json::from_value::<StageRequest>(stage).is_err());
        let mut prepare = base.clone();
        prepare["stagingId"] = serde_json::json!("stage");
        assert!(serde_json::from_value::<NewDeviceRequest>(prepare.clone()).is_ok());
        let mut invalid_prepare = prepare.clone();
        invalid_prepare["bindingAuthority"] = serde_json::json!(4);
        assert!(serde_json::from_value::<NewDeviceRequest>(invalid_prepare).is_err());
        prepare["rendererSettled"] = serde_json::json!(true);
        assert!(serde_json::from_value::<NewDeviceRequest>(prepare).is_err());
        let mut exit = base.clone();
        exit["exitTarget"] = serde_json::json!({"revision":"12","libraryEpoch":"epoch","selectionEpoch":"selection"});
        assert!(serde_json::from_value::<Request>(exit).is_ok());
        for (field, value) in [
            ("bindingAuthority", serde_json::json!(4)),
            ("generating", serde_json::json!("conversation")),
            ("requestId", serde_json::json!(4)),
        ] {
            let mut invalid = base.clone();
            invalid[field] = value;
            assert!(
                serde_json::from_value::<Request>(invalid).is_err(),
                "{field}"
            );
        }
        let mut invalid = base;
        invalid["rendererSettled"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Request>(invalid).is_err());
    }
}
