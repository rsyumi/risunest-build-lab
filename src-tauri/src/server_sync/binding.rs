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
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PendingBinding {
    endpoint: String,
    library_id: String,
    epoch: String,
    server_empty: bool,
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
fn first_state_page(core: &LwwClient) -> Result<StatePage> {
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
    Ok(page)
}
pub(crate) fn inspect(store: &PersistentStore, header: &Header) -> Result<Inspection> {
    assert_authority(store, header)?;
    let core = candidate(store)?;
    core.fresh_admission()?;
    let head = core.client.resolve_identity()?;
    let page = first_state_page(&core)?;
    let config = core
        .access
        .ok_or_else(|| SyncError::new("server-unconfigured", 409))?;
    #[cfg(test)]
    super::hash_metrics::record(
        "c_binding_target_identity",
        format!("{}:{}", config.library_id, head.epoch).len(),
    );
    // A server is identified by its library and epoch, so another spelling of
    // its address reaches the same target.
    let target_id =
        risunest_sync_wire::hash(format!("{}:{}", config.library_id, head.epoch).as_bytes());
    let same_library = |old: &StoredConfig| old.library_id == config.library_id;
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
/// Reports a first binding that stopped after its target switch: the target is this
/// server and a registration is saved, but no binding was ever activated. The server
/// counts as still empty only when it is the library and epoch that were inspected and
/// holds no unit; any failure to tell counts as not empty.
pub(crate) fn pending_binding(store: &PersistentStore) -> Result<Option<PendingBinding>> {
    let state = store.lww_binding_state()?;
    if state.target != SyncTarget::Server("server".into()) || store.server_stored_config()?.is_some() {
        return Ok(None);
    }
    let log = OperationLog::open(store.repository_root())?;
    if log.verified::<VerifiedTarget>("bindings", "active").is_ok() {
        return Ok(None);
    }
    let Some(saved) = log.config("candidate")? else {
        return Ok(None);
    };
    let Ok(selected) = log.verified::<VerifiedTarget>("bindings", "selected") else {
        return Ok(None);
    };
    let still_empty = || -> Result<bool> {
        if saved.library_id != selected.config.library_id
            || state.library_id.as_deref() != Some(&selected.config.library_id)
        {
            return Ok(false);
        }
        let core = candidate(store)?;
        let head = core.client.resolve_identity()?;
        Ok(head.library_id == selected.config.library_id
            && head.epoch == selected.epoch
            && first_state_page(&core)?.items.is_empty())
    };
    let server_empty = still_empty().unwrap_or(false);
    Ok(Some(PendingBinding {
        endpoint: selected.config.endpoint,
        library_id: selected.config.library_id,
        epoch: selected.epoch,
        server_empty,
    }))
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
/// The stored registration's credential, or none when this device can no longer read it.
fn readable(stored: &StoredConfig, root: &std::path::Path) -> Result<Option<super::client::ServerConfig>> {
    match stored.resolve(root) {
        Ok(config) => Ok(Some(config)),
        Err(error) if error.code == "device-credential-unavailable" => Ok(None),
        Err(error) => Err(error),
    }
}
/// Settles the stored registration's pending publications before a binding change. Without a
/// readable credential nothing can be asked, and the change goes ahead.
pub(crate) fn fence_stored(store: &mut PersistentStore, new_device: bool) -> Result<()> {
    let Some(stored) = store.server_stored_config()? else {
        return Ok(());
    };
    let Some(config) = readable(&stored, store.repository_root())? else {
        return Ok(());
    };
    let mut core = LwwClient::new(store.repository_root(), config)?;
    core.access = Some(stored);
    if new_device {
        core.fence_new_device(store)
    } else {
        fence_for_binding_change(&core, store)
    }
}
/// Settles pending publications before a binding change. When a publication's server does not
/// answer, the switch proceeds and carries or detaches that publication instead of waiting.
pub(crate) fn fence_for_binding_change(core: &LwwClient, store: &mut PersistentStore) -> Result<()> {
    for publication in core.log.pending()? {
        if !core.answers(&publication)? {
            return Ok(());
        }
    }
    core.fence(store)
}
pub(crate) fn stage(
    store: &mut PersistentStore,
    header: &Header,
    inspection_id: &str,
    cancelled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<StagedTarget> {
    assert_authority(store, header)?;
    let log = OperationLog::open(store.repository_root())?;
    let target: VerifiedTarget = log.verified("bindings", inspection_id)?;
    if target.authority != header.binding_authority {
        return Err(SyncError::new("binding-authority-changed", 409));
    }
    let mut core = LwwClient::with_cancellation(
        store.repository_root(),
        target.config.resolve(store.repository_root())?,
        cancelled,
    )?;
    core.access = Some(target.config.clone());
    if core.client.resolve_identity()?.epoch != target.epoch {
        return Err(SyncError::new("server-epoch-changed", 409));
    }
    let upper = core.fresh_admission()?;
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
    let old_config = match &old {
        Some(old) => readable(old, store.repository_root())?,
        None => None,
    };
    if let Some(config) = &old_config {
        LwwClient::new(store.repository_root(), config.clone())?.fence_new_device(store)?;
    }
    let preparation = store.prepare_lww_new_device(header, staging_id)?;
    let core = LwwClient::new(
        store.repository_root(),
        stage.target.config.resolve(store.repository_root())?,
    )?;
    let same = old
        .as_ref()
        .is_some_and(|old| old.library_id == stage.target.config.library_id);
    // An old credential this device cannot read leaves that registration listed on the server.
    let former_token = old_config.filter(|_| same).map(|config| config.token);
    let former = former_token.is_some();
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
        || receipt.former_credential_inactive != former
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
            core.fence_new_device(store)?;
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
            old.library_id == target.config.library_id && old.device_id != target.config.device_id
        })
        .ok_or_else(|| SyncError::new("registration-unchanged", 409))?;
    if let Some(config) = readable(&old, &root)? {
        LwwClient::new(&root, config)?.fence_new_device(store)?;
    }
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
    complete_fresh_writer(store, &log, header.binding_authority, &target, &old, &reservation)?;
    Ok(NewDevicePreparation {
        authorization_id: reservation.authorization_id,
        writer_id: reservation.writer_id,
    })
}
/// The steps after a fresh-writer reservation: the new registration claims the reserved writer,
/// the old registration's publications are detached, and the reserved writer replaces the old one.
fn complete_fresh_writer(
    store: &mut PersistentStore,
    log: &OperationLog,
    authority: DecimalU64,
    target: &VerifiedTarget,
    old: &StoredConfig,
    reservation: &FreshWriterReservation,
) -> Result<()> {
    let root = store.repository_root().to_owned();
    // An old credential this device cannot read leaves that registration listed on the server.
    let former_token = readable(old, &root)?.map(|config| config.token);
    let former = former_token.is_some();
    let core = LwwClient::new(&root, target.config.resolve(&root)?)?;
    let (_, receipt): (_, NewDeviceClaimReceipt) = core.client.json(
        reqwest::Method::POST,
        "session/claim-writer",
        &[],
        Some(&NewDeviceClaimRequest {
            writer_id: reservation.writer_id.clone(),
            authorization_id: reservation.authorization_id.clone(),
            former_token,
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
        || receipt.former_credential_inactive != former
    {
        return Err(SyncError::new("new-device-registration-integrity", 409));
    }
    log.save_verified(
        "claims",
        &fresh_writer_record("fresh-writer-claim", &target.config),
        &receipt,
    )?;
    core.detach_inactive(old, &reservation.authorization_id)?;
    store.lww_adopt_fresh_writer(authority, &reservation.old_writer_id, &reservation.writer_id)?;
    Ok(())
}
/// Finishes a fresh-writer swap that stopped after its reservation, so startup installs the new
/// registration instead of falling back to the old one.
fn resume_fresh_writer(
    store: &mut PersistentStore,
    log: &OperationLog,
    authority: DecimalU64,
    target: &VerifiedTarget,
    old: &StoredConfig,
) -> Result<()> {
    let writer = store.lww_clock_state()?.writer_id;
    if claimed_fresh_writer(log, target)?.is_some_and(|claimed| claimed == writer) {
        return Ok(());
    }
    let reservation = claim_record::<FreshWriterReservation>(
        log,
        &fresh_writer_record("fresh-writer", &target.config),
    )?;
    match reservation {
        Some(reservation) if reservation.old_writer_id == writer => {
            complete_fresh_writer(store, log, authority, target, old, &reservation)
        }
        _ => Ok(()),
    }
}
/// The claim of a new-device binding that stopped after replacing the library and
/// before installing its registration: the clock already runs on the writer the
/// claim reserved while another registration is still stored.
fn stopped_new_device_claim(
    store: &PersistentStore,
    log: &OperationLog,
    header: &Header,
) -> Result<Option<VerifiedClaim>> {
    let writer = store.lww_clock_state()?.writer_id;
    let Some(authorization_id) = store.lww_new_device_authorization(&writer)? else {
        return Ok(None);
    };
    let Ok(claim) = log.verified::<VerifiedClaim>("claims", &authorization_id) else {
        return Ok(None);
    };
    let installed = store.server_stored_config()?.is_some_and(|stored| {
        stored.library_id == claim.stage.target.config.library_id
            && stored.device_id == claim.stage.target.config.device_id
    });
    Ok((claim.receipt.writer_id == writer
        && claim.stage.target.authority.0.checked_add(1) == Some(header.binding_authority.0)
        && !installed)
        .then_some(claim))
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
    } else if let Some(claim) = stopped_new_device_claim(store, &log, header)? {
        (claim.stage.target.clone(), Some(claim.stage))
    } else {
        let active = log.verified::<VerifiedTarget>("bindings", "active").ok();
        let selected = log.verified::<VerifiedTarget>("bindings", "selected").ok();
        // A new registration to the stored library is installed only with the writer it
        // claimed, so the old writer never publishes under the new device.
        let stored = store.server_stored_config()?;
        if let (Some(target), Some(old)) = (&selected, &stored) {
            if old.library_id == target.config.library_id
                && old.device_id != target.config.device_id
                && target.authority == header.binding_authority
            {
                resume_fresh_writer(store, &log, header.binding_authority, target, old)?;
            }
        }
        let selected = match selected {
            Some(target)
                if stored.as_ref().is_some_and(|stored| {
                    stored.library_id == target.config.library_id
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
                        || match active.as_ref() {
                            Some(active) => active.target_id == target.target_id,
                            // A first binding stopped after its target switch is retried
                            // without another switch.
                            None => target.authority == header.binding_authority,
                        })
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
    for kind in ["fresh-writer", "fresh-writer-claim"] {
        log.0.execute(
            "DELETE FROM claims WHERE id=?1",
            [fresh_writer_record(kind, &target.config)],
        )?;
    }
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
            admitted_time_upper_ms: core.fresh_admission()?,
        };
        store.lww_stage_receive(&receive)?;
        store.lww_apply_receive(&ApplyReceive {
            header: receive.header.clone(),
            generating: vec![],
        })?;
        store.lww_finish_receive(&receive.header)?;
        core.acknowledge_cursor(stage.cursor)?;
    }
    store.lww_finish_initial_publication(header)?;
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
        let staged = stage(store, &super::lww_tests::header(store), &inspected.inspection_id, None)?;
        let next = store.switch_lww_binding(&crate::persistent_store::sync_selection::SwitchBindingRequest {
            initial_publication: false,
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
                    initial_publication: false,
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
        let staged = stage(&mut target, &request, &inspected.inspection_id, None).unwrap();
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
                    initial_publication: false,
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
        let staged = stage(&mut store, &request, &inspected.inspection_id, None).unwrap();
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
    fn a_new_device_binding_stopped_before_activation_installs_the_claimed_registration() {
        let server = LocalServerFixture::new();
        let (root, mut store) = local();
        let old = configure(&server, &store);
        let bound = bind(&mut store);
        save(&mut store, &["root", "language"], serde_json::json!("ja"));
        let mut core = LwwClient::new(store.repository_root(), old.clone()).unwrap();
        core.access = store.server_stored_config().unwrap();
        core.push(&mut store, &bound, &[]).unwrap();
        let fresh = configure(&server, &store);
        let request = header(&store);
        let inspected = inspect(&store, &request).unwrap();
        let request = header(&store);
        let staged = stage(&mut store, &request, &inspected.inspection_id, None).unwrap();
        let prepared = prepare_new_device(&mut store, &request, &staged.staging_id).unwrap();
        store
            .lww_replace_target_as_new_device(&request, &staged.staging_id, &prepared.authorization_id)
            .unwrap();
        // The app stops before the new device is activated and starts again.
        drop(store);
        let mut store = PersistentStore::open(root.path()).unwrap();
        let restarted = header(&store);
        activate(&mut store, &restarted, None).unwrap();
        assert_eq!(
            store.server_stored_config().unwrap().unwrap().device_id,
            fresh.device_id
        );
        assert_eq!(store.read_root(None).unwrap().value["language"], "ja");
        save(&mut store, &["root", "language"], serde_json::json!("ko"));
        assert!(bound_client(&store)
            .push(&mut store, &restarted, &[])
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
        let staged = stage(&mut store, &request, &inspected.inspection_id, None).unwrap();
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
                initial_publication: false,
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
            initial_publication: false,
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
    #[test]
    fn a_first_binding_stopped_after_its_target_switch_activates_when_retried() {
        use crate::server_sync::lww_tests::{drain_publications, receive_available};
        let server = LocalServerFixture::new();
        let (_a_root, mut a) = local();
        let (_b_root, mut b) = local();
        let peer = server.client(&b);
        configure(&server, &a);
        let inspected = inspect(&a, &header(&a)).unwrap();
        assert!(inspected.empty);
        switch_to(&mut a, SyncTarget::Server("server".into()), Some(inspected.inspection_id));
        // The app stops here. The retry finds the target already switched and switches no further.
        let retried = inspect(&a, &header(&a)).unwrap();
        assert!(retried.empty && !retried.previously_bound_library);
        resume(&mut a);
        assert!(a.server_stored_config().unwrap().is_some());
        let client = bound_client(&a);
        save(&mut a, &["root", "language"], serde_json::json!("ja"));
        drain_publications(&client, &mut a, &[]).unwrap();
        receive_available(&peer, &mut b, &[]).unwrap();
        assert_eq!(b.read_root(None).unwrap().value["language"], "ja");
    }
    #[test]
    fn a_first_binding_stopped_after_its_switch_is_reported_with_whether_the_server_is_still_empty() {
        use crate::server_sync::lww_tests::drain_publications;
        let gone = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = vanishing_server(gone.clone());
        let (_a_root, mut a) = local();
        let (_b_root, mut b) = local();
        let config = configure(&server, &a);
        assert!(pending_binding(&a).unwrap().is_none());
        let inspected = inspect(&a, &header(&a)).unwrap();
        assert!(inspected.empty);
        assert!(pending_binding(&a).unwrap().is_none());
        switch_to(&mut a, SyncTarget::Server("server".into()), Some(inspected.inspection_id));
        let peer = server.client(&b);
        let epoch = peer.client.resolve_identity().unwrap().epoch;
        let pending = pending_binding(&a).unwrap().unwrap();
        assert_eq!(pending.endpoint, config.endpoint);
        assert_eq!(pending.library_id, config.library_id);
        assert_eq!(pending.epoch, epoch);
        assert!(pending.server_empty);
        gone.store(true, std::sync::atomic::Ordering::SeqCst);
        let unreachable = pending_binding(&a).unwrap().unwrap();
        assert!(!unreachable.server_empty);
        gone.store(false, std::sync::atomic::Ordering::SeqCst);
        save(&mut b, &["root", "language"], serde_json::json!("ja"));
        drain_publications(&peer, &mut b, &[]).unwrap();
        let published = pending_binding(&a).unwrap().unwrap();
        assert!(!published.server_empty);
        assert_eq!(published.epoch, epoch);
        resume(&mut a);
        assert!(pending_binding(&a).unwrap().is_none());
    }
    #[test]
    fn a_stopped_first_binding_to_a_server_restored_since_is_not_still_empty() {
        let server = LocalServerFixture::new();
        let (_root, mut a) = local();
        configure(&server, &a);
        let inspected = inspect(&a, &header(&a)).unwrap();
        switch_to(&mut a, SyncTarget::Server("server".into()), Some(inspected.inspection_id));
        assert!(pending_binding(&a).unwrap().unwrap().server_empty);
        server.server.rotate_restored_epoch().unwrap();
        assert!(!pending_binding(&a).unwrap().unwrap().server_empty);
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
    fn switch_owing_initial_publication(store: &mut PersistentStore, inspection_id: String) -> Header {
        let original = store.lww_binding_state().unwrap();
        let state = store
            .switch_lww_binding(&crate::persistent_store::sync_selection::SwitchBindingRequest {
                header: header(store),
                expected_selection_epoch: original.selection_epoch,
                target: SyncTarget::Server("server".into()),
                inspection_id: Some(inspection_id),
                initial_publication: true,
            })
            .unwrap();
        Header { binding_authority: state.target_authority, request_id: uuid::Uuid::new_v4().to_string() }
    }
    fn server_keys(server: &LocalServerFixture, config: &super::super::client::ServerConfig) -> std::collections::BTreeSet<String> {
        let device = server.server.authenticate(&config.library_id, &config.token).unwrap();
        let pin = server.server.create_state_pin(&device).unwrap();
        let page = server.server.state_page(&device, &pin.pin_id, None, 1024).unwrap();
        server.server.release_state_pin(&device, &pin.pin_id).unwrap();
        assert!(page.next_key.is_none());
        page.items.into_iter().map(|item| item.key.as_str().to_owned()).collect()
    }
    #[test]
    fn an_initial_publication_stopped_between_pages_or_commits_finishes_when_the_binding_resumes() {
        use crate::server_sync::lww_tests::drain_publications;
        let mut published = Vec::new();
        for stop in [None, Some(1), Some(2), Some(3)] {
            let server = LocalServerFixture::new();
            let (root, mut a) = local();
            save(&mut a, &["root", "language"], serde_json::json!("ko"));
            save(&mut a, &["root", "askRemoval"], serde_json::json!(true));
            let config = configure(&server, &a);
            let inspected = inspect(&a, &header(&a)).unwrap();
            assert!(inspected.empty);
            let request = switch_owing_initial_publication(&mut a, inspected.inspection_id);
            assert_eq!(a.lww_owed_initial_publication().unwrap(), Some(request.binding_authority.0.to_string()));
            if let Some(commits) = stop {
                a.stop_initial_queue_after_commits(1, commits);
                assert!(activate(&mut a, &request, None).is_err());
                drop(a);
                a = PersistentStore::open(root.path()).unwrap();
                assert!(a.lww_owed_initial_publication().unwrap().is_some());
                a.stop_initial_queue_after_commits(1, usize::MAX);
                resume(&mut a);
            } else {
                activate(&mut a, &request, None).unwrap();
            }
            assert_eq!(a.lww_owed_initial_publication().unwrap(), None);
            drain_publications(&bound_client(&a), &mut a, &[]).unwrap();
            assert!(outbox(&a).is_empty());
            published.push(server_keys(&server, &config));
        }
        assert!(published[0].iter().any(|key| key.contains("language")));
        assert!(published[0].iter().any(|key| key.contains("askRemoval")));
        for keys in &published[1..] {
            assert_eq!(keys, &published[0]);
        }
    }
    #[test]
    fn a_switch_owes_an_initial_publication_only_when_asked_and_a_later_switch_clears_it() {
        let server = LocalServerFixture::new();
        let (_root, mut a) = local();
        save(&mut a, &["root", "language"], serde_json::json!("ko"));
        configure(&server, &a);
        let inspected = inspect(&a, &header(&a)).unwrap();
        let state = switch_to(&mut a, SyncTarget::Server("server".into()), Some(inspected.inspection_id));
        assert_eq!(a.lww_owed_initial_publication().unwrap(), None);
        activate(&mut a, &Header { binding_authority: state.target_authority, request_id: uuid::Uuid::new_v4().to_string() }, None).unwrap();
        assert!(outbox(&a).is_empty());
        switch_to(&mut a, SyncTarget::None, None);
        let original = a.lww_binding_state().unwrap();
        assert!(a
            .switch_lww_binding(&crate::persistent_store::sync_selection::SwitchBindingRequest {
                header: header(&a),
                expected_selection_epoch: original.selection_epoch,
                target: SyncTarget::None,
                inspection_id: None,
                initial_publication: true,
            })
            .is_err());
        let inspected = inspect(&a, &header(&a)).unwrap();
        switch_owing_initial_publication(&mut a, inspected.inspection_id);
        assert!(a.lww_owed_initial_publication().unwrap().is_some());
        switch_to(&mut a, SyncTarget::None, None);
        assert_eq!(a.lww_owed_initial_publication().unwrap(), None);
    }
    /// A server that, once `gone` is set, answers every request with an empty 404 the way an
    /// address that no longer serves this library does.
    fn vanishing_server(gone: std::sync::Arc<std::sync::atomic::AtomicBool>) -> LocalServerFixture {
        use axum::response::IntoResponse;
        LocalServerFixture::with_router(move |router| {
            router.layer(axum::middleware::from_fn(
                move |request: axum::extract::Request, next: axum::middleware::Next| {
                    let gone = gone.clone();
                    async move {
                        if gone.load(std::sync::atomic::Ordering::SeqCst) {
                            axum::http::StatusCode::NOT_FOUND.into_response()
                        } else {
                            next.run(request).await
                        }
                    }
                },
            ))
        })
    }
    fn pending_publication(core: &LwwClient, store: &PersistentStore) {
        use risunest_sync_wire::lww::{PushRequest, UnitChange};
        let authority = store.lww_binding_authority().unwrap();
        let entries = store.lww_read_outbox(authority, 256).unwrap().entries;
        core.log
            .prepare(&super::super::lww_client::Publication {
                authority,
                request: PushRequest {
                    library_id: core.client.config().library_id,
                    writer_id: store.lww_clock_state().unwrap().writer_id,
                    operation_id: uuid::Uuid::new_v4().to_string(),
                    changes: entries.iter().map(|e| UnitChange { key: e.key.clone(), stamp: e.stamp.clone(), value: e.value.clone() }).collect(),
                },
                entries,
                config: store.server_stored_config().unwrap().unwrap(),
            })
            .unwrap();
    }
    #[test]
    fn an_address_answering_an_empty_not_found_never_blocks_a_binding_change_and_a_missing_operation_still_cancels() {
        let gone = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = vanishing_server(gone.clone());
        let (_root, mut store) = local();
        configure(&server, &store);
        bind(&mut store);
        save(&mut store, &["root", "language"], serde_json::json!("ja"));
        let core = bound_client(&store);
        pending_publication(&core, &store);
        gone.store(true, std::sync::atomic::Ordering::SeqCst);
        fence_for_binding_change(&core, &mut store).unwrap();
        let pending = core.log.pending().unwrap();
        assert_eq!(pending.len(), 1);
        let unreachable = core.settle(&pending[0]).unwrap_err();
        assert_eq!((unreachable.code.as_str(), unreachable.status), ("server-unreachable", 503));
        assert_eq!(core.log.pending().unwrap().len(), 1);
        gone.store(false, std::sync::atomic::Ordering::SeqCst);
        fence_for_binding_change(&core, &mut store).unwrap();
        assert!(core.log.pending().unwrap().is_empty());
    }
    #[test]
    fn a_new_device_switch_away_from_an_address_answering_an_empty_not_found_publishes_on_the_new_server() {
        let gone = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let old = vanishing_server(gone.clone());
        let (_root, mut a) = local();
        configure(&old, &a);
        bind(&mut a);
        save(&mut a, &["root", "language"], serde_json::json!("ja"));
        let core = bound_client(&a);
        pending_publication(&core, &a);
        drop(core);
        gone.store(true, std::sync::atomic::Ordering::SeqCst);
        let live = LocalServerFixture::new();
        let fresh = configure(&live, &a);
        let inspected = inspect(&a, &header(&a)).unwrap();
        assert!(!inspected.previously_bound_library);
        let request = header(&a);
        let staged = stage(&mut a, &request, &inspected.inspection_id, None).unwrap();
        let prepared = prepare_new_device(&mut a, &request, &staged.staging_id).unwrap();
        let result = a
            .lww_replace_target_as_new_device(&request, &staged.staging_id, &prepared.authorization_id)
            .unwrap();
        let next = Header { binding_authority: result.binding_authority, request_id: uuid::Uuid::new_v4().to_string() };
        activate(&mut a, &next, Some((&prepared.authorization_id, &result.writer_id))).unwrap();
        assert_eq!(a.server_stored_config().unwrap().unwrap().device_id, fresh.device_id);
        save(&mut a, &["root", "language"], serde_json::json!("ko"));
        let client = bound_client(&a);
        assert!(client.push(&mut a, &next, &[]).unwrap().is_some());
        assert!(client.log.pending().unwrap().is_empty());
        let (_peer_root, mut peer) = local();
        let receiver = live.client(&peer);
        crate::server_sync::lww_tests::receive_available(&receiver, &mut peer, &[]).unwrap();
        assert_eq!(peer.read_root(None).unwrap().value["language"], "ko");
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
    /// Saves `config` as the candidate under another spelling of its endpoint.
    fn configure_endpoint(
        store: &PersistentStore,
        config: &super::super::client::ServerConfig,
        endpoint: &str,
    ) -> super::super::client::ServerConfig {
        let config = super::super::client::ServerConfig { endpoint: endpoint.into(), ..config.clone() };
        let stored = StoredConfig::persist(store.repository_root(), &config).unwrap();
        OperationLog::open(store.repository_root()).unwrap().save_config("candidate", &stored).unwrap();
        config
    }
    #[test]
    fn a_rebind_under_another_endpoint_spelling_keeps_unsent_edits_and_receive_progress() {
        use crate::server_sync::lww_tests::{drain_publications, receive_available};
        let server = LocalServerFixture::new();
        let (_a_root, mut a) = local();
        let (_b_root, mut b) = local();
        let peer = server.client(&b);
        let registration = configure(&server, &a);
        bind(&mut a);
        let client = bound_client(&a);
        save(&mut b, &["root", "askRemoval"], serde_json::json!(true));
        drain_publications(&peer, &mut b, &[]).unwrap();
        receive_available(&client, &mut a, &[]).unwrap();
        drop(client);
        let progress = a.lww_receive_progress(a.lww_binding_authority().unwrap()).unwrap();
        assert!(!progress.is_empty());
        save(&mut a, &["root", "language"], serde_json::json!("ko"));
        switch_to(&mut a, SyncTarget::None, None);
        let retained = outbox(&a);
        assert_eq!(retained.len(), 1);
        configure_endpoint(&a, &registration, &format!("{}/", server.endpoint));
        let inspected = inspect(&a, &header(&a)).unwrap();
        assert!(inspected.previously_bound_library);
        assert!(!inspected.registration_changed && !inspected.server_restored);
        let state = switch_to(&mut a, SyncTarget::Server("server".into()), Some(inspected.inspection_id));
        activate(&mut a, &Header { binding_authority: state.target_authority, request_id: uuid::Uuid::new_v4().to_string() }, None).unwrap();
        assert_eq!(outbox(&a), retained);
        assert_eq!(
            serde_json::to_value(a.lww_receive_progress(state.target_authority).unwrap()).unwrap(),
            serde_json::to_value(&progress).unwrap(),
        );
        let client = bound_client(&a);
        drain_publications(&client, &mut a, &[]).unwrap();
        assert!(outbox(&a).is_empty());
        assert_eq!(server_unit(&server, &registration, &retained[0].0).unwrap().stamp, retained[0].1);
    }
    #[test]
    fn a_pending_publication_under_an_old_endpoint_spelling_settles_through_the_current_one() {
        use crate::server_sync::lww_tests::drain_publications;
        use risunest_sync_wire::lww::{PushRequest, UnitChange};
        let server = LocalServerFixture::new();
        let (_root, mut a) = local();
        let registration = configure(&server, &a);
        bind(&mut a);
        save(&mut a, &["root", "language"], serde_json::json!("ko"));
        let retained = outbox(&a);
        let authority = a.lww_binding_authority().unwrap();
        let entries = a.lww_read_outbox(authority, 256).unwrap().entries;
        let gone = LocalServerFixture::new();
        let unreachable = super::super::client::ServerConfig { endpoint: gone.endpoint.clone(), ..registration.clone() };
        drop(gone);
        let client = bound_client(&a);
        client.log.prepare(&super::super::lww_client::Publication {
            authority,
            request: PushRequest {
                library_id: registration.library_id.clone(),
                writer_id: a.lww_clock_state().unwrap().writer_id,
                operation_id: uuid::Uuid::new_v4().to_string(),
                changes: entries.iter().map(|e| UnitChange { key: e.key.clone(), stamp: e.stamp.clone(), value: e.value.clone() }).collect(),
            },
            entries,
            config: StoredConfig::persist(a.repository_root(), &unreachable).unwrap(),
        }).unwrap();
        drain_publications(&client, &mut a, &[]).unwrap();
        assert!(client.log.pending().unwrap().is_empty());
        assert!(outbox(&a).is_empty());
        assert_eq!(server_unit(&server, &registration, &retained[0].0).unwrap().stamp, retained[0].1);
    }
    #[test]
    fn a_new_registration_under_another_endpoint_spelling_keeps_the_binding_and_publishes_unsent_edits() {
        let server = LocalServerFixture::new();
        let (_root, mut a) = local();
        let old = configure(&server, &a);
        let bound = bind(&mut a);
        save(&mut a, &["root", "language"], serde_json::json!("ko"));
        let retained = outbox(&a);
        let (fresh, _) = server.candidate(&a);
        let fresh = configure_endpoint(&a, &fresh, &format!("{}/", server.endpoint));
        let inspected = fresh_inspection(&a);
        let prepared = fresh_writer(&mut a, &inspected.inspection_id).unwrap();
        resume(&mut a);
        assert_eq!(a.lww_binding_authority().unwrap(), bound.binding_authority);
        assert_eq!(a.server_stored_config().unwrap().unwrap().device_id, fresh.device_id);
        assert_eq!(a.lww_clock_state().unwrap().writer_id, prepared.writer_id);
        let receipt = push_now(&bound_client(&a), &mut a).unwrap().unwrap();
        assert_eq!(receipt.accepted_keys, vec![retained[0].0.clone()]);
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
    fn a_fresh_writer_swap_stopped_after_its_claim_finishes_at_startup_without_publishing_the_old_writer() {
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
        let reserved: FreshWriterReservation = OperationLog::open(root.path())
            .unwrap()
            .verified("claims", &format!("fresh-writer:{}:{}", fresh.library_id, fresh.device_id))
            .unwrap();
        assert_eq!(reserved.old_writer_id, old_writer);
        resume(&mut a);
        assert_eq!(a.server_stored_config().unwrap().unwrap().device_id, fresh.device_id);
        assert_eq!(a.lww_clock_state().unwrap().writer_id, reserved.writer_id);
        assert_eq!(outbox(&a), retained);
        let log = OperationLog::open(root.path()).unwrap();
        for kind in ["fresh-writer", "fresh-writer-claim"] {
            let id = format!("{kind}:{}:{}", fresh.library_id, fresh.device_id);
            assert!(!log.0.query_row("SELECT EXISTS(SELECT 1 FROM claims WHERE id=?1)", [&id], |r| r.get::<_, bool>(0)).unwrap());
        }
        let receipt = push_now(&bound_client(&a), &mut a).unwrap().unwrap();
        assert_eq!(receipt.accepted_keys, vec![retained[0].0.clone()]);
        assert_eq!(server_unit(&server, &fresh, &retained[0].0).unwrap().stamp, retained[0].1);
    }
    #[test]
    fn a_new_registration_whose_old_credential_cannot_be_read_still_swaps_the_writer_and_publishes_unsent_edits() {
        let server = LocalServerFixture::new();
        let (root, mut a) = local();
        let old = configure(&server, &a);
        bind(&mut a);
        save(&mut a, &["root", "language"], serde_json::json!("ko"));
        let core = bound_client(&a);
        pending_publication(&core, &a);
        drop(core);
        let retained = outbox(&a);
        a.server_stored_config().unwrap().unwrap().remove(root.path()).unwrap();
        fence_stored(&mut a, false).unwrap();
        fence_stored(&mut a, true).unwrap();
        let fresh = configure(&server, &a);
        let inspected = fresh_inspection(&a);
        let prepared = fresh_writer(&mut a, &inspected.inspection_id).unwrap();
        resume(&mut a);
        assert_eq!(a.lww_clock_state().unwrap().writer_id, prepared.writer_id);
        assert_eq!(a.server_stored_config().unwrap().unwrap().device_id, fresh.device_id);
        let client = bound_client(&a);
        let receipt = push_now(&client, &mut a).unwrap().unwrap();
        assert_eq!(receipt.accepted_keys, vec![retained[0].0.clone()]);
        assert!(client.log.pending().unwrap().is_empty());
        assert_eq!(server_unit(&server, &fresh, &retained[0].0).unwrap().stamp, retained[0].1);
        assert!(server.server.authenticate(&old.library_id, &old.token).is_ok());
    }
    #[test]
    fn a_new_device_registration_whose_old_credential_cannot_be_read_replaces_this_device() {
        let server = LocalServerFixture::new();
        let (root, mut a) = local();
        let old = configure(&server, &a);
        bind(&mut a);
        save(&mut a, &["root", "language"], serde_json::json!("ja"));
        let core = bound_client(&a);
        pending_publication(&core, &a);
        drop(core);
        a.server_stored_config().unwrap().unwrap().remove(root.path()).unwrap();
        let fresh = configure(&server, &a);
        let inspected = inspect(&a, &header(&a)).unwrap();
        assert!(inspected.registration_changed);
        let request = header(&a);
        let staged = stage(&mut a, &request, &inspected.inspection_id, None).unwrap();
        let prepared = prepare_new_device(&mut a, &request, &staged.staging_id).unwrap();
        let result = a
            .lww_replace_target_as_new_device(&request, &staged.staging_id, &prepared.authorization_id)
            .unwrap();
        let next = Header { binding_authority: result.binding_authority, request_id: uuid::Uuid::new_v4().to_string() };
        activate(&mut a, &next, Some((&prepared.authorization_id, &result.writer_id))).unwrap();
        assert_eq!(a.server_stored_config().unwrap().unwrap().device_id, fresh.device_id);
        save(&mut a, &["root", "language"], serde_json::json!("ko"));
        let client = bound_client(&a);
        assert!(client.push(&mut a, &next, &[]).unwrap().is_some());
        assert!(client.log.pending().unwrap().is_empty());
        assert!(server.server.authenticate(&old.library_id, &old.token).is_ok());
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
        let staged = stage(&mut a, &request, &inspected.inspection_id, None).unwrap();
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
