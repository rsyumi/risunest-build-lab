use super::{
    credentials::StoredConfig,
    lww_client::{LwwClient, OperationLog},
    Result, SyncError,
};
use crate::persistent_store::{
    lww::{ApplyReceive, Header, NewDevicePreparation, Progress, StageReceive},
    sync_selection::SyncTarget,
    PersistentStore,
};
use risunest_sync_wire::{
    lww::{AckRequest, NewDeviceClaimReceipt, NewDeviceClaimRequest, StatePage, StatePin},
    stamp::DecimalU64,
    MAX_METADATA_BYTES,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Inspection {
    pub inspection_id: String,
    pub target_id: String,
    pub library_id: String,
    pub empty: bool,
    pub previously_bound_library: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct VerifiedTarget {
    config: StoredConfig,
    target_id: String,
    epoch: String,
    authority: DecimalU64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StagedTarget {
    target_id: String,
    library_id: String,
    staging_id: String,
    receive_id: String,
}
#[derive(Serialize, Deserialize)]
struct VerifiedStage {
    target: VerifiedTarget,
    cursor: DecimalU64,
    staging_id: String,
    receive_id: String,
}
#[derive(Serialize, Deserialize)]
struct VerifiedClaim {
    stage: VerifiedStage,
    receipt: NewDeviceClaimReceipt,
}

fn candidate(store: &PersistentStore) -> Result<LwwClient> {
    let log = OperationLog::open(store.repository_root())?;
    let config = log
        .config("candidate")?
        .or(store.server_stored_config()?)
        .ok_or_else(|| SyncError::new("server-unconfigured", 409))?;
    let mut core = LwwClient::new(
        store.repository_root(),
        config.resolve(store.repository_root())?,
    )?;
    core.access = Some(config);
    Ok(core)
}
fn assert_authority(store: &PersistentStore, header: &Header) -> Result<()> {
    if store.lww_binding_authority()? != header.binding_authority {
        return Err(SyncError::new("binding-authority-changed", 409));
    }
    Ok(())
}
pub(crate) fn inspect(store: &PersistentStore, header: &Header) -> Result<Inspection> {
    assert_authority(store, header)?;
    let core = candidate(store)?;
    core.admission()?;
    let head = core.client.resolve_identity(false)?;
    let (_, pin): (_, StatePin) = core.client.json(
        reqwest::Method::POST,
        "state/pins",
        &[],
        None::<&AckRequest>,
        &[],
    )?;
    let page = core.client.json::<StatePage>(
        reqwest::Method::GET,
        "state",
        &[("pin", pin.pin_id.clone()), ("limit", "1".into())],
        None::<&AckRequest>,
        &[],
    );
    let _ = core.client.request(
        reqwest::Method::DELETE,
        &format!("state/pins/{}", pin.pin_id),
        &[],
        None,
        &[],
        MAX_METADATA_BYTES,
    );
    let (_, page) = page?;
    if page.pin_id != pin.pin_id || page.start_seq != pin.start_seq {
        return Err(SyncError::new("invalid-state-page", 502));
    }
    let config = core
        .access
        .ok_or_else(|| SyncError::new("server-unconfigured", 409))?;
    #[cfg(test)]
    super::hash_metrics::record(
        "c_binding_target_identity",
        format!("{}:{}:{}", config.endpoint, config.library_id, head.epoch).len(),
    );
    let target_id = risunest_sync_wire::hash(
        format!("{}:{}:{}", config.endpoint, config.library_id, head.epoch).as_bytes(),
    );
    let state = store.lww_binding_state()?;
    let previously_bound_library = state.library_id.as_deref() == Some(&config.library_id)
        && core.log.config("active")?.is_some_and(|old| {
            old.endpoint == config.endpoint && old.library_id == config.library_id
        })
        && core
            .log
            .verified::<VerifiedTarget>("bindings", "active")
            .is_ok_and(|old| old.target_id == target_id);
    let id = store.register_lww_binding_inspection(
        header.binding_authority,
        &SyncTarget::Server("server".into()),
        &target_id,
        &config.library_id,
    )?;
    core.log.save_verified(
        "bindings",
        &id,
        &VerifiedTarget {
            config: config.clone(),
            target_id: target_id.clone(),
            epoch: head.epoch,
            authority: header.binding_authority,
        },
    )?;
    core.log.0.execute("INSERT INTO bindings VALUES('selected',?1) ON CONFLICT(id) DO UPDATE SET body=excluded.body",[serde_json::to_string(&VerifiedTarget{config:config.clone(),target_id:target_id.clone(),epoch:core.log.verified::<VerifiedTarget>("bindings",&id)?.epoch,authority:header.binding_authority}).map_err(|_|SyncError::new("binding-integrity",409))?])?;
    Ok(Inspection {
        inspection_id: id,
        target_id,
        library_id: config.library_id,
        empty: page.items.is_empty(),
        previously_bound_library,
    })
}
pub(crate) fn stage(
    store: &mut PersistentStore,
    header: &Header,
    inspection_id: &str,
) -> Result<StagedTarget> {
    assert_authority(store, header)?;
    let log = OperationLog::open(store.repository_root())?;
    let target: VerifiedTarget = log.verified("bindings", inspection_id)?;
    if target.authority != header.binding_authority {
        return Err(SyncError::new("binding-authority-changed", 409));
    }
    let mut core = LwwClient::new(
        store.repository_root(),
        target.config.resolve(store.repository_root())?,
    )?;
    core.access = Some(target.config.clone());
    if core.client.resolve_identity(false)?.epoch != target.epoch {
        return Err(SyncError::new("server-epoch-changed", 409));
    }
    let upper = core.admission()?;
    let (cursor, changes) = core.state(store, upper)?;
    if core.client.resolve_identity(false)?.epoch != target.epoch {
        return Err(SyncError::new("server-epoch-changed", 409));
    }
    assert_authority(store, header)?;
    let staged = store.lww_stage_binding_units(header, inspection_id, &changes, upper)?;
    let result = StagedTarget {
        target_id: target.target_id.clone(),
        library_id: target.config.library_id.clone(),
        staging_id: staged.staging_id.clone(),
        receive_id: header.request_id.clone(),
    };
    let stage = VerifiedStage {
        target,
        cursor,
        staging_id: staged.staging_id.clone(),
        receive_id: header.request_id.clone(),
    };
    log.save_verified("bindings", &staged.staging_id, &stage)?;
    log.0.execute("INSERT INTO bindings VALUES('selected-stage',?1) ON CONFLICT(id) DO UPDATE SET body=excluded.body",[serde_json::to_string(&stage).map_err(|_|SyncError::new("binding-integrity",409))?])?;
    Ok(result)
}
pub(crate) fn prepare_new_device(
    store: &mut PersistentStore,
    header: &Header,
    staging_id: &str,
) -> Result<NewDevicePreparation> {
    assert_authority(store, header)?;
    let log = OperationLog::open(store.repository_root())?;
    let stage: VerifiedStage = log.verified("bindings", staging_id)?;
    if stage.target.authority != header.binding_authority || stage.receive_id != header.request_id {
        return Err(SyncError::new("binding-stage-integrity", 409));
    }
    let old = store.server_stored_config()?;
    if let Some(old) = &old {
        LwwClient::new(
            store.repository_root(),
            old.resolve(store.repository_root())?,
        )?
        .fence_new_device(store)?;
    }
    let preparation = store.prepare_lww_new_device(header, staging_id)?;
    let core = LwwClient::new(
        store.repository_root(),
        stage.target.config.resolve(store.repository_root())?,
    )?;
    let same = old.as_ref().is_some_and(|old| {
        old.library_id == stage.target.config.library_id
            && old.endpoint == stage.target.config.endpoint
    });
    let former_token = if same {
        Some(
            old.as_ref()
                .unwrap()
                .resolve(store.repository_root())?
                .token,
        )
    } else {
        None
    };
    let (_, receipt): (_, NewDeviceClaimReceipt) = core.client.json(
        reqwest::Method::POST,
        "session/claim-writer",
        &[],
        Some(&NewDeviceClaimRequest {
            writer_id: preparation.writer_id.clone(),
            authorization_id: preparation.authorization_id.clone(),
            former_token,
        }),
        &[],
    )?;
    if receipt.authorization_id != preparation.authorization_id
        || receipt.writer_id != preparation.writer_id
        || receipt.device_id != stage.target.config.device_id
        || receipt.library_id != stage.target.config.library_id
        || receipt.epoch != stage.target.epoch
        || (same && !receipt.former_credential_inactive)
    {
        return Err(SyncError::new("new-device-registration-integrity", 409));
    }
    log.save_verified(
        "claims",
        &preparation.authorization_id,
        &VerifiedClaim { stage, receipt },
    )?;
    if let Some(old) = &old {
        if same {
            core.detach_inactive(old, &preparation.authorization_id)?;
        } else {
            core.fence(store)?;
        }
    }
    store.authorize_lww_new_device(&preparation.authorization_id)?;
    Ok(preparation)
}
pub(crate) fn activate(
    store: &mut PersistentStore,
    header: &Header,
    authorization: Option<(&str, &str)>,
) -> Result<()> {
    assert_authority(store, header)?;
    let state = store.lww_binding_state()?;
    if state.target != SyncTarget::Server("server".into()) {
        return Err(SyncError::new("binding-target-changed", 409));
    }
    let log = OperationLog::open(store.repository_root())?;
    let (target, stage) = if let Some((authorization_id, writer_id)) = authorization {
        let claim: VerifiedClaim = log.verified("claims", authorization_id)?;
        if claim.receipt.writer_id != writer_id
            || store.lww_clock_state()?.writer_id != writer_id
            || claim.stage.target.authority.0.checked_add(1) != Some(header.binding_authority.0)
        {
            return Err(SyncError::new("new-device-registration-integrity", 409));
        }
        (claim.stage.target.clone(), Some(claim.stage))
    } else {
        let active = log.verified::<VerifiedTarget>("bindings", "active").ok();
        let selected = log.verified::<VerifiedTarget>("bindings", "selected").ok();
        let target = selected
            .filter(|target| {
                state.library_id.as_deref() == Some(&target.config.library_id)
                    && (target.authority.0.checked_add(1) == Some(header.binding_authority.0)
                        || active
                            .as_ref()
                            .is_some_and(|active| active.target_id == target.target_id))
            })
            .or(active)
            .ok_or_else(|| SyncError::new("binding-integrity", 409))?;
        let stage = log
            .verified::<VerifiedStage>("bindings", "selected-stage")
            .ok()
            .filter(|stage| {
                stage.target.target_id == target.target_id
                    && stage.target.authority.0.checked_add(1) == Some(header.binding_authority.0)
            });
        (target, stage)
    };
    if state.library_id.as_deref() != Some(&target.config.library_id) {
        return Err(SyncError::new("binding-library-mismatch", 409));
    }
    let mut core = LwwClient::new(
        store.repository_root(),
        target.config.resolve(store.repository_root())?,
    )?;
    core.access = Some(target.config.clone());
    if core.client.resolve_identity(false)?.epoch != target.epoch {
        return Err(SyncError::new("server-epoch-changed", 409));
    }
    store.server_save_config(&target.config)?;
    log.save_config("active", &target.config)?;
    log.0.execute(
        "INSERT INTO bindings VALUES('active',?1) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
        [serde_json::to_string(&target).map_err(|_| SyncError::new("binding-integrity", 409))?],
    )?;
    if let Some(stage) = stage.filter(|stage| {
        store
            .lww_receive_progress(header.binding_authority)
            .is_ok_and(|progress| {
                !progress
                    .iter()
                    .any(|progress| progress.kind == "server" && progress.cursor >= stage.cursor)
            })
    }) {
        let receive = StageReceive {
            header: Header {
                binding_authority: header.binding_authority,
                request_id: format!("binding-{}", stage.receive_id),
            },
            changes: vec![],
            progress: Progress {
                kind: "server".into(),
                cursor: stage.cursor,
                writer_id: None,
            },
            admitted_time_upper_ms: core.admission()?,
        };
        store.lww_stage_receive(&receive)?;
        store.lww_apply_receive(&ApplyReceive {
            header: receive.header.clone(),
            generating: vec![],
        })?;
        store.lww_finish_receive(&receive.header)?;
        core.acknowledge_cursor(stage.cursor)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) struct NativeBindingCompletion {
    pub activation: crate::persistent_store::RevisionResult,
    pub activation_request: crate::persistent_store::sync_selection::ReplaceBindingRequest,
    pub state: crate::persistent_store::sync_selection::BindingState,
    pub staging_id: String,
    pub receive_id: String,
}
#[cfg(test)]
pub(crate) fn first_binding_cycle(
    store: &mut PersistentStore,
    counters: std::sync::Arc<super::client::TestIoCounters>,
    database_activated: impl FnOnce(&crate::persistent_store::RevisionResult),
) -> Result<NativeBindingCompletion> {
    let root = store.repository_root().to_owned();
    super::client::with_test_io(&root, counters, || {
        let original = store.lww_binding_state()?;
        if original.target != SyncTarget::None { return Err(SyncError::new("fixture-first-binding-required",409)); }
        let inspected = inspect(store, &super::lww_tests::header(store))?;
        if inspected.previously_bound_library { return Err(SyncError::new("fixture-first-binding-required",409)); }
        if inspected.empty { return Err(SyncError::new("fixture-nonempty-target-required",409)); }
        let staged = stage(store, &super::lww_tests::header(store), &inspected.inspection_id)?;
        let next = store.switch_lww_binding(&crate::persistent_store::sync_selection::SwitchBindingRequest {
            header: super::lww_tests::header(store),
            expected_selection_epoch: original.selection_epoch,
            target: SyncTarget::Server("server".into()),
            inspection_id: Some(inspected.inspection_id),
        })?;
        let activation_request = crate::persistent_store::sync_selection::ReplaceBindingRequest {
            header: Header { binding_authority: next.target_authority, request_id: staged.receive_id.clone() },
            expected_selection_epoch: next.selection_epoch,
            staging_id: staged.staging_id.clone(), receive_id: staged.receive_id.clone(),
            target_id: staged.target_id, library_id: staged.library_id,
        };
        let activation = store.replace_lww_binding(&activation_request)?;
        database_activated(&activation);
        activate(store, &super::lww_tests::header(store), None)?;
        Ok(NativeBindingCompletion { activation, activation_request, state: store.lww_binding_state()?, staging_id: staged.staging_id, receive_id: staged.receive_id })
    })
}

#[cfg(test)]
pub(crate) fn hydrate_binding_bodies(
    store: &PersistentStore,
    counters: std::sync::Arc<super::client::TestIoCounters>,
    selected_character_id: Option<&str>,
    on_object_done: impl Fn(),
) -> Result<()> {
    if store.server_asset_policy()? != super::residency::AssetPolicy::Full { return Err(SyncError::new("fixture-full-asset-policy-required",409)); }
    super::client::with_test_io(store.repository_root(), counters, || super::commands::hydrate_binding_assets(
        store, &super::lww_tests::header(store), std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)), selected_character_id, on_object_done,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server_sync::lww_tests::{header, local, save, LocalServerFixture};
    #[test]
    fn fixture_first_binding_has_durable_activation_and_a_separate_real_hydration_barrier() {
        use crate::asset_repository::{PayloadCas, body_io::{register_object_purpose,reset_body_io,take_body_io,BodyPurpose}};
        let server = LocalServerFixture::new();
        let (_source_root, mut source) = local(); let (target_root, mut target) = local();
        let sender = server.client(&source);
        save(&mut source, &["root","language"], serde_json::json!("ja"));
        let hash = crate::server_sync::lww_tests::put_asset(&mut source, "assets/first-binding.png", &vec![91;200_000]).object_hash.unwrap();
        crate::server_sync::lww_tests::drain_publications(&sender,&mut source,&[]).unwrap();
        server.prepare_binding_candidate(&target);
        let counters = std::sync::Arc::new(super::super::client::TestIoCounters::default());
        let activated = std::cell::Cell::new(None);
        let completion = first_binding_cycle(&mut target,counters.clone(),|receipt| activated.set(Some(receipt.revision))).unwrap();
        assert_eq!(activated.get(),Some(completion.activation.revision));
        let current_revision = target.revision().unwrap();
        let replay = target.replace_lww_binding(&completion.activation_request).unwrap();
        assert_eq!(replay.revision,completion.activation.revision);
        assert_eq!(target.revision().unwrap(),current_revision);
        assert!(!completion.staging_id.is_empty()); assert!(!completion.receive_id.is_empty());
        assert!(counters.snapshot()[0] > 0);
        assert_eq!(target.read_root(None).unwrap().value["language"],"ja");
        let cas = PayloadCas::new(target.repository_root()).unwrap();
        assert!(cas.stat_object(&hash).unwrap().is_none());
        assert!(super::super::residency::Residency::open(target.repository_root()).unwrap().object(&hash,None).unwrap().is_some());
        counters.reset();
        hydrate_binding_bodies(&target,counters.clone(),None,||{}).unwrap();
        assert!(cas.stat_object(&hash).unwrap().is_some()); assert!(counters.snapshot()[0]>0);
        counters.reset(); reset_body_io(); register_object_purpose(&hash,BodyPurpose::Asset);
        hydrate_binding_bodies(&target,counters.clone(),None,||{}).unwrap();
        let work=take_body_io(); assert!(work.complete()); assert!(work.domains.is_empty());
        assert_eq!(counters.snapshot(),[0;3]);
        let bound=completion.state;
        drop(target);
        let mut target=PersistentStore::open(target_root.path()).unwrap();
        assert_eq!(target.lww_binding_state().unwrap().target_authority,bound.target_authority);
        let receiver=LocalServerFixture::reopen_client(&target,counters.clone()).unwrap();
        assert!(std::sync::Arc::ptr_eq(receiver.client.test_io.as_ref().unwrap(),&counters));
        save(&mut source,&["root","language"],serde_json::json!("ko"));
        crate::server_sync::lww_tests::drain_publications(&sender,&mut source,&[]).unwrap();
        crate::server_sync::lww_tests::receive_available(&receiver,&mut target,&[]).unwrap();
        assert_eq!(target.read_root(None).unwrap().value["language"],"ko");
    }
    fn configure(
        server: &LocalServerFixture,
        store: &PersistentStore,
    ) -> super::super::client::ServerConfig {
        let (config, stored) = server.candidate(store);
        OperationLog::open(store.repository_root())
            .unwrap()
            .save_config("candidate", &stored)
            .unwrap();
        config
    }
    fn bind(store: &mut PersistentStore) -> Header {
        let original = store.lww_binding_state().unwrap();
        let request = header(store);
        let inspected = inspect(store, &request).unwrap();
        let state = store
            .switch_lww_binding(
                &crate::persistent_store::sync_selection::SwitchBindingRequest {
                    header: request,
                    expected_selection_epoch: original.selection_epoch,
                    target: SyncTarget::Server("server".into()),
                    inspection_id: Some(inspected.inspection_id),
                },
            )
            .unwrap();
        let request = Header {
            binding_authority: state.target_authority,
            request_id: uuid::Uuid::new_v4().to_string(),
        };
        activate(store, &request, None).unwrap();
        request
    }
    #[test]
    fn first_binding_is_database_first_and_remote_assets_are_held_before_activation() {
        let server = LocalServerFixture::new();
        let (_a, mut source) = local();
        let (_b, mut target) = local();
        let core = server.client(&source);
        save(&mut source, &["root", "language"], serde_json::json!("ja"));
        let alias = crate::server_sync::lww_tests::put_asset(
            &mut source,
            "assets/staged.png",
            &vec![31; 200_000],
        );
        let request = header(&source);
        core.push(&mut source, &request, &[]).unwrap();
        configure(&server, &target);
        save(
            &mut target,
            &["root", "language"],
            serde_json::json!("local"),
        );
        let original = target.lww_binding_state().unwrap();
        let request = header(&target);
        let inspected = inspect(&target, &request).unwrap();
        let request = header(&target);
        let staged = stage(&mut target, &request, &inspected.inspection_id).unwrap();
        let hash = alias.object_hash.unwrap();
        assert_eq!(target.read_root(None).unwrap().value["language"], "local");
        assert!(
            super::super::residency::Residency::open(target.repository_root())
                .unwrap()
                .object(&hash, None)
                .unwrap()
                .is_some()
        );
        assert!(
            crate::asset_repository::PayloadCas::new(target.repository_root())
                .unwrap()
                .stat_object(&hash)
                .unwrap()
                .is_none()
        );
        let next = target
            .switch_lww_binding(
                &crate::persistent_store::sync_selection::SwitchBindingRequest {
                    header: header(&target),
                    expected_selection_epoch: original.selection_epoch,
                    target: SyncTarget::Server("server".into()),
                    inspection_id: Some(inspected.inspection_id),
                },
            )
            .unwrap();
        target
            .replace_lww_binding(
                &crate::persistent_store::sync_selection::ReplaceBindingRequest {
                    header: Header {
                        binding_authority: next.target_authority,
                        request_id: staged.receive_id.clone(),
                    },
                    expected_selection_epoch: next.selection_epoch,
                    staging_id: staged.staging_id,
                    receive_id: staged.receive_id,
                    target_id: staged.target_id,
                    library_id: staged.library_id,
                },
            )
            .unwrap();
        let request = header(&target);
        activate(&mut target, &request, None).unwrap();
        assert_eq!(target.read_root(None).unwrap().value["language"], "ja");
        assert!(
            crate::asset_repository::PayloadCas::new(target.repository_root())
                .unwrap()
                .stat_object(&hash)
                .unwrap()
                .is_none()
        );
        assert!(target
            .lww_read_outbox(request.binding_authority, 1)
            .unwrap()
            .entries
            .is_empty());
    }
    #[test]
    fn fresh_claim_revokes_actual_old_registration_then_activates_reserved_writer() {
        let server = LocalServerFixture::new();
        let (_root, mut store) = local();
        let old = configure(&server, &store);
        let bound = bind(&mut store);
        let old_writer = store.lww_clock_state().unwrap().writer_id;
        save(&mut store, &["root", "language"], serde_json::json!("ja"));
        let mut core = LwwClient::new(store.repository_root(), old.clone()).unwrap();
        core.access = store.server_stored_config().unwrap();
        core.push(&mut store, &bound, &[]).unwrap();
        let fresh = configure(&server, &store);
        assert_eq!(
            store.server_stored_config().unwrap().unwrap().device_id,
            old.device_id
        );
        let request = header(&store);
        let inspected = inspect(&store, &request).unwrap();
        let request = header(&store);
        let staged = stage(&mut store, &request, &inspected.inspection_id).unwrap();
        let prepared = prepare_new_device(&mut store, &request, &staged.staging_id).unwrap();
        assert_eq!(store.lww_clock_state().unwrap().writer_id, old_writer);
        assert!(server
            .server
            .authenticate(&old.library_id, &old.token)
            .is_err());
        assert!(server
            .server
            .authenticate(&fresh.library_id, &fresh.token)
            .is_ok());
        let result = store
            .lww_replace_target_as_new_device(
                &request,
                &staged.staging_id,
                &prepared.authorization_id,
            )
            .unwrap();
        let next = Header {
            binding_authority: result.binding_authority,
            request_id: uuid::Uuid::new_v4().to_string(),
        };
        activate(
            &mut store,
            &next,
            Some((&prepared.authorization_id, &result.writer_id)),
        )
        .unwrap();
        assert_eq!(result.writer_id, prepared.writer_id);
        assert_ne!(result.writer_id, old_writer);
        assert_eq!(store.read_root(None).unwrap().value["language"], "ja");
        assert_eq!(
            store.server_stored_config().unwrap().unwrap().device_id,
            fresh.device_id
        );
        save(&mut store, &["root", "language"], serde_json::json!("ko"));
        assert!(LwwClient::new(store.repository_root(), fresh)
            .unwrap()
            .push(&mut store, &next, &[])
            .unwrap()
            .is_some());
    }
    #[test]
    fn revoked_unknown_original_operation_is_detached_only_by_explicit_verified_claim() {
        use risunest_sync_wire::lww::{PushRequest, UnitChange};
        let server = LocalServerFixture::new();
        let (_root, mut store) = local();
        let old = configure(&server, &store);
        let bound = bind(&mut store);
        save(
            &mut store,
            &["root", "language"],
            serde_json::json!("discarded"),
        );
        let core = LwwClient::new(store.repository_root(), old.clone()).unwrap();
        let entries = store
            .lww_read_outbox(bound.binding_authority, 256)
            .unwrap()
            .entries;
        let publication = super::super::lww_client::Publication {
            authority: bound.binding_authority,
            request: PushRequest {
                library_id: old.library_id.clone(),
                writer_id: store.lww_clock_state().unwrap().writer_id,
                operation_id: "unknown-before-restore".into(),
                changes: entries
                    .iter()
                    .map(|e| UnitChange {
                        key: e.key.clone(),
                        stamp: e.stamp.clone(),
                        value: e.value.clone(),
                    })
                    .collect(),
            },
            entries,
            config: store.server_stored_config().unwrap().unwrap(),
        };
        core.log.prepare(&publication).unwrap();
        server.server.revoke_device(&old.device_id).unwrap();
        assert_eq!(core.fence(&mut store).unwrap_err().status, 401);
        core.fence_new_device(&mut store).unwrap();
        assert_eq!(core.log.pending().unwrap().len(), 1);
        configure(&server, &store);
        let request = header(&store);
        let inspected = inspect(&store, &request).unwrap();
        let request = header(&store);
        let staged = stage(&mut store, &request, &inspected.inspection_id).unwrap();
        let preparation = prepare_new_device(&mut store, &request, &staged.staging_id).unwrap();
        assert!(core.log.pending().unwrap().is_empty());
        let receipt: String = core
            .log
            .0
            .query_row(
                "SELECT receipt FROM publications WHERE id='unknown-before-restore'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(receipt.contains("auth-inactive-detached") && receipt.contains("unknown"));
        assert!(!receipt.contains("rejected"));
        let result = store
            .lww_replace_target_as_new_device(
                &request,
                &staged.staging_id,
                &preparation.authorization_id,
            )
            .unwrap();
        assert!(store
            .read_root(None)
            .unwrap()
            .value
            .get("language")
            .is_none());
        assert!(store
            .lww_read_outbox(result.binding_authority, 256)
            .unwrap()
            .entries
            .is_empty());
    }
}
