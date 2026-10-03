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
    pub registration_changed: bool,
    pub server_restored: bool,
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
    let head = core.client.resolve_identity()?;
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
    let same_library =
        |old: &StoredConfig| old.endpoint == config.endpoint && old.library_id == config.library_id;
    let active = core.log.verified::<VerifiedTarget>("bindings", "active").ok();
    let previously_bound_library = store.lww_previously_bound(&config.library_id, &target_id)?
        && core.log.config("active")?.is_some_and(|old| same_library(&old))
        && active.as_ref().is_some_and(|old| old.target_id == target_id);
    let registration_changed = store
        .server_stored_config()?
        .is_some_and(|old| same_library(&old) && old.device_id != config.device_id);
    let server_restored = active
        .as_ref()
        .is_some_and(|old| same_library(&old.config) && old.epoch != head.epoch);
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
        registration_changed,
        server_restored,
    })
}
/// Moves the operation log with a binding switch. A retained binding keeps its pending
/// publications and receive position under the new authority; any other switch detaches
/// publications it never settled, leaving their acceptance unknown.
pub(crate) fn carry_operation_log(
    root: &std::path::Path,
    old: DecimalU64,
    new: DecimalU64,
    retain: bool,
) -> Result<()> {
    if !root.join("server-sync/lww-operations.sqlite").exists() {
        return Ok(());
    }
    let log = OperationLog::open(root)?;
    let tx = log.0.unchecked_transaction()?;
    let unacknowledged: Vec<(String, String, bool, String)> = {
        let mut query = tx.prepare(
            "SELECT id,intent,receipt IS NULL,digest FROM publications WHERE acknowledged=0",
        )?;
        let rows = query
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<std::result::Result<_, _>>()?;
        rows
    };
    for (id, intent, pending, digest) in unacknowledged {
        let mut publication: super::lww_client::Publication = serde_json::from_str(&intent)
            .map_err(|_| SyncError::new("publication-integrity", 409))?;
        if publication.authority != old {
            continue;
        }
        if retain {
            publication.authority = new;
            for entry in &mut publication.entries {
                entry.target_authority = new;
            }
            tx.execute(
                "UPDATE publications SET intent=?2 WHERE id=?1",
                rusqlite::params![
                    id,
                    serde_json::to_string(&publication)
                        .map_err(|_| SyncError::new("publication-encoding", 409))?
                ],
            )?;
        } else if pending {
            tx.execute(
                "UPDATE publications SET receipt=?2,acknowledged=1 WHERE id=?1 AND receipt IS NULL",
                rusqlite::params![
                    id,
                    serde_json::json!({"kind":"binding-switched","bodyDigest":digest,"historicalAcceptance":"unknown"}).to_string()
                ],
            )?;
        }
    }
    if retain {
        use rusqlite::OptionalExtension;
        let (old_text, new_text) = (old.0.to_string(), new.0.to_string());
        tx.execute(
            "UPDATE OR REPLACE bootstrap SET authority=?2 WHERE authority=?1",
            rusqlite::params![old_text, new_text],
        )?;
        let page: Option<String> = tx
            .query_row("SELECT body FROM receive_pages WHERE authority=?1", [&old_text], |r| r.get(0))
            .optional()?;
        if let Some(page) = page {
            let mut page: StageReceive = serde_json::from_str(&page)
                .map_err(|_| SyncError::new("receive-page-integrity", 409))?;
            page.header.binding_authority = new;
            tx.execute(
                "UPDATE OR REPLACE receive_pages SET authority=?2,body=?3 WHERE authority=?1",
                rusqlite::params![
                    old_text,
                    new_text,
                    serde_json::to_string(&page)
                        .map_err(|_| SyncError::new("receive-page-integrity", 409))?
                ],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}
/// Settles pending publications before a binding change. When a publication's server does not
/// answer, the switch proceeds and carries or detaches that publication instead of waiting.
pub(crate) fn fence_for_binding_change(core: &LwwClient, store: &mut PersistentStore) -> Result<()> {
    for publication in core.log.pending()? {
        if !answers(store.repository_root(), &publication)? {
            return Ok(());
        }
    }
    core.fence(store)
}
fn answers(root: &std::path::Path, publication: &super::lww_client::Publication) -> Result<bool> {
    let client = super::client::ServerClient::new(publication.config.resolve(root)?)?;
    let attempt = client.request_ambiguous_mutation(
        reqwest::Method::GET,
        &format!("operations/{}", publication.request.operation_id),
        None,
        &[],
        MAX_METADATA_BYTES,
    )?;
    Ok(match attempt {
        super::client::RequestAttempt::Response(reply) => !matches!(reply.status, 502..=504),
        super::client::RequestAttempt::Failure { error, .. } => {
            !super::client::is_ambiguous_transient(&error) && error.code != "directory-unreachable"
        }
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
    if core.client.resolve_identity()?.epoch != target.epoch {
        return Err(SyncError::new("server-epoch-changed", 409));
    }
    let upper = core.admission()?;
    let (cursor, changes) = core.state(store, upper)?;
    if core.client.resolve_identity()?.epoch != target.epoch {
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
#[cfg(test)]
std::thread_local! {
    static STOP_AFTER_FRESH_CLAIM: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FreshWriterReservation {
    authorization_id: String,
    writer_id: String,
    old_writer_id: String,
}
fn fresh_writer_record(kind: &str, config: &StoredConfig) -> String {
    format!("{kind}:{}:{}", config.library_id, config.device_id)
}
fn claim_record<T: serde::de::DeserializeOwned>(log: &OperationLog, id: &str) -> Result<Option<T>> {
    let exists: bool = log.0.query_row(
        "SELECT EXISTS(SELECT 1 FROM claims WHERE id=?1)",
        [id],
        |r| r.get(0),
    )?;
    if exists {
        log.verified("claims", id).map(Some)
    } else {
        Ok(None)
    }
}
/// The writer a registration claimed for this device, which activation requires before it
/// installs a changed registration to the same library.
fn claimed_fresh_writer(log: &OperationLog, target: &VerifiedTarget) -> Result<Option<String>> {
    Ok(claim_record::<NewDeviceClaimReceipt>(
        log,
        &fresh_writer_record("fresh-writer-claim", &target.config),
    )?
    .filter(|receipt| receipt.epoch == target.epoch)
    .map(|receipt| receipt.writer_id))
}
/// Keeps local data across a new registration to the library this device was bound to:
/// the old registration is settled and revoked, and a fresh writer claimed by the new one
/// replaces the old writer, so retained versions are published with their original stamps.
pub(crate) fn prepare_fresh_writer(
    store: &mut PersistentStore,
    header: &Header,
    inspection_id: &str,
) -> Result<NewDevicePreparation> {
    assert_authority(store, header)?;
    let root = store.repository_root().to_owned();
    let log = OperationLog::open(&root)?;
    let target: VerifiedTarget = log.verified("bindings", inspection_id)?;
    if target.authority != header.binding_authority {
        return Err(SyncError::new("binding-authority-changed", 409));
    }
    if !store.lww_previously_bound(&target.config.library_id, &target.target_id)? {
        return Err(SyncError::new("binding-target-changed", 409));
    }
    let old = store
        .server_stored_config()?
        .filter(|old| {
            old.endpoint == target.config.endpoint
                && old.library_id == target.config.library_id
                && old.device_id != target.config.device_id
        })
        .ok_or_else(|| SyncError::new("registration-unchanged", 409))?;
    LwwClient::new(&root, old.resolve(&root)?)?.fence_new_device(store)?;
    let reservation_id = fresh_writer_record("fresh-writer", &target.config);
    let reservation = match claim_record::<FreshWriterReservation>(&log, &reservation_id)? {
        Some(reservation) => reservation,
        None => {
            let reservation = FreshWriterReservation {
                authorization_id: uuid::Uuid::new_v4().to_string(),
                writer_id: uuid::Uuid::new_v4().to_string(),
                old_writer_id: store.lww_clock_state()?.writer_id,
            };
            log.save_verified("claims", &reservation_id, &reservation)?;
            reservation
        }
    };
    let core = LwwClient::new(&root, target.config.resolve(&root)?)?;
    let (_, receipt): (_, NewDeviceClaimReceipt) = core.client.json(
        reqwest::Method::POST,
        "session/claim-writer",
        &[],
        Some(&NewDeviceClaimRequest {
            writer_id: reservation.writer_id.clone(),
            authorization_id: reservation.authorization_id.clone(),
            former_token: Some(old.resolve(&root)?.token),
        }),
        &[],
    )?;
    #[cfg(test)]
    if STOP_AFTER_FRESH_CLAIM.with(|stop| stop.replace(false)) {
        return Err(SyncError::new("fresh-writer-stopped-after-claim", 409));
    }
    if receipt.authorization_id != reservation.authorization_id
        || receipt.writer_id != reservation.writer_id
        || receipt.device_id != target.config.device_id
        || receipt.library_id != target.config.library_id
        || receipt.epoch != target.epoch
        || !receipt.former_credential_inactive
    {
        return Err(SyncError::new("new-device-registration-integrity", 409));
    }
    log.save_verified(
        "claims",
        &fresh_writer_record("fresh-writer-claim", &target.config),
        &receipt,
    )?;
    core.detach_inactive(&old, &reservation.authorization_id)?;
    store.lww_adopt_fresh_writer(
        header.binding_authority,
        &reservation.old_writer_id,
        &reservation.writer_id,
    )?;
    Ok(NewDevicePreparation {
        authorization_id: reservation.authorization_id,
        writer_id: reservation.writer_id,
    })
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
        // A new registration to the stored library is installed only with the writer it
        // claimed, so the old writer never publishes under the new device.
        let stored = store.server_stored_config()?;
        let selected = match selected {
            Some(target)
                if stored.as_ref().is_some_and(|stored| {
                    stored.endpoint == target.config.endpoint
                        && stored.library_id == target.config.library_id
                        && stored.device_id != target.config.device_id
                }) =>
            {
                let writer = store.lww_clock_state()?.writer_id;
                claimed_fresh_writer(&log, &target)?
                    .is_some_and(|claimed| claimed == writer)
                    .then_some(target)
            }
            selected => selected,
        };
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
    if core.client.resolve_identity()?.epoch != target.epoch {
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
    fn switch_to(store: &mut PersistentStore, target: SyncTarget, inspection_id: Option<String>) -> crate::persistent_store::sync_selection::BindingState {
        let original = store.lww_binding_state().unwrap();
        store
            .switch_lww_binding(&crate::persistent_store::sync_selection::SwitchBindingRequest {
                header: header(store),
                expected_selection_epoch: original.selection_epoch,
                target,
                inspection_id,
            })
            .unwrap()
    }
    fn bound_client(store: &PersistentStore) -> LwwClient {
        LocalServerFixture::reopen_client(store, std::sync::Arc::new(super::super::client::TestIoCounters::default())).unwrap()
    }
    #[test]
    fn rebinding_the_same_library_publishes_offline_edits_and_keeps_receiving() {
        use crate::server_sync::lww_tests::{drain_publications, receive_available};
        let server = LocalServerFixture::new();
        let (_a_root, mut a) = local();
        let (_b_root, mut b) = local();
        let peer = server.client(&b);
        configure(&server, &a);
        bind(&mut a);
        let client = bound_client(&a);
        save(&mut a, &["root", "language"], serde_json::json!("ja"));
        drain_publications(&client, &mut a, &[]).unwrap();
        save(&mut b, &["root", "askRemoval"], serde_json::json!(true));
        drain_publications(&peer, &mut b, &[]).unwrap();
        receive_available(&peer, &mut b, &[]).unwrap();
        receive_available(&client, &mut a, &[]).unwrap();
        assert_eq!(a.read_root(None).unwrap().value["askRemoval"], true);
        save(&mut a, &["root", "language"], serde_json::json!("ko"));
        switch_to(&mut a, SyncTarget::None, None);
        save(&mut a, &["root", "askRemoval"], serde_json::json!(false));
        let inspected = inspect(&a, &header(&a)).unwrap();
        assert!(inspected.previously_bound_library);
        assert!(!inspected.registration_changed && !inspected.server_restored);
        let writer = a.lww_clock_state().unwrap().writer_id;
        let state = switch_to(&mut a, SyncTarget::Server("server".into()), Some(inspected.inspection_id));
        activate(&mut a, &Header { binding_authority: state.target_authority, request_id: uuid::Uuid::new_v4().to_string() }, None).unwrap();
        assert_eq!(a.lww_clock_state().unwrap().writer_id, writer);
        let client = bound_client(&a);
        drain_publications(&client, &mut a, &[]).unwrap();
        receive_available(&client, &mut a, &[]).unwrap();
        receive_available(&peer, &mut b, &[]).unwrap();
        assert_eq!(b.read_root(None).unwrap().value["language"], "ko");
        assert_eq!(b.read_root(None).unwrap().value["askRemoval"], false);
        assert_eq!(a.read_root(None).unwrap().value["askRemoval"], false);
        save(&mut b, &["root", "language"], serde_json::json!("en"));
        drain_publications(&peer, &mut b, &[]).unwrap();
        receive_available(&client, &mut a, &[]).unwrap();
        assert_eq!(a.read_root(None).unwrap().value["language"], "en");
    }
    #[test]
    fn switching_away_from_an_unreachable_server_never_settles_its_publication_with_the_next_one() {
        use risunest_sync_wire::lww::{PushRequest, UnitChange};
        let (_root, mut store) = local();
        let dead = LocalServerFixture::new();
        configure(&dead, &store);
        bind(&mut store);
        save(&mut store, &["root", "language"], serde_json::json!("ja"));
        let core = bound_client(&store);
        let authority = store.lww_binding_authority().unwrap();
        let entries = store.lww_read_outbox(authority, 256).unwrap().entries;
        let publication = super::super::lww_client::Publication {
            authority,
            request: PushRequest {
                library_id: core.client.config().library_id,
                writer_id: store.lww_clock_state().unwrap().writer_id,
                operation_id: uuid::Uuid::new_v4().to_string(),
                changes: entries.iter().map(|e| UnitChange { key: e.key.clone(), stamp: e.stamp.clone(), value: e.value.clone() }).collect(),
            },
            entries,
            config: store.server_stored_config().unwrap().unwrap(),
        };
        core.log.prepare(&publication).unwrap();
        drop(core);
        drop(dead);
        let live = LocalServerFixture::new();
        configure(&live, &store);
        bind(&mut store);
        save(&mut store, &["root", "askRemoval"], serde_json::json!(true));
        let client = bound_client(&store);
        crate::server_sync::lww_tests::drain_publications(&client, &mut store, &[]).unwrap();
        assert!(store.lww_read_outbox(store.lww_binding_authority().unwrap(), 256).unwrap().entries.is_empty());
    }
    #[test]
    fn a_switch_stopped_between_library_and_device_commits_recovers_every_retained_row() {
        use crate::server_sync::lww_tests::{drain_publications, receive_available};
        use risunest_sync_wire::lww::{PushRequest, UnitChange};
        let server = LocalServerFixture::new();
        let (root, mut store) = local();
        configure(&server, &store);
        bind(&mut store);
        let client = bound_client(&store);
        save(&mut store, &["root", "language"], serde_json::json!("ja"));
        drain_publications(&client, &mut store, &[]).unwrap();
        receive_available(&client, &mut store, &[]).unwrap();
        save(&mut store, &["root", "language"], serde_json::json!("ko"));
        let old = store.lww_binding_authority().unwrap();
        let entries = store.lww_read_outbox(old, 256).unwrap().entries;
        assert_eq!(entries.len(), 1);
        let operation = uuid::Uuid::new_v4().to_string();
        client.log.prepare(&super::super::lww_client::Publication {
            authority: old,
            request: PushRequest {
                library_id: client.client.config().library_id,
                writer_id: store.lww_clock_state().unwrap().writer_id,
                operation_id: operation.clone(),
                changes: entries.iter().map(|e| UnitChange { key: e.key.clone(), stamp: e.stamp.clone(), value: e.value.clone() }).collect(),
            },
            entries,
            config: store.server_stored_config().unwrap().unwrap(),
        }).unwrap();
        let page = StageReceive {
            header: Header { binding_authority: old, request_id: "synthetic-page".into() },
            changes: vec![],
            progress: Progress { kind: "server".into(), cursor: DecimalU64(1), writer_id: None },
            admitted_time_upper_ms: DecimalU64(1),
        };
        client.log.0.execute("INSERT INTO receive_pages VALUES(?1,?2,NULL,1,0)", rusqlite::params![old.0.to_string(), serde_json::to_string(&page).unwrap()]).unwrap();
        client.log.0.execute("INSERT INTO bootstrap VALUES(?1,'synthetic-pin',NULL,'1')", [old.0.to_string()]).unwrap();
        let stamp = risunest_sync_wire::stamp::Stamp { physical_ms: 1.into(), logical: 0, writer_id: "00000000-0000-4000-8000-000000000001".into() };
        let inline = |value: serde_json::Value| risunest_sync_wire::unit::UnitValue::inline(&serde_json::to_vec(&value).unwrap()).unwrap();
        store.lww_stage_receive(&StageReceive {
            header: Header { binding_authority: old, request_id: "synthetic-unfinished".into() },
            changes: vec![
                crate::persistent_store::lww::Change { key: risunest_sync_wire::unit::UnitKey::new(&["root", "askRemoval"]).unwrap(), stamp: stamp.clone(), value: inline(serde_json::json!(true)) },
                crate::persistent_store::lww::Change { key: risunest_sync_wire::unit::UnitKey::new(&["plugin-local", "orphan", "string", "received"]).unwrap(), stamp, value: inline(serde_json::json!("remote")) },
            ],
            progress: Progress { kind: "server".into(), cursor: DecimalU64(2), writer_id: None },
            admitted_time_upper_ms: DecimalU64(u64::MAX),
        }).unwrap();
        assert_eq!(store.lww_receive_row_counts("synthetic-unfinished").unwrap(), (1, 1));
        drop(client);
        let original = store.lww_binding_state().unwrap();
        let request = crate::persistent_store::sync_selection::SwitchBindingRequest {
            header: header(&store),
            expected_selection_epoch: original.selection_epoch,
            target: SyncTarget::None,
            inspection_id: None,
        };
        store.stop_next_switch_after_library_commit();
        assert!(store.switch_lww_binding(&request).is_err());
        assert_eq!(store.lww_receive_row_counts("synthetic-unfinished").unwrap(), (0, 1));
        drop(store);
        let mut store = PersistentStore::open(root.path()).unwrap();
        let new = store.lww_binding_authority().unwrap();
        assert_eq!(new.0, old.0 + 1);
        store.lww_recover_intents().unwrap();
        for _ in 0..2 {
            let state = store.switch_lww_binding(&request).unwrap();
            assert_eq!((state.target.clone(), state.target_authority), (SyncTarget::None, new));
            assert_eq!(state.progress.as_array().unwrap().len(), 1);
            let outbox = store.lww_read_outbox(new, 256).unwrap().entries;
            assert_eq!(outbox.len(), 1);
            assert_eq!(outbox[0].target_authority, new);
            let log = OperationLog::open(root.path()).unwrap();
            let pending = log.pending().unwrap();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].request.operation_id, operation);
            assert_eq!(pending[0].authority, new);
            assert!(pending[0].entries.iter().all(|entry| entry.target_authority == new));
            let (authority, body): (String, String) = log.0.query_row("SELECT authority,body FROM receive_pages", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            assert_eq!(authority, new.0.to_string());
            assert_eq!(serde_json::from_str::<StageReceive>(&body).unwrap().header.binding_authority, new);
            let bootstrap: Vec<String> = log.0.prepare("SELECT authority FROM bootstrap").unwrap().query_map([], |r| r.get(0)).unwrap().collect::<std::result::Result<_, _>>().unwrap();
            assert_eq!(bootstrap, vec![new.0.to_string()]);
            assert_eq!(store.lww_receive_row_counts("synthetic-unfinished").unwrap(), (0, 0));
        }
    }
    #[test]
    fn a_binding_change_fence_settles_answering_servers_and_leaves_unreachable_ones() {
        use risunest_sync_wire::lww::{PushRequest, UnitChange};
        for reachable in [true, false] {
            let (_root, mut store) = local();
            let mut server = Some(LocalServerFixture::new());
            configure(server.as_ref().unwrap(), &store);
            bind(&mut store);
            save(&mut store, &["root", "language"], serde_json::json!("ja"));
            let core = bound_client(&store);
            let authority = store.lww_binding_authority().unwrap();
            let entries = store.lww_read_outbox(authority, 256).unwrap().entries;
            core.log.prepare(&super::super::lww_client::Publication {
                authority,
                request: PushRequest {
                    library_id: core.client.config().library_id,
                    writer_id: store.lww_clock_state().unwrap().writer_id,
                    operation_id: uuid::Uuid::new_v4().to_string(),
                    changes: entries.iter().map(|e| UnitChange { key: e.key.clone(), stamp: e.stamp.clone(), value: e.value.clone() }).collect(),
                },
                entries,
                config: store.server_stored_config().unwrap().unwrap(),
            }).unwrap();
            if !reachable {
                server.take();
            }
            fence_for_binding_change(&core, &mut store).unwrap();
            assert_eq!(core.log.pending().unwrap().len(), usize::from(!reachable));
        }
    }
    #[test]
    fn a_rebind_stages_an_unfinished_receive_again_without_its_old_rows() {
        use crate::server_sync::lww_tests::{drain_publications, receive_available};
        let server = LocalServerFixture::new();
        let (_a_root, mut a) = local();
        let (_b_root, mut b) = local();
        let peer = server.client(&b);
        configure(&server, &a);
        bind(&mut a);
        let client = bound_client(&a);
        save(&mut b, &["root", "language"], serde_json::json!("ja"));
        drain_publications(&peer, &mut b, &[]).unwrap();
        let receive = header(&a);
        let page = client.receive_page(&mut a, &receive).unwrap();
        assert!(!page.changes.is_empty());
        a.lww_stage_receive(&page).unwrap();
        let unfinished = page.header.request_id.clone();
        assert_ne!(a.lww_receive_row_counts(&unfinished).unwrap().0, 0);
        drop(client);
        switch_to(&mut a, SyncTarget::None, None);
        assert_eq!(a.lww_receive_row_counts(&unfinished).unwrap(), (0, 0));
        let inspected = inspect(&a, &header(&a)).unwrap();
        assert!(inspected.previously_bound_library);
        let state = switch_to(&mut a, SyncTarget::Server("server".into()), Some(inspected.inspection_id));
        activate(&mut a, &Header { binding_authority: state.target_authority, request_id: uuid::Uuid::new_v4().to_string() }, None).unwrap();
        assert_ne!(a.read_root(None).unwrap().value["language"], "ja");
        let client = bound_client(&a);
        receive_available(&client, &mut a, &[]).unwrap();
        assert_eq!(a.read_root(None).unwrap().value["language"], "ja");
    }
    fn fresh_writer(store: &mut PersistentStore, inspection_id: &str) -> Result<NewDevicePreparation> {
        let request = header(store);
        prepare_fresh_writer(store, &request, inspection_id)
    }
    fn push_now(client: &LwwClient, store: &mut PersistentStore) -> Result<Option<risunest_sync_wire::lww::PushReceipt>> {
        let request = header(store);
        client.push(store, &request, &[])
    }
    fn outbox(store: &PersistentStore) -> Vec<(risunest_sync_wire::unit::UnitKey, risunest_sync_wire::stamp::Stamp)> {
        store
            .lww_read_outbox(store.lww_binding_authority().unwrap(), 256)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| (entry.key, entry.stamp))
            .collect()
    }
    fn server_unit(
        server: &LocalServerFixture,
        config: &super::super::client::ServerConfig,
        key: &risunest_sync_wire::unit::UnitKey,
    ) -> Option<risunest_sync_wire::lww::UnitChange> {
        let device = server.server.authenticate(&config.library_id, &config.token).unwrap();
        let pin = server.server.create_state_pin(&device).unwrap();
        let page = server.server.state_page(&device, &pin.pin_id, None, 1024).unwrap();
        server.server.release_state_pin(&device, &pin.pin_id).unwrap();
        page.items.into_iter().find(|item| &item.key == key)
    }
    fn fresh_inspection(store: &PersistentStore) -> Inspection {
        let inspected = inspect(store, &header(store)).unwrap();
        assert!(inspected.previously_bound_library);
        assert!(inspected.registration_changed);
        assert!(!inspected.server_restored);
        inspected
    }
    fn resume(store: &mut PersistentStore) {
        let request = header(store);
        activate(store, &request, None).unwrap();
    }
    #[test]
    fn a_new_registration_after_unbinding_publishes_every_retained_edit_with_its_original_stamp() {
        use crate::server_sync::lww_tests::{drain_publications, receive_available};
        let server = LocalServerFixture::new();
        let (_a_root, mut a) = local();
        let (_b_root, mut b) = local();
        let peer = server.client(&b);
        let old = configure(&server, &a);
        bind(&mut a);
        let client = bound_client(&a);
        save(&mut a, &["root", "language"], serde_json::json!("ja"));
        drain_publications(&client, &mut a, &[]).unwrap();
        drop(client);
        let old_writer = a.lww_clock_state().unwrap().writer_id;
        switch_to(&mut a, SyncTarget::None, None);
        save(&mut a, &["root", "language"], serde_json::json!("ko"));
        save(&mut a, &["root", "askRemoval"], serde_json::json!(true));
        let retained = outbox(&a);
        assert_eq!(retained.len(), 2);
        let issued = a.lww_clock_state().unwrap().issued;
        let fresh = configure(&server, &a);
        let inspected = fresh_inspection(&a);
        let prepared = fresh_writer(&mut a, &inspected.inspection_id).unwrap();
        let clock = a.lww_clock_state().unwrap();
        assert_eq!(clock.writer_id, prepared.writer_id);
        assert_ne!(clock.writer_id, old_writer);
        assert_eq!(clock.issued, issued);
        assert_eq!(outbox(&a), retained);
        assert!(server.server.authenticate(&old.library_id, &old.token).is_err());
        let state = switch_to(&mut a, SyncTarget::Server("server".into()), Some(inspected.inspection_id));
        activate(&mut a, &Header { binding_authority: state.target_authority, request_id: uuid::Uuid::new_v4().to_string() }, None).unwrap();
        assert_eq!(a.server_stored_config().unwrap().unwrap().device_id, fresh.device_id);
        assert_eq!(outbox(&a), retained);
        let client = bound_client(&a);
        drain_publications(&client, &mut a, &[]).unwrap();
        assert!(outbox(&a).is_empty());
        for (key, stamp) in &retained {
            let published = server_unit(&server, &fresh, key).unwrap();
            assert_eq!(&published.stamp, stamp);
            assert_eq!(published.stamp.writer_id, old_writer);
        }
        receive_available(&peer, &mut b, &[]).unwrap();
        assert_eq!(b.read_root(None).unwrap().value["language"], "ko");
        assert_eq!(b.read_root(None).unwrap().value["askRemoval"], true);
        save(&mut b, &["root", "language"], serde_json::json!("en"));
        drain_publications(&peer, &mut b, &[]).unwrap();
        receive_available(&client, &mut a, &[]).unwrap();
        assert_eq!(a.read_root(None).unwrap().value["language"], "en");
        save(&mut a, &["root", "askRemoval"], serde_json::json!(false));
        assert_eq!(outbox(&a)[0].1.writer_id, prepared.writer_id);
        drain_publications(&client, &mut a, &[]).unwrap();
        receive_available(&peer, &mut b, &[]).unwrap();
        assert_eq!(b.read_root(None).unwrap().value["askRemoval"], false);
    }
    #[test]
    fn a_new_registration_while_bound_keeps_the_binding_and_publishes_unsent_edits() {
        use crate::server_sync::lww_tests::drain_publications;
        let server = LocalServerFixture::new();
        let (_root, mut a) = local();
        let old = configure(&server, &a);
        let bound = bind(&mut a);
        let client = bound_client(&a);
        save(&mut a, &["root", "language"], serde_json::json!("ja"));
        drain_publications(&client, &mut a, &[]).unwrap();
        drop(client);
        save(&mut a, &["root", "language"], serde_json::json!("ko"));
        let retained = outbox(&a);
        assert_eq!(retained.len(), 1);
        let issued = a.lww_clock_state().unwrap().issued;
        let fresh = configure(&server, &a);
        let inspected = fresh_inspection(&a);
        let prepared = fresh_writer(&mut a, &inspected.inspection_id).unwrap();
        resume(&mut a);
        assert_eq!(a.lww_binding_authority().unwrap(), bound.binding_authority);
        assert_eq!(a.server_stored_config().unwrap().unwrap().device_id, fresh.device_id);
        let clock = a.lww_clock_state().unwrap();
        assert_eq!((clock.writer_id, clock.issued), (prepared.writer_id, issued));
        let client = bound_client(&a);
        let receipt = push_now(&client, &mut a).unwrap().unwrap();
        assert_eq!(receipt.accepted_keys, vec![retained[0].0.clone()]);
        assert!(push_now(&client, &mut a).unwrap().is_none());
        assert_eq!(server_unit(&server, &fresh, &retained[0].0).unwrap().stamp, retained[0].1);
        assert!(server.server.authenticate(&old.library_id, &old.token).is_err());
    }
    #[test]
    fn a_new_registration_after_the_old_one_was_revoked_still_publishes_unsent_edits() {
        let server = LocalServerFixture::new();
        let (_root, mut a) = local();
        let old = configure(&server, &a);
        bind(&mut a);
        save(&mut a, &["root", "language"], serde_json::json!("ko"));
        let retained = outbox(&a);
        assert_eq!(retained.len(), 1);
        server.server.revoke_device(&old.device_id).unwrap();
        let fresh = configure(&server, &a);
        let inspected = fresh_inspection(&a);
        assert!(inspected.previously_bound_library && inspected.registration_changed && !inspected.server_restored);
        let prepared = fresh_writer(&mut a, &inspected.inspection_id).unwrap();
        resume(&mut a);
        assert_eq!(a.lww_clock_state().unwrap().writer_id, prepared.writer_id);
        let receipt = push_now(&bound_client(&a), &mut a).unwrap().unwrap();
        assert_eq!(receipt.accepted_keys, vec![retained[0].0.clone()]);
        assert_eq!(server_unit(&server, &fresh, &retained[0].0).unwrap().stamp, retained[0].1);
    }
    #[test]
    fn a_new_registration_with_nothing_unsent_still_swaps_the_writer_and_publishes_the_next_edit() {
        use crate::server_sync::lww_tests::drain_publications;
        let server = LocalServerFixture::new();
        let (_root, mut a) = local();
        configure(&server, &a);
        bind(&mut a);
        let client = bound_client(&a);
        save(&mut a, &["root", "language"], serde_json::json!("ja"));
        drain_publications(&client, &mut a, &[]).unwrap();
        drop(client);
        assert!(outbox(&a).is_empty());
        let before = a.lww_clock_state().unwrap();
        let fresh = configure(&server, &a);
        let inspected = fresh_inspection(&a);
        let prepared = fresh_writer(&mut a, &inspected.inspection_id).unwrap();
        resume(&mut a);
        let clock = a.lww_clock_state().unwrap();
        assert_ne!(clock.writer_id, before.writer_id);
        assert_eq!((clock.writer_id, clock.issued), (prepared.writer_id.clone(), before.issued.clone()));
        save(&mut a, &["root", "language"], serde_json::json!("ko"));
        let next = outbox(&a);
        assert_eq!(next[0].1.writer_id, prepared.writer_id);
        assert!(next[0].1.physical_ms >= before.issued.unwrap().physical_ms);
        assert!(push_now(&bound_client(&a), &mut a).unwrap().is_some());
        assert_eq!(server_unit(&server, &fresh, &next[0].0).unwrap().stamp, next[0].1);
    }
    #[test]
    fn a_fresh_writer_claim_stopped_before_the_swap_never_publishes_the_old_writer_and_recovers() {
        let server = LocalServerFixture::new();
        let (root, mut a) = local();
        let old = configure(&server, &a);
        bind(&mut a);
        save(&mut a, &["root", "language"], serde_json::json!("ko"));
        let retained = outbox(&a);
        let old_writer = a.lww_clock_state().unwrap().writer_id;
        let fresh = configure(&server, &a);
        let inspected = fresh_inspection(&a);
        STOP_AFTER_FRESH_CLAIM.with(|stop| stop.set(true));
        assert_eq!(
            fresh_writer(&mut a, &inspected.inspection_id).unwrap_err().code,
            "fresh-writer-stopped-after-claim"
        );
        assert_eq!(a.lww_clock_state().unwrap().writer_id, old_writer);
        assert!(server.server.authenticate(&old.library_id, &old.token).is_err());
        drop(a);
        let mut a = PersistentStore::open(root.path()).unwrap();
        let request = header(&a);
        assert_eq!(activate(&mut a, &request, None).unwrap_err().status, 401);
        assert_eq!(a.server_stored_config().unwrap().unwrap().device_id, old.device_id);
        assert_eq!(push_now(&bound_client(&a), &mut a).unwrap_err().status, 401);
        assert_eq!(outbox(&a), retained);
        let reserved: FreshWriterReservation = OperationLog::open(root.path())
            .unwrap()
            .verified("claims", &format!("fresh-writer:{}:{}", fresh.library_id, fresh.device_id))
            .unwrap();
        assert_eq!(reserved.old_writer_id, old_writer);
        let inspected = fresh_inspection(&a);
        let prepared = fresh_writer(&mut a, &inspected.inspection_id).unwrap();
        assert_eq!(prepared.writer_id, reserved.writer_id);
        assert_eq!(a.lww_clock_state().unwrap().writer_id, reserved.writer_id);
        resume(&mut a);
        assert_eq!(a.server_stored_config().unwrap().unwrap().device_id, fresh.device_id);
        let receipt = push_now(&bound_client(&a), &mut a).unwrap().unwrap();
        assert_eq!(receipt.accepted_keys, vec![retained[0].0.clone()]);
        assert_eq!(server_unit(&server, &fresh, &retained[0].0).unwrap().stamp, retained[0].1);
    }
    #[test]
    fn inspecting_a_new_registration_never_installs_it_with_the_old_writer() {
        let server = LocalServerFixture::new();
        let (_root, mut a) = local();
        let old = configure(&server, &a);
        bind(&mut a);
        save(&mut a, &["root", "language"], serde_json::json!("ko"));
        configure(&server, &a);
        fresh_inspection(&a);
        resume(&mut a);
        assert_eq!(a.server_stored_config().unwrap().unwrap().device_id, old.device_id);
        let receipt = push_now(&bound_client(&a), &mut a).unwrap().unwrap();
        assert_eq!(receipt.accepted_keys.len(), 1);
    }
    #[test]
    fn an_old_publication_that_never_arrived_is_published_once_by_the_fresh_writer() {
        use risunest_sync_wire::lww::{PushRequest, UnitChange};
        let server = LocalServerFixture::new();
        let (root, mut a) = local();
        configure(&server, &a);
        bind(&mut a);
        save(&mut a, &["root", "language"], serde_json::json!("ko"));
        let retained = outbox(&a);
        let old_client = bound_client(&a);
        let authority = a.lww_binding_authority().unwrap();
        let entries = a.lww_read_outbox(authority, 256).unwrap().entries;
        let operation = uuid::Uuid::new_v4().to_string();
        old_client.log.prepare(&super::super::lww_client::Publication {
            authority,
            request: PushRequest {
                library_id: old_client.client.config().library_id,
                writer_id: a.lww_clock_state().unwrap().writer_id,
                operation_id: operation.clone(),
                changes: entries.iter().map(|e| UnitChange { key: e.key.clone(), stamp: e.stamp.clone(), value: e.value.clone() }).collect(),
            },
            entries,
            config: a.server_stored_config().unwrap().unwrap(),
        }).unwrap();
        drop(old_client);
        let fresh = configure(&server, &a);
        let inspected = fresh_inspection(&a);
        fresh_writer(&mut a, &inspected.inspection_id).unwrap();
        let settled: String = OperationLog::open(root.path()).unwrap().0
            .query_row("SELECT receipt FROM publications WHERE id=?1", [&operation], |r| r.get(0))
            .unwrap();
        assert!(settled.contains("rejected") && settled.contains("operation-cancelled"));
        assert_eq!(outbox(&a), retained);
        resume(&mut a);
        let client = bound_client(&a);
        let receipt = push_now(&client, &mut a).unwrap().unwrap();
        assert_eq!(receipt.accepted_keys, vec![retained[0].0.clone()]);
        assert!(push_now(&client, &mut a).unwrap().is_none());
        assert_eq!(server_unit(&server, &fresh, &retained[0].0).unwrap().stamp, retained[0].1);
    }
    #[test]
    fn a_restored_server_is_reported_and_replaces_this_device_as_a_new_device() {
        use crate::server_sync::lww_tests::drain_publications;
        let server = LocalServerFixture::new();
        let (_root, mut a) = local();
        configure(&server, &a);
        bind(&mut a);
        let client = bound_client(&a);
        save(&mut a, &["root", "language"], serde_json::json!("ja"));
        drain_publications(&client, &mut a, &[]).unwrap();
        drop(client);
        save(&mut a, &["root", "language"], serde_json::json!("after-backup"));
        server.server.rotate_restored_epoch().unwrap();
        let fresh = configure(&server, &a);
        let inspected = inspect(&a, &header(&a)).unwrap();
        assert!(inspected.server_restored);
        assert!(inspected.registration_changed);
        assert!(!inspected.previously_bound_library);
        let request = header(&a);
        let staged = stage(&mut a, &request, &inspected.inspection_id).unwrap();
        let prepared = prepare_new_device(&mut a, &request, &staged.staging_id).unwrap();
        let result = a
            .lww_replace_target_as_new_device(&request, &staged.staging_id, &prepared.authorization_id)
            .unwrap();
        let next = Header { binding_authority: result.binding_authority, request_id: uuid::Uuid::new_v4().to_string() };
        activate(&mut a, &next, Some((&prepared.authorization_id, &result.writer_id))).unwrap();
        assert_eq!(a.read_root(None).unwrap().value["language"], "ja");
        assert_eq!(a.server_stored_config().unwrap().unwrap().device_id, fresh.device_id);
        save(&mut a, &["root", "language"], serde_json::json!("ko"));
        assert!(bound_client(&a).push(&mut a, &next, &[]).unwrap().is_some());
        assert!(!inspect(&a, &header(&a)).unwrap().server_restored);
    }
}
